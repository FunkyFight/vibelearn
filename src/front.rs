//! Le mode par défaut : tout se passe dans VS Code, rien dans le terminal.
//! L'extension vibelearn y affiche :
//! - un panneau sur le côté : l'étape en cours (à savoir, à toi), les retours,
//!   les questions et réponses, la checklist et les questions à choix ;
//! - dans l'éditeur, la ligne où écrire surlignée et des annotations courtes en
//!   bout de ligne sur ce que l'apprenant a écrit.
//!
//! Échange avec l'extension, par fichiers dans `.vibelearn/ui/` :
//! - `state.json` (écrit ici) : tout ce qu'il faut afficher ;
//! - `inbox/*.json` (écrits par l'extension) : réponses, questions, commandes ;
//! - `ack.json` (écrit par l'extension) : la dernière version affichée.
//!
//! Le terminal ne sert qu'à lancer et arrêter la session ; il note en une ligne
//! ce que vibelearn vient de montrer.

use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::annotate;
use crate::assist::EditStatus;
use crate::app::{App, AppEvent, Outcome, Phase};
use crate::claude::AgentEvent;
use crate::i18n::{colon, tr};
use crate::modal::{AskKind, Decision, Modal};
use crate::session::{Claim, Entry, Mode, StepStatus};
use crate::threads::{Kind, Place};
use crate::vscode::{self, Editor, Install};
use crate::{paths, prompt};

/// Délai après une sauvegarde avant de relire le fichier (un éditeur écrit parfois en deux fois).
const SETTLE: Duration = Duration::from_millis(400);

// ---------------------------------------------------------------------------
// Messages de l'extension
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Incoming {
    Answer {
        prompt: u64,
        #[serde(default)]
        known: Option<Vec<String>>,
        #[serde(default)]
        unsure: Option<Vec<String>>,
        #[serde(default)]
        choice: Option<usize>,
        #[serde(default)]
        choices: Option<Vec<usize>>,
        #[serde(default)]
        confidence: Option<u8>,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        dont_know: bool,
        #[serde(default)]
        decision: Option<String>,
    },
    /// Une question tapée dans le panneau, à propos de la ligne où est le curseur.
    Ask {
        text: String,
        #[serde(default)]
        file: Option<PathBuf>,
        #[serde(default)]
        line: Option<usize>,
        #[serde(default)]
        selection: Option<String>,
    },
    Command {
        name: String,
    },
    /// Mode assistance : l'éditeur a appliqué (ou refusé) une correction de vibelearn.
    EditResult {
        id: u64,
        ok: bool,
        #[serde(default)]
        line: Option<usize>,
        #[serde(default)]
        error: Option<String>,
    },
    /// « Comment ? » dans l'infobulle d'une remarque : la réponse directe.
    How {
        id: u64,
    },
    /// « Apprends-moi » sur une remarque : une mini-leçon.
    Teach {
        id: u64,
    },
    /// « Oublie » : ce genre de remarque ne l'intéresse pas.
    Forget {
        id: u64,
    },
    /// Le développeur a annulé une correction ; `saved` : le fichier a été sauvegardé après.
    EditUndone {
        id: u64,
        #[serde(default)]
        saved: bool,
    },
}

/// Applique une réponse à l'interaction ouverte. Vrai si elle est complète.
fn apply_answer(modal: &mut Modal, answer: &Incoming) -> bool {
    let Incoming::Answer { known, unsure, choice, choices, confidence, text, dont_know, decision, .. } = answer else {
        return false;
    };
    match modal {
        Modal::Checklist(c) => {
            let Some(known) = known else { return false };
            let unsure = unsure.clone().unwrap_or_default();
            for item in c.items.iter_mut() {
                item.state = if known.contains(&item.id) {
                    Claim::Known
                } else if unsure.contains(&item.id) {
                    Claim::Unsure
                } else {
                    Claim::Unknown
                };
            }
            true
        }
        Modal::Ask(a) => {
            if *dont_know {
                a.dont_know = true;
                return true;
            }
            match a.kind {
                AskKind::Choice => match choice.filter(|i| *i < a.options.len()) {
                    Some(i) => {
                        a.choice = Some(i);
                        true
                    }
                    None => false,
                },
                AskKind::Multi => match choices {
                    Some(list) if !list.is_empty() => {
                        for (i, slot) in a.selected.iter_mut().enumerate() {
                            *slot = list.contains(&i);
                        }
                        true
                    }
                    _ => false,
                },
                AskKind::Confidence => match confidence.filter(|c| (1..=5).contains(c)) {
                    Some(c) => {
                        a.confidence = c;
                        true
                    }
                    None => false,
                },
                AskKind::Free | AskKind::Predict => match text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                    Some(t) => {
                        a.text = t.to_string();
                        true
                    }
                    None => false,
                },
            }
        }
        Modal::Permission(p) => {
            p.decision = Some(match decision.as_deref() {
                Some("allow") => Decision::Allow,
                Some("always") => Decision::Always,
                Some("deny") => Decision::Deny,
                _ => return false,
            });
            true
        }
    }
}

fn claim_name(claim: Claim) -> &'static str {
    match claim {
        Claim::Known => "known",
        Claim::Unsure => "unsure",
        Claim::Unknown => "unknown",
    }
}

/// L'interaction ouverte, telle que le panneau l'affiche.
fn prompt_json(modal: &Modal) -> Value {
    match modal {
        Modal::Checklist(c) => json!({
            "id": c.reply,
            "kind": "checklist",
            "title": c.title,
            "intro": c.intro,
            "items": c.items.iter().map(|i| json!({
                "id": i.id,
                "label": i.label,
                "kind": i.kind,
                "why": i.why,
                "evidence": i.evidence,
                "state": claim_name(i.state),
            })).collect::<Vec<_>>(),
        }),
        Modal::Ask(a) => json!({
            "id": a.reply,
            "kind": match a.kind {
                AskKind::Choice => "choice",
                AskKind::Multi => "multi",
                AskKind::Confidence => "confidence",
                AskKind::Free => "free",
                AskKind::Predict => "predict",
            },
            "question": a.question,
            "code": a.code,
            "options": a.options,
        }),
        Modal::Permission(p) => json!({
            "id": p.reply,
            "kind": "permission",
            "headline": p.headline,
            "detail": p.detail,
            "write": p.write_path.is_some(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Commandes écrites dans le code (`//? indice`) ou dans le panneau
// ---------------------------------------------------------------------------

const COMMANDS: &[&str] = &[
    "indice", "hint", "vérifie", "verifie", "vérifier", "verifier", "check", "bloqué", "bloque", "stuck", "réexplique",
    "reexplique", "explique", "explain", "détail", "detail", "deeper", "creuse", "récap", "recap", "fin", "finish",
    "terminer", "passe", "passer", "skip", "connu", "known", "quiz", "aide", "help", "quit", "stop", "plan", "notes",
];

/// `indice` ou `/run cargo test` : une commande plutôt qu'une question.
fn as_command(text: &str) -> Option<String> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix('/') {
        return Some(rest.trim().to_string());
    }
    let mut words = text.trim_end_matches(['.', '!', ' ']).split_whitespace();
    let first = words.next()?.to_lowercase();
    let alone = words.next().is_none();
    (alone && COMMANDS.contains(&first.as_str())).then_some(first)
}

fn actions(mode: Mode, lesson: bool) -> Value {
    // `place` dit où le panneau range la commande : `bar` (toujours sous la
    // main, en haut), `help` (les aides d'une étape, au-dessus du champ),
    // `menu` (le reste, derrière « ⋯ »).
    let check = ("check", tr("Vérifier", "Check"), "bar");
    // En surveillance il n'y a ni étape ni leçon : seulement demander un retour, ou arrêter.
    let monitor = [check, ("quit", tr("Arrêter la session", "Stop the session"), "menu")];
    let guided = [
        ("hint", tr("Indice", "Hint"), "help"),
        check,
        ("stuck", tr("Je bloque", "I'm stuck"), "help"),
        ("explain", tr("Explique autrement", "Explain differently"), "help"),
        ("deeper", tr("Plus de détails", "Go deeper"), "help"),
        ("recap", tr("Où en est-on ?", "Where are we?"), "menu"),
        ("skip", tr("Passer cette étape", "Skip this step"), "menu"),
        ("finish", tr("Terminer la session", "Wrap up the session"), "menu"),
        ("quit", tr("Arrêter, reprendre plus tard", "Stop, resume later"), "menu"),
    ];
    // Mini-leçon : de quoi se faire aider, et revenir à la surveillance.
    let lesson_actions = [
        ("hint", tr("Indice", "Hint"), "help"),
        ("stuck", tr("Je bloque", "I'm stuck"), "help"),
        ("explain", tr("Explique autrement", "Explain differently"), "help"),
        check,
        ("skip", tr("Arrêter la leçon", "Stop the lesson"), "menu"),
        ("quit", tr("Arrêter la session", "Stop the session"), "menu"),
    ];
    let list: &[(&str, &str, &str)] = match (mode.watches_every_save(), lesson) {
        (true, true) => &lesson_actions,
        (true, false) => &monitor,
        _ => &guided,
    };
    Value::Array(list.iter().map(|(name, label, place)| json!({ "name": name, "label": label, "place": place })).collect())
}

fn strings() -> Value {
    json!({
        "step": tr("Étape", "Step"),
        "of": tr("sur", "of"),
        "know": tr("À savoir", "Good to know"),
        "your_turn": tr("À toi", "Your turn"),
        "line": tr("ligne", "line"),
        "expected": tr("Attendu", "Expected"),
        "where": tr("Où écrire ?", "Where to write?"),
        "write_here": tr("écris ici", "write here"),
        "on_code": tr("Sur ton code", "On your code"),
        "talk": tr("Discussion", "Conversation"),
        "older_one": tr("question précédente", "earlier question"),
        "older": tr("questions précédentes", "earlier questions"),
        "more_actions": tr("Plus d'actions", "More actions"),
        "waiting": tr("j'attends ta réponse", "waiting for your answer"),
        "becomes": tr("devient", "becomes"),
        "tones": {
            "ok": tr("Bien", "Good"),
            "fix": tr("À corriger", "To fix"),
            "design": tr("Conception", "Design"),
            "info": tr("À savoir", "Good to know"),
            "assist": tr("Corrigé par vibelearn", "Fixed by vibelearn"),
        },
        "marks_intro": tr("Tu peux aussi me parler depuis ton code :", "You can also talk to me from your code:"),
        "marks": [
            ["//? …", tr("une question sur cette ligne", "a question about this line")],
            ["//// …", tr("le pseudo-code, en phrases", "the pseudo-code, in sentences")],
            ["////! …", tr("le pseudo-code formel", "the formal pseudo-code")],
            ["//n …", tr("la prochaine étape d'une fonctionnalité", "the next step of a feature")],
            ["//t …", tr("une mini-leçon, là, dans ton code", "a short lesson, right there in your code")],
        ],
        "marks_show": tr("Commentaires à écrire dans ton code", "Comments to write in your code"),
        "marks_title": tr("Commentaires à écrire dans ton code", "Comments to write in your code"),
        "marks_hint": tr("Écris-en un sur sa propre ligne, puis sauvegarde.", "Write one on its own line, then save."),
        "marks_note": tr(
            "`////` et `////!` restent dans ton code ; les autres sont retirés une fois lus. Si ton langage commente avec `#` ou `--`, écris `#?`, `####`, `--?`, `----`.",
            "`////` and `////!` stay in your code; the others are removed once read. If your language comments with `#` or `--`, write `#?`, `####`, `--?`, `----`.",
        ),
        "you": tr("Toi", "You"),
        "done": tr("Étape réussie", "Step done"),
        "go": tr("C'est parti", "Let's go"),
        "claim_known": tr("Je connais", "I know it"),
        "claim_unsure": tr("Pas sûr", "Unsure"),
        "claim_unknown": tr("À voir", "To learn"),
        "checklist_hint": tr("Règle ce que tu connais déjà, puis « C'est parti ».", "Set what you already know, then \"Let's go\"."),
        "ask_placeholder": tr("Pose ta question…", "Ask a question…"),
        "ask_keys": tr("Maj+Entrée : nouvelle ligne", "Shift+Enter: new line"),
        "colon": colon(),
        "ask_about": tr("sur la ligne", "about line"),
        "send": tr("Envoyer", "Send"),
        "dont_know": tr("Je ne sais pas", "I don't know"),
        "allow": tr("Autoriser", "Allow"),
        "always": tr("Toujours autoriser", "Always allow"),
        "deny": tr("Refuser", "Deny"),
        "permission": tr("vibelearn demande ton accord", "vibelearn asks for your approval"),
        "write_warning": tr("Seulement pour un squelette ou un test à compléter, jamais pour la solution.", "Only for scaffolding or a test to fill in, never for the solution."),
        "reading": tr("Je lis ton projet pour voir comment le faire chez toi…", "Reading your project to see how to do it here…"),
        "ended": tr("Session terminée. Relance vibelearn pour continuer.", "Session ended. Run vibelearn again to continue."),
        "watching": tr("Code, puis Ctrl+S : je regarde.", "Code, then Ctrl+S: I'll look."),
        "you_said": tr("Tu as demandé", "You asked"),
        "monitor": tr("surveillance", "monitoring"),
        "watch_title": tr("Code, puis Ctrl+S.", "Code, then Ctrl+S."),
        "watch_text": tr("Je te dis si ça casse ou si ça peut être plus solide.", "I'll tell you if it breaks or could be more solid."),
        "assist": tr("assistance", "assist"),
        "assist_text": tr(
            "Je te dis si ça casse ou si ça peut être plus solide. Je ne corrige moi-même que ce qui découle sans choix de ce que tu viens d'écrire.",
            "I'll tell you if it breaks or could be more solid. I only fix myself what follows with no choice from what you just wrote.",
        ),
        "edits_title": tr("J'ai modifié", "I changed"),
        "undo": tr("Annuler", "Undo"),
        "applying": tr("en cours…", "applying…"),
        "edit_hover": tr("vibelearn a modifié cette ligne", "vibelearn changed this line"),
        "undo_hover": tr("Annuler ce changement", "Undo this change"),
        "undo_also": tr("Ctrl+Z marche aussi", "Ctrl+Z works too"),
        "undo_gone": tr("Cette ligne a changé depuis : rien à annuler.", "This line has changed since: nothing to undo."),
        "effort": tr("effort", "effort"),
        "lesson": tr("leçon", "lesson"),
        "how": tr("Comment ?", "How?"),
        "teach": tr("Apprends-moi", "Teach me"),
        "forget": tr("Oublie", "Forget it"),
        "next_hover": tr("Prochaine étape", "Next step"),
        "next_clear": tr("Efface", "Clear"),
        "pseudo_hover": tr("Pseudo-code", "Pseudo-code"),
    })
}

fn status_text(app: &App) -> String {
    if app.session.completed {
        return tr("session terminée", "session finished").to_string();
    }
    match app.phase {
        Phase::Starting => tr("vibelearn démarre…", "vibelearn is starting…").to_string(),
        Phase::Busy => app.activity.clone().unwrap_or_else(|| tr("vibelearn réfléchit", "vibelearn is thinking").to_string()) + "…",
        Phase::Checking => format!("{} {}…", tr("vérification :", "checking:"), app.activity.clone().unwrap_or_default()),
        Phase::Ended => tr("session arrêtée", "session stopped").to_string(),
        Phase::Idle if app.session.exercise.is_some() => tr("à toi de coder", "your turn to code").to_string(),
        Phase::Idle if app.session.mode.watches_every_save() && app.session.auto_check => format!(
            "{} {} {}",
            tr("j'envoie ton code", "I send your code"),
            app.auto_delay().round(),
            tr("s après ta dernière sauvegarde", "s after your last save"),
        ),
        Phase::Idle if app.session.mode.watches_every_save() => {
            tr("clique « Vérifier » pour m'envoyer ton code", "click \"Check\" to send me your code").to_string()
        }
        Phase::Idle if app.session.exercise.is_some() => tr("à toi de coder", "your turn to code").to_string(),
        Phase::Idle => tr("à toi", "your turn").to_string(),
    }
}

/// Le pseudo-code tel qu'il s'affichera, ligne à ligne : sans bloc de code
/// Markdown autour, ni lignes vides au bord, en 20 lignes au plus.
fn pseudo_body(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim_start().starts_with("```")).map(str::trim_end).collect();
    let start = lines.iter().position(|l| !l.is_empty()).unwrap_or(lines.len());
    let end = lines.iter().rposition(|l| !l.is_empty()).map_or(start, |i| i + 1);
    lines[start..end].iter().take(20).copied().collect::<Vec<_>>().join("\n")
}

// ---------------------------------------------------------------------------
// Boucle
// ---------------------------------------------------------------------------

/// Message de l'apprenant en attente que vibelearn soit disponible.
enum Outgoing {
    /// Une question : la réponse ira dans ce fil.
    Ask { display: String, message: String, thread: u64 },
    Command { name: String },
    /// Une mini-leçon, sur une remarque ou demandée par `//t …` ; `origin`
    /// reste affiché pendant toute la leçon.
    Teach { display: String, message: String, origin: Option<String> },
    /// `//n …` : la prochaine étape d'une fonctionnalité, qui s'affichera à cette ligne.
    Next { display: String, message: String, spot: NextSpot },
    /// `//// …` : du pseudo-code, qui s'affichera à côté de la demande.
    Pseudo { display: String, message: String, spot: PseudoSpot },
}

/// Où montrer le pseudo-code demandé par `//// …`.
#[derive(Clone)]
struct PseudoSpot {
    file: PathBuf,
    request: annotate::PseudoRequest,
}

impl PseudoSpot {
    /// Clé de `pseudo_pending`.
    fn key(&self) -> (PathBuf, String) {
        (self.file.clone(), format!("{}{}", self.request.style.as_str(), self.request.text))
    }
}

/// Où montrer la prochaine étape demandée par `//n …`.
#[derive(Clone)]
struct NextSpot {
    file: PathBuf,
    line: usize,
    feature: String,
}

struct Front {
    root: PathBuf,
    ui: PathBuf,
    editor: Editor,
    /// Le fil de la question à laquelle vibelearn répond en ce moment.
    focus: Option<u64>,
    /// La ligne `//n …` à laquelle vibelearn répond en ce moment.
    focus_next: Option<NextSpot>,
    /// La demande `//// …` à laquelle vibelearn répond en ce moment.
    focus_pseudo: Option<PseudoSpot>,
    /// Demandes `//// …` en attente ou en cours : (fichier, texte).
    pseudo_pending: HashSet<(PathBuf, String)>,
    queue: VecDeque<Outgoing>,
    /// Fichiers sauvegardés, à relire une fois stables (lignes `//?`).
    saved: HashMap<PathBuf, Instant>,
    was_busy: bool,
    // Publication
    last_state: String,
    version: u64,
    reveal: Option<(PathBuf, usize, u64)>,
    notes_revealed: bool,
    opened_demo: Option<String>,
    // Extension
    install_done: Arc<AtomicBool>,
    install_done_at: Option<Instant>,
    acked: bool,
    folder_opened: Option<Instant>,
    warned: bool,
    /// Dernière lecture de `ack.json`.
    ack_checked: Option<Instant>,
    /// Version de l'extension qui affiche, si ce n'est pas la bonne (déjà signalé).
    stale: Option<String>,
    error_reported: bool,
    last_flash: Option<String>,
    printed: usize,
}

impl Front {
    fn new(app: &mut App, editor: Editor) -> Self {
        let sandbox = paths::sandbox_dir(&app.opts.root);
        // Restes des anciens modes (deux fichiers à côté du code).
        let _ = std::fs::remove_file(sandbox.join("toi.md"));
        let _ = std::fs::remove_file(sandbox.join("vibelearn.md"));
        let ui = sandbox.join("ui");
        let _ = std::fs::create_dir_all(ui.join("inbox"));
        let _ = std::fs::remove_file(ui.join("ack.json"));
        // Une session reprise retrouve l'endroit où écrire.
        app.restore_brief();

        // L'extension s'installe (ou se vérifie) en fond : rien n'attend.
        let install_done = Arc::new(AtomicBool::new(!editor.enabled()));
        if editor.enabled() {
            let flag = install_done.clone();
            let editor = editor.clone();
            let dir = sandbox.clone();
            std::thread::spawn(move || {
                match vscode::ensure_installed(&editor, &dir) {
                    Install::AlreadyThere => {}
                    Install::Installed => println!(
                        "  {}",
                        tr(
                            "Extension VS Code vibelearn installée. Si VS Code était déjà ouvert, recharge sa fenêtre : Ctrl+Shift+P, puis « Developer: Reload Window ».",
                            "vibelearn VS Code extension installed. If VS Code was already open, reload its window: Ctrl+Shift+P, then \"Developer: Reload Window\".",
                        )
                    ),
                    Install::Failed(e) => eprintln!(
                        "  {} {e}\n  {}",
                        tr("L'extension VS Code n'a pas pu être installée :", "The VS Code extension could not be installed:"),
                        tr("Sans elle, les explications ne s'affichent que dans ce terminal.", "Without it, explanations only show in this terminal."),
                    ),
                }
                flag.store(true, Ordering::Relaxed);
            });
        }

        Self {
            root: app.opts.root.clone(),
            ui,
            editor,
            focus: None,
            focus_next: None,
            focus_pseudo: None,
            pseudo_pending: HashSet::new(),
            queue: VecDeque::new(),
            saved: HashMap::new(),
            was_busy: false,
            last_state: String::new(),
            version: 0,
            reveal: None,
            notes_revealed: false,
            opened_demo: app.session.demo.as_ref().map(|d| d.title.clone()),
            install_done,
            install_done_at: None,
            acked: false,
            folder_opened: None,
            warned: false,
            ack_checked: None,
            stale: None,
            error_reported: false,
            last_flash: None,
            printed: app.session.transcript.len(),
        }
    }

    fn rel(&self, path: &Path) -> String {
        paths::display_rel(&self.root, path)
    }

    fn note_fs(&mut self, changed: &[PathBuf]) {
        for path in changed {
            if annotate::style_for(path).is_some() && !paths::in_sandbox(&self.root, path) {
                self.saved.insert(path.clone(), Instant::now());
            }
        }
    }

    /// Message pour vibelearn : la question, et la ligne de code dont il parle.
    fn question_message(&self, app: &App, thread: u64, text: &str) -> String {
        let t = app.threads.thread(thread).filter(|t| t.has_location());
        let file = t.map(|t| self.rel(&t.file)).unwrap_or_default();
        let line = t.map(|t| t.line + 1).unwrap_or(0);
        let line_text = t.and_then(|t| t.anchor.as_deref());
        let selection = app.threads.thread(thread).and_then(|t| t.selection.as_deref());
        prompt::code_question(&file, line, line_text, selection, text)
    }

    /// Une question ou une commande de l'apprenant, envoyée quand vibelearn sera libre.
    fn queue_question(&mut self, app: &mut App, at: Option<(&Path, usize)>, text: &str, selection: Option<String>) {
        if let Some(name) = as_command(text) {
            self.queue.push_back(Outgoing::Command { name });
            return;
        }
        match app.threads.learner_question(at, text, selection) {
            Ok(id) => {
                let message = self.question_message(app, id, text);
                self.queue.push_back(Outgoing::Ask { display: text.to_string(), message, thread: id });
            }
            Err(e) => eprintln!("  vibelearn : {e}"),
        }
    }

    fn read_inbox(&mut self, app: &mut App) {
        let Ok(dir) = std::fs::read_dir(self.ui.join("inbox")) else { return };
        let mut files: Vec<PathBuf> =
            dir.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
        files.sort();
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            let _ = std::fs::remove_file(&file);
            match serde_json::from_str::<Incoming>(&text) {
                Ok(message) => self.on_incoming(app, message),
                Err(e) => eprintln!("  vibelearn : {} ({e})", tr("message illisible de VS Code", "unreadable message from VS Code")),
            }
        }
    }

    fn on_incoming(&mut self, app: &mut App, message: Incoming) {
        match &message {
            Incoming::Answer { prompt, .. } => {
                if let Some(modal) = app.modal.as_mut().filter(|m| m.reply_id() == *prompt)
                    && apply_answer(modal, &message)
                {
                    app.resolve_modal();
                }
            }
            Incoming::Ask { text, file, line, selection } => {
                // Une réponse libre attendue : c'est elle.
                if let Some(Modal::Ask(a)) = &mut app.modal
                    && matches!(a.kind, AskKind::Free | AskKind::Predict)
                {
                    a.text = text.trim().to_string();
                    app.resolve_modal();
                    return;
                }
                let at = file.as_deref().filter(|f| f.starts_with(&self.root)).map(|f| (f, line.unwrap_or(0)));
                let selection = selection.clone().filter(|s| !s.trim().is_empty());
                self.queue_question(app, at, text, selection);
            }
            Incoming::Command { name } => self.queue.push_back(Outgoing::Command { name: name.clone() }),
            Incoming::EditResult { id, ok, line, error } => app.edit_result(*id, *ok, *line, error.clone()),
            Incoming::EditUndone { id, saved } => app.edit_undone(*id, *saved),
            Incoming::How { id } => self.on_remark(app, *id, false),
            Incoming::Teach { id } => self.on_remark(app, *id, true),
            Incoming::Forget { id } => app.forget_remark(*id),
        }
    }

    /// « Comment ? » ou « Apprends-moi » sur une remarque de vibelearn.
    fn on_remark(&mut self, app: &mut App, id: u64, teach: bool) {
        let Some(t) = app.threads.thread(id).filter(|t| t.kind == Kind::Feedback && t.has_location()) else { return };
        let (file, line, anchor) = (t.file.clone(), t.line, t.anchor.clone());
        let tone = crate::threads::tone_or_default(t.tone.as_deref()).to_string();
        let remark = t.vibelearn_notes().join(" ");
        let rel = self.rel(&file);
        let short: String = remark.chars().take(80).collect();
        if teach {
            let display = format!("{} {short}", tr("Apprends-moi :", "Teach me:"));
            let message = prompt::teach(&rel, line + 1, anchor.as_deref(), &tone, &remark);
            self.queue.push_back(Outgoing::Teach { display, message, origin: Some(remark) });
            return;
        }
        let display = tr("Comment ?", "How?");
        match app.threads.learner_question(Some((&file, line)), display, None) {
            Ok(thread) => {
                let message = prompt::code_question(&rel, line + 1, anchor.as_deref(), None, &prompt::how(&remark));
                self.queue.push_back(Outgoing::Ask { display: display.to_string(), message, thread });
            }
            Err(e) => eprintln!("  vibelearn : {e}"),
        }
    }

    /// Lignes `//? …` écrites dans le code et sauvegardées.
    fn read_code_questions(&mut self, app: &mut App) {
        let ready: Vec<PathBuf> =
            self.saved.iter().filter(|(_, t)| t.elapsed() >= SETTLE).map(|(p, _)| p.clone()).collect();
        for path in ready {
            self.saved.remove(&path);
            for q in annotate::take_questions(&path) {
                if let Some(Modal::Ask(a)) = &mut app.modal
                    && matches!(a.kind, AskKind::Free | AskKind::Predict)
                {
                    a.text = q.text.clone();
                    app.resolve_modal();
                    continue;
                }
                let index = q.next_code.as_ref().map(|(_, i)| *i).unwrap_or(q.index);
                self.queue_question(app, Some((&path, index)), &q.text, None);
            }
            for q in annotate::take_next_steps(&path) {
                self.queue_next_step(&path, q);
            }
            for q in annotate::take_lessons(&path) {
                self.queue_lesson(&path, q);
            }
            let requests = annotate::pseudo_requests(&path);
            for r in app.threads.sync_pseudo(&path, &requests) {
                self.queue_pseudo(&path, r);
            }
        }
    }

    /// `//n …` : la réponse ira en bout de la ligne où l'apprenant l'a écrit.
    fn queue_next_step(&mut self, path: &Path, q: annotate::CodeQuestion) {
        let lines = annotate::TextFile::load(path).map(|f| f.lines).unwrap_or_default();
        // Là où était le `//n`, même si la ligne est vide : c'est là qu'il veut écrire.
        let line = q.index.min(lines.len().saturating_sub(1));
        let line_text = lines.get(line).map(|l| l.trim()).filter(|l| !l.is_empty());
        let message = prompt::next_step(&self.rel(path), line + 1, line_text, &q.text);
        let display = format!("{} {}", tr("Prochaine étape :", "Next step:"), q.text);
        let spot = NextSpot { file: path.to_path_buf(), line, feature: q.text };
        self.queue.push_back(Outgoing::Next { display, message, spot });
    }

    /// `//// …` : le pseudo-code ira à côté de la demande, qui reste dans le code.
    fn queue_pseudo(&mut self, path: &Path, r: annotate::PseudoRequest) {
        let lines = annotate::TextFile::load(path).map(|f| f.lines).unwrap_or_default();
        let line_text = lines.get(r.line).map(|l| l.trim());
        let message = prompt::pseudo_code(&self.rel(path), r.line + 1, line_text, &r.text, r.style);
        let display = format!("{} {}", tr("Pseudo-code :", "Pseudo-code:"), r.text);
        let spot = PseudoSpot { file: path.to_path_buf(), request: r };
        if !self.pseudo_pending.insert(spot.key()) {
            return;
        }
        self.queue.push_back(Outgoing::Pseudo { display, message, spot });
    }

    /// `//t …` : une mini-leçon sur ce qu'il veut savoir faire, ancrée sur la
    /// ligne de code sous la demande.
    fn queue_lesson(&mut self, path: &Path, q: annotate::CodeQuestion) {
        let (line, anchor) = match &q.next_code {
            Some((text, index)) => (*index, Some(text.as_str())),
            None => (q.index, None),
        };
        let message = prompt::teach_notion(&self.rel(path), line + 1, anchor, &q.text);
        let display = format!("{} {}", tr("Apprends-moi :", "Teach me:"), q.text);
        self.queue.push_back(Outgoing::Teach { display, message, origin: Some(q.text) });
    }

    /// Envoie le prochain message en attente quand vibelearn est disponible.
    fn flush_queue(&mut self, app: &mut App) {
        if app.phase != Phase::Idle || app.modal.is_some() {
            return;
        }
        let Some(next) = self.queue.pop_front() else { return };
        match next {
            Outgoing::Ask { display, message, thread } => {
                self.focus = Some(thread);
                self.focus_next = None;
                self.focus_pseudo = None;
                app.ask_from_code(display, message);
            }
            Outgoing::Teach { display, message, origin } => {
                app.lesson_origin = origin;
                self.focus = None;
                self.focus_next = None;
                self.focus_pseudo = None;
                app.ask_from_code(display, message);
            }
            Outgoing::Next { display, message, spot } => {
                self.focus = None;
                self.focus_next = Some(spot);
                self.focus_pseudo = None;
                app.ask_from_code(display, message);
            }
            Outgoing::Pseudo { display, message, spot } => {
                self.focus = None;
                self.focus_next = None;
                self.focus_pseudo = Some(spot);
                app.ask_from_code(display, message);
            }
            Outgoing::Command { name } => {
                self.focus = None;
                self.focus_next = None;
                self.focus_pseudo = None;
                let before = app.session.transcript.len();
                app.command(&name);
                // Ce que la commande affiche sans passer par vibelearn (plan, notes…).
                if app.phase == Phase::Idle {
                    let notices: Vec<String> = app.session.transcript[before..]
                        .iter()
                        .filter_map(|e| match e {
                            Entry::Notice(t) => Some(t.clone()),
                            _ => None,
                        })
                        .collect();
                    if !notices.is_empty() {
                        app.threads.note(&notices.join("\n\n"));
                    }
                }
            }
        }
    }

    /// Fin d'un tour de vibelearn : la réponse à une question va avec la
    /// question ; le reste, s'il n'a rien montré d'autre, va dans le panneau.
    /// En surveillance, le texte (une proposition de structure) va toujours
    /// dans le panneau, et remplace celui de la sauvegarde précédente.
    fn on_turn_end(&mut self, app: &mut App) {
        let monitor = app.session.mode.watches_every_save();
        let entries = &app.session.transcript;
        let start = app.turn_start.min(entries.len());
        let from_learner = start > 0 && matches!(entries[start - 1], Entry::User(_));
        let text = entries[start..]
            .iter()
            .filter_map(|e| match e {
                Entry::Tutor(t) if !t.trim().is_empty() => Some(t.trim().to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        if monitor && !from_learner && (app.threads.spoke() || !text.is_empty()) {
            app.threads.drop_older_notes();
        }
        // `//// …` : le pseudo-code s'affiche dans le code, à côté de la demande.
        if let Some(spot) = self.focus_pseudo.take() {
            self.pseudo_pending.remove(&spot.key());
            if from_learner && !text.is_empty() {
                match app.threads.post_pseudo(&spot.file, spot.request.line, &spot.request, &pseudo_body(&text)) {
                    Ok(_) => return,
                    Err(e) => eprintln!("  vibelearn : {e}"),
                }
            }
        }
        if text.is_empty() {
            return;
        }
        // `//n …` : la prochaine étape s'affiche dans le code, pas dans le panneau.
        if from_learner && let Some(spot) = self.focus_next.take() {
            match app.threads.post_next(&spot.file, spot.line, &spot.feature, &text) {
                Ok(_) => return,
                Err(e) => eprintln!("  vibelearn : {e}"),
            }
        }
        let answered = from_learner
            && self.focus.is_some_and(|id| app.threads.post(Path::new(""), &Place::Thread(id), Kind::Question, None, None, &text).is_ok());
        if !answered && (from_learner || monitor || !app.threads.spoke()) {
            app.threads.note(&text);
        }
    }

    fn sync(&mut self, app: &mut App) {
        // Les corrections du mode assistance passent par l'éditeur quand il affiche la session.
        app.ui_live = self.acked;
        self.read_inbox(app);
        self.read_code_questions(app);

        let busy = matches!(app.phase, Phase::Busy | Phase::Checking | Phase::Starting);
        if self.was_busy && !busy {
            self.on_turn_end(app);
        }
        self.was_busy = busy;
        self.flush_queue(app);

        if app.show_help {
            app.show_help = false;
            app.threads.note(crate::app::help_text());
        }

        self.report(app);
        self.publish(app);
        self.watch_extension(app);
    }

    /// Journal du terminal, et fichier à montrer dans l'éditeur.
    fn report(&mut self, app: &mut App) {
        for posted in app.threads.drain_posted() {
            let first: String = posted.text.lines().next().unwrap_or("").chars().take(70).collect();
            if posted.file.as_os_str().is_empty() {
                println!("  - {first}");
            } else {
                println!("  → {}:{}  {first}", self.rel(&posted.file), posted.line);
            }
            if self.folder_opened.is_some() && !self.acked {
                // Pas d'extension : le texte entier, ici.
                for line in posted.text.lines().skip(1) {
                    println!("      {line}");
                }
            }
            // Nouvelle étape : l'éditeur va à la ligne où écrire.
            if posted.kind == Kind::Brief {
                self.reveal_at(posted.file.clone(), posted.line.saturating_sub(1));
            }
        }

        let demo = app.session.demo.as_ref().map(|d| d.title.clone());
        if demo != self.opened_demo {
            self.opened_demo = demo;
            if let Some(file) = app.session.demo.as_ref().and_then(|d| d.files.first()) {
                let abs = paths::normalize(&self.root, Path::new(file));
                if abs.is_file() {
                    println!("  → {} ({})", self.rel(&abs), tr("démo", "demo"));
                    self.reveal_at(abs, 0);
                }
            }
        }

        let flash = app.flash.as_ref().map(|(t, _)| t.clone());
        if flash.is_some() && flash != self.last_flash {
            println!("  {}", flash.clone().unwrap_or_default());
            self.last_flash = flash;
        }

        for entry in &app.session.transcript[self.printed.min(app.session.transcript.len())..] {
            match entry {
                Entry::Error(text) => eprintln!("\n{text}\n"),
                Entry::Output(text) => {
                    let lines: Vec<&str> = text.lines().collect();
                    if let (Some(first), Some(last)) = (lines.first(), lines.last()) {
                        println!("  {first} → {last}");
                    }
                }
                _ => {}
            }
        }
        self.printed = app.session.transcript.len();

        if app.session.completed
            && !self.notes_revealed
            && let Some(notes) = &app.session.notes_file
        {
            self.notes_revealed = true;
            self.reveal_at(self.root.join(notes), 0);
        }
    }

    fn reveal_at(&mut self, file: PathBuf, line: usize) {
        let seq = self.reveal.as_ref().map(|(_, _, s)| s + 1).unwrap_or(1);
        // Sans extension, la commande `code` fait le travail.
        if self.folder_opened.is_some() && !self.acked {
            self.editor.open(&file, Some(line + 1));
        }
        self.reveal = Some((file, line, seq));
    }

    /// Tout ce que l'extension affiche.
    fn state(&self, app: &App) -> Value {
        let ended = app.quit || app.phase == Phase::Ended;
        let threads = &app.threads;
        let plan = app.session.plan.as_ref().map(|p| {
            json!({
                "goal": p.goal,
                "steps": p.steps.iter().map(|s| json!({
                    "title": s.title,
                    "status": match s.status {
                        StepStatus::Pending => "pending",
                        StepStatus::Active => "active",
                        StepStatus::Done => "done",
                        StepStatus::Skipped => "skipped",
                    },
                })).collect::<Vec<_>>(),
            })
        });
        let step = app.session.exercise.as_ref().map(|ex| {
            let brief = threads.brief();
            json!({
                "title": brief.and_then(|b| b.title.clone()).unwrap_or_else(|| ex.title.clone()),
                "goal": ex.goal,
                "notes": ex.notes.as_deref().map(crate::app::notes_markdown),
                "criteria": ex.success_criteria,
                "here": ex.here,
                "file": brief.map(|b| &b.file),
                "line": brief.map(|b| b.line),
                "anchor": brief.and_then(|b| b.anchor.as_deref()),
                "hint_level": ex.hint_level,
                "origin": ex.origin,
            })
        });
        let annotations: Vec<Value> = threads
            .of_kind(Kind::Feedback)
            .map(|t| {
                json!({
                    "id": t.id,
                    "file": t.file,
                    "line": t.line,
                    "anchor": t.anchor,
                    "tone": crate::threads::tone_or_default(t.tone.as_deref()),
                    "text": t.vibelearn_notes().join("\n\n"),
                })
            })
            .collect();
        let talk: Vec<Value> = threads
            .of_kind(Kind::Question)
            .map(|t| {
                json!({
                    "id": t.id,
                    "question": t.learner_notes().join("\n"),
                    "answers": t.vibelearn_notes(),
                    "file": t.has_location().then_some(&t.file),
                    "line": t.line,
                })
            })
            .collect();
        let next_steps: Vec<Value> = threads
            .of_kind(Kind::Next)
            .map(|t| {
                json!({
                    "id": t.id,
                    "file": t.file,
                    "line": t.line,
                    "anchor": t.anchor,
                    "feature": t.title,
                    "text": t.vibelearn_notes().join("

"),
                })
            })
            .collect();
        let pseudo: Vec<Value> = threads
            .of_kind(Kind::Pseudo)
            .map(|t| {
                json!({
                    "id": t.id,
                    "file": t.file,
                    "line": t.line,
                    "anchor": t.anchor,
                    "request": t.title,
                    "style": t.tone,
                    "text": t.vibelearn_notes().join("\n"),
                })
            })
            .collect();
        let notes: Vec<Value> =
            threads.of_kind(Kind::Note).map(|t| json!({ "id": t.id, "text": t.vibelearn_notes().join("\n\n") })).collect();
        // Mode assistance : les corrections à appliquer (pending) et celles à montrer (applied).
        let edits: Vec<Value> = app
            .assist_edits
            .iter()
            .filter(|e| matches!(e.status, EditStatus::Pending | EditStatus::Applied))
            .map(|e| {
                json!({
                    "id": e.id,
                    "file": e.file,
                    "line": e.line,
                    "old": e.old,
                    "new": e.new,
                    "reason": e.reason,
                    "status": e.status,
                })
            })
            .collect();
        json!({
            "protocol": 2,
            "root": self.root,
            "topic": app.session.topic,
            "status": if ended { "ended" } else {
                match app.phase {
                    Phase::Starting => "starting",
                    Phase::Busy | Phase::Checking => "busy",
                    Phase::Idle => "idle",
                    Phase::Ended => "ended",
                }
            },
            "status_text": status_text(app),
            "plan": plan,
            "step": if ended { Value::Null } else { step.unwrap_or(Value::Null) },
            "annotations": if ended { Vec::new() } else { annotations },
            "talk": if ended { Vec::new() } else { talk },
            "next_steps": if ended { Vec::new() } else { next_steps },
            "pseudo": if ended { Vec::new() } else { pseudo },
            "notes": if ended { Vec::new() } else { notes },
            "edits": if ended { Vec::new() } else { edits },
            "celebrate": app.celebrate.as_ref().map(|c| json!({ "title": c.title, "summary": c.summary, "seq": c.seq })),
            "prompt": if ended { Value::Null } else { app.modal.as_ref().map(prompt_json).unwrap_or(Value::Null) },
            "reveal": self.reveal.as_ref().map(|(file, line, seq)| json!({ "file": file, "line": line, "seq": seq })),
            "mode": app.session.mode,
            // Le modèle qui répond (connu au démarrage du moteur) et l'effort demandé.
            "model": app.model,
            "effort": app.opts.effort,
            "actions": actions(app.session.mode, app.session.exercise.is_some()),
            "strings": strings(),
            "extension": vscode::version(),
        })
    }

    /// Écrit `state.json` quand quelque chose a changé.
    fn publish(&mut self, app: &App) {
        let state = self.state(app);
        let text = state.to_string();
        if text == self.last_state {
            return;
        }
        self.last_state = text;
        self.version += 1;
        let mut state = state;
        state["version"] = json!(self.version);
        let path = self.ui.join("state.json");
        let tmp = self.ui.join("state.json.tmp");
        if std::fs::write(&tmp, state.to_string()).is_ok() {
            // L'extension peut être en train de le lire : on réessaie un peu.
            for _ in 0..10 {
                if std::fs::rename(&tmp, &path).is_ok() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        }
    }

    /// Lit ce que l'extension dit avoir affiché.
    fn read_ack(&mut self) {
        if self.ack_checked.is_some_and(|t| t.elapsed() < Duration::from_millis(800)) {
            return;
        }
        self.ack_checked = Some(Instant::now());
        let Ok(text) = std::fs::read_to_string(self.ui.join("ack.json")) else { return };
        let Ok(ack) = serde_json::from_str::<Value>(&text) else { return };
        let running = ack["extension"].as_str().unwrap_or("").to_string();
        if running != vscode::version() {
            // Une fenêtre VS Code ouverte avant la mise à jour garde l'ancienne extension.
            if self.stale.as_deref() != Some(running.as_str()) {
                eprintln!(
                    "  {} ({running}). {}",
                    tr("VS Code fait tourner une ancienne version de l'extension vibelearn", "VS Code is running an old version of the vibelearn extension"),
                    tr(
                        "Recharge la fenêtre : Ctrl+Shift+P, puis « Developer: Reload Window ». Ta session continue.",
                        "Reload the window: Ctrl+Shift+P, then \"Developer: Reload Window\". Your session goes on.",
                    ),
                );
                self.stale = Some(running);
            }
            self.acked = false;
            return;
        }
        if let Some(error) = ack["error"].as_str()
            && !self.error_reported
        {
            self.error_reported = true;
            eprintln!("  {} {error}", tr("L'extension vibelearn n'arrive pas à afficher :", "The vibelearn extension cannot display:"));
        }
        if !self.acked && (self.warned || self.stale.is_some()) {
            println!("  {}", tr("VS Code affiche maintenant vibelearn.", "VS Code now shows vibelearn."));
        }
        self.acked = true;
        self.stale = None;
    }

    /// L'extension a-t-elle affiché l'état ? Sinon, on ouvre le dossier dans VS Code.
    fn watch_extension(&mut self, app: &App) {
        self.read_ack();
        // La bonne extension tourne déjà quelque part, il faut juste recharger : rien à ouvrir.
        if self.stale.is_some() {
            return;
        }
        if self.acked || !self.editor.enabled() || !self.install_done.load(Ordering::Relaxed) {
            return;
        }
        let done_at = *self.install_done_at.get_or_insert_with(Instant::now);
        match self.folder_opened {
            // Aucune fenêtre VS Code n'affiche la session : on ouvre le dossier.
            None if done_at.elapsed() > Duration::from_millis(1500) => {
                self.editor.open(&self.root, None);
                self.folder_opened = Some(Instant::now());
            }
            // Un premier lancement de VS Code peut être long : on laisse le temps.
            Some(at) if !self.warned && at.elapsed() > Duration::from_secs(45) && app.phase != Phase::Ended => {
                self.warned = true;
                eprintln!(
                    "  {}",
                    tr(
                        "VS Code n'affiche pas vibelearn : ouvre ce dossier dans VS Code (l'extension vibelearn doit être active). En attendant, les explications s'affichent ici.",
                        "VS Code does not show vibelearn: open this folder in VS Code (the vibelearn extension must be active). Meanwhile, explanations show here.",
                    )
                );
            }
            _ => {}
        }
    }
}

/// Fait tourner la session : tout dans VS Code, rien dans le terminal.
pub async fn run(mut app: App, mut events: mpsc::UnboundedReceiver<AppEvent>, editor: Editor) -> Result<Outcome> {
    println!(
        "vibelearn{}{}\n  {}\n  {}",
        colon(),
        app.session.topic,
        tr(
            "Tout se passe dans VS Code : le panneau vibelearn à droite, et des repères dans ton code.",
            "Everything happens in VS Code: the vibelearn panel on the right, and markers in your code.",
        ),
        tr("Ctrl+C ici pour arrêter ; la session est sauvegardée.", "Ctrl+C here to stop; the session is saved."),
    );
    let mut front = Front::new(&mut app, editor);

    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    let mut ended_since: Option<Instant> = None;
    loop {
        tokio::select! {
            Some(event) = events.recv() => {
                if let AppEvent::Fs(paths) = &event {
                    front.note_fs(paths);
                }
                app.on_event(event);
                while let Ok(event) = events.try_recv() {
                    if let AppEvent::Fs(paths) = &event {
                        front.note_fs(paths);
                    }
                    app.on_event(event);
                }
            }
            _ = ticker.tick() => app.on_tick(),
            _ = tokio::signal::ctrl_c() => break,
        }
        front.sync(&mut app);

        // La session se ferme d'elle-même une fois conclue, ou si le moteur s'est arrêté.
        let finished = app.session.completed && app.phase == Phase::Idle && app.modal.is_none() && front.queue.is_empty();
        if app.quit {
            break;
        }
        if finished || app.phase == Phase::Ended {
            if ended_since.get_or_insert_with(Instant::now).elapsed() > Duration::from_millis(1500) {
                break;
            }
        } else {
            ended_since = None;
        }
    }

    let had_engine = app.shutdown();
    app.quit = true;
    front.report(&mut app);
    front.publish(&app);
    if had_engine {
        let deadline = tokio::time::sleep(Duration::from_millis(2500));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                event = events.recv() => match event {
                    Some(AppEvent::Agent(_, AgentEvent::Exited { .. })) | None => break,
                    Some(_) => {}
                },
            }
        }
    }
    Ok(Outcome {
        completed: app.session.completed,
        notes_file: app.session.notes_file.clone(),
        turns: app.session.turns,
        mode: app.session.mode,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modal::{Ask, Checklist, PermissionPrompt};
    use serde_json::json;

    fn answer(v: Value) -> Incoming {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn the_checklist_answer_says_what_is_known_and_what_is_unsure() {
        let c = Checklist::from_args(
            1,
            &json!({ "title": "T", "items": [
                { "id": "a", "label": "A", "why": "parce que", "evidence": "src/a.rs", "assumed_known": true },
                { "id": "b", "label": "B", "assumed_known": false },
                { "id": "c", "label": "C", "assumed_known": false },
            ]}),
        )
        .unwrap();
        let mut modal = Modal::Checklist(c);
        let shown = prompt_json(&modal);
        assert_eq!(shown["items"][0]["state"], "known");
        assert_eq!(shown["items"][1]["state"], "unknown");
        assert!(!apply_answer(&mut modal, &answer(json!({ "type": "answer", "prompt": 1 }))));
        assert!(apply_answer(&mut modal, &answer(json!({ "type": "answer", "prompt": 1, "known": ["b"], "unsure": ["c"] }))));
        let Modal::Checklist(c) = &modal else { unreachable!() };
        assert_eq!(c.items.iter().map(|i| i.state).collect::<Vec<_>>(), vec![Claim::Unknown, Claim::Known, Claim::Unsure]);
    }

    #[test]
    fn a_choice_and_a_permission_come_back_as_an_index_and_a_decision() {
        let mut modal = Modal::Ask(Ask::from_args(2, &json!({ "kind": "choice", "question": "Q ?", "options": ["un", "deux"] })).unwrap());
        assert!(!apply_answer(&mut modal, &answer(json!({ "type": "answer", "prompt": 2, "choice": 5 }))));
        assert!(apply_answer(&mut modal, &answer(json!({ "type": "answer", "prompt": 2, "choice": 1 }))));
        let Modal::Ask(a) = &modal else { unreachable!() };
        assert_eq!(a.choice, Some(1));

        let mut modal = Modal::Permission(PermissionPrompt {
            reply: 4,
            tool: "Bash".into(),
            headline: "vibelearn veut lancer une commande :".into(),
            detail: "cargo test".into(),
            always_key: "Bash:cargo".into(),
            write_path: None,
            decision: None,
        });
        assert_eq!(prompt_json(&modal)["kind"], "permission");
        assert!(apply_answer(&mut modal, &answer(json!({ "type": "answer", "prompt": 4, "decision": "deny" }))));
        let Modal::Permission(p) = &modal else { unreachable!() };
        assert_eq!(p.decision, Some(Decision::Deny));
    }

    #[test]
    fn a_question_from_the_panel_may_come_with_the_line_under_the_cursor() {
        let m = answer(json!({ "type": "ask", "text": "pourquoi ?", "file": "D:/p/src/a.rs", "line": 3, "selection": "let a" }));
        let Incoming::Ask { text, file, line, selection } = m else { panic!() };
        assert_eq!((text.as_str(), line, selection.as_deref()), ("pourquoi ?", Some(3), Some("let a")));
        assert!(file.is_some());
        assert!(matches!(answer(json!({ "type": "ask", "text": "et sans ligne ?" })), Incoming::Ask { file: None, .. }));
    }

    #[test]
    fn a_single_known_word_or_a_slash_is_a_command_anything_else_is_a_question() {
        assert_eq!(as_command("indice"), Some("indice".into()));
        assert_eq!(as_command("Indice !"), Some("indice".into()));
        assert_eq!(as_command("/run cargo test"), Some("run cargo test".into()));
        assert_eq!(as_command("indice sur la boucle ?"), None);
        assert_eq!(as_command("pourquoi &self ?"), None);
    }
}
