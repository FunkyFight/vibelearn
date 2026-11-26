//! La fenêtre flottante : les remarques de la dernière sauvegarde et la phrase
//! d'analyse de vibelearn, hors de la discussion.
//!
//! Une petite fenêtre native (egui), sans barre de titre et toujours devant,
//! que l'on pose où l'on veut, même hors de VS Code : on la tire par son
//! bandeau, on l'agrandit par son coin. Sa place et sa taille sont retenues
//! dans `~/.vibelearn/float.json`.
//!
//! Elle tourne sur son propre fil. La session lui passe l'état publié pour
//! VS Code ([`Float::show`]) et relit ses clics ([`Float::actions`]).

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Color32, CornerRadius, CursorIcon, FontId, Layout, Pos2, Rect, ResizeDirection, RichText, Sense, Stroke, Vec2,
    ViewportCommand, text::LayoutJob,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::i18n::tr;
use crate::paths;

/// Ce que l'on fait dans la fenêtre.
pub enum Action {
    Goto { file: PathBuf, line: usize },
    Teach(u64),
    Forget(u64),
}

/// Le lien entre la session et la fenêtre.
pub struct Float {
    shared: Arc<Mutex<Shared>>,
    actions: Mutex<Receiver<Action>>,
}

#[derive(Default)]
struct Shared {
    state: Value,
    ctx: Option<egui::Context>,
}

impl Float {
    /// Montre l'état que la session vient de publier.
    pub fn show(&self, state: &Value) {
        let Ok(mut shared) = self.shared.lock() else { return };
        shared.state = state.clone();
        if let Some(ctx) = &shared.ctx {
            ctx.request_repaint();
        }
    }

    /// Les clics faits depuis le dernier appel.
    pub fn actions(&self) -> Vec<Action> {
        self.actions.lock().map(|rx| rx.try_iter().collect()).unwrap_or_default()
    }
}

/// Ouvre la fenêtre, sauf si `VIBELEARN_NO_FLOAT` est posée. `watching` : en
/// surveillance elle s'ouvre tout de suite ; sinon elle attend une remarque.
pub fn open(watching: bool) -> Option<Float> {
    if std::env::var_os("VIBELEARN_NO_FLOAT").is_some() {
        return None;
    }
    let shared = Arc::new(Mutex::new(Shared::default()));
    let (tx, rx) = channel();
    let for_window = shared.clone();
    std::thread::Builder::new()
        .name("vibelearn-float".into())
        .spawn(move || {
            if let Err(e) = run(for_window, tx, watching) {
                eprintln!("  vibelearn : {} ({e})", tr("la fenêtre des remarques n'a pas pu s'ouvrir", "the remarks window could not open"));
            }
        })
        .ok()?;
    Some(Float { shared, actions: Mutex::new(rx) })
}

/// Place et taille retenues d'une session à l'autre.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Placement {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    #[serde(default)]
    folded: bool,
}

fn placement_file() -> Option<PathBuf> {
    paths::global_dir().ok().map(|d| d.join("float.json"))
}

fn load_placement() -> Option<Placement> {
    let text = std::fs::read_to_string(placement_file()?).ok()?;
    serde_json::from_str(&text).ok()
}

fn save_placement(p: &Placement) {
    if let Some(file) = placement_file() {
        let _ = std::fs::write(file, serde_json::to_string(p).unwrap_or_default());
    }
}

const WIDTH: f32 = 320.0;
const HEIGHT: f32 = 210.0;
const BAR: f32 = 24.0;

fn run(shared: Arc<Mutex<Shared>>, tx: Sender<Action>, watching: bool) -> eframe::Result<()> {
    let saved = load_placement();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("vibelearn")
        .with_decorations(false)
        .with_always_on_top()
        .with_taskbar(false)
        .with_resizable(true)
        .with_min_inner_size([180.0, BAR])
        .with_inner_size(saved.map(|p| [p.w, if p.folded { BAR } else { p.h }]).unwrap_or([WIDTH, HEIGHT]))
        .with_visible(watching);
    if let Some(p) = saved {
        viewport = viewport.with_position([p.x, p.y]);
    }
    let options = eframe::NativeOptions {
        viewport,
        persist_window: false,
        // Le fil principal est celui de la session : la fenêtre a le sien.
        event_loop_builder: Some(Box::new(|builder| {
            #[cfg(target_os = "windows")]
            {
                use winit::platform::windows::EventLoopBuilderExtWindows;
                builder.with_any_thread(true);
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                use winit::platform::wayland::EventLoopBuilderExtWayland;
                use winit::platform::x11::EventLoopBuilderExtX11;
                EventLoopBuilderExtX11::with_any_thread(builder, true);
                EventLoopBuilderExtWayland::with_any_thread(builder, true);
            }
            #[cfg(target_os = "macos")]
            let _ = builder;
        })),
        ..Default::default()
    };
    eframe::run_native(
        "vibelearn-float",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            if let Ok(mut s) = shared.lock() {
                s.ctx = Some(cc.egui_ctx.clone());
            }
            Ok(Box::new(Window {
                shared,
                tx,
                open: Default::default(),
                placement: saved,
                placed: saved.is_some(),
                shown: watching,
                dirty: None,
                unfolded_h: saved.map(|p| p.h).unwrap_or(HEIGHT),
            }))
        }),
    )
}

struct Window {
    shared: Arc<Mutex<Shared>>,
    tx: Sender<Action>,
    /// Les remarques dont le détail est déplié.
    open: std::collections::HashSet<u64>,
    placement: Option<Placement>,
    /// Une place a été donnée (retenue, ou choisie au premier affichage).
    placed: bool,
    /// Déjà montrée une fois (hors surveillance, elle attend une remarque).
    shown: bool,
    /// La place a changé, pas encore écrite.
    dirty: Option<Instant>,
    unfolded_h: f32,
}

/// Les couleurs des remarques, comme dans VS Code.
fn tone_color(tone: &str) -> Color32 {
    match tone {
        "fix" => Color32::from_rgb(0xE0, 0xA1, 0x00),
        "design" => Color32::from_rgb(0xE0, 0x6C, 0x98),
        "ok" => Color32::from_rgb(0x3F, 0xA1, 0x5B),
        _ => Color32::from_rgb(0x7F, 0x77, 0xDD),
    }
}

/// Ce qui casse d'abord, les compliments en dernier.
fn tone_rank(tone: &str) -> u8 {
    match tone {
        "fix" => 0,
        "design" => 1,
        "ok" => 3,
        _ => 2,
    }
}

const TEXT: Color32 = Color32::from_rgb(0xCC, 0xCC, 0xCC);
const DIM: Color32 = Color32::from_rgb(0x9D, 0x9D, 0x9D);
const LINK: Color32 = Color32::from_rgb(0x4D, 0xAA, 0xFC);
const BG: Color32 = Color32::from_rgb(0x1F, 0x1F, 0x1F);
const BAR_BG: Color32 = Color32::from_rgb(0x18, 0x18, 0x18);
const CODE_BG: Color32 = Color32::from_rgb(0x2B, 0x2B, 0x2B);

/// Une ligne de texte avec `code`, **gras** retiré : ce que le modèle écrit d'ordinaire.
fn inline(text: &str, size: f32, color: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let clean = text.replace("**", "");
    for (i, part) in clean.split('`').enumerate() {
        if part.is_empty() {
            continue;
        }
        let code = i % 2 == 1;
        job.append(
            part,
            0.0,
            egui::TextFormat {
                font_id: if code { FontId::monospace(size * 0.95) } else { FontId::proportional(size) },
                color,
                background: if code { CODE_BG } else { Color32::TRANSPARENT },
                ..Default::default()
            },
        );
    }
    job
}

/// Un texte en markdown simple : paragraphes, listes, blocs de code.
fn markdown(ui: &mut egui::Ui, text: &str, size: f32, color: Color32) {
    let mut code: Option<Vec<&str>> = None;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            match code.take() {
                Some(lines) => {
                    egui::Frame::new().fill(CODE_BG).corner_radius(CornerRadius::same(3)).inner_margin(4).show(ui, |ui| {
                        ui.label(RichText::new(lines.join("\n")).font(FontId::monospace(size * 0.95)).color(color));
                    });
                }
                None => code = Some(Vec::new()),
            }
            continue;
        }
        if let Some(lines) = code.as_mut() {
            lines.push(line);
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            ui.add_space(2.0);
            continue;
        }
        let item = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* "));
        let body = item.map(|i| format!("• {i}")).unwrap_or_else(|| trimmed.trim_start_matches('#').trim().to_string());
        ui.label(inline(&body, size, color));
    }
    if let Some(lines) = code {
        ui.label(RichText::new(lines.join("\n")).font(FontId::monospace(size * 0.95)).color(color));
    }
}

fn s<'a>(state: &'a Value, key: &str) -> &'a str {
    state["strings"][key].as_str().unwrap_or("")
}

impl eframe::App for Window {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BG.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        let state = self.shared.lock().map(|s| s.state.clone()).unwrap_or(Value::Null);
        let remarks: Vec<&Value> = state["annotations"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
        let review = state["review"]["text"].as_str().filter(|t| !t.trim().is_empty());

        // Hors surveillance, elle n'apparaît qu'à la première remarque.
        if !self.shown && (!remarks.is_empty() || review.is_some()) {
            self.shown = true;
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        }
        self.place(ctx);
        self.remember(ctx);

        let folded = self.placement.is_some_and(|p| p.folded);
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG)).show(ui, |ui| {
            self.bar(ui, &state, remarks.len(), folded);
            if folded {
                return;
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(4.0);
                self.body(ui, &state, &remarks, review);
                ui.add_space(6.0);
            });
            self.grip(ui);
        });
    }
}

impl Window {
    /// La première fois, sans place retenue : en haut à droite de l'écran.
    fn place(&mut self, ctx: &egui::Context) {
        if self.placed {
            return;
        }
        let Some(monitor) = ctx.input(|i| i.viewport().monitor_size) else { return };
        let size = ctx.input(|i| i.viewport().outer_rect.map(|r| r.size())).unwrap_or(Vec2::new(WIDTH, HEIGHT));
        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(Pos2::new(monitor.x - size.x - 24.0, 72.0)));
        self.placed = true;
    }

    /// Retient où elle est et sa taille (au plus une écriture par seconde).
    fn remember(&mut self, ctx: &egui::Context) {
        let Some(rect) = ctx.input(|i| i.viewport().outer_rect) else { return };
        let folded = self.placement.is_some_and(|p| p.folded);
        if !folded {
            self.unfolded_h = rect.height();
        }
        let now = Placement { x: rect.min.x, y: rect.min.y, w: rect.width(), h: self.unfolded_h, folded };
        let changed = self.placement.is_none_or(|p| {
            (p.x - now.x).abs() > 1.0 || (p.y - now.y).abs() > 1.0 || (p.w - now.w).abs() > 1.0 || (p.h - now.h).abs() > 1.0
        });
        if changed {
            self.placement = Some(now);
            self.dirty = Some(Instant::now());
        }
        // On écrit une fois la fenêtre posée, pas à chaque pixel du déplacement.
        match self.dirty {
            Some(t) if t.elapsed() > Duration::from_millis(800) => {
                if let Some(p) = &self.placement {
                    save_placement(p);
                }
                self.dirty = None;
            }
            Some(_) => ctx.request_repaint_after(Duration::from_millis(900)),
            None => {}
        }
    }

    /// Le bandeau : on la tire par là, et on la replie.
    fn bar(&mut self, ui: &mut egui::Ui, state: &Value, count: usize, folded: bool) {
        let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), BAR), Sense::click_and_drag());
        ui.painter().rect_filled(rect, CornerRadius::ZERO, BAR_BG);
        if response.drag_started_by(egui::PointerButton::Primary) {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }
        let status = state["status"].as_str().unwrap_or("");
        let busy = matches!(status, "busy" | "starting");
        let dot = Pos2::new(rect.left() + 11.0, rect.center().y);
        if busy {
            let spinner = Rect::from_center_size(dot, Vec2::splat(10.0));
            ui.put(spinner, egui::Spinner::new().size(10.0).color(LINK));
        } else {
            let color = match status {
                "ended" | "" => DIM,
                _ if !state["prompt"].is_null() => LINK,
                _ => tone_color("ok"),
            };
            ui.painter().circle_filled(dot, 4.0, color);
        }
        let title = match s(state, "float_title") {
            "" => "vibelearn",
            t => t,
        };
        let detail = if busy {
            s(state, "float_reading").to_string()
        } else if folded && count > 0 {
            format!("{count}")
        } else {
            state["status_text"].as_str().unwrap_or("").to_string()
        };
        let mut job = LayoutJob::default();
        job.append(title, 0.0, egui::TextFormat { font_id: FontId::proportional(12.5), color: TEXT, ..Default::default() });
        job.append(&detail, 8.0, egui::TextFormat { font_id: FontId::proportional(12.0), color: DIM, ..Default::default() });
        job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width() - 56.0);
        let galley = ui.painter().layout_job(job);
        ui.painter().galley(Pos2::new(rect.left() + 22.0, rect.center().y - galley.size().y / 2.0), galley, TEXT);

        // Replier / déplier : il ne reste que le bandeau.
        let button = Rect::from_center_size(Pos2::new(rect.right() - 13.0, rect.center().y), Vec2::splat(20.0));
        let fold = ui.interact(button, ui.scope_id().with("fold"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        if fold.hovered() {
            ui.painter().rect_filled(button, CornerRadius::same(3), Color32::from_white_alpha(18));
        }
        ui.painter().text(button.center(), egui::Align2::CENTER_CENTER, if folded { "▾" } else { "▴" }, FontId::proportional(13.0), DIM);
        if fold.clicked() || response.double_clicked() {
            self.toggle_fold(ui.ctx());
        }
    }

    fn toggle_fold(&mut self, ctx: &egui::Context) {
        let Some(p) = self.placement.as_mut() else { return };
        p.folded = !p.folded;
        let h = if p.folded { BAR } else { self.unfolded_h.max(BAR * 3.0) };
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(Vec2::new(p.w, h)));
        save_placement(p);
    }

    fn body(&mut self, ui: &mut egui::Ui, state: &Value, remarks: &[&Value], review: Option<&str>) {
        let size = 12.5;
        let pad = 8.0;
        if state["status"] == "ended" {
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.label(RichText::new(s(state, "ended")).size(size).color(DIM));
            });
            return;
        }
        if let Some(text) = review {
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.vertical(|ui| {
                    ui.set_max_width(ui.available_width() - pad);
                    markdown(ui, text, size, DIM);
                });
            });
            ui.add_space(4.0);
        }
        let mut sorted: Vec<&Value> = remarks.to_vec();
        sorted.sort_by_key(|a| tone_rank(a["tone"].as_str().unwrap_or("info")));
        let watching = matches!(state["mode"].as_str(), Some("monitor" | "assist"));
        let in_lesson = !state["step"].is_null();
        for a in sorted {
            self.remark(ui, state, a, watching && !in_lesson, watching);
        }
        if remarks.is_empty() && review.is_none() {
            ui.horizontal(|ui| {
                ui.add_space(pad);
                let busy = matches!(state["status"].as_str(), Some("busy" | "starting"));
                let text = if busy { s(state, "float_reading") } else { s(state, "float_empty") };
                ui.label(RichText::new(text).size(size).color(DIM));
            });
        }
    }

    /// Une remarque : son titre (clic : la ligne), où, puis le détail à la demande.
    fn remark(&mut self, ui: &mut egui::Ui, state: &Value, a: &Value, teach: bool, forget: bool) {
        let id = a["id"].as_u64().unwrap_or(0);
        let tone = a["tone"].as_str().unwrap_or("info");
        let text = a["text"].as_str().unwrap_or("");
        let (head, rest) = match text.split_once("\n\n") {
            Some((h, r)) => (h.trim(), r.trim()),
            None => (text.trim(), ""),
        };
        let file = PathBuf::from(a["file"].as_str().unwrap_or(""));
        let line = a["line"].as_u64().unwrap_or(0) as usize;
        let name = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let open = self.open.contains(&id);

        let top = ui.cursor().min.y;
        let response = ui
            .horizontal(|ui| {
                ui.add_space(8.0);
                let (dot, _) = ui.allocate_exact_size(Vec2::new(8.0, 14.0), Sense::hover());
                ui.painter().circle_filled(Pos2::new(dot.center().x, dot.top() + 8.0), 3.5, tone_color(tone));
                ui.add_space(2.0);
                ui.vertical(|ui| {
                    ui.set_max_width(ui.available_width() - 8.0);
                    let title = ui
                        .add(egui::Label::new(inline(head, 12.5, TEXT)).sense(Sense::click()).wrap())
                        .on_hover_cursor(CursorIcon::PointingHand);
                    if title.clicked() {
                        let _ = self.tx.send(Action::Goto { file: file.clone(), line });
                    }
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        let loc = ui
                            .add(egui::Label::new(RichText::new(format!("{name}:{}", line + 1)).size(11.5).color(DIM)).sense(Sense::click()))
                            .on_hover_cursor(CursorIcon::PointingHand);
                        if loc.clicked() {
                            let _ = self.tx.send(Action::Goto { file: file.clone(), line });
                        }
                        if !rest.is_empty() && link(ui, &format!("{} {}", if open { "▾" } else { "▸" }, s(state, "float_detail")), DIM) {
                            if open {
                                self.open.remove(&id);
                            } else {
                                self.open.insert(id);
                            }
                        }
                        let hovered = ui.rect_contains_pointer(ui.max_rect().expand2(Vec2::new(400.0, 30.0)));
                        if hovered || open {
                            if teach && link(ui, s(state, "teach"), LINK) {
                                let _ = self.tx.send(Action::Teach(id));
                            }
                            if forget && link(ui, s(state, "forget"), LINK) {
                                let _ = self.tx.send(Action::Forget(id));
                            }
                        }
                    });
                    if open && !rest.is_empty() {
                        ui.add_space(2.0);
                        markdown(ui, rest, 12.0, DIM);
                    }
                });
            })
            .response;
        let row = Rect::from_min_max(Pos2::new(ui.max_rect().left(), top), Pos2::new(ui.max_rect().right(), response.rect.bottom()));
        if ui.rect_contains_pointer(row) {
            ui.painter().rect_filled(row, CornerRadius::ZERO, Color32::from_white_alpha(6));
        }
        ui.add_space(4.0);
    }

    /// Le coin en bas à droite : on la redimensionne par là.
    fn grip(&self, ui: &mut egui::Ui) {
        let corner = ui.ctx().content_rect().right_bottom();
        let rect = Rect::from_min_max(corner - Vec2::splat(14.0), corner);
        let response = ui.interact(rect, ui.scope_id().with("grip"), Sense::drag()).on_hover_cursor(CursorIcon::ResizeNwSe);
        for i in 1..=3 {
            let d = i as f32 * 3.5;
            ui.painter().line_segment([corner - Vec2::new(d, 2.0), corner - Vec2::new(2.0, d)], Stroke::new(1.0, DIM));
        }
        if response.drag_started() {
            ui.ctx().send_viewport_cmd(ViewportCommand::BeginResize(ResizeDirection::SouthEast));
        }
    }
}

/// Un lien discret ; vrai au clic.
fn link(ui: &mut egui::Ui, text: &str, color: Color32) -> bool {
    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        ui.add(egui::Label::new(RichText::new(text).size(11.5).color(color)).sense(Sense::click()))
            .on_hover_cursor(CursorIcon::PointingHand)
            .clicked()
    })
    .inner
}
