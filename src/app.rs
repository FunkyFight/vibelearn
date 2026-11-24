//! Contrôleur de session : reçoit les événements (moteur, fichiers, commandes
//! terminées) et les actions de l'apprenant, et fait évoluer l'état de la
//! session. Ce que vibelearn dit à l'apprenant s'affiche près des lignes de son
//! code, dans le panneau et les repères de VS Code (`threads.rs`, `front.rs`).

use chrono::Utc;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::annotate;
use crate::assist::{self, AssistEdit, EditStatus};
use crate::threads::{Kind, Place, Posted, Threads};
use crate::claude::{AgentEvent, Engine, EngineConfig};
use crate::i18n::{colon, tr};
use crate::modal::{Ask, Checklist, Decision, Modal, PermissionPrompt};
use crate::profile::{Profile, SessionRecord, Source};
use crate::project::ProjectFacts;
use crate::runner::{self, RunOutput};
use crate::session::{Claim, Demo, Entry, Exercise, Plan, SessionConcept, SessionState, Step, StepStatus};
use crate::watch::{self, ChangeKind, FileChange, Tracker};
use crate::{paths, prompt};

pub enum AppEvent {
    /// Événement du moteur, étiqueté par la génération du moteur qui l'a émis.
    Agent(u32, AgentEvent),
    Fs(Vec<PathBuf>),
    CheckDone { cmd: String, trigger: &'static str, output: RunOutput },
    RunDone { cmd: String, share: bool, output: RunOutput },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Le moteur démarre.
    Starting,
    /// C'est à l'apprenant de jouer.
    Idle,
    /// vibelearn travaille.
    Busy,
    /// La commande de vérification tourne avant l'envoi des changements.
    Checking,
    /// Plus de moteur (erreur, connexion requise).
    Ended,
}

/// Une étape réussie : le panneau la fête une fois (`seq` change à chaque fois).
pub struct Celebrate {
    pub title: String,
    pub summary: String,
    pub seq: u32,
}

/// Les faits et pièges d'une étape, en liste markdown (une ligne = un point).
pub fn notes_markdown(notes: &str) -> String {
    notes
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| if l.starts_with("- ") || l.starts_with("* ") { l.to_string() } else { format!("- {l}") })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Ce qu'il reste à dire dans le terminal quand la session se ferme.
pub struct Outcome {
    pub completed: bool,
    pub notes_file: Option<String>,
    pub turns: u32,
    pub mode: crate::session::Mode,
}

/// Comment vibelearn joint le modèle (voir `vibelearn login`).
#[derive(Debug, Clone)]
pub enum Backend {
    /// Le binaire `claude`, avec l'abonnement.
    Claude { bin: PathBuf, keep_api_key: bool },
    /// vibelearn tient lui-même la conversation avec une API (Anthropic, OpenAI).
    Api { dialect: crate::api::Dialect, url: String, key: String },
}

pub struct Options {
    pub root: PathBuf,
    /// `None` : moteur factice (`mock`) ou introuvable.
    pub backend: Option<Backend>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mock: bool,
    pub auto_delay: Duration,
}

pub struct App {
    pub opts: Options,
    pub session: SessionState,
    pub profile: Profile,
    facts: ProjectFacts,
    engine: Option<Engine>,
    generation: u32,
    tx: mpsc::UnboundedSender<AppEvent>,

    pub phase: Phase,
    pub streaming: Option<usize>,
    pub activity: Option<String>,
    pub flash: Option<(String, Instant)>,
    /// Interaction en attente d'une réponse de l'apprenant.
    pub modal: Option<Modal>,
    modal_queue: VecDeque<Modal>,
    /// L'apprenant a demandé l'aide (`/help`).
    pub show_help: bool,
    pub model: Option<String>,

    tracker: Tracker,
    /// Ce que vibelearn montre pendant l'étape : ligne où écrire, annotations, questions.
    pub threads: Threads,
    /// Dernière ligne ajoutée par l'apprenant lors du dernier envoi : fichier,
    /// texte de la ligne, numéro approximatif.
    pub last_spot: Option<(PathBuf, String, usize)>,
    /// Dernière étape réussie, montrée dans le panneau.
    pub celebrate: Option<Celebrate>,
    /// Début, dans le fil, du tour en cours de vibelearn.
    pub turn_start: usize,
    /// Changements en attente d'envoi (pour l'affichage).
    pub pending: Vec<FileChange>,
    last_change: Option<Instant>,
    stats_stale: bool,
    snoozed: bool,

    attachments: Vec<String>,
    approved: HashSet<String>,
    /// Fichiers hors bac à sable que vibelearn a été autorisé à écrire ce tour-ci.
    tutor_paths: HashSet<PathBuf>,
    /// Commandes de vibelearn en cours : identifiant d'appel → fichiers déjà en attente avant.
    shell_running: HashMap<String, BTreeSet<PathBuf>>,
    /// Fichiers dont le modèle connaît la version actuelle en entier (envoyée
    /// ou lue) : pour eux, un diff suffit. Vidé quand le moteur redémarre ou
    /// que le contexte est compacté.
    seen_full: HashSet<String>,
    /// Demandes de permission de commande en attente : identifiant de réponse → identifiant d'appel.
    pending_shell_ids: HashMap<u64, String>,
    free_hint_level: u8,
    last_run: Option<String>,

    /// Corrections du mode assistance : celles du dernier tour qui en a fait.
    pub assist_edits: Vec<AssistEdit>,
    next_edit: u64,
    /// Tour de vibelearn auquel appartiennent les corrections affichées.
    assist_turn: u32,
    /// Vrai quand l'extension VS Code affiche la session (elle applique alors
    /// les corrections dans l'éditeur ; sinon on écrit le fichier).
    pub ui_live: bool,
    /// Mini-leçon demandée sur une remarque : son texte, en attendant que
    /// vibelearn ouvre l'étape (`start_exercise`).
    pub lesson_origin: Option<String>,

    resume_attempt: bool,
    away_minutes: i64,
    turn_ok: bool,
    pub quit: bool,
    last_second: Instant,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        opts: Options,
        session: SessionState,
        profile: Profile,
        facts: ProjectFacts,
        tracker: Tracker,
        tx: mpsc::UnboundedSender<AppEvent>,
        resuming: bool,
        away_minutes: i64,
    ) -> Self {
        let resume_attempt = resuming && session.claude_session_id.is_some();
        let threads = Threads::new(&opts.root);
        Self {
            opts,
            session,
            profile,
            facts,
            engine: None,
            generation: 0,
            tx,
            phase: Phase::Starting,
            streaming: None,
            activity: None,
            flash: None,
            modal: None,
            modal_queue: VecDeque::new(),
            show_help: false,
            model: None,
            tracker,
            threads,
            last_spot: None,
            celebrate: None,
            turn_start: 0,
            pending: Vec::new(),
            last_change: None,
            stats_stale: false,
            snoozed: false,
            attachments: Vec::new(),
            approved: HashSet::new(),
            tutor_paths: HashSet::new(),
            shell_running: HashMap::new(),
            seen_full: HashSet::new(),
            pending_shell_ids: HashMap::new(),
            free_hint_level: 0,
            last_run: None,
            assist_edits: Vec::new(),
            next_edit: 1,
            assist_turn: 0,
            ui_live: false,
            lesson_origin: None,
            resume_attempt,
            away_minutes,
            turn_ok: false,
            quit: false,
            last_second: Instant::now(),
        }
    }

    fn root(&self) -> &Path {
        &self.opts.root
    }

    /// « Morceau 2/4 » d'après le plan.
    fn piece_label(&self) -> Option<String> {
        let plan = self.session.plan.as_ref()?;
        let total = plan.steps.len();
        let current = plan
            .steps
            .iter()
            .position(|s| s.status == StepStatus::Active)
            .map(|i| i + 1)
            .unwrap_or_else(|| (plan.progress().0 + 1).min(total));
        Some(format!("{} {current}/{total}", tr("Morceau", "Piece")))
    }

    /// Marque l'endroit où le morceau se code (ligne surlignée dans l'éditeur) ;
    /// le panneau affiche l'étape elle-même.
    fn post_brief(&mut self, ex: &Exercise) -> Result<Posted, String> {
        let target = ex.target_files.first().ok_or("no target file")?;
        let (file, line) = annotate::split_line(target);
        let near = line.map(|l| l.saturating_sub(1));
        let place = match (&ex.anchor, near) {
            (Some(anchor), near) => Place::At { anchor: anchor.clone(), near },
            (None, Some(index)) => Place::Line(index),
            (None, None) => Place::Top,
        };
        let title = match self.piece_label() {
            Some(label) => format!("{label}{}{}", colon(), ex.title),
            None => ex.title.clone(),
        };
        self.threads.post(Path::new(file), &place, Kind::Brief, None, Some(&title), "")
    }

    /// Une session reprise retrouve la consigne du morceau en cours.
    pub fn restore_brief(&mut self) {
        if self.threads.brief().is_none()
            && let Some(ex) = self.session.exercise.clone()
        {
            let _ = self.post_brief(&ex);
        }
    }

    /// Question posée dans le panneau (ou par une ligne `//?`) : `display` pour l'historique,
    /// `message` avec l'endroit du code pour vibelearn.
    pub fn ask_from_code(&mut self, display: String, message: String) {
        if !self.can_talk() {
            return;
        }
        self.push(Entry::User(display));
        let message = self.with_context(message);
        self.send(&message);
    }

    fn push(&mut self, entry: Entry) -> usize {
        self.session.transcript.push(entry);
        self.session.transcript.len() - 1
    }

    pub fn notice(&mut self, text: impl Into<String>) {
        self.push(Entry::Notice(text.into()));
    }

    fn flash(&mut self, text: impl Into<String>) {
        self.flash = Some((text.into(), Instant::now()));
    }

    fn save(&mut self) {
        let _ = self.session.save(&self.opts.root);
    }

    // -----------------------------------------------------------------------
    // Moteur
    // -----------------------------------------------------------------------

    pub fn start_engine(&mut self) {
        self.generation += 1;
        // Un nouveau processus (ou une reprise) ne garantit pas que le modèle a la version actuelle des fichiers.
        self.seen_full.clear();
        let generation = self.generation;
        let tx = self.tx.clone();
        let on_event = move |ev| {
            let _ = tx.send(AppEvent::Agent(generation, ev));
        };
        let engine = if self.opts.mock {
            Ok(Engine::mock(on_event))
        } else {
            let resume = if self.resume_attempt { self.session.claude_session_id.clone() } else { None };
            let system_prompt = prompt::system_prompt(&self.opts.root, self.session.mode);
            match self.opts.backend.clone() {
                None => {
                    self.push(Entry::Error(tr("binaire claude introuvable", "claude binary not found").into()));
                    self.phase = Phase::Ended;
                    return;
                }
                Some(Backend::Claude { bin, keep_api_key }) => Engine::spawn(
                    EngineConfig {
                        claude_bin: bin,
                        cwd: self.opts.root.clone(),
                        system_prompt,
                        title: format!("vibelearn{}{}", colon(), self.session.topic),
                        model: self.opts.model.clone(),
                        effort: self.opts.effort.clone(),
                        resume,
                        keep_api_key,
                    },
                    on_event,
                ),
                Some(Backend::Api { dialect, url, key }) => Ok(Engine::api(
                    crate::api::ApiConfig {
                        dialect,
                        url,
                        key,
                        model: self.opts.model.clone().unwrap_or_default(),
                        effort: self.opts.effort.clone(),
                        cwd: self.opts.root.clone(),
                        system_prompt,
                        resume,
                    },
                    on_event,
                )),
            }
        };
        match engine {
            Ok(engine) => {
                self.engine = Some(engine);
                self.phase = Phase::Starting;
                self.turn_ok = false;
            }
            Err(e) => {
                self.push(Entry::Error(format!("{e:#}")));
                self.phase = Phase::Ended;
            }
        }
    }

    fn send(&mut self, message: &str) {
        if let Some(engine) = &self.engine {
            engine.send_user(message);
            self.phase = Phase::Busy;
            self.threads.begin_turn();
            self.turn_start = self.session.transcript.len();
        }
    }

    /// Premier message de la session : reprise ou démarrage.
    fn send_first(&mut self) {
        let message = if self.resume_attempt {
            prompt::resumed(&self.session, self.away_minutes)
        } else {
            let previous = (self.session.turns > 0).then(|| self.session.progress_summary());
            let kickoff = prompt::kickoff(&self.session, &self.facts, &self.profile, previous.as_deref());
            // Surveillance : les remarques qu'il a écartées dans ce projet, sessions précédentes comprises.
            match self.session.mode.watches_every_save() {
                true => prompt::with_dismissed(kickoff, &crate::session::dismissed(self.root())),
                false => kickoff,
            }
        };
        self.send(&message);
    }

    /// La conversation d'origine n'a pas pu être restaurée : on repart d'une
    /// conversation neuve en redonnant l'avancement à vibelearn.
    fn restart_without_resume(&mut self) {
        self.resume_attempt = false;
        self.session.claude_session_id = None;
        if let Some(engine) = self.engine.take() {
            engine.shutdown();
        }
        self.notice(tr(
            "La conversation précédente n'a pas pu être restaurée : je reprends avec ton avancement.",
            "The previous conversation could not be restored: resuming from your progress.",
        ));
        self.start_engine();
    }

    pub fn interrupt(&mut self) {
        if let Some(engine) = &self.engine {
            engine.interrupt();
        }
        self.flash(tr("interruption demandée…", "interrupting…"));
    }

    /// Arrêt propre : sauvegarde la session et note la séance dans le profil.
    /// Renvoie vrai si un moteur était en marche (il faut attendre sa fin).
    pub fn shutdown(&mut self) -> bool {
        self.quit = true;
        let had_engine = match self.engine.take() {
            Some(engine) => {
                engine.shutdown();
                true
            }
            None => false,
        };
        self.streaming = None;
        // Le code redevient celui de l'apprenant ; une reprise reposera la consigne.
        self.threads.clear();
        self.save();
        if self.session.turns > 0 {
            let project = self.root().file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            self.profile.sessions.push(SessionRecord {
                topic: self.session.topic.clone(),
                project,
                date: Utc::now().date_naive(),
                minutes: (self.session.active_secs / 60) as u32,
                exercises_passed: self.session.exercises_passed,
                hints_used: self.session.hints_used,
                completed: self.session.completed,
            });
            let _ = self.profile.save();
        }
        had_engine
    }

    // -----------------------------------------------------------------------
    // Événements
    // -----------------------------------------------------------------------

    pub fn on_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Agent(generation, ev) => {
                if generation == self.generation {
                    self.on_agent(ev);
                }
            }
            AppEvent::Fs(paths) => self.on_fs(paths),
            AppEvent::CheckDone { cmd, trigger, output } => {
                if self.phase == Phase::Checking {
                    self.activity = None;
                    self.send_changes(trigger, Some((cmd, output)));
                }
            }
            AppEvent::RunDone { cmd, share, output } => self.on_run_done(cmd, share, output),
        }
    }

    fn on_agent(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::Ready => self.send_first(),
            AgentEvent::Session { session_id, model } => {
                if !session_id.is_empty() {
                    self.session.claude_session_id = Some(session_id);
                }
                self.model = model;
            }
            AgentEvent::BlockStart { kind, name } => match kind.as_str() {
                "text" => {
                    let idx = self.push(Entry::Tutor(String::new()));
                    self.streaming = Some(idx);
                    self.activity = None;
                }
                "thinking" => self.activity = Some(tr("vibelearn réfléchit", "vibelearn is thinking").into()),
                // vibelearn rédige un appel d'outil : on dit lequel, pour que
                // l'attente ait un sens (une checklist prend plusieurs secondes).
                "tool_use" => self.activity = name.as_deref().map(preparing).map(str::to_string),
                _ => {}
            },
            AgentEvent::Delta { text } => {
                let idx = match self.streaming {
                    Some(idx) => idx,
                    None => {
                        let idx = self.push(Entry::Tutor(String::new()));
                        self.streaming = Some(idx);
                        idx
                    }
                };
                if let Some(Entry::Tutor(buf)) = self.session.transcript.get_mut(idx) {
                    buf.push_str(&text);
                }
            }
            AgentEvent::Text { text } => match self.streaming.take() {
                Some(idx) => {
                    if let Some(Entry::Tutor(buf)) = self.session.transcript.get_mut(idx) {
                        *buf = text;
                    }
                }
                None => {
                    self.push(Entry::Tutor(text));
                }
            },
            AgentEvent::ToolUse { name, input, .. } => {
                // Un fichier lu en entier : le modèle en connaît la version actuelle.
                if name == "Read"
                    && input["offset"].is_null()
                    && input["limit"].is_null()
                    && let Some(file) = input["file_path"].as_str()
                {
                    let rel = paths::display_rel(self.root(), &paths::normalize(self.root(), Path::new(file)));
                    self.seen_full.insert(rel);
                }
                if !name.starts_with("mcp__") {
                    let line = describe_tool(self.root(), &name, &input);
                    self.activity = Some(line.clone());
                    self.push(Entry::Activity(line));
                }
            }
            AgentEvent::ToolDone { id, .. } => {
                if let Some(before) = self.shell_running.remove(&id) {
                    self.tracker.absorb_new_since(&before);
                    self.stats_stale = true;
                }
            }
            AgentEvent::ToolCall { id, tool, args } => self.on_tool_call(id, &tool, args),
            AgentEvent::Permission { id, tool, input, reason, tool_use_id } => {
                self.on_permission(id, tool, input, reason, tool_use_id)
            }
            AgentEvent::Cancel { id } => {
                self.pending_shell_ids.remove(&id);
                for e in self.assist_edits.iter_mut().filter(|e| e.reply == Some(id)) {
                    e.status = EditStatus::Failed;
                    e.reply = None;
                }
                self.modal_queue.retain(|m| m.reply_id() != id);
                if self.modal.as_ref().map(Modal::reply_id) == Some(id) {
                    self.modal = self.modal_queue.pop_front();
                }
            }
            AgentEvent::Result { is_error, cost_usd, input_tokens, output_tokens, errors, .. } => {
                if is_error && self.resume_attempt && !self.turn_ok {
                    self.restart_without_resume();
                    return;
                }
                self.end_turn();
                self.session.turns += 1;
                self.session.cost_usd += cost_usd;
                self.session.input_tokens += input_tokens;
                self.session.output_tokens += output_tokens;
                if is_error {
                    let detail = if errors.is_empty() {
                        tr("le tour s'est terminé sur une erreur", "the turn ended with an error").to_string()
                    } else {
                        errors.join(tr(" ; ", "; "))
                    };
                    self.push(Entry::Error(detail));
                } else {
                    self.turn_ok = true;
                }
                self.save();
            }
            AgentEvent::Status { text } => self.activity = Some(text),
            AgentEvent::Compacted => {
                self.seen_full.clear();
                self.activity = Some(tr("contexte compacté", "context compacted").into());
            }
            AgentEvent::AuthRequired => {
                self.end_turn();
                self.phase = Phase::Ended;
                let uses_key = matches!(self.opts.backend, Some(Backend::Api { .. }));
                let message = if uses_key {
                    tr(
                        "Le service a refusé ta clé API. Lance dans ton terminal :\n\n    vibelearn login\n\nEnsuite relance ta commande : ta session est conservée.",
                        "The service refused your API key. Run in your terminal:\n\n    vibelearn login\n\nThen run your command again: your session is kept.",
                    )
                } else {
                    tr(
                        "Tu n'es pas connecté à ton abonnement Claude. Lance dans ton terminal :\n\n    vibelearn login\n\nEnsuite relance ta commande : ta session est conservée.",
                        "You are not signed in to your Claude subscription. Run in your terminal:\n\n    vibelearn login\n\nThen run your command again: your session is kept.",
                    )
                };
                self.push(Entry::Error(message.into()));
            }
            AgentEvent::Fatal { message } => {
                self.end_turn();
                self.phase = Phase::Ended;
                self.push(Entry::Error(message));
            }
            AgentEvent::Exited { code, stderr } => {
                if self.quit || self.phase == Phase::Ended {
                    return;
                }
                if self.resume_attempt && !self.turn_ok {
                    self.restart_without_resume();
                    return;
                }
                self.end_turn();
                self.phase = Phase::Ended;
                let mut msg = format!(
                    "{} (code {})",
                    tr("Le moteur s'est arrêté", "The engine stopped"),
                    code.map(|c| c.to_string()).unwrap_or_else(|| "?".into())
                );
                if !stderr.trim().is_empty() {
                    msg.push_str(&format!("\n\n```\n{}\n```", stderr.trim()));
                }
                self.push(Entry::Error(msg));
                self.save();
            }
        }
    }

    /// Fin d'un tour de vibelearn, quelle qu'en soit la raison.
    fn end_turn(&mut self) {
        self.streaming = None;
        self.activity = None;
        if self.phase != Phase::Ended {
            self.phase = Phase::Idle;
        }
        // Les appels d'outils encore ouverts sont morts avec le tour.
        self.modal_queue.clear();
        self.modal = None;
        // Ce que vibelearn a écrit avec accord ne compte pas comme travail de l'apprenant.
        for path in std::mem::take(&mut self.tutor_paths) {
            self.tracker.absorb(&path);
        }
        self.shell_running.clear();
        self.pending_shell_ids.clear();
        // Une correction pas encore appliquée ne le sera plus : son appel est mort.
        for e in self.assist_edits.iter_mut().filter(|e| e.status == EditStatus::Pending) {
            e.status = EditStatus::Failed;
            e.reply = None;
        }
        self.stats_stale = true;
    }

    // -----------------------------------------------------------------------
    // Outils de vibelearn
    // -----------------------------------------------------------------------

    fn reply(&self, id: u64, text: &str) {
        if let Some(engine) = &self.engine {
            engine.tool_reply(id, text, false);
        }
    }

    fn reply_error(&self, id: u64, text: &str) {
        if let Some(engine) = &self.engine {
            engine.tool_reply(id, text, true);
        }
    }

    /// Vrai si une checklist ou une question attend déjà une réponse.
    fn waiting_on_learner(&self) -> bool {
        let interactive = |m: &Modal| matches!(m, Modal::Checklist(_) | Modal::Ask(_));
        self.modal.as_ref().is_some_and(interactive) || self.modal_queue.iter().any(interactive)
    }

    fn open_modal(&mut self, modal: Modal) {
        if self.modal.is_none() {
            self.modal = Some(modal);
        } else {
            self.modal_queue.push_back(modal);
        }
    }

    fn on_tool_call(&mut self, id: u64, tool: &str, args: Value) {
        self.activity = None;
        match tool {
            // Une seule interaction à la fois : la question suivante doit tenir
            // compte de la réponse précédente.
            "knowledge_checklist" | "ask" if self.waiting_on_learner() => self.reply_error(
                id,
                "Another widget is still waiting for the learner. Use the interactive tools one at a time: \
                 wait for the answer, read it, then decide what to ask next.",
            ),
            "knowledge_checklist" => match Checklist::from_args(id, &args) {
                Ok(c) => self.open_modal(Modal::Checklist(c)),
                Err(e) => self.reply_error(id, &e),
            },
            "ask" => match Ask::from_args(id, &args) {
                Ok(a) => self.open_modal(Modal::Ask(a)),
                Err(e) => self.reply_error(id, &e),
            },
            "set_plan" => {
                let steps: Vec<Step> = args["steps"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|s| {
                        Some(Step {
                            id: s["id"].as_str()?.to_string(),
                            title: s["title"].as_str()?.to_string(),
                            kind: s["kind"].as_str().unwrap_or("concept").to_string(),
                            status: StepStatus::Pending,
                        })
                    })
                    .collect();
                if steps.is_empty() {
                    return self.reply_error(id, "`steps` needs at least one step with `id` and `title`");
                }
                // Le plan est l'implémentation elle-même, dans le projet de
                // l'apprenant : pas de phase de leçons avant, pas de défi après.
                let all_implementation = steps.iter().all(|s| s.kind == "implementation");
                if self.session.mode.is_guided() && !all_implementation {
                    return self.reply_error(
                        id,
                        "Every step of the plan must be a piece of the real implementation in the developer's project \
                         (kind \"implementation\"). No lesson, demo, quiz or challenge steps: explain each notion inside \
                         the piece that needs it. Call set_plan again.",
                    );
                }
                // On conserve l'état des étapes déjà connues quand le plan est révisé.
                let old = self.session.plan.take();
                let steps = steps
                    .into_iter()
                    .map(|mut s| {
                        if let Some(prev) = old.as_ref().and_then(|p| p.steps.iter().find(|o| o.id == s.id)) {
                            s.status = prev.status;
                        }
                        s
                    })
                    .collect();
                self.session.plan = Some(Plan { goal: args["goal"].as_str().unwrap_or("").to_string(), steps });
                self.reply(id, "Plan recorded. The developer sees the current piece number in its instructions.");
            }
            "update_step" => {
                let step_id = args["id"].as_str().unwrap_or("");
                let status = match args["status"].as_str().unwrap_or("") {
                    "active" => StepStatus::Active,
                    "done" => StepStatus::Done,
                    "skipped" => StepStatus::Skipped,
                    other => return self.reply_error(id, &format!("unknown status `{other}`")),
                };
                let Some(plan) = self.session.plan.as_mut() else {
                    return self.reply_error(id, "no plan yet: call set_plan first");
                };
                if !plan.steps.iter().any(|s| s.id == step_id) {
                    let ids = plan.steps.iter().map(|s| s.id.as_str()).collect::<Vec<_>>().join(", ");
                    return self.reply_error(id, &format!("unknown step `{step_id}`; steps are: {ids}"));
                }
                for s in plan.steps.iter_mut() {
                    if s.id == step_id {
                        s.status = status;
                    } else if status == StepStatus::Active && s.status == StepStatus::Active {
                        // Une seule étape active à la fois.
                        s.status = StepStatus::Pending;
                    }
                }
                self.reply(id, "Step updated.");
            }
            "show_demo" => {
                let demo = Demo {
                    title: args["title"].as_str().unwrap_or("Demo").to_string(),
                    files: strings(&args["files"]),
                    run_command: args["run_command"].as_str().filter(|c| !c.trim().is_empty()).map(str::to_string),
                    observe: args["observe"].as_str().map(str::to_string),
                };
                let mut note = format!("**{}** : {}", tr("Démo", "Demo"), demo.title);
                for f in &demo.files {
                    note.push_str(&format!("\n- `{f}`"));
                }
                if let Some(cmd) = &demo.run_command {
                    note.push_str(&format!(
                        "\n\n{} `{cmd}`",
                        tr("Pour la lancer, écris `/run` dans le panneau. Commande :", "To run it, type `/run` in the panel. Command:")
                    ));
                    self.last_run = Some(cmd.clone());
                }
                if let Some(o) = &demo.observe {
                    note.push_str(&format!("\n\n{} {o}", tr("À observer :", "Look for:")));
                }
                self.notice(note);
                self.session.demo = Some(demo);
                self.reply(id, "Demo files opened in the developer's editor. They can run it by typing `/run` in the panel; you will receive the output as <command_output>. Say in a short message what to look at.");
            }
            "start_exercise" => {
                let exercise = Exercise {
                    id: args["id"].as_str().unwrap_or("exercise").to_string(),
                    title: args["title"].as_str().unwrap_or("").to_string(),
                    goal: args["goal"].as_str().unwrap_or("").to_string(),
                    success_criteria: strings(&args["success_criteria"]),
                    target_files: strings(&args["target_files"]),
                    anchor: args["anchor"].as_str().filter(|a| !a.trim().is_empty()).map(str::to_string),
                    notes: args["notes"].as_str().filter(|n| !n.trim().is_empty()).map(str::to_string),
                    here: args["here"].as_str().filter(|h| !h.trim().is_empty()).map(str::to_string),
                    check_command: args["check_command"].as_str().filter(|c| !c.trim().is_empty()).map(str::to_string),
                    hint_level: 0,
                    attempts: 0,
                    verdict: None,
                    summary: None,
                    // Mini-leçon : la remarque d'origine reste affichée jusqu'au bout de la leçon.
                    origin: if self.session.mode.watches_every_save() {
                        self.lesson_origin.take().or_else(|| self.session.exercise.as_ref().and_then(|e| e.origin.clone()))
                    } else {
                        None
                    },
                };
                if exercise.goal.is_empty() || exercise.success_criteria.is_empty() {
                    return self.reply_error(id, "`goal` and `success_criteria` are required");
                }
                if exercise.target_files.is_empty() {
                    return self.reply_error(id, "`target_files` is required: the file where this piece is written");
                }
                // Nouvelle étape : les annotations et questions de la précédente disparaissent.
                self.threads.clear_step();
                let posted = match self.post_brief(&exercise) {
                    Ok(posted) => posted,
                    Err(e) => return self.reply_error(id, &format!("could not write the instructions in their code: {e}")),
                };
                let mut note = format!("**{}** : {}\n\n{}", tr("À toi de coder", "Your turn to code"), exercise.title, exercise.goal);
                for c in &exercise.success_criteria {
                    note.push_str(&format!("\n- {c}"));
                }
                self.notice(note);
                // Le travail de l'exercice se mesure à partir de maintenant.
                self.tracker.commit();
                self.pending.clear();
                self.snoozed = false;
                self.session.exercise = Some(exercise);
                let rel = paths::display_rel(self.root(), &posted.file);
                self.reply(
                    id,
                    &format!(
                        "Step shown in the vibelearn panel (title, goal, notes, criteria), and line {} of {rel} is highlighted in \
                         their editor as the place to write. File watching started. End your turn now, with no text: you will \
                         receive <file_changes> when they save.",
                        posted.line
                    ),
                );
            }
            "exercise_feedback" => {
                let verdict = args["verdict"].as_str().unwrap_or("").to_string();
                let summary = args["summary"].as_str().unwrap_or("").to_string();
                let Some(ex) = self.session.exercise.as_mut() else {
                    return self.reply_error(id, "no exercise in progress: call start_exercise first");
                };
                ex.attempts += 1;
                ex.verdict = Some(verdict.clone());
                ex.summary = Some(summary.clone());
                if verdict == "passed" {
                    let title = ex.title.clone();
                    self.session.exercises_passed += 1;
                    self.session.exercise = None;
                    self.threads.clear_step();
                    self.celebrate = Some(Celebrate { title: title.clone(), summary: summary.clone(), seq: self.session.exercises_passed });
                    self.notice(format!("**{}**{}{title}. {summary}", tr("Étape validée", "Piece done"), colon()));
                }
                self.reply(id, "Recorded.");
            }
            "comment" => {
                let file = args["file"].as_str().unwrap_or("").trim().to_string();
                let text = args["text"].as_str().unwrap_or("").to_string();
                if file.is_empty() || text.trim().is_empty() {
                    return self.reply_error(id, "`file` and `text` are required");
                }
                // Numéro de ligne donné à partir de 1 ; en interne, indice à partir de 0.
                let near = args["line"].as_u64().map(|l| (l as usize).saturating_sub(1));
                let anchor = args["anchor"].as_str().filter(|a| !a.trim().is_empty()).map(str::to_string);
                if anchor.is_none() && near.is_none() {
                    return self.reply_error(id, "give `anchor` (one line of their code, copied exactly) or `line`");
                }
                // Sans texte d'ancre, la ligne seule.
                let anchor = anchor.unwrap_or_default();
                let place = Place::At { anchor, near };
                let tone = crate::threads::tone_or_default(args["tone"].as_str());
                // Le titre va en bout de ligne ; l'explication, au survol et dans le panneau.
                let detail = args["detail"].as_str().map(str::trim).filter(|d| !d.is_empty());
                if detail.is_none() && matches!(tone, "fix" | "design") {
                    return self.reply_error(
                        id,
                        "`detail` is required for fix and design remarks: what happens, in which order, with which consequence, and what to do about it, in plain words",
                    );
                }
                let body = match detail {
                    Some(detail) => format!("{}\n\n{detail}", text.trim()),
                    None => text.trim().to_string(),
                };
                match self.threads.post(Path::new(&file), &place, Kind::Feedback, Some(tone), None, &body) {
                    Ok(posted) => {
                        let rel = paths::display_rel(self.root(), &posted.file);
                        self.push(Entry::Comment(format!("[{rel}:{}] {}", posted.line, text.trim())));
                        self.reply(id, &format!("Annotation shown at the end of {rel} line {}.", posted.line));
                    }
                    Err(e) => self.reply_error(id, &e),
                }
            }
            "assist_edit" => self.on_assist_edit(id, &args),
            "record_mastery" => {
                let mut count = 0;
                for item in args["items"].as_array().into_iter().flatten() {
                    let (Some(cid), Some(label)) = (item["concept_id"].as_str(), item["label"].as_str()) else { continue };
                    let level = item["level"].as_u64().unwrap_or(0).min(4) as u8;
                    self.profile.record(cid, label, level, Source::Demonstrated, item["evidence"].as_str(), &self.session.topic);
                    let entry = self.session.concepts.entry(cid.to_string()).or_insert(SessionConcept {
                        label: label.to_string(),
                        claim: Claim::Unknown,
                        level: None,
                    });
                    entry.label = label.to_string();
                    entry.level = Some(level);
                    count += 1;
                }
                let _ = self.profile.save();
                self.reply(id, &format!("{count} concept(s) recorded in the learner profile."));
            }
            "finish_session" => {
                let summary = args["summary"].as_str().unwrap_or("").to_string();
                let notes = args["notes_markdown"].as_str().unwrap_or("").to_string();
                let next = strings(&args["next_steps"]);
                let file = self.write_notes(&summary, &notes, &next);
                self.session.completed = true;
                self.session.exercise = None;
                self.threads.clear_step();
                let mut note = format!("**{}**\n\n{summary}", tr("Session terminée", "Session complete"));
                if !next.is_empty() {
                    note.push_str(&format!("\n\n{}", tr("Pour la suite :", "Next:")));
                    for n in &next {
                        note.push_str(&format!("\n- {n}"));
                    }
                }
                if let Some(f) = &file {
                    note.push_str(&format!("\n\n{} `{f}`", tr("Fiche de révision :", "Revision sheet:")));
                }
                self.notice(note);
                self.session.notes_file = file;
                self.save();
                self.reply(id, "Session closed and revision sheet saved. Say goodbye in one or two sentences.");
            }
            other => self.reply_error(id, &format!("unknown tool `{other}`")),
        }
    }

    // -----------------------------------------------------------------------
    // Mode assistance
    // -----------------------------------------------------------------------

    /// vibelearn veut finir lui-même un changement laissé incomplet. La
    /// correction est vérifiée ici, puis appliquée par l'extension VS Code (ou
    /// écrite dans le fichier sans elle) ; l'appel attend le résultat.
    fn on_assist_edit(&mut self, id: u64, args: &Value) {
        if !self.session.mode.finishes_refactorings() {
            return self.reply_error(id, "assist_edit does not exist in this session: annotate the line instead.");
        }
        let file = args["file"].as_str().unwrap_or("").trim();
        let old = args["old"].as_str().unwrap_or("");
        let new = args["new"].as_str().unwrap_or("");
        let reason = args["reason"].as_str().unwrap_or("").trim();
        let Some(line) = args["line"].as_u64().filter(|l| *l >= 1) else {
            return self.reply_error(id, "`line` is required: the line number (from 1) where `old` is");
        };
        if file.is_empty() || reason.is_empty() {
            return self.reply_error(id, "`file` and `reason` are required");
        }
        if let Err(e) = assist::check_fragments(old, new) {
            return self.reply_error(id, &e);
        }
        let root = self.opts.root.clone();
        let abs = paths::normalize(&root, Path::new(file));
        if !abs.starts_with(&root) || paths::in_sandbox(&root, &abs) || !abs.is_file() {
            return self.reply_error(id, &format!("`{file}` is not a file of their project"));
        }
        let text = match std::fs::read(&abs) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => return self.reply_error(id, &format!("cannot read `{file}`: {e}")),
        };
        let line = match assist::locate(&text, line as usize - 1, old, assist::SLACK) {
            Ok(line) => line,
            Err(e) => return self.reply_error(id, &e),
        };
        // Pendant une étape (leçon ou mini-leçon), l'assistance continue, sauf là où il écrit le morceau.
        let span = line..=line + assist::unix(old).matches('\n').count();
        if self.session.exercise.is_some()
            && let Some(brief) = self.threads.brief()
            && brief.file == abs
            && span.contains(&brief.line)
        {
            return self.reply_error(
                id,
                "This is where the developer writes the piece of the current step: they write it themselves. Edits elsewhere that do not do the step for them are fine.",
            );
        }
        // Les corrections d'un nouveau tour remplacent celles du précédent.
        if self.assist_turn != self.session.turns {
            self.assist_turn = self.session.turns;
            self.assist_edits.retain(|e| e.status == EditStatus::Pending);
        }
        let edit_id = self.next_edit;
        self.next_edit += 1;
        self.assist_edits.push(AssistEdit {
            id: edit_id,
            file: abs,
            line,
            old: assist::unix(old),
            new: assist::unix(new),
            reason: reason.to_string(),
            status: EditStatus::Pending,
            reply: Some(id),
            asked: Instant::now(),
        });
        self.activity = Some(tr("vibelearn corrige une ligne", "vibelearn is fixing a line").into());
        if !self.ui_live {
            self.apply_on_disk(edit_id);
        }
    }

    /// Sans extension pour l'appliquer dans l'éditeur : on écrit le fichier.
    fn apply_on_disk(&mut self, edit_id: u64) {
        let Some(e) = self.assist_edits.iter().find(|e| e.id == edit_id) else { return };
        let written = std::fs::read(&e.file)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|text| assist::apply(&text, e.line, &e.old, &e.new))
            .map(|fixed| std::fs::write(&e.file, fixed));
        match written {
            Some(Ok(())) => self.edit_result(edit_id, true, None, None),
            Some(Err(err)) => self.edit_result(edit_id, false, None, Some(err.to_string())),
            None => self.edit_result(edit_id, false, None, Some("the line changed in the meantime".into())),
        }
    }

    /// L'éditeur a appliqué (ou refusé) une correction.
    pub fn edit_result(&mut self, edit_id: u64, ok: bool, line: Option<usize>, error: Option<String>) {
        let root = self.opts.root.clone();
        let Some(e) = self.assist_edits.iter_mut().find(|e| e.id == edit_id && e.status == EditStatus::Pending) else {
            return;
        };
        let reply = e.reply.take();
        let rel = paths::display_rel(&root, &e.file);
        if ok {
            e.status = EditStatus::Applied;
            if let Some(line) = line {
                e.line = line;
            }
            let (file, line, old, new) = (e.file.clone(), e.line, e.old.clone(), e.new.clone());
            self.tracker.absorb_edit(&file, line, &old, &new);
            self.stats_stale = true;
            self.push(Entry::Activity(format!("{} {rel}:{} `{old}` → `{new}`", tr("corrigé :", "fixed:"), line + 1)));
            if let Some(id) = reply {
                self.reply(
                    id,
                    &format!(
                        "Edit applied on {rel} line {}. It is shown to the developer on that line with your reason, with an undo button: do not annotate this line.",
                        line + 1
                    ),
                );
            }
        } else {
            e.status = EditStatus::Failed;
            let why = error.unwrap_or_else(|| "the editor did not apply it".into());
            if let Some(id) = reply {
                self.reply_error(id, &format!("Not applied: {why}. If it still matters, annotate the line instead."));
            }
        }
    }

    /// Le développeur a annulé une correction (bouton du panneau ou lien du survol).
    pub fn edit_undone(&mut self, edit_id: u64, saved: bool) {
        let root = self.opts.root.clone();
        let Some(e) = self.assist_edits.iter_mut().find(|e| e.id == edit_id && e.status == EditStatus::Applied) else {
            return;
        };
        e.status = EditStatus::Undone;
        let (file, line, old, new) = (e.file.clone(), e.line, e.old.clone(), e.new.clone());
        // Non sauvegardée, l'annulation partira avec sa prochaine sauvegarde, comme son travail.
        if saved {
            self.tracker.absorb_edit(&file, line, &new, &old);
            self.stats_stale = true;
        }
        let rel = paths::display_rel(&root, &file);
        self.push(Entry::Activity(format!("{} {rel}:{} `{new}` → `{old}`", tr("annulé :", "undone:"), line + 1)));
        self.attachments.push(prompt::assist_undone(&rel, line + 1, &old, &new));
    }

    /// « Oublie » sur une remarque : elle disparaît, ce projet s'en souvient, et
    /// vibelearn ne refait plus ce genre de remarque.
    pub fn forget_remark(&mut self, thread: u64) {
        // Une prochaine étape (`//n …`) s'efface, sans rien en retenir.
        if self.threads.thread(thread).is_some_and(|t| t.kind == Kind::Next) {
            self.threads.remove(thread);
            return;
        }
        // Un pseudo-code (`//// …`) aussi ; la demande restée dans le code n'est plus traitée.
        if self.threads.thread(thread).is_some_and(|t| t.kind == Kind::Pseudo) {
            self.threads.dismiss_pseudo(thread);
            return;
        }
        let Some(t) = self.threads.thread(thread).filter(|t| t.kind == Kind::Feedback) else { return };
        let remark = t.vibelearn_notes().join(" ");
        let (rel, line) = (paths::display_rel(self.root(), &t.file), t.line + 1);
        self.threads.remove(thread);
        crate::session::dismiss(self.root(), &remark);
        self.push(Entry::Activity(format!("{} {remark}", tr("remarque écartée :", "remark dismissed:"))));
        self.attachments.push(prompt::remark_dismissed(&rel, line, &remark));
    }

    /// Une correction que l'éditeur n'a pas appliquée à temps est abandonnée.
    fn expire_edits(&mut self) {
        let late: Vec<u64> = self
            .assist_edits
            .iter()
            .filter(|e| e.status == EditStatus::Pending && e.asked.elapsed() > Duration::from_secs(10))
            .map(|e| e.id)
            .collect();
        for id in late {
            self.edit_result(id, false, None, Some("VS Code did not apply it in time".into()));
        }
    }

    fn write_notes(&self, summary: &str, notes: &str, next: &[String]) -> Option<String> {
        let dir = paths::notes_dir(self.root());
        std::fs::create_dir_all(&dir).ok()?;
        let name = format!("{}-{}.md", Utc::now().format("%Y-%m-%d"), paths::slug(&self.session.topic));
        let mut content = format!("# {}\n\n> {}\n\n{}\n", self.session.topic, summary.replace('\n', "\n> "), notes.trim());
        if !next.is_empty() {
            content.push_str(&format!("\n## {}\n\n", tr("Pour la suite", "Next steps")));
            for n in next {
                content.push_str(&format!("- {n}\n"));
            }
        }
        let path = dir.join(name);
        std::fs::write(&path, content).ok()?;
        Some(paths::display_rel(self.root(), &path))
    }

    // -----------------------------------------------------------------------
    // Permissions
    // -----------------------------------------------------------------------

    fn allow(&mut self, id: u64, tool_use_id: Option<String>, is_shell: bool) {
        if let (true, Some(tid)) = (is_shell, tool_use_id) {
            self.shell_running.insert(tid, self.tracker.dirty_paths());
        }
        if let Some(engine) = &self.engine {
            engine.permission_reply(id, true, None);
        }
    }

    fn on_permission(&mut self, id: u64, tool: String, input: Value, reason: Option<String>, tool_use_id: Option<String>) {
        let root = self.opts.root.clone();
        let is_write = matches!(tool.as_str(), "Write" | "Edit" | "MultiEdit" | "NotebookEdit");
        let is_shell = matches!(tool.as_str(), "Bash" | "PowerShell");
        let file = input["file_path"].as_str().or_else(|| input["notebook_path"].as_str());

        if let (true, Some(file)) = (is_write, file) {
            let abs = paths::normalize(&root, Path::new(file));
            // Le bac à sable de vibelearn : écriture libre.
            if paths::in_sandbox(&root, &abs) {
                return self.allow(id, None, false);
            }
            let rel = paths::display_rel(&root, &abs);
            let key = format!("write:{rel}");
            if self.approved.contains(&key) {
                self.tutor_paths.insert(abs);
                return self.allow(id, None, false);
            }
            let preview = input["content"].as_str().or_else(|| input["new_string"].as_str()).unwrap_or("");
            self.open_modal(Modal::Permission(PermissionPrompt {
                reply: id,
                tool,
                headline: format!("{} {rel}", tr("vibelearn veut écrire dans ton projet :", "vibelearn wants to write in your project:")),
                detail: preview.lines().take(16).collect::<Vec<_>>().join("\n"),
                always_key: key,
                write_path: Some(abs),
                decision: None,
            }));
            return;
        }

        let (key, headline, detail) = if is_shell {
            let command = input["command"].as_str().unwrap_or("").to_string();
            let first = command.split_whitespace().next().unwrap_or("").to_string();
            (
                format!("{tool}:{first}"),
                tr("vibelearn veut lancer une commande :", "vibelearn wants to run a command:").to_string(),
                command,
            )
        } else {
            let mut detail = serde_json::to_string_pretty(&input).unwrap_or_default();
            if detail.chars().count() > 600 {
                detail = detail.chars().take(600).collect::<String>() + "…";
            }
            if let Some(r) = reason.filter(|r| !r.is_empty()) {
                detail.push_str(&format!("\n\n{r}"));
            }
            (tool.clone(), format!("{} {tool}", tr("vibelearn veut utiliser l'outil", "vibelearn wants to use the tool")), detail)
        };
        if self.approved.contains(&key) {
            return self.allow(id, tool_use_id, is_shell);
        }
        // On garde l'identifiant de l'appel pour savoir quand la commande se termine.
        if let (true, Some(tid)) = (is_shell, tool_use_id) {
            self.pending_shell_ids.insert(id, tid);
        }
        self.open_modal(Modal::Permission(PermissionPrompt {
            reply: id,
            tool,
            headline,
            detail,
            always_key: key,
            write_path: None,
            decision: None,
        }));
    }

    /// L'apprenant a répondu à l'interaction ouverte (la fenêtre a rempli son
    /// état) : on enregistre la réponse et on la renvoie à vibelearn.
    pub fn resolve_modal(&mut self) {
        let Some(modal) = self.modal.take() else { return };
        match modal {
            Modal::Checklist(c) => {
                for item in &c.items {
                    let level = self.session.concepts.get(&item.id).and_then(|e| e.level);
                    self.session.concepts.insert(
                        item.id.clone(),
                        SessionConcept { label: item.label.clone(), claim: item.state, level },
                    );
                    if item.state == Claim::Known {
                        self.profile.record(
                            &item.id,
                            &item.label,
                            2,
                            Source::SelfReported,
                            item.evidence.as_deref(),
                            &self.session.topic,
                        );
                    }
                }
                let _ = self.profile.save();
                self.push(Entry::Widget(format!(
                    "{}{}{} {}, {} {}, {} {}",
                    c.title,
                    colon(),
                    c.count(Claim::Known),
                    tr("connus", "known"),
                    c.count(Claim::Unsure),
                    tr("pas sûr", "unsure"),
                    c.count(Claim::Unknown),
                    tr("à apprendre", "to learn"),
                )));
                self.reply(c.reply, &c.result().to_string());
                self.activity = Some(tr("vibelearn lit ta réponse", "vibelearn is reading your answer").into());
            }
            Modal::Ask(a) => {
                self.push(Entry::Widget(format!(
                    "{}\n\n{} {}",
                    a.question,
                    tr("Ta réponse :", "Your answer:"),
                    a.answer_text()
                )));
                self.reply(a.reply, &a.result().to_string());
                self.activity = Some(tr("vibelearn lit ta réponse", "vibelearn is reading your answer").into());
            }
            Modal::Permission(p) => {
                let shell_id = self.pending_shell_ids.remove(&p.reply);
                let is_shell = matches!(p.tool.as_str(), "Bash" | "PowerShell");
                match p.decision.unwrap_or(Decision::Deny) {
                    Decision::Deny => {
                        let message = if p.write_path.is_some() {
                            "The learner declined this write. You may only write inside .vibelearn/ without asking; \
                             for their real code, guide them to make the change themselves."
                        } else if is_shell {
                            "The learner declined to run this command. If it matters, ask them to run it themselves (in their terminal, or by typing `/run <command>` in the panel)."
                        } else {
                            "Denied by the learner."
                        };
                        if let Some(engine) = &self.engine {
                            engine.permission_reply(p.reply, false, Some(message));
                        }
                        self.push(Entry::Activity(format!("{} {}", tr("refusé :", "denied:"), p.tool)));
                    }
                    decision => {
                        if decision == Decision::Always {
                            self.approved.insert(p.always_key.clone());
                        }
                        if let Some(path) = p.write_path {
                            self.tutor_paths.insert(path);
                        }
                        self.allow(p.reply, shell_id, is_shell);
                    }
                }
            }
        }
        self.modal = self.modal_queue.pop_front();
    }

    // -----------------------------------------------------------------------
    // Fichiers
    // -----------------------------------------------------------------------

    fn on_fs(&mut self, paths: Vec<PathBuf>) {
        let (tutor, learner): (Vec<PathBuf>, Vec<PathBuf>) =
            paths.into_iter().partition(|p| self.tutor_paths.contains(p));
        for p in tutor {
            self.tracker.absorb(&p);
        }
        if self.tracker.touch(&learner) {
            self.last_change = Some(Instant::now());
            self.snoozed = false;
            self.stats_stale = true;
        }
    }

    /// Vrai quand les sauvegardes partent vers vibelearn : pendant un
    /// exercice, ou tout le temps en surveillance.
    fn watching(&self) -> bool {
        self.session.exercise.is_some() || self.session.mode.watches_every_save()
    }

    /// Commande lancée avant chaque envoi : celle de l'exercice, sinon (en
    /// surveillance) celle détectée pour le projet.
    fn check_command(&self) -> Option<String> {
        match &self.session.exercise {
            Some(ex) => ex.check_command.clone(),
            None if self.session.mode.watches_every_save() => self.facts.check_command.map(str::to_string),
            None => None,
        }
    }

    /// Vrai quand un envoi automatique est en préparation.
    fn auto_armed(&self) -> bool {
        self.session.auto_check
            && self.watching()
            && self.phase == Phase::Idle
            && self.modal.is_none()
            && !self.snoozed
            && !self.pending.is_empty()
            && self.shell_running.is_empty()
    }

    /// Secondes restantes avant l'envoi automatique des changements.
    pub fn countdown(&self) -> Option<f32> {
        if !self.auto_armed() {
            return None;
        }
        let elapsed = self.last_change?.elapsed();
        Some(self.opts.auto_delay.saturating_sub(elapsed).as_secs_f32())
    }

    pub fn auto_delay(&self) -> f32 {
        self.opts.auto_delay.as_secs_f32()
    }

    fn submit_changes(&mut self, trigger: &'static str) {
        if self.phase == Phase::Checking {
            return;
        }
        match self.check_command() {
            Some(cmd) => {
                self.phase = Phase::Checking;
                self.activity = Some(cmd.clone());
                let tx = self.tx.clone();
                let root = self.opts.root.clone();
                tokio::spawn(async move {
                    let output = runner::run(&cmd, &root, Duration::from_secs(180)).await;
                    let _ = tx.send(AppEvent::CheckDone { cmd, trigger, output });
                });
            }
            None => self.send_changes(trigger, None),
        }
    }

    fn send_changes(&mut self, trigger: &'static str, check: Option<(String, RunOutput)>) {
        // Recalculé au dernier moment : l'apprenant a pu continuer pendant la vérification.
        let changes = self.tracker.changes();
        let diffs = if changes.is_empty() {
            "(no file changes since the last report)".to_string()
        } else {
            watch::render(&changes, &mut self.seen_full)
        };
        self.last_spot = change_spot(self.root(), &changes);
        // Surveillance : les annotations des fichiers touchés décrivaient l'état
        // d'avant ; vibelearn redit ce qui tient encore.
        if self.session.mode.watches_every_save() {
            let touched: Vec<PathBuf> = changes.iter().map(|c| paths::normalize(self.root(), Path::new(&c.path))).collect();
            self.threads.drop_feedback_in(&touched);
        }
        let exercise = self.session.exercise.as_ref().map(|e| e.id.clone());
        let message = prompt::file_changes(
            trigger,
            exercise.as_deref(),
            &diffs,
            check.as_ref().map(|(cmd, out)| prompt::CheckOutput { cmd, exit: out.code, output: &out.output }),
        );

        let mut line = if changes.is_empty() {
            tr("état actuel envoyé à vibelearn", "current state sent to vibelearn").to_string()
        } else {
            watch::summary(&changes)
        };
        if let Some((cmd, out)) = &check {
            let mark = if out.ok() { "ok" } else { tr("échec", "failed") };
            line.push_str(&format!(", `{cmd}` : {mark}"));
        }
        self.push(Entry::Changes(line));
        if let Some((cmd, out)) = check.as_ref().filter(|(_, out)| !out.ok() && !out.output.trim().is_empty()) {
            self.push(Entry::Output(format!("$ {cmd}\n{}", last_lines(&out.output, 14))));
        }

        self.tracker.commit();
        self.pending.clear();
        let attachments = std::mem::take(&mut self.attachments);
        let full = join_parts(attachments, message);
        self.send(&full);
    }

    // -----------------------------------------------------------------------
    // Saisie de l'apprenant
    // -----------------------------------------------------------------------

    /// L'apprenant reporte l'envoi automatique de ses changements.
    #[cfg_attr(not(feature = "window"), allow(dead_code))]
    pub fn snooze(&mut self) {
        self.snoozed = true;
    }

    /// L'apprenant demande l'évaluation immédiate de son code.
    #[cfg_attr(not(feature = "window"), allow(dead_code))]
    pub fn send_now(&mut self) {
        if self.phase == Phase::Idle && self.watching() {
            self.submit_changes("manual");
        }
    }

    #[cfg_attr(not(feature = "window"), allow(dead_code))]
    pub fn set_auto(&mut self, on: bool) {
        self.session.auto_check = on;
        self.snoozed = false;
    }

    /// Vrai si l'apprenant peut écrire à vibelearn maintenant.
    #[cfg_attr(not(feature = "window"), allow(dead_code))]
    pub fn can_write(&self) -> bool {
        matches!(self.phase, Phase::Idle | Phase::Busy) && self.modal.is_none()
    }

    /// Message libre de l'apprenant (ou commande s'il commence par `/`).
    #[cfg_attr(not(feature = "window"), allow(dead_code))]
    pub fn submit(&mut self, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        if let Some(rest) = text.strip_prefix('/') {
            return self.command(rest);
        }
        if !self.can_talk() {
            return;
        }
        self.push(Entry::User(text.clone()));
        let message = self.with_context(text);
        self.send(&message);
    }

    fn can_talk(&mut self) -> bool {
        match self.phase {
            Phase::Ended => {
                self.flash(tr("la session est arrêtée : relance vibelearn", "the session has stopped: run vibelearn again"));
                false
            }
            Phase::Starting | Phase::Checking => {
                self.flash(tr("un instant…", "one moment…"));
                false
            }
            _ => true,
        }
    }

    /// Ajoute au message ce que vibelearn doit savoir : sorties de commandes
    /// et modifications de fichiers pas encore envoyées.
    fn with_context(&mut self, text: String) -> String {
        let mut parts = std::mem::take(&mut self.attachments);
        let changes = self.tracker.changes();
        if !changes.is_empty() {
            let diffs = watch::render(&changes, &mut self.seen_full);
            let exercise = self.session.exercise.as_ref().map(|ex| ex.id.as_str());
            parts.push(if self.watching() {
                prompt::file_changes("message", exercise, &diffs, None)
            } else {
                prompt::pending_changes(&diffs)
            });
            self.tracker.commit();
            self.pending.clear();
        }
        join_parts(parts, text)
    }

    fn learner_command(&mut self, name: &str, label: &str) {
        if !self.can_talk() {
            return;
        }
        self.push(Entry::User(label.to_string()));
        let message = self.with_context(prompt::learner_command(name));
        self.send(&message);
    }

    /// Commande de l'apprenant, sans la barre oblique (`hint`, `run cargo test`…).
    pub fn command(&mut self, line: &str) {
        let (name, arg) = match line.split_once(char::is_whitespace) {
            Some((n, a)) => (n, a.trim()),
            None => (line, ""),
        };
        match name {
            "help" | "aide" | "?" => self.show_help = true,
            "quit" | "q" | "exit" => self.quit = true,
            "stop" => {
                if self.phase == Phase::Busy {
                    self.interrupt();
                }
            }
            "hint" | "indice" => {
                if !self.can_talk() {
                    return;
                }
                let level = match self.session.exercise.as_mut() {
                    Some(ex) => {
                        ex.hint_level = (ex.hint_level + 1).min(4);
                        ex.hint_level
                    }
                    None => {
                        self.free_hint_level = (self.free_hint_level + 1).min(4);
                        self.free_hint_level
                    }
                };
                self.session.hints_used += 1;
                self.push(Entry::User(format!("{} ({} {level}/4)", tr("Un indice, s'il te plaît", "A hint, please"), tr("niveau", "level"))));
                let message = self.with_context(prompt::hint(level));
                self.send(&message);
            }
            "check" | "verifier" | "vérifier" | "verifie" | "vérifie" => {
                if !self.can_talk() {
                    return;
                }
                if self.watching() {
                    self.submit_changes("manual");
                } else {
                    self.learner_command("check", tr("Vérifie mon code.", "Check my code."));
                }
            }
            "auto" => {
                self.session.auto_check = match arg {
                    "on" => true,
                    "off" => false,
                    _ => !self.session.auto_check,
                };
                self.snoozed = false;
                self.notice(if self.session.auto_check {
                    tr(
                        "Envoi automatique **activé** : tes changements partent quelques secondes après ta dernière sauvegarde.",
                        "Auto-send **on**: your changes are sent a few seconds after your last save.",
                    )
                } else {
                    tr(
                        "Envoi automatique **désactivé** : clique sur « Vérifier » dans le panneau vibelearn quand tu veux un retour.",
                        "Auto-send **off**: click \"Check\" in the vibelearn panel when you want feedback.",
                    )
                });
            }
            // Mini-leçon : l'arrêter rend tout de suite la main à la surveillance.
            "skip" | "passer" | "passe" if self.session.mode.watches_every_save() && self.session.exercise.is_some() => {
                self.session.exercise = None;
                self.threads.clear_step();
                self.learner_command("skip", tr("J'arrête la leçon.", "I'm stopping the lesson."));
            }
            "skip" | "passer" | "passe" => self.learner_command("skip", tr("Je passe cette étape.", "I'll skip this step.")),
            "stuck" | "bloque" | "bloqué" => self.learner_command("stuck", tr("Je suis bloqué.", "I'm stuck.")),
            "explain" | "explique" | "reexplique" | "réexplique" => self.learner_command("explain", tr("Réexplique-moi autrement.", "Explain it another way.")),
            "deeper" | "creuse" | "détail" | "detail" => self.learner_command("deeper", tr("Va plus en détail.", "Go deeper.")),
            "known" | "connu" => self.learner_command("known", tr("Je connais déjà ça.", "I already know this.")),
            "quiz" => self.learner_command("quiz", tr("Interroge-moi sur ce qu'on a vu.", "Quiz me on what we covered.")),
            "recap" | "récap" => self.learner_command("recap", tr("Où en est-on ?", "Where are we?")),
            "finish" | "fin" | "terminer" => self.learner_command("finish", tr("Terminons la session.", "Let's wrap up.")),
            "run" | "sh" => {
                let share = name == "run";
                let cmd = if !arg.is_empty() {
                    Some(arg.to_string())
                } else {
                    self.last_run
                        .clone()
                        .or_else(|| self.check_command())
                };
                let Some(cmd) = cmd else {
                    return self.flash(tr("aucune commande à lancer", "no command to run"));
                };
                self.last_run = Some(cmd.clone());
                self.flash(format!("$ {cmd}"));
                let tx = self.tx.clone();
                let root = self.opts.root.clone();
                tokio::spawn(async move {
                    // Une démo peut être une fenêtre (un jeu) qu'on garde ouverte
                    // un moment : on laisse une demi-heure avant d'abandonner.
                    let output = runner::run(&cmd, &root, Duration::from_secs(1800)).await;
                    let _ = tx.send(AppEvent::RunDone { cmd, share, output });
                });
            }
            "diff" => {
                let changes = self.tracker.changes();
                if changes.is_empty() {
                    self.flash(tr("aucun changement en attente", "no pending changes"));
                } else {
                    self.push(Entry::Tutor(watch::render_diffs(&changes)));
                }
            }
            "plan" => match &self.session.plan {
                Some(plan) => {
                    let mut out = format!("**{}**{}{}", tr("Plan", "Plan"), colon(), plan.goal);
                    for s in &plan.steps {
                        let mark = match s.status {
                            StepStatus::Done => "[x]",
                            StepStatus::Active => "[>]",
                            StepStatus::Skipped => "[-]",
                            StepStatus::Pending => "[ ]",
                        };
                        out.push_str(&format!("\n- {mark} {}", s.title));
                    }
                    self.notice(out);
                }
                None => self.flash(tr("pas encore de plan", "no plan yet")),
            },
            "notes" => {
                let dir = paths::notes_dir(self.root());
                let mut files: Vec<String> = std::fs::read_dir(&dir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| paths::display_rel(self.root(), &e.path()))
                    .collect();
                files.sort();
                if files.is_empty() {
                    self.notice(tr(
                        "Pas encore de fiche : elle est écrite quand la session se termine.",
                        "No revision sheet yet: one is written when the session ends.",
                    ));
                } else {
                    let list = files.iter().map(|f| format!("- `{f}`")).collect::<Vec<_>>().join("\n");
                    self.notice(format!("**{}**\n{list}", tr("Fiches de révision", "Revision sheets")));
                }
            }
            other => self.flash(format!("{} /{other}", tr("commande inconnue :", "unknown command:"))),
        }
    }

    fn on_run_done(&mut self, cmd: String, share: bool, output: RunOutput) {
        let status = match output.code {
            Some(0) => "ok".to_string(),
            Some(c) => format!("exit {c}"),
            None => tr("échec", "failed").to_string(),
        };
        self.push(Entry::Output(format!("$ {cmd}\n{}\n{status} ({:.1}s)", output.output.trim_end(), output.secs)));
        if !share {
            return;
        }
        let block = prompt::command_output(&cmd, output.code, &output.output);
        if self.phase == Phase::Idle {
            let message = self.with_context(block);
            self.send(&message);
        } else {
            // vibelearn est occupé : la sortie partira avec le prochain message.
            self.attachments.push(block);
        }
    }

    // -----------------------------------------------------------------------
    // Horloge
    // -----------------------------------------------------------------------

    /// Avance l'horloge ; à appeler régulièrement (environ dix fois par seconde).
    pub fn on_tick(&mut self) {
        if self.last_second.elapsed() >= Duration::from_secs(1) {
            self.last_second = Instant::now();
            if self.phase != Phase::Ended {
                self.session.active_secs += 1;
            }
        }
        if self.flash.as_ref().is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(4)) {
            self.flash = None;
        }
        // Les statistiques des fichiers attendent une courte accalmie : un
        // éditeur écrit souvent en plusieurs fois.
        if self.stats_stale && self.last_change.is_none_or(|t| t.elapsed() > Duration::from_millis(250)) {
            self.pending = self.tracker.changes();
            self.stats_stale = false;
            // Une annotation dont la ligne a changé parlait d'un code qui n'existe plus.
            self.threads.drop_stale();
        }
        self.expire_edits();
        if self.countdown().is_some_and(|left| left <= 0.0) {
            self.submit_changes("auto");
        }
    }

    /// Vrai si l'affichage doit se rafraîchir en continu (animation, compte à
    /// rebours, message temporaire, statistiques de fichiers en attente).
    #[cfg_attr(not(feature = "window"), allow(dead_code))]
    pub fn animating(&self) -> bool {
        self.working() || self.countdown().is_some() || self.flash.is_some() || self.stats_stale
    }

    /// Vrai quand vibelearn (ou une commande) travaille et que rien n'attend
    /// l'apprenant.
    #[cfg_attr(not(feature = "window"), allow(dead_code))]
    pub fn working(&self) -> bool {
        self.modal.is_none() && matches!(self.phase, Phase::Starting | Phase::Busy | Phase::Checking)
    }
}

/// Ce que vibelearn est en train de préparer, d'après l'outil qu'il rédige.
fn preparing(tool: &str) -> &'static str {
    match tool.strip_prefix("mcp__tutor__").unwrap_or(tool) {
        "knowledge_checklist" => tr("vibelearn prépare la liste des notions à cocher", "vibelearn is preparing the checklist"),
        "ask" => tr("vibelearn prépare une question", "vibelearn is preparing a question"),
        "set_plan" => tr("vibelearn prépare le plan", "vibelearn is preparing the plan"),
        "start_exercise" => tr("vibelearn prépare la prochaine étape", "vibelearn is preparing the next piece"),
        "show_demo" => tr("vibelearn prépare la démo", "vibelearn is preparing the demo"),
        "assist_edit" => tr("vibelearn prépare une correction", "vibelearn is preparing a fix"),
        "finish_session" => tr("vibelearn rédige ta fiche de révision", "vibelearn is writing your revision sheet"),
        "exercise_feedback" | "record_mastery" | "update_step" => tr("vibelearn met à jour ton suivi", "vibelearn is updating your progress"),
        "Write" | "Edit" | "MultiEdit" => tr("vibelearn écrit un fichier de démo", "vibelearn is writing a demo file"),
        "Read" | "Glob" | "Grep" => tr("vibelearn explore ton projet", "vibelearn is exploring your project"),
        "Bash" | "PowerShell" => tr("vibelearn prépare une commande", "vibelearn is preparing a command"),
        _ => tr("vibelearn travaille", "vibelearn is working"),
    }
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn join_parts(mut parts: Vec<String>, last: String) -> String {
    parts.push(last);
    parts.join("\n\n")
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Mode d'emploi, montré quand l'apprenant écrit `aide` dans le panneau.
pub fn help_text() -> &'static str {
    tr(
        "**Comment me parler**\n\n\
         - **Code et sauvegarde** : je regarde à chaque sauvegarde.\n\
         - **Une question** : tape-la dans le panneau ; elle porte sur la ligne où est ton curseur (clic droit > poser une question sur cette ligne).\n\
         - **Par où commencer ?** Écris `//n wall jump` (le nom de la fonctionnalité) là où tu veux coder et sauvegarde : la prochaine étape s'affiche au bout de cette ligne.\n\
         - **Les boutons du panneau** : « Vérifier » en haut ; « Indice », « Je bloque » et « Explique autrement » au-dessus du champ ; le menu « ⋯ » pour le reste.\n\
         - Dans le panneau, `/run <commande>` lance une commande et me montre la sortie.",
        "**How to talk to me**\n\n\
         - **Code and save**: I look each time you save.\n\
         - **A question**: type it in the panel; it is about the line under your cursor (right click > ask about this line).\n\
         - **Where to start?** Write `//n wall jump` (the feature's name) where you want to code and save: the next step shows at the end of that line.\n\
         - **The panel buttons**: \"Check\" at the top; \"Hint\", \"I'm stuck\" and \"Explain differently\" above the field; the \"⋯\" menu for the rest.\n\
         - In the panel, `/run <command>` runs a command and shows me the output.",
    )
}

/// La dernière ligne ajoutée dans le premier bloc modifié : c'est là que
/// l'apprenant travaillait, donc là qu'un retour sans endroit précis ira.
fn change_spot(root: &Path, changes: &[FileChange]) -> Option<(PathBuf, String, usize)> {
    for change in changes.iter().filter(|c| c.kind != ChangeKind::Deleted) {
        let mut line = 0usize;
        let mut in_hunk = false;
        let mut last: Option<(String, usize)> = None;
        for l in change.diff.lines() {
            if let Some(header) = l.strip_prefix("@@") {
                if last.is_some() {
                    break;
                }
                in_hunk = true;
                line = header
                    .split_whitespace()
                    .find_map(|p| p.strip_prefix('+'))
                    .and_then(|p| p.split(',').next())
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(1);
                continue;
            }
            if !in_hunk {
                continue;
            }
            match l.chars().next() {
                Some('+') => {
                    if !l[1..].trim().is_empty() {
                        last = Some((l[1..].to_string(), line));
                    }
                    line += 1;
                }
                Some('-') => {}
                _ => line += 1,
            }
        }
        if let Some((text, line)) = last {
            return Some((root.join(&change.path), text, line));
        }
    }
    None
}

/// Décrit en une ligne ce que fait vibelearn (affiché en gris dans le fil).
fn describe_tool(root: &Path, name: &str, input: &Value) -> String {
    let path = |key: &str| {
        input[key]
            .as_str()
            .map(|p| paths::display_rel(root, &paths::normalize(root, Path::new(p))))
            .unwrap_or_default()
    };
    let short = |key: &str| {
        let s = input[key].as_str().unwrap_or("");
        let first = s.lines().next().unwrap_or("");
        if first.chars().count() > 70 { first.chars().take(70).collect::<String>() + "…" } else { first.to_string() }
    };
    match name {
        "Read" => format!("{} {}", tr("lit", "reads"), path("file_path")),
        "Glob" => format!("{} {}", tr("cherche les fichiers", "finds files"), short("pattern")),
        "Grep" => format!("{} « {} »", tr("cherche", "searches"), short("pattern")),
        "Write" => format!("{} {}", tr("écrit", "writes"), path("file_path")),
        "Edit" | "MultiEdit" => format!("{} {}", tr("modifie", "edits"), path("file_path")),
        "Bash" | "PowerShell" => format!("{} `{}`", tr("lance", "runs"), short("command")),
        "WebSearch" => format!("{} {}", tr("recherche sur le web :", "web search:"), short("query")),
        "WebFetch" => format!("{} {}", tr("consulte", "fetches"), short("url")),
        other => other.to_string(),
    }
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;
