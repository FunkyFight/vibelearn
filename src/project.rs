//! Reconnaissance rapide du projet, donnée au tuteur au démarrage : pile
//! technique, arborescence, historique git récent.

use ignore::WalkBuilder;
use std::path::Path;
use std::process::Command;

use crate::paths;

const MAX_TREE_ENTRIES: usize = 160;

pub struct ProjectFacts {
    pub stack: Vec<&'static str>,
    pub check_command: Option<&'static str>,
    pub tree: String,
    pub git: String,
}

pub fn gather(root: &Path) -> ProjectFacts {
    let (stack, check_command) = detect_stack(root);
    ProjectFacts { stack, check_command, tree: file_tree(root), git: git_summary(root) }
}

fn detect_stack(root: &Path) -> (Vec<&'static str>, Option<&'static str>) {
    let has = |f: &str| root.join(f).exists();
    let mut stack = Vec::new();
    let mut check = None;
    let mut add = |name: &'static str, cmd: Option<&'static str>| {
        stack.push(name);
        if check.is_none() {
            check = cmd;
        }
    };
    if has("Cargo.toml") {
        add("Rust (Cargo)", Some("cargo check --quiet"));
    }
    if has("package.json") {
        let ts = has("tsconfig.json");
        add(
            if ts { "TypeScript (npm)" } else { "JavaScript (npm)" },
            if ts { Some("npx tsc --noEmit") } else { None },
        );
    }
    if has("pyproject.toml") || has("requirements.txt") || has("setup.py") {
        add("Python", None);
    }
    if has("go.mod") {
        add("Go", Some("go build ./..."));
    }
    if has("pom.xml") || has("build.gradle") || has("build.gradle.kts") {
        add("JVM (Java/Kotlin)", None);
    }
    if has("CMakeLists.txt") || has("Makefile") {
        add("C/C++ (make/cmake)", None);
    }
    if has("Gemfile") {
        add("Ruby", None);
    }
    if has("composer.json") {
        add("PHP", None);
    }
    if has("mix.exs") {
        add("Elixir", None);
    }
    if has("pubspec.yaml") {
        add("Dart/Flutter", None);
    }
    if has("default.project.json") || has("wally.toml") {
        add("Roblox (Rojo/Luau)", None);
    }
    (stack, check)
}

fn file_tree(root: &Path) -> String {
    let mut lines = Vec::new();
    let mut truncated = false;
    let skip = crate::watch::Skip::new(root);
    let walker = WalkBuilder::new(root)
        .max_depth(Some(4))
        .hidden(true)
        .require_git(false)
        .filter_entry(move |e| !skip.skips(e.path()))
        .sort_by_file_path(|a, b| a.cmp(b))
        .build();
    for entry in walker.flatten() {
        let path = entry.path();
        if path == root {
            continue;
        }
        let rel = paths::display_rel(root, path);
        if rel.starts_with(paths::SANDBOX) || rel.starts_with("target") || rel.starts_with("node_modules") {
            continue;
        }
        if lines.len() >= MAX_TREE_ENTRIES {
            truncated = true;
            break;
        }
        let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
        if is_dir {
            lines.push(format!("{rel}/"));
        } else {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            lines.push(format!("{rel} ({})", human_size(size)));
        }
    }
    if lines.is_empty() {
        return "(empty project)".to_string();
    }
    if truncated {
        lines.push("… (truncated, use Glob for the rest)".to_string());
    }
    lines.join("\n")
}

fn human_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} kB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(root).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

fn git_summary(root: &Path) -> String {
    let Some(branch) = git(root, &["rev-parse", "--abbrev-ref", "HEAD"]) else {
        return "(not a git repository, or no commits yet)".to_string();
    };
    let mut out = format!("branch: {branch}\n");
    if let Some(count) = git(root, &["rev-list", "--count", "HEAD"]) {
        out.push_str(&format!("commits: {count}\n"));
    }
    if let Some(log) = git(root, &["log", "-n", "15", "--pretty=format:%ad %s", "--date=short"]) {
        out.push_str("recent commits:\n");
        for line in log.lines() {
            out.push_str(&format!("  {line}\n"));
        }
    }
    if let Some(status) = git(root, &["status", "--short"]) {
        out.push_str("uncommitted:\n");
        for line in status.lines().take(20) {
            out.push_str(&format!("  {line}\n"));
        }
    }
    out
}
