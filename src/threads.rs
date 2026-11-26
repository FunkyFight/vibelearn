//! Ce que vibelearn montre pendant une étape, hors du code de l'apprenant :
//! - l'endroit où écrire (ligne surlignée dans l'éditeur) ;
//! - des annotations courtes en bout de ligne, sur les lignes qu'il a écrites ;
//! - la prochaine étape d'une fonctionnalité, en bout de la ligne où il l'a
//!   demandée (`//n wall jump`) ;
//! - le pseudo-code demandé par `//// …`, sur plusieurs lignes à côté de la
//!   demande (qui reste dans le code) ;
//! - les questions de l'apprenant avec leurs réponses, et les messages, dans le
//!   panneau vibelearn de VS Code.
//!
//! Rien n'est écrit dans le code : le front publie tout dans
//! `.vibelearn/ui/state.json`, que l'extension VS Code affiche. Tout ce qui
//! concerne une étape s'efface quand elle est réussie.

use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::annotate::{self, TextFile};
use crate::{paths, watch};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// L'endroit où le morceau en cours s'écrit.
    Brief,
    /// Une annotation de vibelearn en bout de ligne.
    Feedback,
    /// Une question de l'apprenant, avec la réponse.
    Question,
    /// La prochaine étape d'une fonctionnalité (`//n …`), en bout de ligne.
    /// Elle reste jusqu'à la suivante au même endroit, une nouvelle étape, ou « Efface ».
    Next,
    /// Le pseudo-code demandé par `//// …` ; `title` est la demande.
    /// Il reste tant que la demande est dans le code (même d'une étape à l'autre).
    Pseudo,
    /// Un message de vibelearn dans le panneau, sans ligne.
    Note,
    /// Ce que vibelearn dit d'une sauvegarde, sans ligne : il va dans la
    /// fenêtre flottante, avec les annotations, pas dans la discussion.
    Review,
}

/// Tons d'une annotation, chacun avec son icône dans VS Code :
/// - `ok` : la ligne est juste ;
/// - `fix` : quelque chose ne marche pas ou est faux ici ;
/// - `design` : ça marche, mais la structure résistera mal au changement
///   (duplication, responsabilités mêlées, cas codés en dur…) ;
/// - `info` : un fait à connaître sur cette ligne.
pub const TONES: [&str; 4] = ["ok", "fix", "design", "info"];
/// Ton retenu quand celui donné est inconnu.
pub const DEFAULT_TONE: &str = "info";

/// Le ton s'il est connu, sinon [`DEFAULT_TONE`].
pub fn tone_or_default(tone: Option<&str>) -> &'static str {
    tone.and_then(|t| TONES.iter().copied().find(|known| *known == t)).unwrap_or(DEFAULT_TONE)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    Vibelearn,
    Learner,
}

#[derive(Debug, Clone, Serialize)]
pub struct Note {
    pub author: Author,
    /// Markdown.
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Thread {
    pub id: u64,
    /// Chemin absolu du fichier (vide pour un message sans ligne).
    pub file: PathBuf,
    /// Ligne concernée (à partir de 0).
    pub line: usize,
    /// Texte de la ligne : l'extension la retrouve si le code bouge.
    pub anchor: Option<String>,
    pub kind: Kind,
    /// Ton d'une annotation : un de [`TONES`].
    pub tone: Option<String>,
    pub title: Option<String>,
    pub notes: Vec<Note>,
    /// Code sélectionné par l'apprenant quand il a posé sa question.
    pub selection: Option<String>,
    #[serde(skip)]
    turn: u32,
}

impl Thread {
    pub fn has_location(&self) -> bool {
        !self.file.as_os_str().is_empty()
    }

    fn by(&self, author: Author) -> Vec<&str> {
        self.notes.iter().filter(|n| n.author == author).map(|n| n.body.as_str()).collect()
    }

    pub fn learner_notes(&self) -> Vec<&str> {
        self.by(Author::Learner)
    }

    pub fn vibelearn_notes(&self) -> Vec<&str> {
        self.by(Author::Vibelearn)
    }
}

/// Où accrocher une annotation.
#[derive(Debug, Clone, PartialEq)]
pub enum Place {
    /// Sur la ligne qui contient ce texte (la plus proche de `near`, indice à partir de 0).
    At { anchor: String, near: Option<usize> },
    /// Sur la ligne d'indice donné.
    Line(usize),
    /// À la suite d'un fil existant (réponse à une question).
    Thread(u64),
    Top,
}

/// Ce qui vient d'être montré.
#[derive(Debug, Clone)]
pub struct Posted {
    #[cfg_attr(not(test), allow(dead_code))]
    pub id: u64,
    pub file: PathBuf,
    /// Ligne concernée, à partir de 1.
    pub line: usize,
    pub kind: Kind,
    pub text: String,
}

pub struct Threads {
    root: PathBuf,
    threads: Vec<Thread>,
    next_id: u64,
    turn: u32,
    /// vibelearn a parlé (annotation, consigne, réponse) depuis le dernier message envoyé.
    spoke: bool,
    /// Fichiers créés vides par vibelearn pour y montrer où écrire.
    created: BTreeSet<PathBuf>,
    posted: Vec<Posted>,
    /// Demandes `//// …` dont le pseudo-code a été effacé : on n'y répond plus
    /// tant qu'elles restent dans le code.
    pseudo_dismissed: BTreeSet<(PathBuf, String)>,
}

impl Threads {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            threads: Vec::new(),
            next_id: 0,
            turn: 0,
            spoke: false,
            created: BTreeSet::new(),
            posted: Vec::new(),
            pseudo_dismissed: BTreeSet::new(),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn all(&self) -> &[Thread] {
        &self.threads
    }

    pub fn of_kind(&self, kind: Kind) -> impl Iterator<Item = &Thread> {
        self.threads.iter().filter(move |t| t.kind == kind)
    }

    pub fn thread(&self, id: u64) -> Option<&Thread> {
        self.threads.iter().find(|t| t.id == id)
    }

    pub fn brief(&self) -> Option<&Thread> {
        self.of_kind(Kind::Brief).next()
    }

    /// Un nouveau message part vers vibelearn.
    pub fn begin_turn(&mut self) {
        self.turn += 1;
        self.spoke = false;
    }

    /// Vrai si vibelearn a montré quelque chose depuis le dernier envoi.
    pub fn spoke(&self) -> bool {
        self.spoke
    }

    pub fn drain_posted(&mut self) -> Vec<Posted> {
        std::mem::take(&mut self.posted)
    }

    /// Chemin absolu d'un fichier du projet que vibelearn peut annoter.
    pub fn resolve(&self, file: &Path) -> Result<PathBuf, String> {
        let abs = paths::normalize(&self.root, file);
        let rel = abs.strip_prefix(&self.root).map_err(|_| format!("{} is outside the project", file.display()))?;
        if let Some(dir) = rel.components().find(|c| watch::HARD_IGNORED.iter().any(|d| c.as_os_str() == *d)) {
            return Err(format!("vibelearn does not annotate files under {}", dir.as_os_str().to_string_lossy()));
        }
        Ok(abs)
    }

    fn push(&mut self, mut thread: Thread) -> Posted {
        self.next_id += 1;
        thread.id = self.next_id;
        let posted = Posted {
            id: thread.id,
            file: thread.file.clone(),
            line: thread.line + 1,
            kind: thread.kind,
            // Une prochaine étape se résume par l'étape elle-même, pas par la fonctionnalité.
            text: match thread.kind {
                Kind::Next | Kind::Pseudo => thread.notes.first().map(|n| n.body.clone()),
                _ => thread.title.clone().or_else(|| thread.notes.first().map(|n| n.body.clone())),
            }
            .unwrap_or_default(),
        };
        self.threads.push(thread);
        self.posted.push(posted.clone());
        posted
    }

    /// Montre l'endroit où écrire (`Brief`) ou une annotation (`Feedback`) sur une ligne,
    /// ou complète un fil (`Place::Thread`).
    pub fn post(&mut self, file: &Path, place: &Place, kind: Kind, tone: Option<&str>, title: Option<&str>, body: &str) -> Result<Posted, String> {
        if body.trim().is_empty() && title.is_none() {
            return Err("the comment is empty".into());
        }
        if let Place::Thread(id) = place {
            return self.append(*id, body);
        }
        let path = self.resolve(file)?;
        // Les annotations des tours précédents s'effacent quand vibelearn en pose de nouvelles.
        let turn = self.turn;
        match kind {
            Kind::Feedback if !self.spoke => self.threads.retain(|t| !(t.kind == Kind::Feedback && t.turn < turn)),
            Kind::Brief => self.threads.retain(|t| t.kind != Kind::Brief),
            _ => {}
        }

        // Un fichier à créer (nouvelle étape) : on le crée vide pour pouvoir l'ouvrir.
        if !path.exists() {
            if !matches!(place, Place::Top | Place::Line(_)) {
                return Err(format!("{} does not exist", paths::display_rel(&self.root, &path)));
            }
            if !path.parent().is_some_and(Path::is_dir) {
                return Err(format!("the folder of {} does not exist", paths::display_rel(&self.root, &path)));
            }
            std::fs::write(&path, "").map_err(|e| e.to_string())?;
            self.created.insert(path.clone());
        }
        let lines = TextFile::load(&path).map(|f| f.lines).unwrap_or_default();
        let line = position(&lines, annotate::style_for(&path), place, &path, &self.root)?;
        let anchor = lines.get(line).map(|l| l.trim().to_string()).filter(|l| !l.is_empty());
        self.spoke = true;
        Ok(self.push(Thread {
            id: 0,
            file: path,
            line,
            anchor,
            kind,
            tone: tone.map(str::to_string),
            title: title.map(str::to_string),
            notes: if body.trim().is_empty() {
                Vec::new()
            } else {
                vec![Note { author: Author::Vibelearn, body: body.trim().to_string() }]
            },
            selection: None,
            turn: self.turn,
        }))
    }

    /// La prochaine étape de `feature`, montrée à la ligne d'indice `line` ;
    /// elle remplace celle qui y était déjà.
    pub fn post_next(&mut self, file: &Path, line: usize, feature: &str, body: &str) -> Result<Posted, String> {
        let path = self.resolve(file)?;
        self.threads.retain(|t| !(t.kind == Kind::Next && t.file == path && t.line == line));
        self.post(&path, &Place::Line(line), Kind::Next, None, Some(feature), body)
    }

    /// Le pseudo-code de la demande `request`, écrite à la ligne d'indice `line` ;
    /// il remplace celui de la même demande.
    /// Sa forme va dans `tone` (`natural` ou `formal`), la demande dans `title`.
    pub fn post_pseudo(&mut self, file: &Path, line: usize, request: &annotate::PseudoRequest, body: &str) -> Result<Posted, String> {
        let path = self.resolve(file)?;
        let key = pseudo_key(request);
        self.threads.retain(|t| !(t.kind == Kind::Pseudo && t.file == path && thread_pseudo_key(t) == key));
        self.post(&path, &Place::Line(line), Kind::Pseudo, Some(request.style.as_str()), Some(&request.text), body)
    }

    /// Les demandes `//// …` lues dans `file` : le pseudo-code des demandes
    /// retirées du code (ou passées de `////` à `////!`) disparaît ; renvoie
    /// celles qui attendent une réponse.
    pub fn sync_pseudo(&mut self, file: &Path, present: &[annotate::PseudoRequest]) -> Vec<annotate::PseudoRequest> {
        let Ok(path) = self.resolve(file) else { return Vec::new() };
        let keys: Vec<String> = present.iter().map(pseudo_key).collect();
        self.threads.retain(|t| !(t.kind == Kind::Pseudo && t.file == path && !keys.contains(&thread_pseudo_key(t))));
        self.pseudo_dismissed.retain(|(f, key)| *f != path || keys.contains(key));
        present
            .iter()
            .zip(&keys)
            .filter(|(_, key)| {
                !self.pseudo_dismissed.contains(&(path.clone(), (*key).clone()))
                    && !self.threads.iter().any(|t| t.kind == Kind::Pseudo && t.file == path && thread_pseudo_key(t) == **key)
            })
            .map(|(r, _)| r.clone())
            .collect()
    }

    /// « Efface » sur un pseudo-code : il part, et la demande restée dans le
    /// code n'est plus traitée tant qu'elle ne change pas.
    pub fn dismiss_pseudo(&mut self, id: u64) {
        if let Some(t) = self.thread(id).filter(|t| t.kind == Kind::Pseudo) {
            self.pseudo_dismissed.insert((t.file.clone(), thread_pseudo_key(t)));
            self.remove(id);
        }
    }

    /// Retire les messages des tours précédents (en surveillance, sans étape
    /// pour les effacer, seul le retour de la dernière sauvegarde reste).
    pub fn drop_older_notes(&mut self) {
        let turn = self.turn;
        self.threads.retain(|t| !(matches!(t.kind, Kind::Note | Kind::Review) && t.turn < turn));
    }

    /// Un message de vibelearn dans le panneau, sans ligne.
    pub fn note(&mut self, body: &str) -> Posted {
        self.message(Kind::Note, body)
    }

    /// Le retour de vibelearn sur la dernière sauvegarde (la phrase d'analyse).
    /// Il remplace celui de la sauvegarde précédente.
    pub fn review(&mut self, body: &str) -> Posted {
        self.threads.retain(|t| t.kind != Kind::Review);
        self.message(Kind::Review, body)
    }

    fn message(&mut self, kind: Kind, body: &str) -> Posted {
        self.spoke = true;
        self.push(Thread {
            id: 0,
            file: PathBuf::new(),
            line: 0,
            anchor: None,
            kind,
            tone: None,
            title: None,
            notes: vec![Note { author: Author::Vibelearn, body: body.trim().to_string() }],
            selection: None,
            turn: self.turn,
        })
    }

    /// La réponse de vibelearn à la suite d'un fil.
    fn append(&mut self, id: u64, body: &str) -> Result<Posted, String> {
        let thread = self.threads.iter_mut().find(|t| t.id == id).ok_or("that thread is gone")?;
        thread.notes.push(Note { author: Author::Vibelearn, body: body.trim().to_string() });
        let posted = Posted {
            id,
            file: thread.file.clone(),
            line: thread.line + 1,
            kind: thread.kind,
            text: body.trim().to_string(),
        };
        self.spoke = true;
        self.posted.push(posted.clone());
        Ok(posted)
    }

    /// L'apprenant pose une question, à propos d'une ligne ou non.
    pub fn learner_question(&mut self, at: Option<(&Path, usize)>, text: &str, selection: Option<String>) -> Result<u64, String> {
        let (file, line, anchor) = match at {
            Some((file, line)) => {
                let path = self.resolve(file)?;
                let lines = TextFile::load(&path).map(|f| f.lines).unwrap_or_default();
                let line = line.min(lines.len().saturating_sub(1));
                let anchor = lines.get(line).map(|l| l.trim().to_string()).filter(|l| !l.is_empty());
                (path, line, anchor)
            }
            None => (PathBuf::new(), 0, None),
        };
        let turn = self.turn + 1;
        let posted = self.push(Thread {
            id: 0,
            file,
            line,
            anchor,
            kind: Kind::Question,
            tone: None,
            title: None,
            notes: vec![Note { author: Author::Learner, body: text.trim().to_string() }],
            selection,
            // La question appartient au tour qui va partir.
            turn,
        });
        // Sa propre question n'a pas à être annoncée dans le terminal.
        self.posted.retain(|p| p.id != posted.id);
        Ok(posted.id)
    }

    /// Retire les annotations dont la ligne n'existe plus telle quelle : elles
    /// parlaient d'un code que le développeur a changé depuis.
    pub fn drop_stale(&mut self) {
        let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut files: std::collections::HashMap<PathBuf, Option<Vec<String>>> = std::collections::HashMap::new();
        self.threads.retain(|t| {
            let (Kind::Feedback, Some(anchor)) = (t.kind, &t.anchor) else { return true };
            let lines = files.entry(t.file.clone()).or_insert_with(|| TextFile::load(&t.file).ok().map(|f| f.lines));
            let anchor = squash(anchor);
            lines.as_ref().is_some_and(|lines| lines.iter().any(|l| squash(l) == anchor))
        });
    }

    /// Retire les annotations posées sur ces fichiers (surveillance : la
    /// sauvegarde qui part les a rendues caduques ; vibelearn redit ce qui tient).
    pub fn drop_feedback_in(&mut self, files: &[PathBuf]) {
        self.threads.retain(|t| !(t.kind == Kind::Feedback && files.contains(&t.file)));
    }

    /// Retire un fil (une remarque que le développeur a écartée).
    pub fn remove(&mut self, id: u64) {
        self.threads.retain(|t| t.id != id);
    }

    /// Nouvelle étape : ce qui concernait la précédente disparaît.
    /// Le pseudo-code reste : il suit sa demande `//// …`, pas l'étape.
    pub fn clear_step(&mut self) {
        self.threads.retain(|t| t.kind == Kind::Pseudo);
    }

    /// Plus rien à montrer ; les fichiers créés vides par vibelearn disparaissent.
    pub fn clear(&mut self) {
        self.threads.clear();
        for path in std::mem::take(&mut self.created) {
            if std::fs::read(&path).is_ok_and(|b| b.iter().all(|c| c.is_ascii_whitespace())) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// Ce qui identifie une demande `//// …` : sa forme et son texte.
fn pseudo_key(r: &annotate::PseudoRequest) -> String {
    format!("{}\u{0}{}", r.style.as_str(), r.text)
}

fn thread_pseudo_key(t: &Thread) -> String {
    format!("{}\u{0}{}", t.tone.as_deref().unwrap_or(""), t.title.as_deref().unwrap_or(""))
}

/// Ligne (indice) d'une annotation.
fn position(lines: &[String], style: Option<annotate::Style>, place: &Place, path: &Path, root: &Path) -> Result<usize, String> {
    let last = lines.len().saturating_sub(1);
    match place {
        Place::Top => Ok(0),
        Place::Line(index) => Ok((*index).min(last)),
        Place::At { anchor, near } => annotate::find_anchor(lines, style, anchor, *near).ok_or_else(|| {
            format!(
                "no line of {} contains `{}`: copy one line of their code exactly as it is in the file",
                paths::display_rel(root, path),
                anchor.trim(),
            )
        }),
        Place::Thread(_) => unreachable!("traité par append"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_project(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vibelearn-threads-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {\n    let a = 1;\n}\n").unwrap();
        dir
    }

    #[test]
    fn a_design_remark_keeps_its_own_tone_and_an_unknown_tone_falls_back_to_info() {
        assert_eq!(tone_or_default(Some("design")), "design");
        assert_eq!(tone_or_default(Some("fix")), "fix");
        assert_eq!(tone_or_default(Some("solid")), DEFAULT_TONE);
        assert_eq!(tone_or_default(None), DEFAULT_TONE);
    }

    #[test]
    fn a_pseudo_code_follows_its_request_in_the_code() {
        let root = temp_project("pseudo");
        let file = root.join("src/main.rs");
        std::fs::write(&file, "fn main() {\n    //// lire un nombre\n}\n").unwrap();
        let mut t = Threads::new(&root);
        let reqs = annotate::pseudo_requests(&file);
        assert_eq!(t.sync_pseudo(&file, &reqs), reqs, "nouvelle demande");
        let p = t.post_pseudo(&file, 1, &reqs[0], "lire une ligne\nla convertir").unwrap();
        assert!(t.sync_pseudo(&file, &reqs).is_empty(), "déjà répondue");
        let formal = vec![annotate::PseudoRequest { style: annotate::PseudoStyle::Formal, ..reqs[0].clone() }];
        assert_eq!(t.sync_pseudo(&file, &formal), formal, "passée en `////!` : nouvelle réponse");
        assert_eq!(t.of_kind(Kind::Pseudo).count(), 0, "l'ancienne forme part");
        let p2 = t.post_pseudo(&file, 1, &reqs[0], "y").unwrap();
        assert!(t.sync_pseudo(&file, &reqs).is_empty());
        assert_eq!(p2.id, t.of_kind(Kind::Pseudo).next().unwrap().id);
        let _ = p;
        let p = p2;
        t.clear_step();
        assert_eq!(t.of_kind(Kind::Pseudo).count(), 1, "survit à une nouvelle étape");
        t.dismiss_pseudo(p.id);
        assert!(t.sync_pseudo(&file, &reqs).is_empty(), "effacée : plus de réponse tant qu'elle reste");
        assert!(t.sync_pseudo(&file, &[]).is_empty());
        assert_eq!(t.sync_pseudo(&file, &reqs), reqs, "réécrite : nouvelle réponse");
        t.post_pseudo(&file, 1, &reqs[0], "x").unwrap();
        t.sync_pseudo(&file, &[]);
        assert_eq!(t.of_kind(Kind::Pseudo).count(), 0, "demande retirée : le pseudo-code part");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_next_step_replaces_the_one_on_its_line_and_outlives_save_feedback() {
        let root = temp_project("next");
        let file = root.join("src/main.rs");
        let mut t = Threads::new(&root);
        t.post_next(&file, 1, "wall jump", "Détecte le mur").unwrap();
        let p = t.post_next(&file, 1, "wall jump", "Inverse la vitesse").unwrap();
        assert_eq!(p.text, "Inverse la vitesse", "le terminal montre l'étape, pas la fonctionnalité");
        t.drop_feedback_in(&[file.clone()]);
        t.drop_stale();
        let next: Vec<_> = t.of_kind(Kind::Next).collect();
        assert_eq!(next.len(), 1);
        assert_eq!((next[0].title.as_deref(), next[0].vibelearn_notes()), (Some("wall jump"), vec!["Inverse la vitesse"]));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_annotation_sits_on_the_line_it_is_about_and_never_touches_the_code() {
        let root = temp_project("place");
        let mut t = Threads::new(&root);
        let p = t
            .post(Path::new("src/main.rs"), &Place::At { anchor: "let a = 1;".into(), near: None }, Kind::Feedback, Some("ok"), None, "`a` existe")
            .unwrap();
        assert_eq!(p.line, 2);
        let a = t.thread(p.id).unwrap();
        assert_eq!((a.line, a.anchor.as_deref(), a.tone.as_deref()), (1, Some("let a = 1;"), Some("ok")));
        assert_eq!(fs::read_to_string(root.join("src/main.rs")).unwrap(), "fn main() {\n    let a = 1;\n}\n");
        assert!(t.post(Path::new("src/main.rs"), &Place::At { anchor: "absent".into(), near: None }, Kind::Feedback, None, None, "x").is_err());
        assert!(t.post(Path::new("target/x.rs"), &Place::Top, Kind::Feedback, None, None, "x").is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_annotation_goes_away_when_its_line_changes_or_its_file_is_saved_in_monitoring() {
        let root = temp_project("stale");
        let file = root.join("src/main.rs");
        let mut t = Threads::new(&root);
        t.post(&file, &Place::At { anchor: "let a = 1;".into(), near: None }, Kind::Feedback, Some("fix"), None, "x").unwrap();
        t.post(&file, &Place::At { anchor: "fn main() {".into(), near: None }, Kind::Feedback, Some("design"), None, "y").unwrap();
        // Seule l'indentation change : la ligne est la même.
        fs::write(&file, "fn main() {\n  let a = 1;\n}\n").unwrap();
        t.drop_stale();
        assert_eq!(t.of_kind(Kind::Feedback).count(), 2);
        fs::write(&file, "fn main() {\n    let a: u8 = 1;\n}\n").unwrap();
        t.drop_stale();
        assert_eq!(t.of_kind(Kind::Feedback).map(|a| a.notes[0].body.as_str()).collect::<Vec<_>>(), vec!["y"]);
        t.drop_feedback_in(std::slice::from_ref(&file));
        assert_eq!(t.of_kind(Kind::Feedback).count(), 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn old_annotations_go_away_when_vibelearn_annotates_again_and_everything_goes_at_the_next_step() {
        let root = temp_project("turns");
        let file = root.join("src/main.rs");
        let mut t = Threads::new(&root);
        t.begin_turn();
        t.post(&file, &Place::At { anchor: "fn main".into(), near: None }, Kind::Brief, None, Some("Étape 1"), "").unwrap();
        t.post(&file, &Place::At { anchor: "let a".into(), near: None }, Kind::Feedback, Some("fix"), None, "Premier").unwrap();
        let question = t.learner_question(Some((&file, 1)), "pourquoi ?", Some("let a".into())).unwrap();
        t.begin_turn();
        assert!(!t.spoke(), "un tour sans retour ne retire rien");
        t.post(&file, &Place::Thread(question), Kind::Feedback, None, None, "Réponse").unwrap();
        assert_eq!(t.all().len(), 3);
        t.begin_turn();
        t.post(&file, &Place::Top, Kind::Feedback, Some("ok"), None, "Second").unwrap();
        let kinds: Vec<Kind> = t.all().iter().map(|th| th.kind).collect();
        assert_eq!(kinds, vec![Kind::Brief, Kind::Question, Kind::Feedback], "la question et sa réponse restent pendant l'étape");
        assert_eq!(t.thread(question).unwrap().vibelearn_notes(), vec!["Réponse"]);
        t.note("Un message");
        t.clear_step();
        assert!(t.all().is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_review_of_a_save_replaces_the_previous_one_and_stays_out_of_the_conversation() {
        let mut t = Threads::new(Path::new("."));
        t.note("Une réponse");
        t.review("Premier retour");
        t.review("Second retour");
        let reviews: Vec<_> = t.of_kind(Kind::Review).map(|r| r.vibelearn_notes()).collect();
        assert_eq!(reviews, vec![vec!["Second retour"]]);
        assert_eq!(t.of_kind(Kind::Note).count(), 1);
        t.begin_turn();
        t.drop_older_notes();
        assert!(t.all().is_empty(), "le tour suivant efface le retour et les messages");
    }

    #[test]
    fn a_new_file_to_write_is_created_empty_and_goes_away_if_it_stays_empty() {
        let root = temp_project("create");
        let mut t = Threads::new(&root);
        t.post(Path::new("src/cri.rs"), &Place::Top, Kind::Brief, None, Some("Étape 1 : Le trait"), "").unwrap();
        assert!(root.join("src/cri.rs").is_file());
        t.clear();
        assert!(!root.join("src/cri.rs").exists());
        let _ = fs::remove_dir_all(&root);
    }
}
