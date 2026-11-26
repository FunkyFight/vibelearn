//! vibelearn : un tuteur lancé depuis ton terminal. Tu dis ce que tu veux apprendre,
//! Claude (via ton abonnement) t'y amène dans ton propre projet sans jamais te
//! donner la réponse : tu réponds à ses questions en codant.

// La table des textes du panneau (`front::strings`) dépasse la profondeur par défaut de `json!`.
#![recursion_limit = "256"]

mod annotate;
mod api;
mod app;
mod assist;
mod claude;
mod config;
mod float;
mod front;
#[cfg(feature = "window")]
mod gui;
mod i18n;
mod localtools;
mod login;
mod mock;
mod modal;
mod paths;
mod profile;
mod project;
mod prompt;
mod runner;
mod session;
mod threads;
mod tools;
mod update;
mod vscode;
mod watch;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{Args, Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;
use tokio::sync::mpsc;

use app::{App, AppEvent, Backend, Options};
use config::Config;
use i18n::tr;
use login::{Login, Provider};
use profile::Profile;
use session::{Mode, SessionState};

#[derive(Parser)]
#[command(
    name = "vibelearn",
    version,
    about = "Learn by coding in your own project: vibelearn guides you and reviews your code, and never writes your task for you."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,

    /// What you want to learn or build, in plain words (e.g. vibelearn "traits in Rust")
    topic: Vec<String>,

    #[command(flatten)]
    opts: RunOpts,

    /// Interface language: fr or en (default: the system's)
    #[arg(long, global = true)]
    lang: Option<String>,
}

#[derive(Args, Clone, Default)]
struct RunOpts {
    /// Start from scratch even if a session on this topic already exists
    #[arg(long)]
    new: bool,

    /// Model (Claude: opus, sonnet, haiku or a full name; API: its name). Otherwise: the one from vibelearn login, vibelearn config model, or your subscription's
    #[arg(long)]
    model: Option<String>,

    /// Thinking effort: low, medium, high, xhigh, max (otherwise: vibelearn config effort, or medium)
    #[arg(long)]
    effort: Option<String>,

    /// Path of the `claude` binary (otherwise: PATH, native installer, desktop app)
    #[arg(long)]
    claude_bin: Option<PathBuf>,

    /// Seconds without changes before your changes are sent automatically (otherwise: vibelearn config auto-delay, or 6)
    #[arg(long)]
    auto_delay: Option<u64>,

    /// Only send your changes on demand (the "Check" button of the vibelearn panel)
    #[arg(long)]
    no_auto: bool,

    /// VS Code command (default: code; "none": neither opens the editor nor installs the extension)
    #[arg(long)]
    editor: Option<String>,

    /// Open a window instead of working in VS Code (binary built with `--features window`)
    #[arg(long, hide = true)]
    window: bool,

    /// Use ANTHROPIC_API_KEY if it is set, instead of the subscription
    #[arg(long)]
    use_api_key: bool,

    /// Scripted tutor, no model: to try the interface offline
    #[arg(long, hide = true)]
    mock: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Fix by itself what follows without a choice from what you just wrote (a renamed name left behind, a forgotten value), show each change, and comment on the rest at each save
    Assist {
        /// What you are working on, if you want to say (optional)
        about: Vec<String>,
        #[command(flatten)]
        opts: RunOpts,
    },
    /// Comment on your code at each save without teaching or fixing anything: errors, bugs, and how to make it more SOLID
    Monitor {
        /// What you are working on, if you want to say (optional)
        about: Vec<String>,
        #[command(flatten)]
        opts: RunOpts,
    },
    /// Restructure existing code (SOLID, DRY, extensible) without changing what it does; you are the one rewriting it
    Refactor {
        /// What to restructure, in plain words (e.g. vibelearn refactor the Game1 loop). Empty: the whole project
        target: Vec<String>,
        #[command(flatten)]
        opts: RunOpts,
    },
    /// Resume the session in progress in this project
    Resume(RunOpts),
    /// Review the concepts that are due (spaced repetition)
    Review(RunOpts),
    /// Show or change the settings (e.g. vibelearn config auto-delay 10; "default" to reset)
    Config {
        /// The setting (nothing: the list)
        key: Option<String>,
        /// The new value (nothing: the current value)
        value: Option<String>,
    },
    /// Show your profile: concepts, levels, upcoming reviews
    Profile,
    /// Check the installation: connection to the model, claude binary
    Doctor {
        #[arg(long)]
        claude_bin: Option<PathBuf>,
    },
    /// Connect vibelearn to a model: Claude subscription, official Claude API, Anthropic-compatible API, OpenAI-compatible API or OpenAI Responses API (Codex)
    Login {
        /// claude, claude-api, anthropic, openai or codex (nothing: a menu)
        provider: Option<String>,
        /// API URL (default: the official provider's)
        #[arg(long)]
        url: Option<String>,
        /// Model to use with this API
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        claude_bin: Option<PathBuf>,
    },
    /// Forget the saved API key, or sign out of your Claude subscription
    Logout {
        #[arg(long)]
        claude_bin: Option<PathBuf>,
    },
    /// Install the latest GitHub release now (vibelearn also does it by itself, in the background, unless auto-update is off)
    Update {
        /// Reinstall the latest release even if this version is not older
        #[arg(long)]
        force: bool,
    },
}

enum Start {
    Topic(String),
    /// Ce qu'il faut restructurer (vide : tout le projet).
    Refactor(String),
    /// Ce sur quoi l'apprenant travaille (vide : non précisé).
    Monitor(String),
    /// Comme `Monitor`, avec les corrections évidentes faites par vibelearn.
    Assist(String),
    Resume,
    Review,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    i18n::init(cli.lang.as_deref());
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vibelearn : {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn claude_bin(explicit: Option<&Path>) -> Result<PathBuf> {
    let from_env = std::env::var_os("VIBELEARN_CLAUDE").map(PathBuf::from);
    claude::locate(explicit.or(from_env.as_deref()))
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Some(Cmd::Doctor { claude_bin: bin }) => doctor(bin.as_deref()).await,
        Some(Cmd::Login { provider, url, model, claude_bin: bin }) => login(provider.as_deref(), url, model, bin.as_deref()).await,
        Some(Cmd::Logout { claude_bin: bin }) => {
            let current = Login::load();
            if current.provider.uses_claude() && current.key.is_none() {
                let bin = claude_bin(bin.as_deref())?;
                claude::passthrough(&bin, &["auth", "logout"])?;
            } else {
                Login::forget()?;
                println!(
                    "{}",
                    tr(
                        "Clé API oubliée. vibelearn repasse par ton abonnement Claude (vibelearn login pour en choisir une autre).",
                        "API key forgotten. vibelearn is back on your Claude subscription (vibelearn login to choose another).",
                    )
                );
            }
            Ok(())
        }
        Some(Cmd::Profile) => {
            show_profile();
            Ok(())
        }
        Some(Cmd::Resume(opts)) => learn(Start::Resume, opts).await,
        Some(Cmd::Review(opts)) => learn(Start::Review, opts).await,
        Some(Cmd::Refactor { target, opts }) => learn(Start::Refactor(target.join(" ").trim().to_string()), opts).await,
        Some(Cmd::Monitor { about, opts }) => learn(Start::Monitor(about.join(" ").trim().to_string()), opts).await,
        Some(Cmd::Assist { about, opts }) => learn(Start::Assist(about.join(" ").trim().to_string()), opts).await,
        Some(Cmd::Config { key, value }) => configure(key.as_deref(), value.as_deref()),
        Some(Cmd::Update { force }) => update::run(force).await,
        None => {
            let topic = cli.topic.join(" ").trim().to_string();
            if !topic.is_empty() {
                return learn(Start::Topic(topic), cli.opts).await;
            }
            let root = std::env::current_dir()?;
            if SessionState::load(&root).is_some_and(|s| !s.completed) {
                return learn(Start::Resume, cli.opts).await;
            }
            println!(
                "{}",
                tr(
                    "Dis-moi ce que tu veux apprendre, depuis le dossier de ton projet :\n\n    vibelearn \"les traits en Rust\"\n    vibelearn ajouter une authentification JWT à mon API\n\nPour restructurer du code existant (SOLID, DRY, extensible) :\n\n    vibelearn refactor la boucle de jeu de Game1\n\nPour qu'il relise chaque sauvegarde et finisse lui-même les oublis évidents :\n\n    vibelearn assist\n\nAutres commandes : monitor, resume, review, profile, config, doctor, login, update   (vibelearn --help)",
                    "Tell me what you want to learn, from your project folder:\n\n    vibelearn \"traits in Rust\"\n    vibelearn add JWT auth to my API\n\nTo restructure existing code (SOLID, DRY, extensible):\n\n    vibelearn refactor the game loop in Game1\n\nTo have it review each save and finish the obvious leftovers itself:\n\n    vibelearn assist\n\nOther commands: monitor, resume, review, profile, config, doctor, login, update   (vibelearn --help)",
                )
            );
            Ok(())
        }
    }
}

/// `vibelearn login [fournisseur]` : choisit comment joindre le modèle et l'enregistre.
async fn login(provider: Option<&str>, url: Option<String>, model: Option<String>, bin: Option<&Path>) -> Result<()> {
    let choice = match provider {
        Some(p) => login::Choice::parse(p)
            .with_context(|| format!("{} « {p} » ({})", tr("fournisseur inconnu", "unknown provider"), login::Choice::names()))?,
        None => login::choose()?,
    };
    let provider = choice.provider;
    let url = url.or_else(|| choice.url.map(str::to_string));
    let ready = tr("Lance : vibelearn \"ce que tu veux apprendre\"", "Run: vibelearn \"what you want to learn\"");
    if provider == Provider::Claude {
        let bin = claude_bin(bin)?;
        let code = claude::passthrough(&bin, &["auth", "login"])?;
        if code != 0 {
            bail!("{}", tr("la connexion n'a pas abouti", "sign-in did not complete"));
        }
        // Un modèle choisi ici reste le réglage de l'abonnement.
        Login { provider, url: None, key: None, model: None }.save()?;
        if let Some(model) = model {
            let mut config = Config::load();
            config.set("model", &model)?;
            config.save()?;
        }
        println!("{} {ready}", tr("Connecté à ton abonnement Claude.", "Signed in to your Claude subscription."));
        return Ok(());
    }
    let saved = login::login_with_key(provider, url, model).await?;
    println!(
        "{} {}, {}, {}. {ready}",
        tr("Connecté :", "Connected:"),
        login::Choice::of(&saved).label(),
        saved.url(),
        saved.model.as_deref().unwrap_or("-"),
    );
    Ok(())
}

/// Le moteur à lancer d'après `vibelearn login`, et le modèle à demander.
fn backend(opts: &RunOpts, config: &Config) -> Result<(Backend, Option<String>)> {
    let login = Login::load();
    let key = || login.key.clone().filter(|k| !k.is_empty());
    let missing_key = || {
        anyhow::anyhow!(
            "{}",
            tr("la connexion enregistrée n'a pas de clé API : relance vibelearn login", "the saved connection has no API key: run vibelearn login again")
        )
    };
    Ok(match login.provider {
        Provider::Claude => (
            Backend::Claude { bin: claude_bin(opts.claude_bin.as_deref())?, keep_api_key: opts.use_api_key },
            config.model(opts.model.clone()),
        ),
        Provider::Anthropic | Provider::Openai | Provider::Codex => {
            let model = opts.model.clone().or_else(|| login.model.clone()).with_context(|| {
                tr("aucun modèle choisi : relance vibelearn login, ou passe --model", "no model chosen: run vibelearn login again, or pass --model")
            })?;
            let dialect = match login.provider {
                Provider::Anthropic => api::Dialect::Anthropic,
                Provider::Codex => api::Dialect::Responses,
                _ => api::Dialect::Chat,
            };
            (Backend::Api { dialect, url: login.url(), key: key().ok_or_else(missing_key)? }, Some(model))
        }
    })
}

fn same_topic(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// `vibelearn config [clé [valeur]]` : liste, lit ou change un réglage.
fn configure(key: Option<&str>, value: Option<&str>) -> Result<()> {
    let mut config = Config::load();
    let show = |setting: &config::Setting, config: &Config| {
        let (value, saved) = setting.current(config);
        let origin = if saved { "" } else { tr("  (défaut)", "  (default)") };
        println!("  {:<12} {value}{origin}\n  {:<12} {}", setting.key, "", setting.about());
    };
    match (key, value) {
        (None, _) => {
            for setting in config::SETTINGS {
                show(setting, &config);
            }
            println!("\n{}", tr("Changer : vibelearn config <réglage> <valeur>   (« default » pour revenir au défaut)", "Change: vibelearn config <setting> <value>   (\"default\" to reset)"));
        }
        (Some(key), None) => show(config::find(key)?, &config),
        (Some(key), Some(value)) => {
            config.set(key, value)?;
            config.save()?;
            show(config::find(key)?, &config);
        }
    }
    Ok(())
}

/// Reprend la session en cours si elle porte sur le même sujet dans le même
/// mode ; sinon la range dans l'historique et en commence une nouvelle.
fn start_or_continue(
    root: &Path,
    existing: Option<SessionState>,
    topic: &str,
    mode: Mode,
    fresh: bool,
    archived: &mut Option<String>,
) -> Result<(SessionState, bool)> {
    match existing {
        Some(s) if !fresh && !s.completed && s.mode == mode && same_topic(&s.topic, topic) => Ok((s, true)),
        Some(s) => {
            if !s.completed {
                *archived = Some(s.topic.clone());
            }
            SessionState::archive(root)?;
            Ok((SessionState::new(topic, mode), false))
        }
        None => Ok((SessionState::new(topic, mode), false)),
    }
}

async fn learn(start: Start, opts: RunOpts) -> Result<()> {
    let root = std::env::current_dir().context("dossier courant")?;
    update::cleanup();
    let profile = Profile::load();
    let existing = SessionState::load(&root);
    let mut archived: Option<String> = None;

    let (session, resuming) = match start {
        Start::Resume => match existing {
            Some(s) if !s.completed => (s, true),
            Some(_) => bail!(
                "{}",
                tr(
                    "la dernière session de ce projet est terminée. Lance : vibelearn \"un nouveau sujet\"",
                    "the last session in this project is finished. Run: vibelearn \"a new topic\"",
                )
            ),
            None => bail!("{}", tr("aucune session à reprendre dans ce dossier", "no session to resume in this folder")),
        },
        Start::Topic(topic) => start_or_continue(&root, existing, &topic, Mode::Learn, opts.new, &mut archived)?,
        Start::Refactor(target) => {
            let target = if target.is_empty() { tr("tout le projet", "the whole project").to_string() } else { target };
            start_or_continue(&root, existing, &target, Mode::Refactor, opts.new, &mut archived)?
        }
        // La surveillance a son propre fichier : elle ne range pas la session d'apprentissage en cours.
        Start::Monitor(about) => {
            let about = if about.is_empty() { tr("non précisé", "not specified").to_string() } else { about };
            (SessionState::new(&about, Mode::Monitor), false)
        }
        Start::Assist(about) => {
            let about = if about.is_empty() { tr("non précisé", "not specified").to_string() } else { about };
            (SessionState::new(&about, Mode::Assist), false)
        }
        Start::Review => {
            let due = profile.due(Utc::now().date_naive()).len();
            if due == 0 {
                println!("{}", tr("Rien à réviser aujourd'hui. Reviens demain, ou apprends autre chose.", "Nothing due for review today. Come back tomorrow, or learn something new."));
                return Ok(());
            }
            let topic = format!("{} ({due})", tr("Révision", "Review"));
            (SessionState::new(&topic, Mode::Review), false)
        }
    };

    let config = Config::load();
    let (backend, model) = if opts.mock { (None, config.model(opts.model.clone())) } else {
        let (b, m) = backend(&opts, &config)?;
        (Some(b), m)
    };
    paths::ensure_sandbox(&root)?;
    let away_minutes = std::fs::metadata(paths::session_file(&root))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|d| (d.as_secs() / 60) as i64)
        .unwrap_or(0);

    eprintln!("{}", tr("Lecture du projet…", "Reading the project…"));
    // Une session interrompue brutalement a pu laisser ses commentaires dans le code.
    let cleaned = annotate::strip_project(&root);
    if cleaned > 0 {
        eprintln!(
            "{} {cleaned} {}",
            tr("Commentaires d'une session précédente retirés de", "Comments from a previous session removed from"),
            tr("fichier(s).", "file(s)."),
        );
    }
    let facts = project::gather(&root);
    let tracker = watch::Tracker::new(&root);

    let (tx, rx) = mpsc::unbounded_channel();
    let fs_tx = tx.clone();
    let watcher = watch::start(&root, move |paths| {
        let _ = fs_tx.send(AppEvent::Fs(paths));
    });

    let mut session = session;
    if opts.no_auto {
        session.auto_check = false;
    }
    let mut app = App::new(
        Options {
            root: root.clone(),
            backend,
            // L'option l'emporte sur `vibelearn login` et `vibelearn config` ; un tuteur doit répondre vite (effort moyen par défaut).
            model,
            effort: Some(config.effort(opts.effort.clone())),
            mock: opts.mock,
            auto_delay: Duration::from_secs(config.auto_delay(opts.auto_delay)),
        },
        session,
        profile,
        facts,
        tracker,
        tx,
        resuming,
        away_minutes,
    );
    if let Some(updated) = update::announce() {
        app.notice(updated);
    }
    // A newer release is fetched while the session runs, for the next launch.
    if config.auto_update() && !opts.mock {
        update::spawn_auto();
    }
    if resuming {
        app.notice(tr("Session reprise là où tu l'avais laissée.", "Session resumed where you left off."));
    }
    if let Some(topic) = archived {
        app.notice(format!(
            "{} « {topic} » {}",
            tr("La session précédente", "The previous session"),
            tr("a été rangée dans `.vibelearn/history/`.", "was moved to `.vibelearn/history/`."),
        ));
    }
    if let Err(e) = &watcher {
        app.notice(format!(
            "{} ({e}). {}",
            tr("La surveillance des fichiers n'a pas pu démarrer", "File watching could not start"),
            tr("Clique sur « Vérifier » dans le panneau vibelearn pour avoir un retour.", "Click \"Check\" in the vibelearn panel to get feedback."),
        ));
    }
    app.start_engine();

    // Par défaut tout passe par des commentaires dans le code ; la fenêtre
    // n'existe que si le binaire a été compilé avec elle.
    let outcome = if opts.window {
        #[cfg(feature = "window")]
        {
            let autoplay = opts.mock && std::env::var_os("VIBELEARN_AUTOPLAY").is_some();
            gui::run(app, rx, autoplay)?
        }
        #[cfg(not(feature = "window"))]
        {
            drop((app, rx));
            bail!(
                "{}",
                tr(
                    "ce binaire n'inclut pas la fenêtre : réinstalle avec `cargo install --path . --features window`",
                    "this binary was built without the window: reinstall with `cargo install --path . --features window`",
                )
            );
        }
    } else {
        let editor = opts.editor.clone().or_else(|| std::env::var("VIBELEARN_EDITOR").ok());
        front::run(app, rx, vscode::Editor::new(editor.as_deref())).await?
    };
    drop(watcher);

    if outcome.completed {
        match &outcome.notes_file {
            Some(file) => println!("{} {file}", tr("Session terminée. Ta fiche de révision :", "Session complete. Your revision sheet:")),
            None => println!("{}", tr("Session terminée.", "Session complete.")),
        }
    } else if outcome.mode.is_guided() && outcome.turns > 0 {
        println!("{}", tr("Session sauvegardée. Pour reprendre : vibelearn resume", "Session saved. To continue: vibelearn resume"));
    }
    Ok(())
}

async fn doctor(explicit: Option<&Path>) -> Result<()> {
    println!("vibelearn {}", env!("CARGO_PKG_VERSION"));
    let login = Login::load();
    println!("  [..] {} : {}", tr("connexion", "connection"), login::Choice::of(&login).label());
    if !login.provider.uses_claude() || login.key.is_some() {
        println!(
            "       {}, {} {}, {} {}",
            login.url(),
            tr("clé", "key"),
            login.masked_key(),
            tr("modèle", "model"),
            login.model.as_deref().unwrap_or("-")
        );
        match login::check(login.provider, &login.url(), login.key.as_deref().unwrap_or("")).await {
            login::Check::Ok(_) => println!("  [ok] {}", tr("clé acceptée par le service", "key accepted by the service")),
            login::Check::Refused(why) => println!("  [!!] {} ({why}) → vibelearn login", tr("clé refusée", "key refused")),
            login::Check::Unknown(why) => println!("  [?]  {} ({why})", tr("clé impossible à vérifier", "could not check the key")),
        }
    }
    if !login.provider.uses_claude() {
        print_profile_line();
        return Ok(());
    }
    let bin = match claude_bin(explicit) {
        Ok(bin) => bin,
        Err(e) => {
            println!("  [!!] claude : {e}");
            return Ok(());
        }
    };
    let version = claude::version(&bin).unwrap_or_else(|| "?".into());
    println!("  [ok] claude : {} ({version})", bin.display());
    if login.provider == Provider::Claude {
        match claude::auth_status(&bin, false) {
            Ok(status) if status.logged_in => {
                println!("  [ok] {} ({})", tr("abonnement connecté", "subscription signed in"), status.method);
            }
            Ok(_) => println!(
                "  [!!] {}",
                tr("abonnement non connecté → lance : vibelearn login", "subscription not signed in → run: vibelearn login")
            ),
            Err(e) => println!("  [!!] {} : {e:#}", tr("état de connexion illisible", "could not read sign-in status")),
        }
    }
    print_profile_line();
    Ok(())
}

fn print_profile_line() {
    let profile = Profile::load();
    if let Ok(path) = paths::profile_file() {
        println!("  [ok] {} : {} ({} concepts)", tr("profil", "profile"), path.display(), profile.concepts.len());
    }
}

fn show_profile() {
    let profile = Profile::load();
    let today = Utc::now().date_naive();
    if profile.concepts.is_empty() {
        println!("{}", tr("Profil vide : lance une première session avec vibelearn \"un sujet\".", "Empty profile: start a first session with vibelearn \"a topic\"."));
        return;
    }
    let due = profile.due(today);
    println!(
        "{}{}{} concepts, {} {}, {} {}, {} {}",
        tr("Profil d'apprenant", "Learner profile"),
        i18n::colon(),
        profile.concepts.len(),
        profile.sessions.len(),
        tr("séances", "sessions"),
        profile.streak(today),
        tr("jours d'affilée", "day streak"),
        due.len(),
        tr("à réviser aujourd'hui (vibelearn review)", "due today (vibelearn review)"),
    );
    println!();
    let mut rows: Vec<_> = profile.concepts.iter().collect();
    rows.sort_by(|a, b| b.1.level.cmp(&a.1.level).then(a.0.cmp(b.0)));
    for (id, rec) in rows {
        let level = rec.level as usize;
        let bar = format!("{}{}", "█".repeat(level), "░".repeat(4 - level.min(4)));
        let review = match rec.next_review {
            Some(d) if d <= today => tr("à réviser", "due").to_string(),
            Some(d) => format!("{} {d}", tr("révision le", "review on")),
            None => String::new(),
        };
        let claimed = if rec.source == profile::Source::SelfReported { tr(" (déclaré)", " (self-reported)") } else { "" };
        println!("  {bar} {:<34} {:<10}{claimed}  {review}   [{id}]", rec.label, profile::level_name(rec.level));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_project(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vibelearn-main-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn refactor_takes_a_free_target_and_the_usual_options() {
        let cli = Cli::try_parse_from(["vibelearn", "refactor", "la", "boucle", "de", "Game1", "--mock"]).unwrap();
        let Some(Cmd::Refactor { target, opts }) = cli.command else { panic!("refactor attendu") };
        assert_eq!(target.join(" "), "la boucle de Game1");
        assert!(opts.mock);
        let cli = Cli::try_parse_from(["vibelearn", "refactor"]).unwrap();
        assert!(matches!(cli.command, Some(Cmd::Refactor { target, .. }) if target.is_empty()));
    }

    #[test]
    fn monitor_needs_no_description() {
        let cli = Cli::try_parse_from(["vibelearn", "monitor"]).unwrap();
        assert!(matches!(cli.command, Some(Cmd::Monitor { about, .. }) if about.is_empty()));
        let cli = Cli::try_parse_from(["vibelearn", "monitor", "le", "jugement", "--auto-delay", "3"]).unwrap();
        let Some(Cmd::Monitor { about, opts }) = cli.command else { panic!("monitor attendu") };
        assert_eq!((about.join(" ").as_str(), opts.auto_delay), ("le jugement", Some(3)));
    }

    #[test]
    fn assist_needs_no_description_either() {
        let cli = Cli::try_parse_from(["vibelearn", "assist"]).unwrap();
        assert!(matches!(cli.command, Some(Cmd::Assist { about, .. }) if about.is_empty()));
        let cli = Cli::try_parse_from(["vibelearn", "assist", "le", "refactor", "de", "BeatPattern", "--mock"]).unwrap();
        let Some(Cmd::Assist { about, opts }) = cli.command else { panic!("assist attendu") };
        assert_eq!((about.join(" ").as_str(), opts.mock), ("le refactor de BeatPattern", true));
    }

    #[test]
    fn login_takes_an_optional_provider_url_and_model() {
        let cli = Cli::try_parse_from(["vibelearn", "login"]).unwrap();
        assert!(matches!(cli.command, Some(Cmd::Login { provider: None, .. })));
        let cli = Cli::try_parse_from(["vibelearn", "login", "openai", "--url", "http://localhost:11434/v1", "--model", "qwen3"]).unwrap();
        let Some(Cmd::Login { provider, url, model, .. }) = cli.command else { panic!("login attendu") };
        assert_eq!((provider.as_deref(), url.as_deref(), model.as_deref()), (Some("openai"), Some("http://localhost:11434/v1"), Some("qwen3")));
    }

    #[test]
    fn a_refactor_on_the_same_words_as_a_learning_session_starts_a_new_session() {
        let root = temp_project("modes");
        let learning = SessionState::new("Game1", Mode::Learn);
        let mut archived = None;
        let (s, resumed) = start_or_continue(&root, Some(learning.clone()), "game1", Mode::Learn, false, &mut archived).unwrap();
        assert!(resumed && s.mode == Mode::Learn && archived.is_none());
        let (s, resumed) = start_or_continue(&root, Some(learning), "Game1", Mode::Refactor, false, &mut archived).unwrap();
        assert!(!resumed && s.mode == Mode::Refactor);
        assert_eq!(archived.as_deref(), Some("Game1"), "la session d'apprentissage est rangée, pas perdue");
        let _ = std::fs::remove_dir_all(&root);
    }
}
