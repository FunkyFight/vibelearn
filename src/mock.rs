//! Moteur factice : déroule une session type (reconnaissance, checklist, plan,
//! consigne et commentaires dans le code, question, démo, évaluation) sans
//! appeler de modèle. Sert aux tests de bout en bout et à `vibelearn --mock`.

use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::claude::{AgentEvent, MockCmd};

type Emit = Arc<dyn Fn(AgentEvent) + Send + Sync>;

/// Ce que le faux assistant corrige, pour essayer le mode assistance hors ligne.
pub const MOCK_MARK: &str = "vibelearn-test";

/// Les lignes ajoutées qui contiennent `MOCK_MARK` une fois : (fichier, numéro
/// à partir de 1), lues dans les diffs unifiés de `<file_changes>`.
fn marked_lines(changes: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut file = String::new();
    let mut line = 0;
    for l in changes.lines() {
        if let Some(path) = l.strip_prefix("+++ b/") {
            file = path.trim().to_string();
        } else if let Some(hunk) = l.strip_prefix("@@ ") {
            // `@@ -a,b +c,d @@` : la première ligne ajoutée ou gardée porte le numéro c.
            line = hunk
                .split_whitespace()
                .find_map(|p| p.strip_prefix('+'))
                .and_then(|p| p.split(',').next()?.parse().ok())
                .unwrap_or(1);
        } else if l.starts_with('+') && !l.starts_with("+++") {
            if l.matches(MOCK_MARK).count() == 1 && !file.is_empty() {
                out.push((file.clone(), line));
            }
            line += 1;
        } else if l.starts_with(' ') {
            line += 1;
        }
    }
    out
}

struct Mock {
    cmds: mpsc::UnboundedReceiver<MockCmd>,
    emit: Emit,
    next_id: u64,
}

impl Mock {
    async fn say(&self, text: &str) {
        (self.emit)(AgentEvent::BlockStart { kind: "text".into(), name: None });
        let chars: Vec<char> = text.chars().collect();
        for chunk in chars.chunks(12) {
            (self.emit)(AgentEvent::Delta { text: chunk.iter().collect() });
            tokio::time::sleep(Duration::from_millis(6)).await;
        }
        (self.emit)(AgentEvent::Text { text: text.to_string() });
    }

    fn activity(&self, id: &str, name: &str, input: Value) {
        (self.emit)(AgentEvent::ToolUse { id: id.into(), name: name.into(), input });
        (self.emit)(AgentEvent::ToolDone { id: id.into(), is_error: false });
    }

    /// Appelle un outil du tuteur et attend la réponse de l'application.
    async fn call(&mut self, tool: &str, args: Value) -> Option<String> {
        self.next_id += 1;
        let id = self.next_id;
        // Comme le vrai modèle : l'appel est d'abord « en cours de rédaction ».
        (self.emit)(AgentEvent::BlockStart { kind: "tool_use".into(), name: Some(format!("mcp__tutor__{tool}")) });
        tokio::time::sleep(Duration::from_millis(30)).await;
        (self.emit)(AgentEvent::ToolCall { id, tool: tool.into(), args });
        loop {
            match self.cmds.recv().await? {
                MockCmd::ToolReply { id: rid, text } if rid == id => return Some(text),
                MockCmd::Shutdown => return None,
                MockCmd::Interrupt => {
                    (self.emit)(AgentEvent::Cancel { id });
                    return Some("{\"cancelled\":true}".into());
                }
                _ => {}
            }
        }
    }

    async fn permission(&mut self, tool: &str, input: Value) -> Option<(bool, Option<String>)> {
        self.next_id += 1;
        let id = self.next_id;
        (self.emit)(AgentEvent::Permission { id, tool: tool.into(), input, reason: None, tool_use_id: None });
        loop {
            match self.cmds.recv().await? {
                MockCmd::PermissionReply { id: rid, allow, message } if rid == id => return Some((allow, message)),
                MockCmd::Shutdown => return None,
                _ => {}
            }
        }
    }

    async fn next_user(&mut self) -> Option<String> {
        loop {
            match self.cmds.recv().await? {
                MockCmd::User(text) => return Some(text),
                MockCmd::Shutdown => return None,
                _ => {}
            }
        }
    }

    fn result(&self) {
        (self.emit)(AgentEvent::Result {
            is_error: false,
            duration_ms: 1200,
            cost_usd: 0.01,
            input_tokens: 1500,
            output_tokens: 300,
            errors: Vec::new(),
        });
    }

    /// Surveillance : des remarques sur la sauvegarde, sans exercice ni plan.
    async fn monitor_feedback(&mut self) -> Option<()> {
        self.call(
            "comment",
            json!({ "file": "src/main.rs", "anchor": "trait Cri {", "tone": "fix", "text": "Aucun type n'implémente `Cri`", "detail": "Le trait est déclaré, mais aucun `impl Cri for …` n'existe : rien ne peut encore appeler `cri()`." }),
        )
        .await?;
        self.call(
            "comment",
            json!({ "file": "src/main.rs", "anchor": "fn main() {}", "tone": "design", "text": "Un type par animal", "detail": "Si `main` connaît chaque animal, le dixième obligera à la modifier. Un type par animal qui implémente `Cri`, et `main` ne fait que les assembler." }),
        )
        .await?;
        self.say(
            "Structure proposée : **un type par animal**, qui implémente `Cri`.\n\n```\nstruct Chat;\nimpl Cri for Chat { fn cri(&self) -> String }\n```\n\nLe prochain animal = un type de plus, rien d'autre ne change.",
        )
        .await;
        Some(())
    }

    /// Assistance : pour essayer hors ligne, chaque `vibelearn-test` écrit et
    /// sauvegardé dans un fichier modifié devient `vibelearn-ok`.
    async fn assist_feedback(&mut self, changes: &str) -> Option<()> {
        let mut fixed = 0;
        for (file, line) in marked_lines(changes) {
            self.call(
                "assist_edit",
                json!({ "file": file, "line": line, "old": MOCK_MARK, "new": "vibelearn-ok", "reason": "essai du mode assistance" }),
            )
            .await?;
            fixed += 1;
        }
        if fixed == 0 {
            self.say(&format!("Rien à compléter. Pour essayer, écris `{MOCK_MARK}` dans ton code et sauvegarde.")).await;
        }
        Some(())
    }

    async fn first_turn(&mut self) -> Option<()> {
        self.say("Je regarde ton projet pour voir comment y implémenter ça.").await;
        self.activity("t1", "Read", json!({ "file_path": "Cargo.toml" }));
        self.activity("t2", "Grep", json!({ "pattern": "trait " }));
        self
            .call(
                "knowledge_checklist",
                json!({
                    "title": "Ce que tu connais déjà",
                    "intro": "J'ai pré-coché ce que ton code laisse deviner.",
                    "items": [
                        { "id": "rust.structs", "label": "Structs et impl", "why": "Base de tout le reste.", "evidence": "src/main.rs définit déjà des structs.", "assumed_known": true, "kind": "prerequisite" },
                        { "id": "rust.ownership", "label": "Ownership et emprunts", "why": "Indispensable pour partager des données.", "evidence": "Tu passes des &str partout.", "assumed_known": true, "kind": "prerequisite" },
                        { "id": "rust.traits", "label": "Traits", "why": "Le cœur de ce que tu veux apprendre.", "assumed_known": false, "kind": "core" },
                        { "id": "rust.generics", "label": "Génériques et bornes de traits", "why": "Pour écrire du code polymorphe.", "assumed_known": false, "kind": "core" },
                        { "id": "rust.dyn", "label": "Objets-traits (dyn)", "why": "Bonus : dispatch dynamique.", "assumed_known": false, "kind": "bonus" },
                    ],
                }),
            )
            .await?;
        // Pas de phase de leçons : le plan est l'implémentation, morceau par morceau.
        self.call(
            "set_plan",
            json!({
                "goal": "Un trait Cri défini et implémenté dans ton projet",
                "steps": [
                    { "id": "p1", "title": "Déclarer le trait Cri", "kind": "implementation" },
                    { "id": "p2", "title": "L'implémenter pour un type", "kind": "implementation" },
                ],
            }),
        )
        .await?;
        self.call("update_step", json!({ "id": "p1", "status": "active" })).await?;
        // La notion est donnée avec le morceau qui en a besoin, pièges compris,
        // dans la consigne écrite là où le morceau se code.
        self.call(
            "start_exercise",
            json!({
                "id": "ex1",
                "title": "Déclarer le trait",
                "goal": "Ton projet a besoin d'un comportement commun à plusieurs types. Comment l'exprimer sans dupliquer de code ?",
                "notes": "Un trait liste des signatures de méthodes : `fn nom(&self) -> Type;`, sans corps.\nPiège : le `;` après la signature ; le corps s'écrit dans `impl Trait for Type`.",
                "success_criteria": ["Un trait est déclaré", "Au moins un type l'implémente"],
                "anchor": "fn main() {",
                "here": "le trait se déclare au-dessus de main",
                "target_files": ["src/main.rs"],
                "check_command": "echo check-ok",
            }),
        )
        .await?;
        // Ce texte-là n'est pas montré : la consigne est déjà dans le code.
        self.say("À toi de jouer.").await;
        Some(())
    }

    async fn run(mut self) -> Option<()> {
        (self.emit)(AgentEvent::Ready);
        let mut first = true;
        let mut feedback = 0;
        let mut monitor = false;
        let mut assist = false;
        let mut lesson = false;
        loop {
            let text = self.next_user().await?;
            if first {
                first = false;
                (self.emit)(AgentEvent::Session {
                    session_id: "00000000-0000-4000-8000-000000000000".into(),
                    model: Some("mock".into()),
                });
                assist = text.contains("mode: assist");
                monitor = assist || text.contains("mode: monitor");
                if monitor {
                    self.activity("t1", "Read", json!({ "file_path": "Cargo.toml" }));
                    self.say("Je regarde tes sauvegardes.").await;
                } else {
                    self.first_turn().await?;
                }
            } else if lesson && text.contains("<learner_command name=\"skip\"") {
                lesson = false;
            } else if monitor && text.contains("<learner_command name=\"teach\"") {
                // Mini-leçon sur une remarque : un morceau, à la ligne de la remarque.
                let attr = |name: &str| {
                    let raw = text.split(&format!(" {name}=\"")).nth(1)?.split('"').next()?;
                    Some(raw.replace("&quot;", "\"").replace("&lt;", "<").replace("&amp;", "&"))
                };
                let mut args = json!({
                    "id": "lesson",
                    "title": "La notion derrière la remarque",
                    "goal": "Fais ce que dit la remarque, à cette ligne.",
                    "notes": "Ce qu'il faut savoir.\nPiège : le classique.",
                    "success_criteria": ["La remarque ne s'applique plus"],
                    "target_files": [attr("file").unwrap_or_else(|| "src/main.rs".into())],
                    "here": "écris ici",
                });
                if let Some(anchor) = attr("line_text") {
                    args["anchor"] = json!(anchor);
                }
                self.call("start_exercise", args).await?;
                lesson = true;
            } else if lesson && text.contains("<file_changes") {
                self.call("exercise_feedback", json!({ "id": "lesson", "verdict": "passed", "summary": "La notion est appliquée." })).await?;
                self.call("record_mastery", json!({ "items": [{ "concept_id": "mock.lesson", "label": "La notion de la remarque", "level": 3 }] })).await?;
                lesson = false;
            } else if assist && text.contains("<file_changes") {
                self.assist_feedback(&text).await?;
            } else if monitor && text.contains("<file_changes") {
                self.monitor_feedback().await?;
            } else if text.contains("<file_changes") {
                feedback += 1;
                if feedback == 1 {
                    self.call(
                        "exercise_feedback",
                        json!({ "id": "ex1", "verdict": "progress", "summary": "Le trait est déclaré, il manque une implémentation." }),
                    )
                    .await?;
                    self.call("update_step", json!({ "id": "p1", "status": "done" })).await?;
                    self.call("update_step", json!({ "id": "p2", "status": "active" })).await?;
                    self.call(
                        "comment",
                        json!({ "file": "src/main.rs", "anchor": "trait Cri {", "tone": "fix", "text": "Il manque un type qui implémente `Cri`", "detail": "Le trait est déclaré ; tant qu'aucun type ne l'implémente, il ne sert à rien." }),
                    )
                    .await?;
                    self.call(
                        "comment",
                        json!({ "file": "src/main.rs", "anchor": "fn main() {}", "tone": "design", "text": "Garde `main` vide de logique", "detail": "Chaque animal dans son type : le dixième ne touchera pas `main`." }),
                    )
                    .await?;
                } else {
                    self.call(
                        "exercise_feedback",
                        json!({ "id": "ex1", "verdict": "passed", "summary": "Trait déclaré et implémenté." }),
                    )
                    .await?;
                    self.call(
                        "record_mastery",
                        json!({ "items": [{ "concept_id": "rust.traits", "label": "Traits", "level": 3, "evidence": "implémenté dans src/main.rs" }] }),
                    )
                    .await?;
                    self.call("update_step", json!({ "id": "p2", "status": "done" })).await?;
                }
            } else if text.contains("<pseudo_code") {
                // `//// …` : du pseudo-code seul, affiché à côté de la demande.
                self.say("[add] forceSaut : float, réglable\ndt <- secondes écoulées depuis la dernière image, depuis gameTime\nsaute <- IsKeyDown(clavier, Keys.Space)\nsi saute et auSol :\n  velocity.Y <- -forceSaut\nvelocity.Y <- velocity.Y + gravite * dt\nposition <- position + velocity * dt").await;
            } else if text.contains("<next_step") {
                // `//n …` : une seule étape, en texte, qui s'affiche au bout de la ligne.
                self.say("Détecte d'abord le contact avec un mur, avant de toucher à la vitesse.\nTant que le joueur ne sait pas qu'il touche un mur, le saut n'a rien sur quoi s'appuyer : affiche-le pour le vérifier.").await;
            } else if text.contains("pose-moi") {
                // Les questions sont rares : seulement quand un choix de l'apprenant est nécessaire.
                self.call(
                    "ask",
                    json!({
                        "kind": "choice",
                        "question": "Deux façons de faire tiennent la route ici. Laquelle préfères-tu pour ton projet ?",
                        "options": ["Une méthode par défaut dans le trait", "Une implémentation par type"],
                    }),
                )
                .await?;
                self.say("Entendu, on part là-dessus.").await;
            } else if text.contains("démo") {
                self.call(
                    "show_demo",
                    json!({
                        "title": "Un trait minimal",
                        "files": [".vibelearn/demos/01-trait/main.rs"],
                        "run_command": "echo miaou",
                        "observe": "Regarde quelle méthode est appelée.",
                    }),
                )
                .await?;
                self.say("Une démo est prête si tu veux la voir tourner ; sinon continue dans ton code.").await;
            } else if text.contains("name=\"hint\"") {
                self.say("Indice : quel mot-clé sert à *déclarer* un comportement partagé ?").await;
            } else if text.contains("name=\"finish\"") {
                self.call(
                    "finish_session",
                    json!({
                        "summary": "Tu sais maintenant déclarer et implémenter un trait.",
                        "notes_markdown": "# Traits\n\n- `trait Nom { fn m(&self); }`\n- `impl Nom for Type { ... }`\n",
                        "next_steps": ["Génériques avec bornes", "Objets-traits"],
                    }),
                )
                .await?;
                self.say("Session terminée, bravo !").await;
            } else if text.contains("permission") {
                let (allow, message) = self.permission("Bash", json!({ "command": "cargo test --quiet" })).await?;
                let reply = if allow {
                    "Commande autorisée.".to_string()
                } else {
                    format!("Commande refusée : {}", message.unwrap_or_default())
                };
                self.say(&reply).await;
            } else {
                let short: String = text.chars().take(120).collect();
                self.say(&format!("(mock) Tu as dit : {short}")).await;
            }
            self.result();
        }
    }
}

pub async fn run(cmds: mpsc::UnboundedReceiver<MockCmd>, emit: Emit) {
    let mock = Mock { cmds, emit: emit.clone(), next_id: 0 };
    let _ = mock.run().await;
    emit(AgentEvent::Exited { code: Some(0), stderr: String::new() });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_test_mark_in_the_added_lines_of_a_diff() {
        let changes = "## src/a.cs (modified, +2 -1)\n```diff\n--- a/src/a.cs\n+++ b/src/a.cs\n@@ -3,3 +3,4 @@\n class A\n-    int x;\n+    int y; // vibelearn-test\n+    int z;\n // vibelearn-test\n```\n";
        assert_eq!(marked_lines(changes), vec![("src/a.cs".to_string(), 4)]);
    }
}
