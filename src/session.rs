//! État d'une session d'apprentissage, persistant dans `.vibelearn/session.json`
//! pour pouvoir fermer le terminal et reprendre plus tard.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::paths;

const MAX_SAVED_ENTRIES: usize = 400;

/// Les remarques que le développeur a écartées (« Oublie ») dans ce projet,
/// pour que vibelearn ne refasse plus ce genre de remarque, d'une session à l'autre.
fn dismissed_file(root: &Path) -> std::path::PathBuf {
    paths::sandbox_dir(root).join("dismissed.json")
}

pub fn dismissed(root: &Path) -> Vec<String> {
    std::fs::read_to_string(dismissed_file(root)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn dismiss(root: &Path, remark: &str) {
    let mut list = dismissed(root);
    let remark = remark.trim().to_string();
    if remark.is_empty() || list.contains(&remark) {
        return;
    }
    list.push(remark);
    if paths::ensure_sandbox(root).is_ok()
        && let Ok(text) = serde_json::to_string_pretty(&list)
    {
        let _ = std::fs::write(dismissed_file(root), text);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Construire quelque chose de nouveau en apprenant (`vibelearn <objectif>`).
    Learn,
    /// Réviser les concepts arrivés à échéance (`vibelearn review`).
    Review,
    /// Restructurer du code existant sans changer ce qu'il fait (`vibelearn refactor <cible>`).
    Refactor,
    /// Commenter chaque sauvegarde sans rien enseigner (`vibelearn monitor`).
    Monitor,
    /// Comme la surveillance, et compléter soi-même ce qui découle sans choix
    /// de ce que le développeur vient d'écrire (`vibelearn assist`).
    Assist,
}

impl Mode {
    /// Les modes guidés par un plan d'étapes ; les autres n'en ont pas
    /// (révision : une question par concept ; surveillance : aucun).
    pub fn is_guided(self) -> bool {
        matches!(self, Mode::Learn | Mode::Refactor)
    }

    /// Les modes sans plan où chaque sauvegarde part vers vibelearn.
    pub fn watches_every_save(self) -> bool {
        matches!(self, Mode::Monitor | Mode::Assist)
    }

    /// Les modes où vibelearn finit lui-même les refactorings évidents
    /// (`assist_edit`). Pas en refactor : là, finir le refactoring est l'exercice.
    pub fn finishes_refactorings(self) -> bool {
        matches!(self, Mode::Assist | Mode::Learn)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Active,
    Done,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub status: StepStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub goal: String,
    pub steps: Vec<Step>,
}

impl Plan {
    pub fn progress(&self) -> (usize, usize) {
        let done = self
            .steps
            .iter()
            .filter(|s| matches!(s.status, StepStatus::Done | StepStatus::Skipped))
            .count();
        (done, self.steps.len())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exercise {
    pub id: String,
    pub title: String,
    pub goal: String,
    pub success_criteria: Vec<String>,
    #[serde(default)]
    pub target_files: Vec<String>,
    /// Ligne de code au-dessus de laquelle la consigne est écrite.
    #[serde(default)]
    pub anchor: Option<String>,
    /// Faits et pièges à connaître pour ce morceau, écrits avec la consigne.
    #[serde(default)]
    pub notes: Option<String>,
    /// Courte étiquette montrée sur la ligne où écrire.
    #[serde(default)]
    pub here: Option<String>,
    #[serde(default)]
    pub check_command: Option<String>,
    #[serde(default)]
    pub hint_level: u8,
    #[serde(default)]
    pub attempts: u32,
    /// Dernier verdict du tuteur : passed, progress, wip, off_track.
    #[serde(default)]
    pub verdict: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    /// Mini-leçon (surveillance, assistance) : la remarque de vibelearn qu'elle approfondit.
    #[serde(default)]
    pub origin: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Demo {
    pub title: String,
    pub files: Vec<String>,
    pub run_command: Option<String>,
    pub observe: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Claim {
    Known,
    Unsure,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConcept {
    pub label: String,
    pub claim: Claim,
    /// Niveau observé pendant cette session (0-4), s'il y en a un.
    #[serde(default)]
    pub level: Option<u8>,
}

/// Une ligne du fil de conversation affiché.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "text", rename_all = "snake_case")]
pub enum Entry {
    User(String),
    Tutor(String),
    /// Ce que fait le tuteur en coulisses (lecture de fichier, recherche…).
    Activity(String),
    /// Message de l'application elle-même.
    Notice(String),
    /// Résumé d'un envoi de changements de fichiers.
    Changes(String),
    /// Trace d'une interaction (checklist, question) et de la réponse donnée.
    Widget(String),
    /// Sortie d'une commande lancée par l'apprenant.
    Output(String),
    Error(String),
    /// Annotation posée par vibelearn sur une ligne (`[fichier:ligne] texte`).
    Comment(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub topic: String,
    pub mode: Mode,
    pub claude_session_id: Option<String>,
    pub started: DateTime<Utc>,
    #[serde(default)]
    pub active_secs: u64,
    #[serde(default)]
    pub completed: bool,
    #[serde(default)]
    pub plan: Option<Plan>,
    #[serde(default)]
    pub exercise: Option<Exercise>,
    #[serde(default)]
    pub exercises_passed: u32,
    #[serde(default)]
    pub demo: Option<Demo>,
    #[serde(default)]
    pub concepts: BTreeMap<String, SessionConcept>,
    #[serde(default)]
    pub hints_used: u32,
    #[serde(default)]
    pub turns: u32,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default = "yes")]
    pub auto_check: bool,
    #[serde(default)]
    pub notes_file: Option<String>,
    #[serde(default)]
    pub transcript: Vec<Entry>,
}

fn yes() -> bool {
    true
}

impl SessionState {
    pub fn new(topic: &str, mode: Mode) -> Self {
        Self {
            topic: topic.to_string(),
            mode,
            claude_session_id: None,
            started: Utc::now(),
            active_secs: 0,
            completed: false,
            plan: None,
            exercise: None,
            exercises_passed: 0,
            demo: None,
            concepts: BTreeMap::new(),
            hints_used: 0,
            turns: 0,
            cost_usd: 0.0,
            input_tokens: 0,
            output_tokens: 0,
            auto_check: true,
            notes_file: None,
            transcript: Vec::new(),
        }
    }

    pub fn load(root: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(paths::session_file(root)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        paths::ensure_sandbox(root)?;
        let mut copy = self.clone();
        let len = copy.transcript.len();
        if len > MAX_SAVED_ENTRIES {
            copy.transcript.drain(..len - MAX_SAVED_ENTRIES);
        }
        // Une révision est courte et ne doit pas écraser la session d'apprentissage en cours.
        let path = match self.mode {
            Mode::Learn | Mode::Refactor => paths::session_file(root),
            Mode::Review => paths::sandbox_dir(root).join("review.json"),
            Mode::Monitor => paths::sandbox_dir(root).join("monitor.json"),
            Mode::Assist => paths::sandbox_dir(root).join("assist.json"),
        };
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&copy)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Range la session courante dans `.vibelearn/history/` (nouveau sujet).
    pub fn archive(root: &Path) -> Result<()> {
        let path = paths::session_file(root);
        let Some(old) = Self::load(root) else {
            let _ = std::fs::remove_file(&path);
            return Ok(());
        };
        let dir = paths::history_dir(root);
        std::fs::create_dir_all(&dir)?;
        let name = format!("{}-{}.json", old.started.format("%Y%m%d-%H%M"), paths::slug(&old.topic));
        std::fs::rename(&path, dir.join(name))?;
        Ok(())
    }

    /// Résumé de l'avancement, donné au tuteur quand on reprend une session.
    pub fn progress_summary(&self) -> String {
        let mut out = String::new();
        if let Some(plan) = &self.plan {
            out.push_str(&format!("goal: {}\nplan:\n", plan.goal));
            for s in &plan.steps {
                out.push_str(&format!("  - [{:?}] {} ({})\n", s.status, s.title, s.id).to_lowercase());
            }
        }
        if let Some(ex) = &self.exercise {
            out.push_str(&format!(
                "current exercise {}: {} (hints used: {}, last verdict: {})\n",
                ex.id,
                ex.title,
                ex.hint_level,
                ex.verdict.as_deref().unwrap_or("none")
            ));
        }
        if !self.concepts.is_empty() {
            out.push_str("concepts this session:\n");
            for (id, c) in &self.concepts {
                let level = c.level.map(|l| format!("level {l}/4")).unwrap_or_else(|| format!("{:?}", c.claim).to_lowercase());
                out.push_str(&format!("  - {id}: {level}\n"));
            }
        }
        if out.is_empty() {
            out.push_str("(the session had only just started)\n");
        }
        out
    }
}
