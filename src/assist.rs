//! Mode assistance (`vibelearn assist`) : les petites corrections que vibelearn
//! fait lui-même dans le code du développeur, pour finir un changement qu'il a
//! laissé incomplet (un nom renommé qui traîne, une valeur devenue constante,
//! les restes d'un refactoring vers sa nouvelle classe ou méthode).
//! Une correction remplace un passage de quelques lignes ; l'extension VS Code
//! l'applique dans l'éditeur (Ctrl+Z l'annule), la montre et propose de l'annuler.

use serde::Serialize;
use std::path::PathBuf;
use std::time::Instant;

/// Taille maximale d'un passage : une correction reste petite.
pub const MAX_CHARS: usize = 800;
pub const MAX_LINES: usize = 8;
/// Écart toléré entre la ligne annoncée par vibelearn et celle où commence le passage.
pub const SLACK: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EditStatus {
    /// En attente de l'éditeur.
    Pending,
    Applied,
    /// Annulée par le développeur.
    Undone,
    Failed,
}

#[derive(Debug, Clone)]
pub struct AssistEdit {
    pub id: u64,
    /// Chemin absolu.
    pub file: PathBuf,
    /// Indice de la ligne où commence le passage (à partir de 0).
    pub line: usize,
    /// Passage remplacé et remplaçant, lignes séparées par `\n`.
    pub old: String,
    pub new: String,
    pub reason: String,
    pub status: EditStatus,
    /// L'appel d'outil qui attend de savoir si la correction est faite.
    pub reply: Option<u64>,
    pub asked: Instant,
}

/// Retire les `\r` : vibelearn parle en `\n`, le fichier garde ses fins de ligne.
pub fn unix(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// Refuse ce qui n'est pas une petite correction.
pub fn check_fragments(old: &str, new: &str) -> Result<(), String> {
    if old.trim().is_empty() {
        return Err("`old` is empty: give the exact text to replace".into());
    }
    if new.trim().is_empty() {
        return Err("`new` is empty: removing code is a decision for the developer, annotate it instead".into());
    }
    if old == new {
        return Err("`old` and `new` are the same".into());
    }
    let too_big = |s: &str| s.chars().count() > MAX_CHARS || s.lines().count() > MAX_LINES;
    if too_big(old) || too_big(new) {
        return Err(format!(
            "an assist edit is at most {MAX_LINES} lines and {MAX_CHARS} characters: split it into several calls, or annotate it if it is really one large change"
        ));
    }
    Ok(())
}

/// Débuts (en octets) des occurrences de `needle` dans `text`.
fn starts(text: &str, needle: &str) -> Vec<usize> {
    text.match_indices(needle).map(|(i, _)| i).collect()
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset].matches('\n').count()
}

/// La ligne (indice) où commence `old`, à moins de `slack` lignes de `near`,
/// la plus proche de `near`. `old` ne doit commencer qu'une fois sur cette ligne.
pub fn locate(text: &str, near: usize, old: &str, slack: usize) -> Result<usize, String> {
    let text = unix(text);
    let old = unix(old);
    let lines: Vec<usize> = starts(&text, &old).into_iter().map(|i| line_of(&text, i)).collect();
    let close: Vec<usize> = lines.iter().copied().filter(|l| l.abs_diff(near) <= slack).collect();
    let Some(&best) = close.iter().min_by_key(|l| l.abs_diff(near)) else {
        let shown = text.split('\n').nth(near).map(str::trim).unwrap_or("");
        return Err(format!(
            "`{}` is not at line {} (line {} is: `{shown}`). Copy the text exactly from the file as it is now.",
            first_line(&old),
            near + 1,
            near + 1
        ));
    };
    if close.iter().filter(|l| **l == best).count() > 1 {
        return Err(format!("`{}` appears several times on line {}: include more of the line so it appears once", first_line(&old), best + 1));
    }
    Ok(best)
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim()
}

/// Le texte avec `old` remplacé par `new` là où `old` commence sur la ligne
/// `line`. Les fins de ligne du fichier (`\r\n` ou `\n`) sont gardées.
pub fn apply(text: &str, line: usize, old: &str, new: &str) -> Option<String> {
    let crlf = text.contains("\r\n");
    let eol = |s: &str| if crlf { unix(s).replace('\n', "\r\n") } else { unix(s) };
    let (old, new) = (eol(old), eol(new));
    let mut hits = starts(text, &old).into_iter().filter(|i| line_of(text, *i) == line);
    let at = hits.next()?;
    if hits.next().is_some() {
        return None;
    }
    Some(format!("{}{new}{}", &text[..at], &text[at + old.len()..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE: &str = "class A\r\n{\r\n    const int BeatCount = 7;\r\n    for (int i = 0; i < 7; i++)\r\n}\r\n";

    #[test]
    fn finds_the_fragment_near_the_announced_line_and_keeps_crlf() {
        // Annoncé ligne 3 (indice 2) alors que le `< 7` est ligne 4 : dans la marge.
        let line = locate(CODE, 2, "i < 7", SLACK).unwrap();
        assert_eq!(line, 3);
        let fixed = apply(CODE, line, "i < 7", "i < BeatCount").unwrap();
        assert!(fixed.contains("for (int i = 0; i < BeatCount; i++)\r\n"), "{fixed:?}");
        assert!(fixed.contains("const int BeatCount = 7;\r\n"), "seule la ligne visée change");
    }

    #[test]
    fn replaces_a_few_lines_left_over_from_a_refactoring() {
        let code = "int Compare(Verdict a, Verdict b)\r\n{\r\n    if (a.window < b.window) return -1;\r\n    if (a.window > b.window) return 1;\r\n    return 0;\r\n}\r\n";
        let old = "    if (a.window < b.window) return -1;\n    if (a.window > b.window) return 1;\n    return 0;";
        let line = locate(code, 2, old, SLACK).unwrap();
        assert_eq!(line, 2);
        let fixed = apply(code, line, old, "    return a.CompareTo(b);").unwrap();
        assert_eq!(fixed, "int Compare(Verdict a, Verdict b)\r\n{\r\n    return a.CompareTo(b);\r\n}\r\n");
    }

    #[test]
    fn refuses_a_fragment_that_is_ambiguous_absent_or_too_big() {
        assert!(locate("a = a + 1;\n", 0, "a", SLACK).unwrap_err().contains("several times"));
        assert!(locate(CODE, 0, "BeatCount", 0).unwrap_err().contains("not at line 1"));
        assert!(locate(CODE, 0, "nulle part", SLACK).is_err());
        assert!(check_fragments("7", "7").is_err());
        assert!(check_fragments("x", "").is_err(), "supprimer du code est une décision");
        assert!(check_fragments(&"x\n".repeat(MAX_LINES + 1), "y").is_err());
        assert!(check_fragments(&"x".repeat(MAX_CHARS + 1), "y").is_err());
        assert!(check_fragments("reactionKey1", "reactionKey").is_ok());
        assert!(check_fragments("a\nb", "c").is_ok());
        assert_eq!(apply("x\n", 5, "x", "y"), None);
    }
}
