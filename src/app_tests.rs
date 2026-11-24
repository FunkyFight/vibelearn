//! Tests du contrôleur de session, joués contre le tuteur factice : une session
//! complète, de la checklist à la fiche de révision, sans fenêtre ni modèle.

use super::*;
use crate::project;
use crate::session::Mode;
use serde_json::json;

fn temp_project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vibelearn-app-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() {\n    println!(\"Hello\");\n}\n").unwrap();
    dir
}

fn mock_app(root: &Path) -> (App, mpsc::UnboundedReceiver<AppEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let app = App::new(
        Options {
            root: root.to_path_buf(),
            backend: None,
            model: None,
            effort: None,
            mock: true,
            auto_delay: Duration::from_secs(60),
        },
        SessionState::new("les traits", Mode::Learn),
        Profile::default(),
        project::gather(root),
        Tracker::new(root),
        tx,
        false,
        0,
    );
    (app, rx)
}

/// Fait tourner le contrôleur jusqu'à ce que `done` soit vrai.
async fn pump(app: &mut App, rx: &mut mpsc::UnboundedReceiver<AppEvent>, what: &str, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done(app) {
        assert!(Instant::now() < deadline, "attente dépassée : {what} (phase {:?})", app.phase);
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Some(event)) => app.on_event(event),
            Ok(None) => panic!("canal fermé pendant : {what}"),
            Err(_) => app.on_tick(),
        }
    }
}

fn idle(app: &App) -> bool {
    app.phase == Phase::Idle && app.modal.is_none()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_scripted_session_runs_from_checklist_to_revision_sheet() {
    let root = temp_project("session");
    let (mut app, mut rx) = mock_app(&root);
    app.start_engine();

    // Le tuteur explore puis ouvre la checklist : il attend, il ne « travaille » plus.
    pump(&mut app, &mut rx, "checklist", |a| matches!(a.modal, Some(Modal::Checklist(_)))).await;
    assert!(!app.working(), "pendant une question, l'interface ne doit pas dire que le tuteur travaille");
    let Some(Modal::Checklist(c)) = &mut app.modal else { unreachable!() };
    assert_eq!(c.count(Claim::Known), 2, "les notions déduites du code sont pré-cochées");
    c.items[0].state = Claim::Unknown;
    c.items[3].state = Claim::Unsure;
    app.resolve_modal();

    // Après la checklist, on passe directement à l'implémentation : un plan fait
    // de morceaux à coder, et aucune question entre les deux.
    pump(&mut app, &mut rx, "premier morceau", |a| idle(a) && a.session.exercise.is_some()).await;
    let plan = app.session.plan.as_ref().expect("plan");
    assert_eq!(plan.steps.len(), 2);
    assert!(plan.steps.iter().all(|s| s.kind == "implementation"));
    assert_eq!(app.session.concepts["rust.structs"].claim, Claim::Unknown);
    assert!(
        !app.session.transcript.iter().any(|e| matches!(e, Entry::Widget(t) if t.contains('?'))),
        "aucune question ne doit précéder le premier morceau"
    );
    // L'étape montre la ligne où écrire ; le code de l'apprenant n'est pas touché.
    let file = root.join("src").join("main.rs");
    let brief = app.threads.brief().expect("endroit où écrire");
    assert_eq!(brief.title.as_deref(), Some("Morceau 1/2 : Déclarer le trait"));
    assert_eq!((brief.file.clone(), brief.line), (file.clone(), 0));
    let notes = app.session.exercise.as_ref().and_then(|e| e.notes.as_deref()).map(notes_markdown).unwrap_or_default();
    assert!(notes.contains("- Piège : le `;` après la signature"), "{notes}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "fn main() {\n    println!(\"Hello\");\n}\n");
    std::fs::write(&file, "trait Cri {\n    fn cri(&self) -> String;\n}\n\nfn main() {}\n").unwrap();
    app.on_event(AppEvent::Fs(vec![file.clone()]));
    pump(&mut app, &mut rx, "changements détectés", |a| !a.pending.is_empty()).await;
    assert!(app.countdown().is_some(), "l'envoi automatique est armé pendant un exercice");

    // Envoi : la commande de vérification tourne, puis le tuteur répond.
    app.send_now();
    pump(&mut app, &mut rx, "premier retour", |a| {
        idle(a) && a.session.exercise.as_ref().is_some_and(|e| e.verdict.as_deref() == Some("progress"))
    })
    .await;
    assert!(app.pending.is_empty(), "ce qui est envoyé n'est plus en attente");
    assert!(app.session.transcript.iter().any(|e| matches!(e, Entry::Changes(t) if t.contains("echo check-ok"))));
    // Le retour est une annotation sur la ligne concernée.
    let feedback = app.threads.all().iter().find(|t| t.kind == crate::threads::Kind::Feedback).expect("retour");
    assert_eq!((feedback.line, feedback.anchor.as_deref()), (0, Some("trait Cri {")));
    assert_eq!(feedback.tone.as_deref(), Some("fix"));
    assert!(
        feedback.notes[0].body.starts_with("Il manque un type qui implémente `Cri`\n\nLe trait est déclaré"),
        "titre puis explication : {}",
        feedback.notes[0].body
    );
    // Une remarque ⚠ ou ⊞ sans explication est refusée.
    app.on_tool_call(500, "comment", json!({ "file": "src/main.rs", "line": 1, "tone": "fix", "text": "Vague" }));
    assert!(!app.threads.all().iter().any(|t| t.notes.iter().any(|n| n.body == "Vague")));
    assert!(
        app.threads.all().iter().any(|t| t.kind == crate::threads::Kind::Feedback && t.tone.as_deref() == Some("design")),
        "le conseil de conception reste à côté de la correction, dans le même tour"
    );
    assert_eq!(
        app.last_spot.as_ref().map(|(_, text, _)| text.as_str()),
        Some("fn main() {}"),
        "la dernière ligne écrite du premier bloc modifié"
    );

    app.command("hint");
    pump(&mut app, &mut rx, "indice", |a| idle(a) && a.session.hints_used == 1).await;
    assert_eq!(app.session.exercise.as_ref().unwrap().hint_level, 1);

    // Seconde modification : l'exercice est réussi.
    let solved = "trait Cri {\n    fn cri(&self) -> String;\n}\nstruct Chat;\nimpl Cri for Chat {\n    fn cri(&self) -> String { \"miaou\".into() }\n}\nfn main() {}\n";
    std::fs::write(&file, solved).unwrap();
    app.on_event(AppEvent::Fs(vec![file.clone()]));
    pump(&mut app, &mut rx, "seconds changements", |a| !a.pending.is_empty()).await;
    app.send_now();
    pump(&mut app, &mut rx, "réussite", |a| idle(a) && a.session.exercise.is_none()).await;
    assert_eq!(app.session.exercises_passed, 1);
    // L'étape réussie est fêtée dans le panneau, et tout ce qui la concernait disparaît.
    assert_eq!(app.celebrate.as_ref().map(|c| c.summary.as_str()), Some("Trait déclaré et implémenté."));
    assert!(app.threads.all().is_empty(), "plus d'annotation ni de question de l'étape réussie");
    assert_eq!(app.session.concepts["rust.traits"].level, Some(3));

    // Une question reste possible quand un choix de l'apprenant est nécessaire.
    app.submit("pose-moi une question".into());
    pump(&mut app, &mut rx, "question", |a| matches!(a.modal, Some(Modal::Ask(_)))).await;
    let Some(Modal::Ask(a)) = &mut app.modal else { unreachable!() };
    assert!(!a.can_submit());
    a.choice = Some(1);
    app.resolve_modal();
    pump(&mut app, &mut rx, "après la question", idle).await;

    // Une démo est un supplément facultatif, annoncé dans le panneau.
    app.submit("montre une démo".into());
    pump(&mut app, &mut rx, "démo", |a| idle(a) && a.session.demo.is_some()).await;

    // Une commande du tuteur demande l'accord de l'apprenant ; il refuse.
    app.submit("demande une permission".into());
    pump(&mut app, &mut rx, "permission", |a| matches!(a.modal, Some(Modal::Permission(_)))).await;
    let Some(Modal::Permission(p)) = &mut app.modal else { unreachable!() };
    assert!(p.detail.contains("cargo test"));
    p.decision = Some(Decision::Deny);
    app.resolve_modal();
    pump(&mut app, &mut rx, "après refus", idle).await;
    assert!(app.session.transcript.iter().any(|e| matches!(e, Entry::Activity(t) if t.contains("Bash"))));

    // Fin de session : la fiche de révision est écrite dans le bac à sable.
    app.command("finish");
    pump(&mut app, &mut rx, "fin", |a| idle(a) && a.session.completed).await;
    let notes = app.session.notes_file.clone().expect("fiche de révision");
    assert!(root.join(&notes).is_file());
    assert!(paths::in_sandbox(&root, Path::new(&notes)));

    app.shutdown();
    assert!(paths::session_file(&root).is_file(), "la session est sauvegardée pour une reprise");
    assert!(app.threads.all().is_empty(), "plus rien à montrer une fois la session arrêtée");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_second_question_is_refused_while_one_is_open() {
    let root = temp_project("one-at-a-time");
    let (mut app, _rx) = mock_app(&root);
    let question = json!({ "kind": "free", "question": "Pourquoi ?" });
    app.on_tool_call(1, "ask", question.clone());
    app.on_tool_call(2, "ask", question);
    assert!(matches!(app.modal, Some(Modal::Ask(_))));
    assert!(app.modal_queue.is_empty(), "une seule question à la fois");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn the_tutor_writes_freely_in_its_sandbox_but_must_ask_elsewhere() {
    let root = temp_project("permissions");
    let (mut app, _rx) = mock_app(&root);
    let sandbox_file = root.join(".vibelearn").join("demos").join("a.rs");
    app.on_permission(1, "Write".into(), json!({ "file_path": sandbox_file, "content": "x" }), None, None);
    assert!(app.modal.is_none(), "écrire dans .vibelearn/ ne demande rien");

    let project_file = root.join("src").join("main.rs");
    app.on_permission(2, "Write".into(), json!({ "file_path": project_file, "content": "fn main() {}" }), None, None);
    let Some(Modal::Permission(p)) = &app.modal else { panic!("écrire dans le projet doit demander l'accord") };
    assert!(p.write_path.is_some());
    assert!(p.headline.contains("src/main.rs"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn the_plan_is_made_of_implementation_pieces_only() {
    let root = temp_project("plan");
    let (mut app, _rx) = mock_app(&root);

    // Une phase de leçons ou un « défi final » : refusé, rien n'est affiché.
    for (id, steps) in [
        (1, json!([{ "id": "a", "title": "Démo", "kind": "demo" }, { "id": "b", "title": "Coder", "kind": "implementation" }])),
        (2, json!([{ "id": "a", "title": "Coder", "kind": "implementation" }, { "id": "b", "title": "Défi final", "kind": "capstone" }])),
    ] {
        app.on_tool_call(id, "set_plan", json!({ "goal": "G", "steps": steps }));
        assert!(app.session.plan.is_none());
    }

    // Le plan accepté est l'implémentation elle-même, morceau par morceau.
    app.on_tool_call(
        3,
        "set_plan",
        json!({ "goal": "G", "steps": [
            { "id": "a", "title": "Dessiner le carré", "kind": "implementation" },
            { "id": "b", "title": "Le déplacer avec Q et D", "kind": "implementation" },
        ]}),
    );
    assert_eq!(app.session.plan.as_ref().expect("plan accepté").steps.len(), 2);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread")]
async fn in_monitor_mode_every_save_is_sent_without_an_exercise() {
    for (mode, sent) in [(Mode::Monitor, true), (Mode::Learn, false)] {
        let root = temp_project(&format!("monitor-{mode:?}"));
        let (mut app, _rx) = mock_app(&root);
        app.session = SessionState::new("le jugement des appuis", mode);
        app.phase = Phase::Idle;
        let file = root.join("src").join("main.rs");
        std::fs::write(&file, "fn main() {\n    println!(\"Hi\");\n}\n").unwrap();
        app.on_event(AppEvent::Fs(vec![file]));
        // Les changements sont comptés après une courte accalmie.
        tokio::time::sleep(Duration::from_millis(300)).await;
        app.on_tick();
        assert!(!app.pending.is_empty(), "{mode:?} : changement vu");
        assert!(app.session.exercise.is_none());
        assert_eq!(app.countdown().is_some(), sent, "{mode:?} : envoi automatique armé ?");
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn in_assist_mode_an_obvious_fix_is_written_and_not_reported_as_the_developers_work() {
    let root = temp_project("assist");
    let (mut app, mut rx) = mock_app(&root);
    app.session = SessionState::new("le refactor", Mode::Assist);
    app.start_engine();
    pump(&mut app, &mut rx, "démarrage", idle).await;

    // Sans VS Code qui affiche la session, la correction est écrite dans le fichier.
    let file = root.join("src").join("main.rs");
    std::fs::write(&file, "fn main() {\n    println!(\"Hello\"); // vibelearn-test\n}\n").unwrap();
    app.on_event(AppEvent::Fs(vec![file.clone()]));
    app.command("check");
    pump(&mut app, &mut rx, "correction", |a| idle(a) && !a.assist_edits.is_empty()).await;

    let edit = &app.assist_edits[0];
    assert_eq!((edit.status, edit.line), (EditStatus::Applied, 1));
    assert!(std::fs::read_to_string(&file).unwrap().contains("// vibelearn-ok"));
    // La correction ne repart pas vers vibelearn comme un changement du développeur.
    app.on_event(AppEvent::Fs(vec![file.clone()]));
    tokio::time::sleep(Duration::from_millis(300)).await;
    app.on_tick();
    assert!(app.pending.is_empty(), "{:?}", app.pending.iter().map(|c| &c.diff).collect::<Vec<_>>());

    // Annulée par le développeur : vibelearn l'apprendra avec le prochain message.
    let id = app.assist_edits[0].id;
    app.edit_undone(id, false);
    assert_eq!(app.assist_edits[0].status, EditStatus::Undone);
    assert!(app.attachments.iter().any(|a| a.contains("<assist_undone")));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread")]
async fn with_vs_code_the_fix_waits_for_the_editor_and_exists_in_assist_and_learn_only() {
    let root = temp_project("assist-ui");
    let (mut app, _rx) = mock_app(&root);
    let args = json!({ "file": "src/main.rs", "line": 2, "old": "Hello", "new": "Bonjour", "reason": "test" });
    for mode in [Mode::Refactor, Mode::Monitor, Mode::Review] {
        app.session = SessionState::new("x", mode);
        app.on_tool_call(1, "assist_edit", args.clone());
        assert!(app.assist_edits.is_empty(), "refusé en {mode:?}");
    }
    app.session = SessionState::new("x", Mode::Learn);
    app.ui_live = true;
    app.on_tool_call(9, "assist_edit", args.clone());
    assert_eq!(app.assist_edits.len(), 1, "accepté en apprentissage");
    app.assist_edits.clear();

    app.session = SessionState::new("x", Mode::Assist);
    app.ui_live = true;
    app.on_tool_call(2, "assist_edit", json!({ "file": "src/main.rs", "line": 1, "old": "Nulle part", "new": "x", "reason": "test" }));
    assert!(app.assist_edits.is_empty(), "fragment absent : refusé");
    app.on_tool_call(3, "assist_edit", args);
    assert_eq!(app.assist_edits[0].status, EditStatus::Pending);
    assert!(std::fs::read_to_string(root.join("src/main.rs")).unwrap().contains("Hello"), "rien d'écrit : c'est l'éditeur qui applique");
    app.edit_result(app.assist_edits[0].id, true, Some(1), None);
    assert_eq!(app.assist_edits[0].status, EditStatus::Applied);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_short_lesson_on_a_remark_keeps_the_assistance_away_from_its_piece() {
    let root = temp_project("lesson");
    let (mut app, mut rx) = mock_app(&root);
    app.session = SessionState::new("le tri", Mode::Assist);
    app.start_engine();
    pump(&mut app, &mut rx, "démarrage", idle).await;

    // « Apprends-moi » sur une remarque de la ligne 2.
    app.lesson_origin = Some("Il faut IComparable<Verdict>.".into());
    let message = prompt::teach("src/main.rs", 2, Some("println!(\"Hello\");"), "fix", "Il faut IComparable<Verdict>.");
    app.ask_from_code("Apprends-moi".into(), message);
    pump(&mut app, &mut rx, "étape de la leçon", |a| idle(a) && a.session.exercise.is_some()).await;
    let ex = app.session.exercise.as_ref().unwrap();
    assert_eq!(ex.origin.as_deref(), Some("Il faut IComparable<Verdict>."), "la remarque d'origine reste affichée");
    assert_eq!(app.threads.brief().map(|b| b.line), Some(1), "la ligne de la remarque est celle où écrire");

    // Pendant la leçon, l'assistance continue, sauf sur la ligne où il écrit le morceau.
    app.on_tool_call(99, "assist_edit", json!({ "file": "src/main.rs", "line": 2, "old": "Hello", "new": "Bonjour", "reason": "x" }));
    assert!(app.assist_edits.is_empty(), "la ligne de la leçon reste à lui");
    app.on_tool_call(98, "assist_edit", json!({ "file": "src/main.rs", "line": 1, "old": "fn main()", "new": "pub fn main()", "reason": "x" }));
    assert_eq!(app.assist_edits.len(), 1, "ailleurs, l'assistance continue");

    // Sa sauvegarde réussit la leçon : retour à la surveillance.
    let file = root.join("src").join("main.rs");
    std::fs::write(&file, "fn main() {\n    println!(\"Hello\");\n    let x = 1;\n}\n").unwrap();
    app.on_event(AppEvent::Fs(vec![file]));
    app.command("check");
    pump(&mut app, &mut rx, "leçon réussie", |a| idle(a) && a.session.exercise.is_none()).await;
    assert!(app.celebrate.is_some());
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_short_lesson_goes_straight_back_to_watching() {
    let root = temp_project("lesson-stop");
    let (mut app, _rx) = mock_app(&root);
    app.session = SessionState::new("x", Mode::Monitor);
    app.phase = Phase::Idle;
    app.on_tool_call(1, "start_exercise", json!({ "id": "l", "title": "T", "goal": "G", "success_criteria": ["C"], "target_files": ["src/main.rs"] }));
    assert!(app.session.exercise.is_some());
    app.command("skip");
    assert!(app.session.exercise.is_none() && app.threads.brief().is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dismissed_remark_disappears_and_is_remembered_for_the_next_sessions() {
    let root = temp_project("forget");
    let (mut app, _rx) = mock_app(&root);
    app.session = SessionState::new("x", Mode::Assist);
    let place = Place::At { anchor: "fn main() {".into(), near: None };
    let posted = app.threads.post(Path::new("src/main.rs"), &place, Kind::Feedback, Some("design"), None, "Passe par InputState.").unwrap();
    app.forget_remark(posted.id);
    assert_eq!(app.threads.of_kind(Kind::Feedback).count(), 0);
    assert!(app.attachments.iter().any(|a| a.contains("<remark_dismissed") && a.contains("Passe par InputState.")));
    assert_eq!(crate::session::dismissed(&root), vec!["Passe par InputState.".to_string()]);
    let kickoff = prompt::with_dismissed("<vibelearn_kickoff>\n</vibelearn_kickoff>".into(), &crate::session::dismissed(&root));
    assert!(kickoff.contains("never make these kinds of remarks") && kickoff.contains("- Passe par InputState.") && kickoff.ends_with("</vibelearn_kickoff>"));
    let _ = std::fs::remove_dir_all(&root);
}
