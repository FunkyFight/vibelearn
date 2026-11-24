//! Outils de fichiers et de commande que vibelearn exécute lui-même quand il
//! tient la conversation (API OpenAI) : les équivalents de ceux du binaire
//! `claude` (Read, Glob, Grep, Write, Edit, Bash/PowerShell), avec les mêmes
//! noms, pour que l'application les traite de la même façon.
//!
//! Lire et chercher sont libres ; écrire et lancer une commande passent par la
//! demande de permission de l'application.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::paths;

/// Le shell exposé au modèle, sous le nom que l'application connaît.
pub const SHELL: &str = if cfg!(windows) { "PowerShell" } else { "Bash" };

const MAX_OUTPUT: usize = 30_000;
const READ_LINES: usize = 2000;
const LINE_CHARS: usize = 2000;
const LIST_LIMIT: usize = 200;
const SEARCH_FILE_BYTES: u64 = 2_000_000;

/// Libre, ou soumis à l'accord de l'apprenant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Free,
    Ask,
}

pub fn access(name: &str) -> Option<Access> {
    match name {
        "Read" | "Glob" | "Grep" => Some(Access::Free),
        "Write" | "Edit" => Some(Access::Ask),
        n if n == SHELL => Some(Access::Ask),
        _ => None,
    }
}

/// (nom, description, schéma des arguments)
pub fn definitions() -> Vec<(&'static str, String, Value)> {
    let shell_about = if cfg!(windows) {
        "Run a PowerShell command in the project root (Windows PowerShell 5.1). The developer is asked first. \
         Returns stdout, stderr and the exit code. Default timeout 120 s (timeout_ms, at most 600000)."
    } else {
        "Run a bash command in the project root. The developer is asked first. \
         Returns stdout, stderr and the exit code. Default timeout 120 s (timeout_ms, at most 600000)."
    };
    vec![
        (
            "Read",
            "Read a text file. Paths are absolute or relative to the project root. Returns numbered lines \
             (`number<TAB>text`), at most 2000 from `offset` (1-based line number)."
                .to_string(),
            json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string" },
                    "offset": { "type": "integer", "description": "First line to read (1-based)" },
                    "limit": { "type": "integer", "description": "Number of lines to read" },
                },
                "required": ["file_path"],
            }),
        ),
        (
            "Glob",
            "Find files by glob pattern (`**/*.rs`, `src/**/*.cs`), relative to `path` (default: the project root). \
             Files ignored by git are skipped. Newest first, at most 200."
                .to_string(),
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string", "description": "Folder to search in" },
                },
                "required": ["pattern"],
            }),
        ),
        (
            "Grep",
            "Search file contents with a regular expression (Rust regex syntax). output_mode: files_with_matches (default, \
             the paths), content (`path:line:text`), or count. `glob` filters the files (`*.cs`). Files ignored by git are skipped."
                .to_string(),
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string", "description": "File or folder to search in" },
                    "glob": { "type": "string" },
                    "case_insensitive": { "type": "boolean" },
                    "output_mode": { "type": "string", "enum": ["files_with_matches", "content", "count"] },
                    "head_limit": { "type": "integer", "description": "Maximum lines of output (default 200)" },
                },
                "required": ["pattern"],
            }),
        ),
        (
            "Write",
            "Create or overwrite a file. Writing inside .vibelearn/ is free; anywhere else the developer is asked first."
                .to_string(),
            json!({
                "type": "object",
                "properties": { "file_path": { "type": "string" }, "content": { "type": "string" } },
                "required": ["file_path", "content"],
            }),
        ),
        (
            "Edit",
            "Replace `old_string` with `new_string` in a file. `old_string` must appear exactly once, unless replace_all \
             is true. Same permission rule as Write."
                .to_string(),
            json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean" },
                },
                "required": ["file_path", "old_string", "new_string"],
            }),
        ),
        (
            SHELL,
            shell_about.to_string(),
            json!({
                "type": "object",
                "properties": { "command": { "type": "string" }, "timeout_ms": { "type": "integer" } },
                "required": ["command"],
            }),
        ),
    ]
}

/// Exécute un outil. `Err` porte le message d'erreur renvoyé au modèle.
pub async fn run(root: &Path, name: &str, input: &Value) -> Result<String, String> {
    let out = match name {
        "Read" => read(root, input),
        "Glob" => glob(root, input),
        "Grep" => grep(root, input),
        "Write" => write(root, input),
        "Edit" => edit(root, input),
        n if n == SHELL => shell(root, input).await,
        other => Err(format!("unknown tool `{other}`")),
    }?;
    Ok(clip(out))
}

fn clip(mut text: String) -> String {
    if text.len() > MAX_OUTPUT {
        let mut cut = MAX_OUTPUT;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n… (output truncated)");
    }
    text
}

fn arg<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input[key].as_str().ok_or_else(|| format!("missing argument `{key}`"))
}

fn path_arg(root: &Path, input: &Value, key: &str) -> Result<PathBuf, String> {
    Ok(paths::normalize(root, Path::new(arg(input, key)?)))
}

fn rel(root: &Path, path: &Path) -> String {
    paths::display_rel(root, path)
}

fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|b| *b == 0)
}

fn read(root: &Path, input: &Value) -> Result<String, String> {
    let path = path_arg(root, input, "file_path")?;
    if path.is_dir() {
        return Err(format!("{} is a folder: use Glob to list it", rel(root, &path)));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", rel(root, &path)))?;
    if looks_binary(&bytes) {
        return Err(format!("{} is a binary file", rel(root, &path)));
    }
    let text = String::from_utf8_lossy(&bytes);
    let offset = input["offset"].as_u64().unwrap_or(1).max(1) as usize;
    let limit = input["limit"].as_u64().map(|l| l as usize).unwrap_or(READ_LINES).clamp(1, READ_LINES);
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Ok("(empty file)".into());
    }
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate().skip(offset - 1).take(limit) {
        let line = line.trim_end_matches('\r');
        let shown: String = line.chars().take(LINE_CHARS).collect();
        out.push_str(&format!("{:>6}\t{shown}\n", i + 1));
    }
    let last = (offset - 1 + limit).min(lines.len());
    if last < lines.len() {
        out.push_str(&format!("… ({} lines in total; read on with offset={})\n", lines.len(), last + 1));
    }
    if out.is_empty() {
        return Err(format!("offset {offset} is past the end ({} lines)", lines.len()));
    }
    Ok(out)
}

/// Fichiers du dossier, en respectant `.gitignore`.
fn walk(base: &Path) -> impl Iterator<Item = PathBuf> {
    // `.gitignore` compte même hors d'un dépôt git : `.vibelearn/` s'ignore lui-même ainsi.
    let skip = crate::watch::Skip::new(base);
    ignore::WalkBuilder::new(base)
        .hidden(false)
        .require_git(false)
        .filter_entry(move |e| e.file_name() != ".git" && !skip.skips(e.path()))
        .build()
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(|e| e.into_path())
}

fn glob_matcher(pattern: &str) -> Result<globset::GlobMatcher, String> {
    globset::GlobBuilder::new(pattern)
        .literal_separator(false)
        .build()
        .map(|g| g.compile_matcher())
        .map_err(|e| format!("invalid glob `{pattern}`: {e}"))
}

/// Le motif s'applique au chemin relatif, ou au seul nom s'il n'a pas de `/`.
fn glob_hit(matcher: &globset::GlobMatcher, pattern: &str, rel_path: &str) -> bool {
    matcher.is_match(rel_path) || (!pattern.contains('/') && rel_path.rsplit('/').next().is_some_and(|n| matcher.is_match(n)))
}

fn base_dir(root: &Path, input: &Value) -> PathBuf {
    input["path"].as_str().filter(|p| !p.is_empty()).map(|p| paths::normalize(root, Path::new(p))).unwrap_or_else(|| root.to_path_buf())
}

fn glob(root: &Path, input: &Value) -> Result<String, String> {
    let pattern = arg(input, "pattern")?;
    let matcher = glob_matcher(pattern)?;
    let base = base_dir(root, input);
    let mut found: Vec<(SystemTime, PathBuf)> = walk(&base)
        .filter(|p| {
            let r = p.strip_prefix(&base).unwrap_or(p).to_string_lossy().replace('\\', "/");
            glob_hit(&matcher, pattern, &r)
        })
        .map(|p| (std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH), p))
        .collect();
    if found.is_empty() {
        return Ok("No files found".into());
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    let total = found.len();
    let mut out: Vec<String> = found.into_iter().take(LIST_LIMIT).map(|(_, p)| rel(root, &p)).collect();
    if total > LIST_LIMIT {
        out.push(format!("… ({total} files, first {LIST_LIMIT} shown)"));
    }
    Ok(out.join("\n"))
}

fn grep(root: &Path, input: &Value) -> Result<String, String> {
    let pattern = arg(input, "pattern")?;
    let re = regex::RegexBuilder::new(pattern)
        .case_insensitive(input["case_insensitive"].as_bool().unwrap_or(false))
        .build()
        .map_err(|e| format!("invalid regex: {e}"))?;
    let filter = match input["glob"].as_str().filter(|g| !g.is_empty()) {
        Some(g) => Some((glob_matcher(g)?, g.to_string())),
        None => None,
    };
    let mode = input["output_mode"].as_str().unwrap_or("files_with_matches");
    let limit = input["head_limit"].as_u64().map(|n| n as usize).unwrap_or(LIST_LIMIT).max(1);
    let base = base_dir(root, input);
    let files: Vec<PathBuf> = if base.is_file() { vec![base.clone()] } else { walk(&base).collect() };

    let mut out = Vec::new();
    let mut truncated = false;
    for file in files {
        if let Some((m, g)) = &filter {
            let r = file.strip_prefix(&base).unwrap_or(&file).to_string_lossy().replace('\\', "/");
            if !glob_hit(m, g, &r) {
                continue;
            }
        }
        if std::fs::metadata(&file).map(|m| m.len() > SEARCH_FILE_BYTES).unwrap_or(true) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&file) else { continue };
        if looks_binary(&bytes) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let name = rel(root, &file);
        match mode {
            "content" => {
                for (i, line) in text.lines().enumerate() {
                    if re.is_match(line) {
                        let shown: String = line.trim_end_matches('\r').chars().take(LINE_CHARS).collect();
                        out.push(format!("{name}:{}:{shown}", i + 1));
                    }
                }
            }
            "count" => {
                let n = text.lines().filter(|l| re.is_match(l)).count();
                if n > 0 {
                    out.push(format!("{name}:{n}"));
                }
            }
            _ => {
                if re.is_match(&text) {
                    out.push(name);
                }
            }
        }
        if out.len() >= limit {
            truncated = true;
            out.truncate(limit);
            break;
        }
    }
    if out.is_empty() {
        return Ok("No matches found".into());
    }
    if truncated {
        out.push(format!("… (stopped at {limit} lines; narrow the search or raise head_limit)"));
    }
    Ok(out.join("\n"))
}

fn write(root: &Path, input: &Value) -> Result<String, String> {
    let path = path_arg(root, input, "file_path")?;
    let content = arg(input, "content")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, content).map_err(|e| format!("cannot write {}: {e}", rel(root, &path)))?;
    Ok(format!("Wrote {} ({} lines)", rel(root, &path), content.lines().count()))
}

fn edit(root: &Path, input: &Value) -> Result<String, String> {
    let path = path_arg(root, input, "file_path")?;
    let mut old = arg(input, "old_string")?.to_string();
    let mut new = arg(input, "new_string")?.to_string();
    if old.is_empty() {
        return Err("old_string is empty: use Write to create a file".into());
    }
    if old == new {
        return Err("old_string and new_string are the same".into());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", rel(root, &path)))?;
    // Un fichier en CRLF : le modèle écrit ses passages en LF.
    if text.contains("\r\n") && !text.contains(&old) && old.contains('\n') && !old.contains('\r') {
        old = old.replace('\n', "\r\n");
        new = new.replace("\r\n", "\n").replace('\n', "\r\n");
    }
    let count = text.matches(&old).count();
    let all = input["replace_all"].as_bool().unwrap_or(false);
    match count {
        0 => return Err(format!("old_string was not found in {}", rel(root, &path))),
        n if n > 1 && !all => {
            return Err(format!("old_string appears {n} times in {}: add context to make it unique, or set replace_all", rel(root, &path)));
        }
        _ => {}
    }
    let updated = if all { text.replace(&old, &new) } else { text.replacen(&old, &new, 1) };
    std::fs::write(&path, updated).map_err(|e| format!("cannot write {}: {e}", rel(root, &path)))?;
    Ok(format!("Edited {} ({} replacement{})", rel(root, &path), if all { count } else { 1 }, if all && count > 1 { "s" } else { "" }))
}

async fn shell(root: &Path, input: &Value) -> Result<String, String> {
    let command = arg(input, "command")?;
    let timeout = Duration::from_millis(input["timeout_ms"].as_u64().unwrap_or(120_000).clamp(1_000, 600_000));
    let mut cmd = if cfg!(windows) {
        let mut c = tokio::process::Command::new("powershell.exe");
        c.args(["-NoProfile", "-NonInteractive", "-Command", command]);
        c
    } else {
        // L'outil s'appelle Bash : le modèle écrit du bash. `sh` seulement s'il n'y en a pas.
        let has_bash = std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("bash").is_file()));
        let mut c = tokio::process::Command::new(if has_bash { "bash" } else { "sh" });
        c.args(["-c", command]);
        c
    };
    cmd.current_dir(root).stdin(std::process::Stdio::null()).kill_on_drop(true);
    let output = match tokio::time::timeout(timeout, cmd.output()).await {
        Err(_) => return Err(format!("the command did not finish within {} s and was stopped", timeout.as_secs())),
        Ok(Err(e)) => return Err(format!("could not start the command: {e}")),
        Ok(Ok(o)) => o,
    };
    let mut out = String::new();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stdout.trim().is_empty() {
        out.push_str(stdout.trim_end());
        out.push('\n');
    }
    if !stderr.trim().is_empty() {
        out.push_str("[stderr]\n");
        out.push_str(stderr.trim_end());
        out.push('\n');
    }
    out.push_str(&format!("[exit code {}]", output.status.code().map(|c| c.to_string()).unwrap_or_else(|| "?".into())));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vibelearn-tools-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/game")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {\n    let speed = 3;\n    println!(\"{speed}\");\n}\n").unwrap();
        std::fs::write(dir.join("src/game/enemy.cs"), "class Enemy {\r\n    int hp = 3;\r\n    int speed = 2;\r\n}\r\n").unwrap();
        dir
    }

    #[tokio::test]
    async fn read_numbers_lines_and_pages() {
        let root = project("read");
        let out = run(&root, "Read", &json!({"file_path": "src/main.rs"})).await.unwrap();
        assert!(out.starts_with("     1\tfn main() {\n     2\t    let speed = 3;"), "{out}");
        let out = run(&root, "Read", &json!({"file_path": "src/main.rs", "offset": 2, "limit": 1})).await.unwrap();
        assert!(out.contains("     2\t    let speed = 3;") && out.contains("offset=3"), "{out}");
        assert!(run(&root, "Read", &json!({"file_path": "src/nope.rs"})).await.is_err());
        assert!(run(&root, "Read", &json!({"file_path": "src"})).await.unwrap_err().contains("Glob"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn glob_and_grep_find_by_name_and_content() {
        let root = project("search");
        let out = run(&root, "Glob", &json!({"pattern": "*.cs"})).await.unwrap();
        assert_eq!(out, "src/game/enemy.cs");
        let out = run(&root, "Glob", &json!({"pattern": "src/**/*.rs"})).await.unwrap();
        assert_eq!(out, "src/main.rs");
        let out = run(&root, "Grep", &json!({"pattern": "speed"})).await.unwrap();
        assert_eq!(out.lines().count(), 2, "{out}");
        let out = run(&root, "Grep", &json!({"pattern": "SPEED = \\d", "case_insensitive": true, "glob": "*.cs", "output_mode": "content"}))
            .await
            .unwrap();
        assert_eq!(out, "src/game/enemy.cs:3:    int speed = 2;");
        let out = run(&root, "Grep", &json!({"pattern": "speed", "output_mode": "count", "path": "src/main.rs"})).await.unwrap();
        assert_eq!(out, "src/main.rs:2");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn edit_needs_a_unique_passage_and_keeps_crlf() {
        let root = project("edit");
        let err = run(&root, "Edit", &json!({"file_path": "src/game/enemy.cs", "old_string": "int", "new_string": "float"})).await.unwrap_err();
        assert!(err.contains("2 times"), "{err}");
        run(&root, "Edit", &json!({"file_path": "src/game/enemy.cs", "old_string": "int hp = 3;\n    int speed", "new_string": "int hp = 5;\n    int pace"}))
            .await
            .unwrap();
        let text = std::fs::read_to_string(root.join("src/game/enemy.cs")).unwrap();
        assert_eq!(text, "class Enemy {\r\n    int hp = 5;\r\n    int pace = 2;\r\n}\r\n");
        run(&root, "Write", &json!({"file_path": ".vibelearn/demos/a.txt", "content": "x\ny\n"})).await.unwrap();
        assert_eq!(std::fs::read_to_string(root.join(".vibelearn/demos/a.txt")).unwrap(), "x\ny\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn the_shell_reports_output_and_exit_code() {
        let root = project("shell");
        let out = run(&root, SHELL, &json!({"command": "echo vibe"})).await.unwrap();
        assert!(out.starts_with("vibe") && out.ends_with("[exit code 0]"), "{out}");
        assert_eq!(access("Read"), Some(Access::Free));
        assert_eq!(access(SHELL), Some(Access::Ask));
        assert_eq!(access("ask"), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
