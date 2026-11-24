//! Lecture et retouche des fichiers de l'apprenant, à l'identique.
//!
//! vibelearn n'écrit plus d'explications dans le code (elles s'affichent en
//! panneau de VS Code, voir `threads.rs`). Il ne touche au code que pour :
//! - retirer une ligne `//? question` ou `//n fonctionnalité` que l'apprenant y
//!   a écrite pour lui ;
//! - nettoyer les commentaires `//» …` laissés par une ancienne version.
//!
//! Une ligne `//// …` (pseudo-code demandé) est lue mais jamais retirée.
//!
//! Le reste du fichier n'est jamais modifié : fins de ligne (CRLF ou LF), BOM
//! et indentation sont conservés tels quels.

use ignore::WalkBuilder;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::watch;

/// Marque des anciens commentaires de vibelearn (`//» …`).
pub const MARK: char = '»';
/// Marque des lignes où l'apprenant s'adresse à vibelearn (`//? …`).
pub const ASK: char = '?';
/// Marque des lignes où l'apprenant demande la prochaine étape d'une
/// fonctionnalité (`//n wall jump`).
pub const NEXT: char = 'n';
/// Marque des lignes où l'apprenant demande à apprendre à faire quelque chose
/// (`//t les coroutines`) : une mini-leçon, là, dans son code.
pub const TEACH: char = 't';

/// Marque d'une demande de pseudo-code : le début de commentaire répété
/// (`////` en C, Rust, C#…, `####` en Python, `----` en Lua). Elle reste dans
/// le code ; le pseudo-code s'affiche à côté, sur plusieurs lignes.
pub fn pseudo_mark(style: Style) -> Option<String> {
    match style.open.len() {
        1 => Some(style.open.repeat(4)),
        2 if style.close.is_empty() => Some(style.open.repeat(2)),
        _ => None,
    }
}

const MAX_BYTES: u64 = 1024 * 1024;
const CHANGED: &str = "the file changed during the edit";

/// Syntaxe d'une ligne de commentaire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub open: &'static str,
    pub close: &'static str,
}

/// Syntaxe de commentaire d'un fichier source, d'après son extension.
pub fn style_for(path: &Path) -> Option<Style> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let (open, close) = match ext.as_str() {
        "rs" | "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "cs" | "java" | "kt" | "kts" | "scala" | "swift" | "go"
        | "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" | "dart" | "php" | "zig" | "glsl" | "vert" | "frag" | "hlsl" | "fx"
        | "shader" | "wgsl" | "scss" | "less" | "proto" | "groovy" | "gradle" | "jsonc" => ("//", ""),
        "py" | "pyw" | "rb" | "sh" | "bash" | "zsh" | "fish" | "ps1" | "psm1" | "r" | "pl" | "toml" | "yaml" | "yml"
        | "gd" | "nim" | "ex" | "exs" | "jl" | "cmake" | "tf" | "coffee" | "cr" => ("#", ""),
        "lua" | "sql" | "hs" | "elm" | "ada" | "adb" | "ads" => ("--", ""),
        "lisp" | "clj" | "cljs" | "el" | "scm" | "rkt" | "asm" => (";", ""),
        "vb" | "bas" | "vbs" => ("'", ""),
        "css" => ("/*", " */"),
        "html" | "htm" | "xml" | "svg" | "xaml" | "vue" | "svelte" => ("<!--", " -->"),
        _ => return None,
    };
    Some(Style { open, close })
}

/// Le contenu entre la marque et la fin de commentaire.
fn after_mark(line: &str, style: Style, mark: char) -> Option<&str> {
    let rest = line.trim_start().strip_prefix(style.open)?.strip_prefix(mark)?;
    Some(match style.close.trim() {
        "" => rest,
        close => rest.trim_end().strip_suffix(close).unwrap_or(rest),
    })
}

/// Texte d'une ancienne ligne écrite par vibelearn (sans la marque), sinon rien.
pub fn marked_text(line: &str, style: Style) -> Option<&str> {
    let rest = after_mark(line, style, MARK)?;
    Some(rest.strip_prefix(' ').unwrap_or(rest).trim_end())
}

/// Texte d'une ligne `//? …` écrite par l'apprenant pour vibelearn.
pub fn question_text(line: &str, style: Style) -> Option<&str> {
    let text = after_mark(line, style, ASK)?.trim();
    (!text.is_empty()).then_some(text)
}

/// Texte d'une ligne `//n …` : la fonctionnalité dont l'apprenant veut la
/// prochaine étape. Un espace doit suivre le `n`, pour qu'un commentaire
/// ordinaire (`//nombre de vies`) ne compte pas.
pub fn next_text(line: &str, style: Style) -> Option<&str> {
    word_after(line, style, NEXT)
}

/// Texte d'une ligne `//t …` : ce que l'apprenant veut apprendre à faire.
/// Un espace doit suivre le `t` (`//todo` ne compte pas).
pub fn teach_text(line: &str, style: Style) -> Option<&str> {
    word_after(line, style, TEACH)
}

/// Forme du pseudo-code demandé.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PseudoStyle {
    /// `//// …` : des étapes en langage naturel, chacune précise.
    Natural,
    /// `////! …` : du pseudo-code formel (affectations, appels, conditions).
    Formal,
}

impl PseudoStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Natural => "natural",
            Self::Formal => "formal",
        }
    }
}

/// Texte d'une ligne `//// …` (ou `////! …`) : ce que l'apprenant veut
/// accomplir, et la forme du pseudo-code voulue. Un espace doit suivre la
/// marque (`////////` ne compte pas).
pub fn pseudo_text(line: &str, style: Style) -> Option<(PseudoStyle, &str)> {
    let mark = pseudo_mark(style)?;
    let rest = line.trim_start().strip_prefix(mark.as_str())?;
    let (kind, rest) = match rest.strip_prefix('!') {
        Some(rest) => (PseudoStyle::Formal, rest),
        None => (PseudoStyle::Natural, rest),
    };
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let text = rest.trim();
    (!text.is_empty()).then_some((kind, text))
}

fn word_after(line: &str, style: Style, mark: char) -> Option<&str> {
    let rest = after_mark(line, style, mark)?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let text = rest.trim();
    (!text.is_empty()).then_some(text)
}

/// Retire d'un texte les lignes `//» …` (pour les diffs).
pub fn strip_marked(text: &str, style: Style) -> String {
    if !text.contains(MARK) {
        return text.to_string();
    }
    text.split_inclusive('\n')
        .filter(|line| marked_text(line.trim_end_matches(['\n', '\r']), style).is_none())
        .collect()
}

/// Sépare `fichier:42` en chemin et numéro de ligne (sans confondre avec `C:\…`).
pub fn split_line(target: &str) -> (&str, Option<usize>) {
    match target.rsplit_once(':') {
        Some((path, line)) if !path.is_empty() && !line.is_empty() && line.chars().all(|c| c.is_ascii_digit()) => {
            (path, line.parse().ok())
        }
        _ => (target, None),
    }
}

/// Une ligne qui est du code (ni ancien commentaire de vibelearn, ni demande à vibelearn).
fn is_code(line: &str, style: Option<Style>) -> bool {
    style.is_none_or(|s| marked_text(line, s).is_none() && question_text(line, s).is_none() && next_text(line, s).is_none() && teach_text(line, s).is_none() && pseudo_text(line, s).is_none())
}

/// La ligne qui contient `anchor` (la plus proche de `near`, indice à partir de 0).
pub fn find_anchor(lines: &[String], style: Option<Style>, anchor: &str, near: Option<usize>) -> Option<usize> {
    let Some(wanted) = anchor.lines().map(str::trim).find(|l| !l.is_empty()) else {
        return near.filter(|_| !lines.is_empty()).map(|i| i.min(lines.len() - 1));
    };
    let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let wanted = squash(wanted);
    let matching = |pred: &dyn Fn(&str) -> bool| -> Vec<usize> {
        (0..lines.len()).filter(|&i| is_code(&lines[i], style) && pred(&squash(&lines[i]))).collect()
    };
    let mut found = matching(&|l| l == wanted);
    if found.is_empty() {
        found = matching(&|l| l.contains(&wanted));
    }
    match near {
        Some(target) => found.into_iter().min_by_key(|i| i.abs_diff(target)),
        None => found.first().copied(),
    }
}

// ---------------------------------------------------------------------------
// Fichier texte relu et réécrit à l'identique
// ---------------------------------------------------------------------------

pub struct TextFile {
    bom: bool,
    crlf: bool,
    newline_at_end: bool,
    pub lines: Vec<String>,
    stamp: Option<SystemTime>,
}

impl TextFile {
    pub fn load(path: &Path) -> Result<Self, String> {
        let meta = std::fs::metadata(path).map_err(|_| format!("{} does not exist", path.display()))?;
        if !meta.is_file() {
            return Err(format!("{} is not a file", path.display()));
        }
        if meta.len() > MAX_BYTES {
            return Err(format!("{} is too large", path.display()));
        }
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let bom = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
        let text = String::from_utf8(bytes[if bom { 3 } else { 0 }..].to_vec())
            .map_err(|_| format!("{} is not UTF-8 text", path.display()))?;
        if text.contains('\0') {
            return Err(format!("{} is not a text file", path.display()));
        }
        let newline_at_end = text.ends_with('\n');
        let mut lines: Vec<String> = if text.is_empty() {
            Vec::new()
        } else {
            text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect()
        };
        if newline_at_end {
            lines.pop();
        }
        Ok(Self { bom, crlf: text.contains("\r\n"), newline_at_end, lines, stamp: meta.modified().ok() })
    }

    fn save(&self, path: &Path) -> Result<(), String> {
        // L'apprenant vient peut-être de sauvegarder : on ne réécrit pas par-dessus.
        let now = std::fs::metadata(path).ok().and_then(|m| m.modified().ok());
        if now != self.stamp {
            return Err(CHANGED.into());
        }
        let eol = if self.crlf { "\r\n" } else { "\n" };
        let mut text = self.lines.join(eol);
        if self.newline_at_end && !self.lines.is_empty() {
            text.push_str(eol);
        }
        let mut bytes = Vec::with_capacity(text.len() + 3);
        if self.bom {
            bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        }
        bytes.extend_from_slice(text.as_bytes());
        std::fs::write(path, bytes).map_err(|e| e.to_string())
    }
}

/// Relit le fichier, applique `f`, et réécrit seulement s'il a changé. Si le
/// fichier bouge entre la lecture et l'écriture, on recommence.
pub fn edit<R>(path: &Path, mut f: impl FnMut(&mut TextFile) -> Result<R, String>) -> Result<R, String> {
    for _ in 0..5 {
        let mut file = TextFile::load(path)?;
        let before = file.lines.clone();
        let result = f(&mut file)?;
        if file.lines == before {
            return Ok(result);
        }
        match file.save(path) {
            Ok(()) => return Ok(result),
            Err(e) if e == CHANGED => std::thread::sleep(Duration::from_millis(40)),
            Err(e) => return Err(e),
        }
    }
    Err(format!("{} keeps changing", path.display()))
}

/// Une demande `//? …` ou `//n …` relevée dans le code (et retirée du fichier).
#[derive(Debug, Clone)]
pub struct CodeQuestion {
    /// Indice (à partir de 0) de la ligne où elle était.
    pub index: usize,
    pub text: String,
    /// Première ligne de code sous la demande : texte et indice.
    pub next_code: Option<(String, usize)>,
}

/// Retire les lignes `//? …` d'un fichier et les renvoie (plusieurs lignes
/// qui se suivent forment une seule question).
pub fn take_questions(path: &Path) -> Vec<CodeQuestion> {
    take_lines(path, question_text)
}

/// Retire les lignes `//n …` d'un fichier et les renvoie : les fonctionnalités
/// dont l'apprenant veut la prochaine étape.
pub fn take_next_steps(path: &Path) -> Vec<CodeQuestion> {
    take_lines(path, next_text)
}

/// Retire les lignes `//t …` d'un fichier et les renvoie : ce que l'apprenant
/// veut apprendre à faire, en mini-leçon.
pub fn take_lessons(path: &Path) -> Vec<CodeQuestion> {
    take_lines(path, teach_text)
}

/// Une demande de pseudo-code (`//// …`), laissée dans le fichier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PseudoRequest {
    /// Indice (à partir de 0) de la dernière ligne de la demande : le
    /// pseudo-code s'affiche à partir d'elle.
    pub line: usize,
    /// Plusieurs lignes `////` qui se suivent forment une seule demande.
    pub text: String,
    /// Celle de la première ligne de la demande.
    pub style: PseudoStyle,
}

/// Les lignes `//// …` d'un fichier, sans y toucher.
pub fn pseudo_requests(path: &Path) -> Vec<PseudoRequest> {
    let Some(style) = style_for(path) else { return Vec::new() };
    let Ok(file) = TextFile::load(path) else { return Vec::new() };
    let mut out = Vec::new();
    let mut i = 0;
    while i < file.lines.len() {
        let Some((kind, first)) = pseudo_text(&file.lines[i], style) else {
            i += 1;
            continue;
        };
        let mut parts = vec![first.to_string()];
        while let Some((_, more)) = file.lines.get(i + 1).and_then(|l| pseudo_text(l, style)) {
            parts.push(more.to_string());
            i += 1;
        }
        out.push(PseudoRequest { line: i, text: parts.join(" "), style: kind });
        i += 1;
    }
    out
}

fn take_lines(path: &Path, text_of: for<'a> fn(&'a str, Style) -> Option<&'a str>) -> Vec<CodeQuestion> {
    let Some(style) = style_for(path) else { return Vec::new() };
    let result = edit(path, |file| {
        let mut out: Vec<String> = Vec::with_capacity(file.lines.len());
        let mut found: Vec<(usize, String)> = Vec::new();
        let mut i = 0;
        while i < file.lines.len() {
            let Some(first) = text_of(&file.lines[i], style) else {
                out.push(file.lines[i].clone());
                i += 1;
                continue;
            };
            let mut parts = vec![first.to_string()];
            i += 1;
            while let Some(more) = file.lines.get(i).and_then(|l| text_of(l, style)) {
                parts.push(more.to_string());
                i += 1;
            }
            found.push((out.len(), parts.join(" ")));
        }
        if !found.is_empty() {
            file.lines = out;
        }
        Ok(found
            .into_iter()
            .map(|(index, text)| {
                let next_code = file.lines[index..]
                    .iter()
                    .enumerate()
                    .find(|(_, l)| !l.trim().is_empty() && is_code(l, Some(style)))
                    .map(|(k, l)| (l.trim().to_string(), index + k));
                CodeQuestion { index, text, next_code }
            })
            .collect::<Vec<_>>())
    });
    result.unwrap_or_default()
}

/// Retire du projet les commentaires `//» …` laissés par une ancienne version.
pub fn strip_project(root: &Path) -> usize {
    let skip = watch::Skip::new(root);
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(move |e| !skip.skips(e.path()))
        .build();
    let mut cleaned = 0;
    for entry in walker.flatten().take(20_000) {
        let path = entry.path();
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Some(style) = style_for(path) else { continue };
        if std::fs::metadata(path).map(|m| m.len() > MAX_BYTES).unwrap_or(true) {
            continue;
        }
        let needle = format!("{}{MARK}", style.open);
        if !std::fs::read(path).is_ok_and(|b| String::from_utf8_lossy(&b).contains(&needle)) {
            continue;
        }
        let stripped = edit(path, |file| {
            let before = file.lines.len();
            file.lines.retain(|l| marked_text(l, style).is_none());
            Ok(file.lines.len() != before)
        });
        if stripped.unwrap_or(false) {
            cleaned += 1;
        }
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_project(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vibelearn-annotate-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        dir
    }

    #[test]
    fn a_question_written_in_the_code_is_taken_out_without_touching_the_rest() {
        let root = temp_project("question");
        let file = root.join("src/Game1.cs");
        let code = "\u{feff}class Game1\r\n{\r\n    //? pourquoi un float\r\n    //? et pas un int ?\r\n    float y;\r\n}\r\n";
        fs::write(&file, code).unwrap();
        let qs = take_questions(&file);
        assert_eq!(qs.len(), 1);
        assert_eq!(qs[0].text, "pourquoi un float et pas un int ?");
        assert_eq!(qs[0].index, 2);
        assert_eq!(qs[0].next_code, Some(("float y;".to_string(), 2)));
        assert_eq!(fs::read_to_string(&file).unwrap(), "\u{feff}class Game1\r\n{\r\n    float y;\r\n}\r\n", "BOM et CRLF conservés");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_next_step_request_is_taken_out_but_an_ordinary_comment_starting_with_n_stays() {
        let root = temp_project("next");
        let file = root.join("src/player.rs");
        fs::write(&file, "fn update() {
    //n wall jump
    //nombre de sauts
    move_x();
}
").unwrap();
        assert!(take_questions(&file).is_empty(), "un `//n` n'est pas une question");
        let steps = take_next_steps(&file);
        assert_eq!(steps.len(), 1);
        assert_eq!((steps[0].text.as_str(), steps[0].index), ("wall jump", 1));
        assert_eq!(fs::read_to_string(&file).unwrap(), "fn update() {
    //nombre de sauts
    move_x();
}
");
        assert_eq!(next_text("# n dash", Style { open: "#", close: "" }), None, "la marque colle au commentaire");
        assert_eq!(next_text("#n dash", Style { open: "#", close: "" }), Some("dash"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_lesson_request_is_taken_out_but_a_todo_stays() {
        let root = temp_project("teach");
        let file = root.join("src/Player.cs");
        fs::write(&file, "void Update() {\n    //t le pattern State\n    Move();\n    //todo dash\n}\n").unwrap();
        assert!(take_next_steps(&file).is_empty() && take_questions(&file).is_empty());
        let lessons = take_lessons(&file);
        assert_eq!(lessons.len(), 1);
        assert_eq!((lessons[0].text.as_str(), lessons[0].index), ("le pattern State", 1));
        assert_eq!(lessons[0].next_code, Some(("Move();".to_string(), 1)));
        assert_eq!(fs::read_to_string(&file).unwrap(), "void Update() {\n    Move();\n    //todo dash\n}\n");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pseudo_code_request_stays_in_the_file_and_spans_its_lines() {
        let root = temp_project("pseudo");
        let file = root.join("src/Player.cs");
        let code = "void Update() {\r\n    //// faire sauter le joueur\r\n    //// quand on appuie sur espace\r\n    Move();\r\n    //////// séparateur\r\n    // commentaire ordinaire\r\n    ////! freiner\r\n}\r\n";
        fs::write(&file, code).unwrap();
        let reqs = pseudo_requests(&file);
        assert_eq!(
            reqs,
            vec![
                PseudoRequest { line: 2, text: "faire sauter le joueur quand on appuie sur espace".into(), style: PseudoStyle::Natural },
                PseudoRequest { line: 6, text: "freiner".into(), style: PseudoStyle::Formal },
            ]
        );
        assert!(take_questions(&file).is_empty() && take_lessons(&file).is_empty() && take_next_steps(&file).is_empty());
        assert_eq!(fs::read_to_string(&file).unwrap(), code, "la demande reste dans le code");
        let py = Style { open: "#", close: "" };
        assert_eq!(pseudo_text("    #### lire le fichier", py), Some((PseudoStyle::Natural, "lire le fichier")));
        assert_eq!(pseudo_text("####! lire", py), Some((PseudoStyle::Formal, "lire")));
        assert_eq!(pseudo_text("////!! x", Style { open: "//", close: "" }), None);
        assert_eq!(pseudo_text("## titre", py), None);
        assert_eq!(pseudo_text("---- x", Style { open: "--", close: "" }), Some((PseudoStyle::Natural, "x")));
        assert_eq!(pseudo_mark(Style { open: "/*", close: " */" }), None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn leftover_comments_from_an_older_version_are_cleaned() {
        let root = temp_project("strip");
        fs::write(root.join("src/a.py"), "#» vieux\nx = 1\n").unwrap();
        fs::write(root.join("src/b.css"), "/*» vieux */\na { }\n").unwrap();
        fs::write(root.join("notes.md"), "» ceci est à l'apprenant\n").unwrap();
        assert_eq!(strip_project(&root), 2);
        assert_eq!(fs::read_to_string(root.join("src/a.py")).unwrap(), "x = 1\n");
        assert_eq!(fs::read_to_string(root.join("src/b.css")).unwrap(), "a { }\n");
        assert!(fs::read_to_string(root.join("notes.md")).unwrap().starts_with('»'));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_anchor_is_found_by_its_text_nearest_to_the_hint() {
        let lines: Vec<String> = ["fn a() {", "}", "fn b() {", "    }", "}"].iter().map(|s| s.to_string()).collect();
        assert_eq!(find_anchor(&lines, None, "fn b()", None), Some(2));
        assert_eq!(find_anchor(&lines, None, "}", Some(5)), Some(4));
        assert_eq!(find_anchor(&lines, None, "absent", None), None);
    }

    #[test]
    fn a_target_can_carry_a_line_number_without_breaking_windows_paths() {
        assert_eq!(split_line("Game1.cs:42"), ("Game1.cs", Some(42)));
        assert_eq!(split_line("src/main.rs"), ("src/main.rs", None));
        assert_eq!(split_line("C:\\proj\\a.rs"), ("C:\\proj\\a.rs", None));
        assert_eq!(split_line("C:\\proj\\a.rs:7"), ("C:\\proj\\a.rs", Some(7)));
    }
}
