//! La fenêtre de vibelearn (egui, rendu wgpu) : conversation à gauche, suivi à
//! droite (parcours, exercice, fichiers surveillés), zone de message en bas.
//! Les interactions de vibelearn (checklist, question, permission) s'ouvrent en
//! boîte de dialogue, avec de vraies cases et de vrais boutons.

use anyhow::{Result, anyhow};
use eframe::egui::{
    self, Align, Color32, CornerRadius, Frame, Key, KeyboardShortcut, Layout, Margin, Modifiers, RichText, ScrollArea,
    Sense, Stroke, TextEdit, Ui, vec2,
};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::app::{App, AppEvent, Outcome, Phase};
use crate::claude::AgentEvent;
use crate::i18n::{colon, tr};
use crate::modal::{Ask, AskKind, Checklist, Decision, Modal, PermissionPrompt};
use crate::profile;
use crate::session::{Claim, Entry, StepStatus};
use crate::watch::ChangeKind;

const GREEN: Color32 = Color32::from_rgb(70, 170, 100);
const AMBER: Color32 = Color32::from_rgb(215, 150, 40);
const RED: Color32 = Color32::from_rgb(215, 80, 80);
/// Largeur maximale du texte : au-delà, les lignes deviennent pénibles à lire.
const READING_WIDTH: f32 = 860.0;
/// Seules les dernières entrées sont dessinées (la session complète reste sauvegardée).
const MAX_SHOWN_ENTRIES: usize = 300;

/// Ouvre la fenêtre et ne rend la main qu'à sa fermeture. `autoplay` (tuteur
/// factice seulement) répond tout seul aux interactions : une démonstration.
pub fn run(app: App, mut events: mpsc::UnboundedReceiver<AppEvent>, autoplay: bool) -> Result<Outcome> {
    let outcome: Arc<Mutex<Option<Outcome>>> = Arc::new(Mutex::new(None));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("vibelearn{}{}", colon(), app.session.topic))
            .with_inner_size([1180.0, 820.0])
            .with_min_inner_size([720.0, 520.0])
            .with_maximized(false),
        centered: true,
        persist_window: false,
        ..Default::default()
    };
    let slot = outcome.clone();
    eframe::run_native(
        "vibelearn",
        options,
        Box::new(move |cc| {
            setup_style(&cc.egui_ctx);
            // Les événements du moteur et des fichiers réveillent la fenêtre,
            // même réduite : la session continue pendant que tu codes ailleurs.
            let ctx = cc.egui_ctx.clone();
            let (tx, rx) = mpsc::unbounded_channel();
            tokio::spawn(async move {
                while let Some(event) = events.recv().await {
                    if tx.send(event).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            });
            Ok(Box::new(Gui {
                app,
                events: rx,
                outcome: slot,
                input: String::new(),
                run_line: String::new(),
                markdown: CommonMarkCache::default(),
                last_tick: Instant::now(),
                focus_answer: true,
                autoplay,
                modal_since: None,
            }))
        }),
    )
    .map_err(|e| anyhow!("{} : {e}", tr("la fenêtre n'a pas pu s'ouvrir", "the window could not open")))?;

    let result = outcome.lock().unwrap().take();
    result.ok_or_else(|| anyhow!("{}", tr("la fenêtre s'est fermée de façon inattendue", "the window closed unexpectedly")))
}

/// Ajoute, en dernier recours, une police de symboles du système : les polices
/// d'egui n'ont pas tous les pictogrammes (dont l'icône « copier » des blocs
/// de code, qui s'affichait comme un carré vide).
fn add_symbol_font(ctx: &egui::Context) {
    use egui::FontFamily;
    use egui::epaint::text::{FontData, FontInsert, FontPriority, InsertFontFamily};
    let candidates = [
        "C:\\Windows\\Fonts\\seguisym.ttf",
        "/System/Library/Fonts/Apple Symbols.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ];
    let Some(bytes) = candidates.iter().find_map(|path| std::fs::read(path).ok()) else { return };
    let families = [FontFamily::Proportional, FontFamily::Monospace]
        .into_iter()
        .map(|family| InsertFontFamily { family, priority: FontPriority::Lowest })
        .collect();
    ctx.add_font(FontInsert::new("system-symbols", FontData::from_owned(bytes), families));
}

/// Typographie généreuse : c'est un outil où l'on lit beaucoup.
fn setup_style(ctx: &egui::Context) {
    use egui::{FontFamily, FontId, TextStyle};
    add_symbol_font(ctx);
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::new(23.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(16.5, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(15.5, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(14.5, FontFamily::Monospace)),
            (TextStyle::Small, FontId::new(13.0, FontFamily::Proportional)),
        ]
        .into();
        style.spacing.item_spacing = vec2(8.0, 8.0);
        style.spacing.button_padding = vec2(12.0, 7.0);
        style.spacing.interact_size.y = 30.0;
    });
}

struct Gui {
    app: App,
    events: mpsc::UnboundedReceiver<AppEvent>,
    outcome: Arc<Mutex<Option<Outcome>>>,
    /// Message en cours de rédaction.
    input: String,
    /// Commande saisie dans « Lancer une commande ».
    run_line: String,
    markdown: CommonMarkCache,
    last_tick: Instant,
    /// Donner le focus au champ de réponse à l'ouverture d'une question.
    focus_answer: bool,
    /// Démonstration : répondre automatiquement aux interactions.
    autoplay: bool,
    modal_since: Option<Instant>,
}

impl Gui {
    /// Démonstration avec vibelearn factice : après quelques secondes, répond
    /// à l'interaction ouverte pour que le scénario se déroule tout seul.
    fn autoplay_step(&mut self) {
        let Some(modal) = &mut self.app.modal else {
            self.modal_since = None;
            return;
        };
        if self.modal_since.get_or_insert_with(Instant::now).elapsed() < Duration::from_millis(3500) {
            return;
        }
        match modal {
            Modal::Checklist(_) => {}
            Modal::Ask(a) => {
                a.text = "miaou".into();
                a.choice = Some(0);
                if let Some(first) = a.selected.first_mut() {
                    *first = true;
                }
            }
            Modal::Permission(p) => p.decision = Some(Decision::Deny),
        }
        self.app.resolve_modal();
        self.modal_since = None;
    }
}

impl eframe::App for Gui {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(event) = self.events.try_recv() {
            self.app.on_event(event);
        }
        if self.autoplay {
            self.autoplay_step();
        }
        if self.last_tick.elapsed() >= Duration::from_millis(100) {
            self.last_tick = Instant::now();
            self.app.on_tick();
        }
        // L'horloge continue même fenêtre réduite (envoi automatique, statistiques).
        let wait = if self.app.animating() { 100 } else { 500 };
        ctx.request_repaint_after(Duration::from_millis(wait));
        if self.app.quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::Panel::top("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::bottom("composer").show(ui, |ui| self.composer(ui));
        egui::Panel::right("follow-up")
            .resizable(true)
            .default_size(340.0)
            .min_size(260.0)
            .show(ui, |ui| self.sidebar(ui));
        egui::CentralPanel::default().show(ui, |ui| self.conversation(ui));
        self.interaction(&ctx);
        self.help(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let had_engine = self.app.shutdown();
        if had_engine {
            // Laisse au moteur le temps de fermer la conversation proprement.
            let deadline = Instant::now() + Duration::from_millis(2500);
            while Instant::now() < deadline {
                match self.events.try_recv() {
                    Ok(AppEvent::Agent(_, AgentEvent::Exited { .. })) | Err(mpsc::error::TryRecvError::Disconnected) => break,
                    Ok(_) => {}
                    Err(mpsc::error::TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        }
        *self.outcome.lock().unwrap() = Some(Outcome {
            completed: self.app.session.completed,
            notes_file: self.app.session.notes_file.clone(),
            turns: self.app.session.turns,
            mode: self.app.session.mode,
        });
    }
}

// ---------------------------------------------------------------------------
// Petits éléments communs
// ---------------------------------------------------------------------------

fn weak(text: impl Into<String>) -> RichText {
    RichText::new(text).weak()
}

fn small(text: impl Into<String>) -> RichText {
    RichText::new(text).small().weak()
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(title.to_uppercase()).small().strong().color(ui.visuals().hyperlink_color));
}

/// Carte : cadre arrondi avec une marge confortable.
fn card(ui: &Ui, stroke: Color32) -> Frame {
    Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(12))
}

fn code_block(ui: &mut Ui, text: &str, max_height: f32) {
    Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ScrollArea::both().id_salt(("code", text.len())).max_height(max_height).auto_shrink([false, true]).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(text.trim_end()).monospace()).extend());
            });
        });
}

/// Pastille d'état d'une étape du parcours.
fn step_dot(ui: &mut Ui, status: StepStatus) {
    let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
    let center = rect.center();
    let accent = ui.visuals().hyperlink_color;
    let muted = ui.visuals().weak_text_color();
    match status {
        StepStatus::Done => {
            ui.painter().circle_filled(center, 6.0, GREEN);
        }
        StepStatus::Active => {
            ui.painter().circle_stroke(center, 6.5, Stroke::new(2.0, accent));
            ui.painter().circle_filled(center, 3.0, accent);
        }
        StepStatus::Skipped => {
            ui.painter().line_segment([center - vec2(5.0, 0.0), center + vec2(5.0, 0.0)], Stroke::new(2.0, muted));
        }
        StepStatus::Pending => {
            ui.painter().circle_stroke(center, 5.5, Stroke::new(1.5, muted));
        }
    }
}

fn human(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1} M", n as f64 / 1e6)
    } else if n >= 1000 {
        format!("{:.1} k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

// ---------------------------------------------------------------------------
// Barre d'état
// ---------------------------------------------------------------------------

impl Gui {
    fn status_bar(&mut self, ui: &mut Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("vibelearn").strong());
            ui.label(weak(self.app.session.topic.clone()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let s = &self.app.session;
                let mut right = format!("{} min, {} {}", s.active_secs / 60, human(s.input_tokens + s.output_tokens), tr("jetons", "tokens"));
                if let Some(model) = &self.app.model {
                    right = format!("{model}, {right}");
                }
                ui.label(small(right));
            });
        });
        ui.horizontal(|ui| {
            let app = &self.app;
            if let Some((text, _)) = &app.flash {
                ui.label(RichText::new(text).color(AMBER));
                return;
            }
            if app.modal.is_some() {
                ui.label(RichText::new(tr("À toi : vibelearn attend ta réponse.", "Your turn: vibelearn is waiting for your answer.")).color(GREEN));
                return;
            }
            match app.phase {
                Phase::Starting => {
                    ui.spinner();
                    ui.label(tr("Démarrage de Claude…", "Starting Claude…"));
                }
                Phase::Busy => {
                    ui.spinner();
                    let what = app.activity.clone().unwrap_or_else(|| tr("vibelearn réfléchit", "vibelearn is thinking").to_string());
                    ui.label(format!("{what}…"));
                }
                Phase::Checking => {
                    ui.spinner();
                    ui.label(format!("{} {}", tr("Vérification :", "Checking:"), app.activity.clone().unwrap_or_default()));
                }
                Phase::Ended => {
                    ui.label(RichText::new(tr("Session arrêtée. Tu peux fermer la fenêtre.", "Session stopped. You can close the window.")).color(RED));
                }
                Phase::Idle => {
                    let text = match (app.countdown(), app.session.exercise.is_some()) {
                        (Some(left), _) => format!(
                            "{} {:.0} s",
                            tr("Tes changements partent chez vibelearn dans", "Your changes go to vibelearn in"),
                            left.ceil()
                        ),
                        (None, true) => tr("À toi de coder : je surveille tes fichiers.", "Your turn to code: I'm watching your files.").to_string(),
                        (None, false) => tr("À toi.", "Your turn.").to_string(),
                    };
                    ui.label(RichText::new(text).color(GREEN));
                }
            }
        });
        if self.app.phase == Phase::Busy && self.app.modal.is_none() {
            // Le bouton est sur sa propre ligne pour ne pas bouger avec le texte.
            if ui.small_button(tr("Interrompre vibelearn", "Interrupt vibelearn")).clicked() {
                self.app.interrupt();
            }
        }
        ui.add_space(4.0);
    }

    // -----------------------------------------------------------------------
    // Conversation
    // -----------------------------------------------------------------------

    fn conversation(&mut self, ui: &mut Ui) {
        ScrollArea::vertical().id_salt("conversation").auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            let full = ui.available_width();
            let width = full.min(READING_WIDTH);
            ui.horizontal(|ui| {
                ui.add_space(((full - width) / 2.0).max(0.0));
                ui.vertical(|ui| {
                    ui.set_width(width);
                    ui.add_space(8.0);
                    let total = self.app.session.transcript.len();
                    let first = total.saturating_sub(MAX_SHOWN_ENTRIES);
                    if first > 0 {
                        ui.label(small(format!("… {first} {}", tr("messages plus anciens masqués", "older messages hidden"))));
                    }
                    for index in first..total {
                        let streaming = self.app.streaming == Some(index);
                        ui.push_id(index, |ui| {
                            entry(ui, &mut self.markdown, &self.app.session.transcript[index], streaming);
                        });
                    }
                    if self.app.working() && self.app.streaming.is_none() {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            let what = match (&self.app.activity, self.app.phase) {
                                (Some(a), _) => a.clone(),
                                (None, Phase::Starting) => tr("démarrage de Claude", "starting Claude").to_string(),
                                _ => tr("vibelearn réfléchit", "vibelearn is thinking").to_string(),
                            };
                            ui.label(weak(format!("{what}…")));
                        });
                    }
                    ui.add_space(12.0);
                });
            });
        });
    }

    // -----------------------------------------------------------------------
    // Zone de message et actions
    // -----------------------------------------------------------------------

    fn composer(&mut self, ui: &mut Ui) {
        ui.add_space(8.0);
        let enabled = self.app.can_write();
        let idle = self.app.phase == Phase::Idle && self.app.modal.is_none();

        // Les actions fréquentes sont des boutons : pas de commandes à retenir.
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button(tr("Un indice", "A hint")).on_hover_text(tr("Quatre niveaux, jamais la solution.", "Four levels, never the solution.")).clicked() {
                    self.app.command("hint");
                }
                if ui.button(tr("Vérifier mon code", "Check my code")).on_hover_text(tr("vibelearn regarde l'état actuel de tes fichiers.", "vibelearn looks at the current state of your files.")).clicked() {
                    self.app.command("check");
                }
                if ui.button(tr("Je suis bloqué", "I'm stuck")).clicked() {
                    self.app.command("stuck");
                }
                if ui.button(tr("Réexplique", "Explain again")).clicked() {
                    self.app.command("explain");
                }
                ui.menu_button(tr("Plus…", "More…"), |ui| {
                    let items: [(&str, &str); 8] = [
                        ("deeper", tr("Va plus en détail", "Go deeper")),
                        ("known", tr("Je connais déjà", "I already know this")),
                        ("skip", tr("Passer cette étape", "Skip this step")),
                        ("quiz", tr("Interroge-moi", "Quiz me")),
                        ("recap", tr("Où en est-on ?", "Where are we?")),
                        ("diff", tr("Voir mes changements en attente", "Show my pending changes")),
                        ("notes", tr("Mes fiches de révision", "My revision sheets")),
                        ("finish", tr("Terminer la session", "Finish the session")),
                    ];
                    for (command, label) in items {
                        if ui.button(label).clicked() {
                            self.app.command(command);
                            ui.close();
                        }
                    }
                    ui.separator();
                    ui.label(small(tr("Lancer une commande (vibelearn voit la sortie) :", "Run a command (vibelearn sees the output):")));
                    let line = ui.add(TextEdit::singleline(&mut self.run_line).hint_text("cargo test").desired_width(260.0));
                    let go = ui.button(tr("Lancer", "Run")).clicked() || (line.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)));
                    if go && !self.run_line.trim().is_empty() {
                        self.app.command(&format!("run {}", self.run_line.trim()));
                        self.run_line.clear();
                        ui.close();
                    }
                });
                if ui.button(tr("Aide", "Help")).clicked() {
                    self.app.show_help = true;
                }
            });
        });

        let hint = match self.app.phase {
            Phase::Ended => tr("La session est arrêtée.", "The session has stopped."),
            _ if self.app.modal.is_some() => tr("Réponds d'abord à la question ouverte.", "Answer the open question first."),
            Phase::Idle if self.app.session.exercise.is_some() => {
                tr("Code dans ton éditeur, ou écris ici pour poser une question…", "Code in your editor, or write here to ask a question…")
            }
            Phase::Starting | Phase::Checking => tr("Un instant…", "One moment…"),
            _ => tr("Écris ici… (Entrée pour envoyer, Maj+Entrée pour une nouvelle ligne)", "Write to vibelearn… (Enter to send, Shift+Enter for a new line)"),
        };
        let mut send = false;
        ui.horizontal(|ui| {
            let button_width = 96.0;
            let field = ui.add_enabled(
                enabled,
                TextEdit::multiline(&mut self.input)
                    .hint_text(hint)
                    .desired_rows(2)
                    .desired_width(ui.available_width() - button_width - 12.0)
                    .return_key(KeyboardShortcut::new(Modifiers::SHIFT, Key::Enter)),
            );
            if field.has_focus() && ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift) {
                send = true;
            }
            let label = RichText::new(tr("Envoyer", "Send")).strong();
            if ui.add_enabled(enabled && !self.input.trim().is_empty(), egui::Button::new(label).min_size(vec2(button_width, 44.0))).clicked() {
                send = true;
            }
            // Dès que c'est à l'apprenant d'écrire, le champ est prêt à recevoir la frappe.
            if idle && !field.has_focus() && ui.ctx().memory(|m| m.focused().is_none()) {
                field.request_focus();
            }
        });
        if send && enabled && !self.input.trim().is_empty() {
            let text = std::mem::take(&mut self.input);
            self.app.submit(text);
        }
        ui.add_space(8.0);
    }

    // -----------------------------------------------------------------------
    // Panneau de suivi
    // -----------------------------------------------------------------------

    fn sidebar(&mut self, ui: &mut Ui) {
        ScrollArea::vertical().id_salt("follow-up-scroll").auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(4.0);
            self.plan_section(ui);
            self.exercise_section(ui);
            self.files_section(ui);
            self.session_section(ui);
            ui.add_space(12.0);
        });
    }

    fn plan_section(&mut self, ui: &mut Ui) {
        section(ui, tr("Plan", "Plan"));
        let Some(plan) = &self.app.session.plan else {
            ui.label(weak(tr("Le plan arrive…", "The plan is coming…")));
            return;
        };
        let (done, total) = plan.progress();
        ui.label(weak(&plan.goal));
        ui.add(egui::ProgressBar::new(done as f32 / total.max(1) as f32).desired_height(8.0).fill(GREEN));
        // Deux temps : ce qu'il faut voir avant, puis ce que tu voulais construire.
        let mut caption = "";
        for step in &plan.steps {
            let wanted = if step.kind == "implementation" {
                tr("À coder dans ton projet", "To code in your project")
            } else {
                tr("Notions", "Notions")
            };
            if wanted != caption {
                caption = wanted;
                ui.add_space(2.0);
                ui.label(small(caption));
            }
            ui.horizontal(|ui| {
                step_dot(ui, step.status);
                let text = match step.status {
                    StepStatus::Active => RichText::new(&step.title).strong(),
                    StepStatus::Done | StepStatus::Skipped => RichText::new(&step.title).weak(),
                    StepStatus::Pending => RichText::new(&step.title),
                };
                ui.add(egui::Label::new(text).wrap());
            });
        }
    }

    fn exercise_section(&mut self, ui: &mut Ui) {
        if let Some(ex) = &self.app.session.exercise {
            section(ui, tr("À coder maintenant", "To code now"));
            card(ui, ui.visuals().hyperlink_color).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(&ex.title).strong());
                for criterion in &ex.success_criteria {
                    ui.horizontal(|ui| {
                        ui.label(weak("•"));
                        ui.add(egui::Label::new(criterion).wrap());
                    });
                }
                if let Some(cmd) = &ex.check_command {
                    ui.label(small(format!("{} {cmd}", tr("Vérifié automatiquement par :", "Checked automatically by:"))));
                }
                let mut meta = format!("{} {} / 4", tr("Indices utilisés :", "Hints used:"), ex.hint_level);
                if ex.attempts > 0 {
                    meta.push_str(&format!(", {} {}", tr("essais :", "tries:"), ex.attempts));
                }
                ui.label(small(meta));
                if let Some(summary) = &ex.summary {
                    let color = match ex.verdict.as_deref() {
                        Some("progress") => GREEN,
                        Some("off_track") => AMBER,
                        _ => ui.visuals().weak_text_color(),
                    };
                    ui.add(egui::Label::new(RichText::new(summary).color(color)).wrap());
                }
            });
        } else if let Some(demo) = &self.app.session.demo {
            let run = demo.run_command.clone();
            section(ui, tr("Démo", "Demo"));
            card(ui, ui.visuals().widgets.noninteractive.bg_stroke.color).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(&demo.title).strong());
                for file in &demo.files {
                    ui.add(egui::Label::new(RichText::new(file).monospace()).wrap());
                }
                if let Some(observe) = &demo.observe {
                    ui.label(weak(observe));
                }
                if let Some(cmd) = &run {
                    ui.label(small(cmd));
                }
            });
            if run.is_some() && ui.add_enabled(self.app.can_write(), egui::Button::new(tr("Lancer la démo", "Run the demo"))).clicked() {
                self.app.command("run");
            }
        }
    }

    fn files_section(&mut self, ui: &mut Ui) {
        section(ui, tr("Tes fichiers", "Your files"));
        let mut auto = self.app.session.auto_check;
        if ui
            .checkbox(&mut auto, tr("Envoyer mes changements automatiquement", "Send my changes automatically"))
            .on_hover_text(tr(
                "Quelques secondes après ta dernière sauvegarde, pendant un exercice.",
                "A few seconds after your last save, during an exercise.",
            ))
            .changed()
        {
            self.app.set_auto(auto);
        }
        if self.app.pending.is_empty() {
            ui.label(weak(tr("Aucun changement en attente.", "No pending changes.")));
            return;
        }
        for change in self.app.pending.iter().take(8) {
            let stat = match change.kind {
                ChangeKind::Added => format!("+{}", change.added),
                ChangeKind::Deleted => tr("supprimé", "deleted").to_string(),
                ChangeKind::Modified => format!("+{} −{}", change.added, change.removed),
            };
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new(&change.path).monospace()).truncate());
                ui.label(small(stat));
            });
        }
        if self.app.pending.len() > 8 {
            ui.label(small(format!("… +{}", self.app.pending.len() - 8)));
        }
        if let Some(left) = self.app.countdown() {
            let ratio = 1.0 - (left / self.app.auto_delay().max(0.1)).clamp(0.0, 1.0);
            ui.add(egui::ProgressBar::new(ratio).desired_height(8.0));
            ui.label(small(format!("{} {:.0} s", tr("Envoi à vibelearn dans", "Sending to vibelearn in"), left.ceil())));
            ui.horizontal(|ui| {
                if ui.button(tr("Envoyer maintenant", "Send now")).clicked() {
                    self.app.send_now();
                }
                if ui.button(tr("Pas encore", "Not yet")).clicked() {
                    self.app.snooze();
                }
            });
        } else if self.app.session.exercise.is_some() && self.app.phase == Phase::Idle && ui.button(tr("Envoyer à vibelearn", "Send to vibelearn")).clicked() {
            self.app.send_now();
        }
    }

    fn session_section(&mut self, ui: &mut Ui) {
        section(ui, "Session");
        let s = &self.app.session;
        ui.label(small(format!(
            "{} min, {} {}, {} {}",
            s.active_secs / 60,
            s.exercises_passed,
            tr("étapes validées", "pieces done"),
            s.hints_used,
            tr("indices", "hints"),
        )));
        let mut learned: Vec<_> = s.concepts.values().filter(|c| c.level.is_some()).collect();
        learned.sort_by_key(|c| std::cmp::Reverse(c.level));
        for concept in learned.iter().take(8) {
            let level = concept.level.unwrap_or(0);
            ui.add(egui::Label::new(&concept.label).truncate());
            ui.horizontal(|ui| {
                ui.add(egui::ProgressBar::new(level as f32 / 4.0).desired_width(110.0).desired_height(8.0).fill(GREEN));
                ui.label(small(profile::level_name(level)));
            });
        }
    }

    // -----------------------------------------------------------------------
    // Interactions de vibelearn et aide
    // -----------------------------------------------------------------------

    fn interaction(&mut self, ctx: &egui::Context) {
        let Some(modal) = &mut self.app.modal else {
            self.focus_answer = true;
            return;
        };
        let screen = ctx.content_rect();
        let width = (screen.width() - 80.0).clamp(360.0, 680.0);
        let list_height = (screen.height() - 300.0).max(160.0);
        let mut done = false;
        let frame = Frame::window(&ctx.global_style()).inner_margin(Margin::same(20));
        egui::Modal::new(egui::Id::unique("interaction")).frame(frame).show(ctx, |ui| {
            ui.set_width(width);
            done = match modal {
                Modal::Checklist(c) => checklist(ui, c, list_height),
                Modal::Ask(a) => ask(ui, a, &mut self.markdown, list_height, &mut self.focus_answer),
                Modal::Permission(p) => permission(ui, p, list_height),
            };
        });
        if done {
            self.app.resolve_modal();
            self.focus_answer = true;
        }
    }

    fn help(&mut self, ctx: &egui::Context) {
        let mut open = self.app.show_help;
        egui::Window::new(tr("Aide", "Help")).open(&mut open).collapsible(false).resizable(false).default_width(520.0).show(ctx, |ui| {
            ui.label(tr(
                "Tu réponds à vibelearn en codant dans ton éditeur : il voit ce que tu modifies et te répond ici. Il ne te donnera pas la solution, mais il découpera le problème autant qu'il faut.",
                "You answer vibelearn by coding in your editor: it sees what you change and replies here. It will not give you the solution, but it will break the problem down as far as needed.",
            ));
            ui.add_space(6.0);
            let rows: [(&str, &str); 7] = [
                (tr("Un indice", "A hint"), tr("un coup de pouce de plus, sur 4 niveaux", "one more nudge, across 4 levels")),
                (tr("Vérifier mon code", "Check my code"), tr("évaluation immédiate de tes fichiers", "evaluate your files right now")),
                (tr("Je suis bloqué", "I'm stuck"), tr("vibelearn découpe l'étape en plus petit", "vibelearn splits the step into smaller ones")),
                (tr("Réexplique", "Explain again"), tr("la même notion sous un autre angle", "the same idea from another angle")),
                (tr("Plus…", "More…"), tr("quiz, récap, passer une étape, lancer une commande, terminer", "quiz, recap, skip a step, run a command, finish")),
                (tr("Envoi automatique", "Auto-send"), tr("tes changements partent quelques secondes après ta sauvegarde", "your changes are sent a few seconds after you save")),
                (tr("Fermer la fenêtre", "Close the window"), tr("la session est sauvegardée ; reprends avec `vibelearn resume`", "the session is saved; come back with `vibelearn resume`")),
            ];
            egui::Grid::new("help-grid").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                for (name, what) in rows {
                    ui.label(RichText::new(name).strong());
                    ui.add(egui::Label::new(what).wrap());
                    ui.end_row();
                }
            });
        });
        self.app.show_help = open;
    }
}

// ---------------------------------------------------------------------------
// Entrées de la conversation
// ---------------------------------------------------------------------------

fn entry(ui: &mut Ui, cache: &mut CommonMarkCache, entry: &Entry, streaming: bool) {
    match entry {
        Entry::User(text) => {
            ui.add_space(6.0);
            ui.label(small(tr("Toi", "You")));
            Frame::new()
                .fill(ui.visuals().selection.bg_fill.gamma_multiply(0.35))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.add(egui::Label::new(text).wrap());
                });
        }
        Entry::Tutor(text) | Entry::Comment(text) => {
            ui.add_space(6.0);
            if streaming {
                CommonMarkViewer::new().show(ui, cache, &format!("{text} ▍"));
            } else {
                CommonMarkViewer::new().show(ui, cache, text);
            }
        }
        Entry::Activity(text) => {
            ui.label(small(text.clone()));
        }
        Entry::Notice(text) => {
            ui.add_space(4.0);
            card(ui, ui.visuals().hyperlink_color).show(ui, |ui| {
                ui.set_width(ui.available_width());
                CommonMarkViewer::new().show(ui, cache, text);
            });
        }
        Entry::Changes(text) => {
            ui.add_space(4.0);
            ui.label(RichText::new(format!("{} {}", tr("Envoyé à vibelearn :", "Sent to vibelearn:"), text.replace('`', ""))).small().color(GREEN));
        }
        Entry::Widget(text) => {
            // Question posée et réponse donnée : même rendu que le reste (le
            // tuteur écrit en markdown), en plus discret.
            ui.add_space(4.0);
            ui.scope(|ui| {
                let muted = ui.visuals().weak_text_color();
                ui.visuals_mut().override_text_color = Some(muted);
                CommonMarkViewer::new().show(ui, cache, text);
            });
        }
        Entry::Output(text) => {
            code_block(ui, text, 260.0);
        }
        Entry::Error(text) => {
            ui.add_space(4.0);
            card(ui, RED).show(ui, |ui| {
                ui.set_width(ui.available_width());
                CommonMarkViewer::new().show(ui, cache, text);
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Contenu des boîtes de dialogue (renvoient vrai quand l'apprenant a répondu)
// ---------------------------------------------------------------------------

fn waiting_label(ui: &mut Ui) {
    ui.label(RichText::new(tr("vibelearn attend ta réponse.", "vibelearn is waiting for your answer.")).small().color(GREEN));
}

fn primary(ui: &mut Ui, enabled: bool, label: &str) -> bool {
    let button = egui::Button::new(RichText::new(label).strong()).fill(ui.visuals().selection.bg_fill).min_size(vec2(120.0, 36.0));
    ui.add_enabled(enabled, button).clicked()
}

fn checklist(ui: &mut Ui, c: &mut Checklist, list_height: f32) -> bool {
    ui.heading(&c.title);
    waiting_label(ui);
    if let Some(intro) = &c.intro {
        ui.add(egui::Label::new(intro).wrap());
    }
    ui.add_space(4.0);
    ScrollArea::vertical().id_salt("checklist").max_height(list_height).auto_shrink([false, true]).show(ui, |ui| {
        for (index, item) in c.items.iter_mut().enumerate() {
            ui.push_id(index, |ui| {
                let stroke = match item.state {
                    Claim::Known => GREEN,
                    Claim::Unsure => AMBER,
                    Claim::Unknown => ui.visuals().widgets.noninteractive.bg_stroke.color,
                };
                card(ui, stroke).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(&item.label).strong());
                        match item.kind.as_str() {
                            "prerequisite" => {
                                ui.label(small(tr("prérequis", "prerequisite")));
                            }
                            "bonus" => {
                                ui.label(small("bonus"));
                            }
                            _ => {}
                        }
                    });
                    if let Some(why) = &item.why {
                        ui.add(egui::Label::new(weak(why)).wrap());
                    }
                    if let Some(evidence) = &item.evidence {
                        ui.add(egui::Label::new(small(format!("{} {evidence}", tr("Ce que j'ai vu :", "What I saw:")))).wrap());
                    }
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut item.state, Claim::Known, tr("Je connais", "I know it"));
                        ui.selectable_value(&mut item.state, Claim::Unsure, tr("Pas sûr", "Not sure"));
                        ui.selectable_value(&mut item.state, Claim::Unknown, tr("À apprendre", "To learn"));
                        if item.assumed {
                            ui.label(small(tr("pré-coché d'après ton code", "pre-ticked from your code")));
                        }
                    });
                });
            });
        }
    });
    ui.add_space(4.0);
    ui.label(small(format!(
        "{} {}, {} {}, {} {}",
        c.count(Claim::Known),
        tr("connues", "known"),
        c.count(Claim::Unsure),
        tr("pas sûr", "not sure"),
        c.count(Claim::Unknown),
        tr("à apprendre", "to learn"),
    )));
    let mut done = false;
    ui.horizontal(|ui| {
        done = primary(ui, true, tr("Valider", "Confirm"));
        if ui.button(tr("Je connais tout", "I know all of it")).clicked() {
            c.set_all(Claim::Known);
        }
        if ui.button(tr("Tout est à apprendre", "It's all new")).clicked() {
            c.set_all(Claim::Unknown);
        }
    });
    done
}

fn ask(ui: &mut Ui, a: &mut Ask, cache: &mut CommonMarkCache, max_height: f32, focus: &mut bool) -> bool {
    let title = match a.kind {
        AskKind::Predict => tr("Prédis avant de lancer", "Predict before running"),
        AskKind::Free => tr("Avec tes mots", "In your own words"),
        AskKind::Confidence => tr("Ta confiance", "Your confidence"),
        AskKind::Multi => tr("Question (plusieurs réponses possibles)", "Question (several answers possible)"),
        AskKind::Choice => tr("Question", "Question"),
    };
    ui.heading(title);
    waiting_label(ui);
    ScrollArea::vertical().id_salt("question").max_height(max_height).auto_shrink([false, true]).show(ui, |ui| {
        CommonMarkViewer::new().show(ui, cache, &a.question);
        if let Some(code) = &a.code {
            code_block(ui, code, 280.0);
        }
        ui.add_space(4.0);
        match a.kind {
            AskKind::Choice => {
                for (index, option) in a.options.iter().enumerate() {
                    ui.radio_value(&mut a.choice, Some(index), option);
                }
            }
            AskKind::Multi => {
                for (index, option) in a.options.iter().enumerate() {
                    ui.checkbox(&mut a.selected[index], option);
                }
            }
            AskKind::Predict | AskKind::Free => {
                let field = ui.add(
                    TextEdit::multiline(&mut a.text)
                        .desired_rows(4)
                        .desired_width(f32::INFINITY)
                        .hint_text(tr("Ta réponse…", "Your answer…")),
                );
                if std::mem::take(focus) {
                    field.request_focus();
                }
            }
            AskKind::Confidence => {
                ui.horizontal(|ui| {
                    ui.label(weak(tr("pas sûr du tout", "not sure at all")));
                    for level in 1..=5u8 {
                        ui.selectable_value(&mut a.confidence, level, level.to_string());
                    }
                    ui.label(weak(tr("certain", "certain")));
                });
            }
        }
    });
    ui.add_space(6.0);
    let mut done = false;
    ui.horizontal(|ui| {
        if primary(ui, a.can_submit(), tr("Valider", "Confirm")) {
            done = true;
        }
        // Ctrl+Entrée valide aussi, pratique après avoir tapé une réponse.
        if a.can_submit() && ui.input(|i| i.key_pressed(Key::Enter) && i.modifiers.command) {
            done = true;
        }
        if a.kind != AskKind::Confidence && ui.button(tr("Je ne sais pas", "I don't know")).on_hover_text(tr("C'est une réponse utile, pas une faute.", "That is a useful answer, not a mistake.")).clicked() {
            a.dont_know = true;
            done = true;
        }
    });
    done
}

fn permission(ui: &mut Ui, p: &mut PermissionPrompt, max_height: f32) -> bool {
    ui.heading(tr("Autorisation", "Permission"));
    ui.add(egui::Label::new(RichText::new(&p.headline).strong()).wrap());
    if !p.detail.trim().is_empty() {
        code_block(ui, &p.detail, max_height.min(260.0));
    }
    if p.write_path.is_some() {
        ui.add(egui::Label::new(weak(tr(
            "Attendu seulement pour un squelette ou un test à compléter, jamais pour la solution.",
            "Only expected for scaffolding or a test to fill in, never for the solution.",
        )))
        .wrap());
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if primary(ui, true, tr("Autoriser", "Allow")) {
            p.decision = Some(Decision::Allow);
        }
        if ui.button(tr("Toujours, pendant cette session", "Always, for this session")).clicked() {
            p.decision = Some(Decision::Always);
        }
        if ui.button(tr("Refuser", "Deny")).clicked() {
            p.decision = Some(Decision::Deny);
        }
    });
    p.decision.is_some()
}
