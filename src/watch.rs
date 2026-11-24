//! Surveillance des fichiers du projet : on garde une copie de référence des
//! fichiers texte, et on calcule les diffs de ce que l'apprenant a modifié
//! depuis le dernier envoi au tuteur.

use ignore::WalkBuilder;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use similar::{ChangeTag, TextDiff};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::i18n::tr;
use crate::{annotate, paths};

const MAX_FILE_BYTES: u64 = 400 * 1024;
const MAX_FILES: usize = 6000;
const MAX_LINES_PER_FILE: usize = 220;
const MAX_LINES_TOTAL: usize = 700;
/// Un fichier touché que le modèle n'a pas encore vu en entier lui est envoyé
/// en entier jusqu'à cette taille : il juge alors la version actuelle, pas ce
/// dont il se souvient (un diff ne montre que 3 lignes autour de chaque
/// changement). Ensuite, les diffs suffisent : ils s'appliquent à une version
/// qu'il connaît.
const FULL_FILE_LINES: usize = 400;
const FULL_LINES_TOTAL: usize = 1200;

/// Dossiers jamais surveillés, même sans `.gitignore`.
pub const HARD_IGNORED: &[&str] = &[
    ".git", paths::SANDBOX, "node_modules", "target", ".venv", "venv", "__pycache__", ".idea", ".vs",
];

/// Dossiers générés par Unity à la racine du projet (cache des imports,
/// paquets copiés, journaux) : des dizaines de milliers de fichiers que Unity
/// réécrit sans cesse, et qui ne sont pas le code de l'apprenant.
const UNITY_GENERATED: &[&str] = &["Library", "Temp", "Logs", "UserSettings", "obj", "Build", "Builds", "MemoryCaptures"];
/// Sorties de compilation .NET (MonoGame, Godot C#…) à la racine du projet.
const DOTNET_GENERATED: &[&str] = &["bin", "obj"];

/// Ce que vibelearn ne lit ni ne surveille dans un projet : [`HARD_IGNORED`]
/// à toute profondeur, et les dossiers générés par le moteur à la racine,
/// même sans `.gitignore` ni dépôt git.
#[derive(Debug, Clone)]
pub struct Skip {
    generated: Vec<PathBuf>,
}

impl Skip {
    pub fn new(root: &Path) -> Self {
        let unity = root.join("ProjectSettings").join("ProjectVersion.txt").is_file();
        let dotnet = std::fs::read_dir(root).is_ok_and(|dir| {
            dir.flatten().any(|e| e.path().extension().is_some_and(|x| x == "csproj" || x == "fsproj" || x == "sln"))
        });
        let mut names: Vec<&str> = Vec::new();
        if unity {
            names.extend(UNITY_GENERATED);
        }
        if dotnet {
            names.extend(DOTNET_GENERATED);
        }
        Self { generated: names.into_iter().map(|n| root.join(n)).collect() }
    }

    /// Vrai pour un chemin absolu à ne pas lire.
    pub fn skips(&self, path: &Path) -> bool {
        path.components().any(|c| HARD_IGNORED.iter().any(|d| c.as_os_str() == *d))
            || self.generated.iter().any(|g| path.starts_with(g))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: String,
    pub kind: ChangeKind,
    pub added: usize,
    pub removed: usize,
    pub diff: String,
    /// Contenu actuel (fins de ligne `\n`), sauf pour un fichier supprimé.
    pub text: Option<String>,
}

pub struct Tracker {
    root: PathBuf,
    skip: Skip,
    gitignore: Gitignore,
    /// Contenu de chaque fichier texte au dernier envoi (chemins relatifs).
    baseline: HashMap<PathBuf, String>,
    /// Fichiers touchés depuis le dernier envoi.
    dirty: BTreeSet<PathBuf>,
}

impl Tracker {
    pub fn new(root: &Path) -> Self {
        let mut builder = GitignoreBuilder::new(root);
        builder.add(root.join(".gitignore"));
        builder.add(root.join(".git").join("info").join("exclude"));
        let gitignore = builder.build().unwrap_or_else(|_| Gitignore::empty());

        let skip = Skip::new(root);
        let mut baseline = HashMap::new();
        let walk_skip = skip.clone();
        // `.gitignore` compte même hors d'un dépôt git.
        let walker = WalkBuilder::new(root)
            .hidden(false)
            .require_git(false)
            .filter_entry(move |e| !walk_skip.skips(e.path()))
            .build();
        for entry in walker.flatten() {
            if baseline.len() >= MAX_FILES {
                break;
            }
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let Ok(rel) = entry.path().strip_prefix(root) else { continue };
            if let Some(text) = read_text(entry.path()) {
                baseline.insert(rel.to_path_buf(), text);
            }
        }
        Self { root: root.to_path_buf(), skip, gitignore, baseline, dirty: BTreeSet::new() }
    }

    fn rel(&self, abs: &Path) -> Option<PathBuf> {
        let rel = abs.strip_prefix(&self.root).ok()?;
        if rel.as_os_str().is_empty() {
            return None;
        }
        Some(rel.to_path_buf())
    }

    fn is_ignored(&self, rel: &Path) -> bool {
        if self.skip.skips(&self.root.join(rel)) {
            return true;
        }
        let name = rel.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
        // Fichiers temporaires d'éditeurs.
        if name.ends_with('~') || name.ends_with(".swp") || name.ends_with(".tmp") || name.starts_with(".#") {
            return true;
        }
        self.gitignore.matched_path_or_any_parents(rel, false).is_ignore()
    }

    /// Signale des chemins modifiés (absolus). Renvoie vrai si au moins un compte,
    /// y compris un fichier déjà en attente : chaque sauvegarde relance le délai
    /// avant l'envoi automatique.
    pub fn touch(&mut self, abs_paths: &[PathBuf]) -> bool {
        let mut any = false;
        for abs in abs_paths {
            let Some(rel) = self.rel(abs) else { continue };
            if self.is_ignored(&rel) || abs.is_dir() {
                continue;
            }
            self.dirty.insert(rel);
            any = true;
        }
        any
    }

    /// Intègre l'état actuel d'un fichier à la référence sans le signaler :
    /// sert pour ce que le tuteur a écrit lui-même.
    pub fn absorb(&mut self, abs: &Path) {
        let Some(rel) = self.rel(abs) else { return };
        self.dirty.remove(&rel);
        match read_text(abs) {
            Some(text) => {
                self.baseline.insert(rel, text);
            }
            None => {
                self.baseline.remove(&rel);
            }
        }
    }

    /// Reporte dans la référence une correction faite par vibelearn (mode
    /// assistance) : elle ne compte pas comme un changement du développeur, et
    /// ses propres changements en attente dans ce fichier restent visibles.
    pub fn absorb_edit(&mut self, abs: &Path, near: usize, old: &str, new: &str) {
        let Some(rel) = self.rel(abs) else { return };
        let Some(text) = self.baseline.get(&rel) else { return };
        // La référence date du dernier envoi : la ligne a pu bouger depuis.
        let Ok(line) = crate::assist::locate(text, near, old, 50) else { return };
        if let Some(fixed) = crate::assist::apply(text, line, old, new) {
            self.baseline.insert(rel, fixed);
        }
    }

    /// Intègre tout ce qui est en attente (commande lancée par le tuteur).
    pub fn absorb_all(&mut self) {
        let dirty: Vec<PathBuf> = self.dirty.iter().cloned().collect();
        for rel in dirty {
            let abs = self.root.join(&rel);
            self.absorb(&abs);
        }
    }

    #[cfg(test)]
    pub fn has_dirty(&self) -> bool {
        !self.dirty.is_empty()
    }

    /// Fichiers actuellement en attente (pour délimiter une fenêtre de temps).
    pub fn dirty_paths(&self) -> BTreeSet<PathBuf> {
        self.dirty.clone()
    }

    /// Intègre à la référence ce qui a été touché depuis `before` : sert pour
    /// les fichiers modifiés par une commande que le tuteur a lancée.
    pub fn absorb_new_since(&mut self, before: &BTreeSet<PathBuf>) {
        let new: Vec<PathBuf> = self.dirty.difference(before).cloned().collect();
        for rel in new {
            let abs = self.root.join(&rel);
            self.absorb(&abs);
        }
    }

    /// Diffs des fichiers en attente. Les fichiers revenus à l'identique sont
    /// retirés de la liste d'attente.
    pub fn changes(&mut self) -> Vec<FileChange> {
        let mut out = Vec::new();
        let mut unchanged = Vec::new();
        for rel in &self.dirty {
            let abs = self.root.join(rel);
            let path = rel.to_string_lossy().replace('\\', "/");
            let old = self.baseline.get(rel);
            let new = if abs.is_file() { read_text(&abs) } else { None };
            match (old, new) {
                (Some(old), Some(new)) => {
                    if *old == new {
                        unchanged.push(rel.clone());
                        continue;
                    }
                    let (added, removed, diff) = unified(old, &new, &path);
                    out.push(FileChange { path, kind: ChangeKind::Modified, added, removed, diff, text: Some(new) });
                }
                // Fichier créé par vibelearn pour une consigne, encore vide de code.
                (None, Some(new)) if new.trim().is_empty() => unchanged.push(rel.clone()),
                (None, Some(new)) => {
                    let (added, _, diff) = unified("", &new, &path);
                    out.push(FileChange { path, kind: ChangeKind::Added, added, removed: 0, diff, text: Some(new) });
                }
                (Some(old), None) if !abs.exists() => {
                    out.push(FileChange {
                        path,
                        kind: ChangeKind::Deleted,
                        added: 0,
                        removed: old.lines().count(),
                        diff: String::new(),
                        text: None,
                    });
                }
                // Binaire, trop gros, ou jamais connu : rien à montrer.
                _ => unchanged.push(rel.clone()),
            }
        }
        for rel in unchanged {
            self.dirty.remove(&rel);
        }
        out
    }

    /// Les changements en attente deviennent la nouvelle référence.
    pub fn commit(&mut self) {
        self.absorb_all();
    }
}

fn read_text(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.iter().take(4096).any(|b| *b == 0) {
        return None;
    }
    let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    // Les commentaires de vibelearn ne sont pas le travail de l'apprenant.
    Some(match annotate::style_for(path) {
        Some(style) => annotate::strip_marked(text, style),
        None => text.to_string(),
    })
}

fn unified(old: &str, new: &str, path: &str) -> (usize, usize, String) {
    let diff = TextDiff::from_lines(old, new);
    let mut added = 0;
    let mut removed = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {}
        }
    }
    let text = diff
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string();
    (added, removed, text)
}

/// Ce que reçoit le modèle : le diff de chaque fichier touché, plus son contenu
/// actuel entier (numéroté) s'il n'est pas dans `seen` (les fichiers dont il
/// connaît déjà la version actuelle) et reste de taille raisonnable. Les
/// fichiers envoyés en entier rejoignent `seen`.
pub fn render(changes: &[FileChange], seen: &mut HashSet<String>) -> String {
    render_with(changes, Some(seen))
}

/// Les seuls diffs, pour les montrer à l'apprenant (`/diff`).
pub fn render_diffs(changes: &[FileChange]) -> String {
    render_with(changes, None)
}

fn numbered(text: &str) -> String {
    text.lines().enumerate().map(|(i, l)| format!("{:>5}  {l}\n", i + 1)).collect()
}

fn render_with(changes: &[FileChange], mut seen: Option<&mut HashSet<String>>) -> String {
    let mut out = String::new();
    let mut budget = MAX_LINES_TOTAL;
    let mut full_budget = if seen.is_some() { FULL_LINES_TOTAL } else { 0 };
    for c in changes {
        let kind = match c.kind {
            ChangeKind::Added => "new file",
            ChangeKind::Modified => "modified",
            ChangeKind::Deleted => "deleted",
        };
        out.push_str(&format!("## {} ({kind}, +{} -{})\n", c.path, c.added, c.removed));
        if c.kind == ChangeKind::Deleted {
            if let Some(seen) = seen.as_deref_mut() {
                seen.remove(&c.path);
            }
            continue;
        }
        let known = seen.as_deref().is_some_and(|s| s.contains(&c.path));
        let whole = c.text.as_deref().filter(|t| {
            let n = t.lines().count();
            !known && n <= FULL_FILE_LINES && n <= full_budget
        });
        if whole.is_some()
            && let Some(seen) = seen.as_deref_mut()
        {
            seen.insert(c.path.clone());
        }
        // Un nouveau fichier : son contenu entier tient lieu de diff.
        if let (Some(text), ChangeKind::Added) = (whole, c.kind) {
            full_budget -= text.lines().count();
            out.push_str(&format!("Full content (line numbers on the left):\n```\n{}```\n", numbered(text)));
            continue;
        }
        if budget == 0 {
            out.push_str("(diff omitted: too many changes at once; Read the file)\n");
            continue;
        }
        let lines: Vec<&str> = c.diff.lines().collect();
        let take = lines.len().min(MAX_LINES_PER_FILE).min(budget);
        out.push_str("```diff\n");
        out.push_str(&lines[..take].join("\n"));
        out.push_str("\n```\n");
        if take < lines.len() {
            out.push_str(&format!("(diff truncated: {} more lines; Read the file for the rest)\n", lines.len() - take));
        }
        budget -= take;
        match whole {
            Some(text) => {
                full_budget -= text.lines().count();
                out.push_str(&format!(
                    "Current content of {}, as it is now (line numbers on the left):\n```\n{}```\n",
                    c.path,
                    numbered(text)
                ));
            }
            None if seen.is_some() && !known => out.push_str(&format!(
                "(too long to include in full: Read {} before commenting on anything outside this diff)\n",
                c.path
            )),
            None => {}
        }
    }
    out
}

/// Résumé d'une ligne pour l'apprenant : `src/a.rs (+3 −1), b.rs (nouveau)`.
pub fn summary(changes: &[FileChange]) -> String {
    changes
        .iter()
        .map(|c| match c.kind {
            ChangeKind::Added => format!("{} ({}, +{})", c.path, tr("nouveau", "new"), c.added),
            ChangeKind::Deleted => format!("{} ({})", c.path, tr("supprimé", "deleted")),
            ChangeKind::Modified => format!("{} (+{} −{})", c.path, c.added, c.removed),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Démarre la surveillance récursive ; `on_paths` reçoit les chemins touchés.
pub fn start(root: &Path, on_paths: impl Fn(Vec<PathBuf>) + Send + 'static) -> notify::Result<RecommendedWatcher> {
    // Unity réécrit `Library/` sans arrêt : ces événements ne vont pas plus loin.
    let skip = Skip::new(root);
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res
            && !matches!(event.kind, EventKind::Access(_))
        {
            let paths: Vec<PathBuf> = event.paths.into_iter().filter(|p| !skip.skips(p)).collect();
            if !paths.is_empty() {
                on_paths(paths);
            }
        }
    })?;
    watcher.watch(root, RecursiveMode::Recursive)?;
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_project(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vibelearn-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::create_dir_all(dir.join(".vibelearn")).unwrap();
        fs::write(dir.join(".gitignore"), "*.log\n").unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {\n    println!(\"a\");\n}\n").unwrap();
        dir
    }

    #[test]
    fn a_unity_project_without_git_skips_what_unity_generates() {
        let root = temp_project("unity");
        for dir in ["ProjectSettings", "Library/PackageCache/com.unity.x", "Temp", "Assets/Scripts/Library"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(root.join("ProjectSettings/ProjectVersion.txt"), "m_EditorVersion: 6000.0\n").unwrap();
        fs::write(root.join("Library/PackageCache/com.unity.x/A.cs"), "class A {}\n").unwrap();
        fs::write(root.join("Assets/Scripts/Library/Book.cs"), "class Book {}\n").unwrap();
        let skip = Skip::new(&root);
        assert!(skip.skips(&root.join("Library/PackageCache/com.unity.x/A.cs")));
        assert!(skip.skips(&root.join("Temp")));
        assert!(!skip.skips(&root.join("Assets/Scripts/Library/Book.cs")), "seulement à la racine");
        assert!(skip.skips(&root.join("src/node_modules/x.js")), "les dossiers de toujours, à toute profondeur");
        let tracker = Tracker::new(&root);
        assert!(tracker.baseline.keys().all(|k| !k.starts_with("Library")));
        assert!(tracker.baseline.contains_key(Path::new("Assets/Scripts/Library/Book.cs")));
        assert!(!Skip::new(&temp_project("plain")).skips(&root.join("Library/x")), "hors Unity, `Library` est un dossier comme un autre");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore = "mesure sur un vrai projet : VIBELEARN_BENCH_ROOT"]
    fn bench_tracker_on_a_real_project() {
        let Ok(root) = std::env::var("VIBELEARN_BENCH_ROOT") else { return };
        let t = std::time::Instant::now();
        let tracker = Tracker::new(Path::new(&root));
        eprintln!("BENCH {} fichiers en {:?}", tracker.baseline.len(), t.elapsed());
    }

    #[test]
    fn reports_edits_new_files_and_deletions_then_resets() {
        let root = temp_project("diff");
        let mut t = Tracker::new(&root);

        fs::write(root.join("src/main.rs"), "fn main() {\n    println!(\"b\");\n    run();\n}\n").unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn run() {}\n").unwrap();
        assert!(t.touch(&[root.join("src/main.rs"), root.join("src/lib.rs")]));

        let changes = t.changes();
        assert_eq!(changes.len(), 2);
        let main = changes.iter().find(|c| c.path == "src/main.rs").unwrap();
        assert_eq!((main.kind, main.added, main.removed), (ChangeKind::Modified, 2, 1));
        assert!(main.diff.contains("+    run();"));
        let lib = changes.iter().find(|c| c.path == "src/lib.rs").unwrap();
        assert_eq!(lib.kind, ChangeKind::Added);
        let mut seen = HashSet::new();
        let model = render(&changes, &mut seen);
        assert!(model.contains("## src/lib.rs (new file, +1 -0)"));
        assert!(model.contains("Current content of src/main.rs, as it is now"), "{model}");
        assert!(model.contains("    1  fn main() {\n    2      println!(\"b\");\n    3      run();\n    4  }\n"), "{model}");
        assert!(model.contains("Full content (line numbers on the left):\n```\n    1  pub fn run() {}\n```"), "{model}");
        let shown = render_diffs(&changes);
        assert!(shown.contains("+    run();") && !shown.contains("Current content"), "{shown}");
        // Déjà vus en entier : le diff suffit, rien n'est renvoyé.
        assert!(seen.contains("src/main.rs") && seen.contains("src/lib.rs"));
        let again = render(&changes, &mut seen);
        assert!(again.contains("+    run();") && !again.contains("Current content") && !again.contains("Full content"), "{again}");

        t.commit();
        assert!(!t.has_dirty());

        fs::remove_file(root.join("src/lib.rs")).unwrap();
        t.touch(&[root.join("src/lib.rs")]);
        let changes = t.changes();
        assert_eq!(changes[0].kind, ChangeKind::Deleted);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ignores_build_output_sandbox_gitignored_and_reverted_files() {
        let root = temp_project("ignore");
        let mut t = Tracker::new(&root);
        fs::write(root.join("target/out.rs"), "x").unwrap();
        fs::write(root.join(".vibelearn/demo.rs"), "x").unwrap();
        fs::write(root.join("debug.log"), "x").unwrap();
        assert!(!t.touch(&[
            root.join("target/out.rs"),
            root.join(".vibelearn/demo.rs"),
            root.join("debug.log"),
        ]));

        // Touché mais contenu identique : rien à signaler.
        assert!(t.touch(&[root.join("src/main.rs")]));
        assert!(t.changes().is_empty());
        assert!(!t.has_dirty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn vibelearn_comments_are_not_reported_as_the_learners_work() {
        let root = temp_project("comments");
        let mut t = Tracker::new(&root);
        let file = root.join("src/main.rs");
        fs::write(&file, "//» Morceau 1/2\nfn main() {\n    //» Ici, affiche le cri.\n    println!(\"a\");\n}\n").unwrap();
        t.touch(std::slice::from_ref(&file));
        assert!(t.changes().is_empty());
        fs::write(&file, "//» Morceau 1/2\nfn main() {\n    println!(\"b\");\n}\n").unwrap();
        t.touch(&[file]);
        let changes = t.changes();
        assert_eq!((changes[0].added, changes[0].removed), (1, 1));
        assert!(!changes[0].diff.contains('»'));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_assist_edit_is_not_reported_but_the_developers_own_changes_are() {
        let root = temp_project("assist");
        let file = root.join("src/main.rs");
        fs::write(&file, "const N: u32 = 7;\nfn main() {\n    for _ in 0..7 {}\n}\n").unwrap();
        let mut t = Tracker::new(&root);
        // Le développeur ajoute une ligne, vibelearn en corrige une autre.
        fs::write(&file, "const N: u32 = 7;\nfn main() {\n    for _ in 0..N {}\n    run();\n}\n").unwrap();
        t.absorb_edit(&file, 2, "0..7", "0..N");
        t.touch(std::slice::from_ref(&file));
        let changes = t.changes();
        assert_eq!((changes[0].added, changes[0].removed), (1, 0), "{}", changes[0].diff);
        assert!(changes[0].diff.contains("+    run();") && !changes[0].diff.contains("0..7"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_long_file_gets_its_diff_and_a_request_to_read_it() {
        let root = temp_project("long");
        let file = root.join("src/main.rs");
        let long: String = (0..500).map(|i| format!("// {i}\n")).collect();
        fs::write(&file, &long).unwrap();
        let mut t = Tracker::new(&root);
        fs::write(&file, long.replace("// 250\n", "// changed\n")).unwrap();
        t.touch(std::slice::from_ref(&file));
        let mut seen = HashSet::new();
        let model = render(&t.changes(), &mut seen);
        assert!(model.contains("+// changed") && !model.contains("Current content") && seen.is_empty());
        assert!(model.contains("Read src/main.rs before commenting"), "{model}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn absorbed_writes_are_not_reported() {
        let root = temp_project("absorb");
        let mut t = Tracker::new(&root);
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        t.touch(&[root.join("src/main.rs")]);
        t.absorb(&root.join("src/main.rs"));
        assert!(t.changes().is_empty());
        let _ = fs::remove_dir_all(&root);
    }
}
