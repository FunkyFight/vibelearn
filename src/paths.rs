//! Emplacements des données : global (`~/.vibelearn`) et par projet (`.vibelearn/`).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Dossier du bac à sable du tuteur dans le projet (démos, notes, session).
pub const SANDBOX: &str = ".vibelearn";

pub fn global_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("VIBELEARN_HOME") {
        return Ok(PathBuf::from(dir));
    }
    let home = dirs::home_dir().context("dossier personnel introuvable")?;
    Ok(home.join(".vibelearn"))
}

pub fn profile_file() -> Result<PathBuf> {
    Ok(global_dir()?.join("profile.json"))
}

pub fn config_file() -> Result<PathBuf> {
    Ok(global_dir()?.join("config.json"))
}

/// Connexion choisie avec `vibelearn login` (fournisseur, URL, clé, modèle).
pub fn login_file() -> Result<PathBuf> {
    Ok(global_dir()?.join("login.json"))
}

/// Conversations tenues par vibelearn lui-même (API OpenAI), pour `vibelearn resume`.
pub fn conversations_dir(root: &Path) -> PathBuf {
    sandbox_dir(root).join("conversations")
}

pub fn sandbox_dir(root: &Path) -> PathBuf {
    root.join(SANDBOX)
}

pub fn session_file(root: &Path) -> PathBuf {
    sandbox_dir(root).join("session.json")
}

pub fn history_dir(root: &Path) -> PathBuf {
    sandbox_dir(root).join("history")
}

pub fn notes_dir(root: &Path) -> PathBuf {
    sandbox_dir(root).join("notes")
}

pub fn demos_dir(root: &Path) -> PathBuf {
    sandbox_dir(root).join("demos")
}

/// Crée le bac à sable. Il s'ignore lui-même dans git (`.gitignore` contenant `*`)
/// pour ne jamais toucher au `.gitignore` du projet.
pub fn ensure_sandbox(root: &Path) -> Result<()> {
    let dir = sandbox_dir(root);
    std::fs::create_dir_all(demos_dir(root))?;
    std::fs::create_dir_all(notes_dir(root))?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(&ignore, "*\n")?;
    }
    Ok(())
}

/// Transforme un texte libre en nom de fichier court et sûr.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in text.to_lowercase().chars() {
        let c = match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
        if out.len() >= 48 {
            break;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "session".to_string() } else { out }
}

/// Chemin affiché relativement à la racine du projet, avec des `/`.
pub fn display_rel(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.to_string_lossy().replace('\\', "/")
}

/// Normalise un chemin (absolu ou relatif à la racine) sans toucher au disque :
/// résout `.` et `..` pour pouvoir tester l'appartenance à un dossier.
pub fn normalize(root: &Path, path: &Path) -> PathBuf {
    use std::path::Component;
    let joined = if path.is_absolute() { path.to_path_buf() } else { root.join(path) };
    let mut out = PathBuf::new();
    for comp in joined.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Vrai si `path` se trouve dans le bac à sable `.vibelearn/` du projet.
pub fn in_sandbox(root: &Path, path: &Path) -> bool {
    let norm = normalize(root, path);
    let sandbox = normalize(root, Path::new(SANDBOX));
    let a = norm.to_string_lossy().replace('\\', "/").to_lowercase();
    let b = sandbox.to_string_lossy().replace('\\', "/").to_lowercase();
    a == b || a.starts_with(&format!("{b}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_is_filename_safe() {
        assert_eq!(slug("Les traits en Rust !"), "les-traits-en-rust");
        assert_eq!(slug("Élève / çà"), "eleve-ca");
        assert_eq!(slug("???"), "session");
    }

    #[test]
    fn sandbox_membership_resists_dotdot() {
        let root = Path::new(if cfg!(windows) { "C:\\proj" } else { "/proj" });
        assert!(in_sandbox(root, Path::new(".vibelearn/demos/a.rs")));
        assert!(in_sandbox(root, &root.join(".vibelearn").join("notes").join("x.md")));
        assert!(!in_sandbox(root, Path::new(".vibelearn/../src/main.rs")));
        assert!(!in_sandbox(root, Path::new("src/main.rs")));
        assert!(!in_sandbox(root, Path::new(".vibelearn-evil/x")));
    }
}
