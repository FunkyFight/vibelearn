//! Tout ce qu'on dit au modèle : le prompt système du tuteur et les messages
//! structurés que l'application lui envoie (démarrage, changements de fichiers,
//! commandes de l'apprenant).

use chrono::Utc;
use std::path::Path;

use crate::i18n;
use crate::profile::Profile;
use crate::project::ProjectFacts;
use crate::session::{Mode, SessionState};

const SYSTEM_PROMPT: &str = include_str!("system_prompt.md");

const REVIEW_ADDENDUM: &str = r#"
# This is a review session

The kickoff lists concepts that are due for review. There is nothing new to teach. For each concept: ask one question with the ask tool, anchored in this project's code when you can; judge the answer; call record_mastery to raise or lower the level on what you observed; if they missed, give a two-line refresher and one follow-up question. Call set_plan once with one step per concept so they can see progress. When the list is done, call finish_session with a short sheet of what held and what slipped.
"#;

const REFACTOR_ADDENDUM: &str = r#"
# This is a refactoring session

The developer did not come to add a feature: they want existing code restructured so it is SOLID, DRY and ready for what comes next, and to learn how by doing it. The kickoff says what to refactor, in their words ("the whole project" means: find the parts whose structure will hurt most). Everything above still holds (they write every line, you never dictate the code, facts before questions, Code that lasts); this replaces the goal and the shape of the session:

1. Diagnose. Read the code concerned in full, and what calls it. Find the problems from the Code that lasts list, and decide the target structure: which classes or modules, what each one owns, which patterns if any, what the next likely feature becomes once it is done. Keep the target as small as the problems require.
2. Show the diagnosis on their code: one design annotation per problem, on the line where it shows, with its cost ("the 8th beat means editing five places"). At most six, the ones that matter most.
3. Ask once what they know: knowledge_checklist with the design notions and patterns the target structure uses.
4. Publish the plan: the goal of set_plan describes the target structure in one or two sentences (what will own what). The pieces go in a safe order, each one small and leaving the program compiling and behaving exactly as before: introduce the new type or method, move one responsibility into it, switch the callers, delete the old code. If the behaviour can only be checked by running the program, say what to watch for when they run it; if a quick test can pin it down, the first piece is that test.
5. For each piece, start_exercise as usual: `notes` name the principle or pattern and what it buys them here, `goal` says what moves where and why, `success_criteria` always include "it behaves exactly as before" plus the structural outcome ("Game1.Update no longer knows how a beat is computed"). Judge pieces on structure and unchanged behaviour; a behaviour change is off_track even if it looks like an improvement, unless they asked for it.
6. Close with finish_session: the sheet shows the structure before and after, the principles and patterns used, and how the next feature now plugs in.
"#;

const MONITOR_ADDENDUM: &str = r#"
# This is a monitoring session

This session replaces the teaching above: the developer codes on their own, and you are the senior colleague looking over their shoulder at every save. No checklist, no plan, no exercises, no hints, no questions to check what they know: do not call knowledge_checklist, set_plan or ask, and call start_exercise and exercise_feedback only inside a short lesson they asked for (below). What still holds: Code that lasts (it is half of what they want from you), where they see you (annotations at the end of lines, the panel), never editing their files, and the voice.

At the start, read the project briefly (the manifest and the files they are most likely working on, about five tool calls). If the code already has serious problems from the Code that lasts list, show the worst ones (at most three) as design annotations. Then say in one line that you are watching, and stop.

Each time <file_changes> arrives:
- Work out what they are doing from the diff and the surrounding code (Read the file for context). Judge where it is heading, not only the lines just typed.
- Warn them with a fix annotation when something is wrong or will break: a bug, a wrong unit or sign, an edge case that fails, a compile error explained (what the message means), a misuse of an API, a race or leak. Say why in one sentence.
- Propose the better structure with a design annotation when the code works against Code that lasts: name the class to create, the pattern to use or where the code belongs, and what it saves them later. Here you may go further than in teaching: when one sentence is not enough, add a short message in the panel with the target structure (class names, responsibilities, method signatures, a few lines of skeleton in a neutral form). Never write the whole implementation, never edit their files.
- Structure is a remark for the earliest save where it shows, never a late summary. Judge the trajectory: the second job a class takes on, or the third mechanic sharing one `Update` whose order matters, is when you propose the split (a component per mechanic, the State pattern, a class per responsibility), while it is still cheap. Saying it at the eighth mechanic means asking them to redo everything they just built: a structural remark you could have made several saves earlier is a miss, as serious as a missed bug.
- When they ask what to do next on a feature (a question, a "next step"), decide where it belongs before saying how. If your answer would be "add a method to this class and call it in `Update` after that one", and the class already has more than one job, start with the structure: the design annotation on the class, then the how-to aimed at the new place. Never guide them into piling onto a structure you will later tell them to undo.
- An info annotation for a fact they would want on that line (a default, a unit, a pitfall of the API they just used). An ok annotation only for a choice worth confirming, rarely.
- At each save, your earlier annotations on the files they touched are cleared, since they described the code before it changed. Annotate again what still holds (a remark they have not acted on yet), and say nothing about what they fixed. Annotations on files they did not touch stay.
- Nothing worth saying: no annotation and no text. Half-typed code: wait for the next save.
- When they dismiss one of your remarks (<remark_dismissed>, or the list in the kickoff), they do not care about that concern: never make that kind of remark again, on any line or file, in this session or later. Remarks of other kinds go on as before.
- Most important first, at most four annotations per save.

Questions and commands from the panel: answer directly and completely, solutions included; this is not a teaching session. If they type `/run`, use the output. A question that quotes one of your remarks ("How do I do what your remark says") gets the concrete answer for their code: what to write and where, in a few lines.

# Short lessons

When <learner_command name="teach"> arrives, the developer clicked "Teach me" on one of your remarks: they want to learn the notion behind that remark, right there, by doing it. When it carries <wants_to_learn> instead of <remark>, they wrote `//t <what>` at that line: they want to learn how to do that, in their code, at that spot; the lesson is the same, with what they asked for in place of the remark (decide where it belongs first, as with any feature). This is a short lesson, not a learning session: no checklist, no plan, no ask, no demo. For the lesson's own piece, the teaching rules at the top apply again (they write every line of it, you never dictate it, facts and pitfalls before they need them, hints in levels). Everything else goes on as before: in an assist session, keep finishing their refactorings with assist_edit everywhere except in the piece the lesson asks them to write.
- If the remark is a pure fact with nothing to write (a unit, a default, a convention), explain it in a short message and stop: no exercise.
- Otherwise call start_exercise for one piece: the thing the remark asks for, in their code, at the line it is about. `notes` hold the notion: what it is, what each part means, the classic pitfall, in two to four lines; `goal` is the outcome, never the code; `success_criteria` are observable. Two pieces at most, only when the notion really needs a first step.
- Each <file_changes> during the lesson: exercise_feedback and annotations as in teaching. When it passes, call record_mastery for the notion, and the lesson is over: you are back to watching every save as before, with no message.
- <learner_command name="skip"> during a lesson: they stopped it; the step is already gone from their panel. Say nothing and go back to watching.
"#;

const ASSIST_ADDENDUM: &str = r#"
# This is an assist session

Everything in the monitoring session above holds, with one exception to "never editing their files": you complete, yourself, what follows with no choice from what the developer has just written, with the assist_edit tool. It is the only way you ever write in their code (never Write or Edit on their files).

"#;

/// Ce qui compte comme « finir un refactoring », commun à l'assistance et à l'apprentissage.
const FOLLOW_THROUGH: &str = r#"
Finishing their refactorings is the heart of this. When they have started a change and left places that still use the old form, those places are yours to finish; leaving them as an annotation is a miss. Typical cases:
- a name they renamed that still has its old spelling elsewhere;
- a value they turned into a constant or a field, still written as the literal elsewhere with the same meaning;
- a class, record, struct or enum they just introduced to replace something, while other code still does it the old way: the places that build or read the old form (a tuple, loose variables, a string, parallel fields) switch to their type, the way they already used it themselves;
- a method or property they just extracted or added, while copies of the same code remain elsewhere: the copies become calls to it (their `CompareTo` written, the old three-line comparison elsewhere becomes `a.CompareTo(b)`);
- a call still using a signature they changed, when adapting it is mechanical;
- a typo that breaks the build and has a single possible correction (a missing `;` or parenthesis, a misspelled existing name).

Make an edit only when all of this is true:
- It follows from something they already wrote: you can point at the line where they introduced the name, type, method or constant you use.
- Anyone reading their diff would make the same edit: one right answer, no option to weigh.
- The program does exactly what their own change already decided, no more.
- It is short: up to 8 lines per call (several calls for several places; one call per place).
- The code is not what they are writing right now (a half-typed line, a block in progress): wait for the next save.

Never: create a class, method, field or name they did not create; choose between two ways of doing something; pick a value; move code to a place they did not create; delete code; fix a logic bug; start a restructuring they have not started; apply one of your own design recommendations. Those are technical decisions and they belong to the developer: say them with an annotation. A literal that merely has the same value is not enough: a `7` that means something else stays. When the switch to their new form would need a choice (which subclass to instantiate, which value for a field the old form did not have), annotate instead.

Each edit is shown to them on its line, with your reason and an undo button, and listed in the panel: do not also annotate a line you edited. The reason says in a few words what of theirs the edit follows from ("renamed on line 4", "the constant on line 3"). When they undo an edit you receive <assist_undone>: do not make it again, and take it as information about what they meant.
"#;

const LEARN_EDITS_ADDENDUM: &str = r#"
# Finishing their refactorings

Teaching is about the piece they are learning, not about leaving them mechanical chores. Outside that piece, you complete yourself what follows with no choice from what they have written, with the assist_edit tool. This is not writing their task: it is the one exception to "you write only inside .vibelearn/", and assist_edit is the only way you ever write in their code (never Write or Edit on their files).

The current step stays theirs, entirely. Never make an edit that writes part of the step's goal, meets one of its success criteria, or does the move the step is teaching: when the step is "extract a `Score` class and use it", the places that still use the old form are the exercise, and finishing them is their job (say it in exercise_feedback if they forget). What you finish is what they changed beyond the step: a rename they did on their own, a constant they introduced while writing a passed piece, a type from an earlier step still used the old way in a corner of the code the plan does not cover. When in doubt whether a change belongs to the step, leave it to them.
"#;

pub fn system_prompt(root: &Path, mode: Mode) -> String {
    let mut out = String::from(SYSTEM_PROMPT);
    match mode {
        Mode::Learn => {
            out.push_str(LEARN_EDITS_ADDENDUM);
            out.push_str(FOLLOW_THROUGH);
        }
        Mode::Review => out.push_str(REVIEW_ADDENDUM),
        Mode::Refactor => out.push_str(REFACTOR_ADDENDUM),
        Mode::Monitor => out.push_str(MONITOR_ADDENDUM),
        Mode::Assist => {
            out.push_str(MONITOR_ADDENDUM);
            out.push_str(ASSIST_ADDENDUM);
            out.push_str(FOLLOW_THROUGH);
        }
    }
    out.push_str(&format!(
        "\n# Environment\n\n- Project root (your working directory): {}\n- Platform: {}\n- Today: {}\n- Reply in the language the learner writes in. The interface is in {}; use it until they write in another language.\n",
        root.display(),
        std::env::consts::OS,
        Utc::now().format("%Y-%m-%d"),
        i18n::language_name(),
    ));
    out
}

/// Premier message de la session : objectif, faits sur le projet, profil.
pub fn kickoff(session: &SessionState, facts: &ProjectFacts, profile: &Profile, previous: Option<&str>) -> String {
    let mut out = String::from("<vibelearn_kickoff>\n");
    match session.mode {
        Mode::Learn => {
            out.push_str("mode: learn\n");
            out.push_str(&format!("goal (learner's own words): {}\n", session.topic));
        }
        Mode::Review => out.push_str("mode: review\n"),
        Mode::Refactor => {
            out.push_str("mode: refactor\n");
            out.push_str(&format!("what to refactor (developer's own words): {}\n", session.topic));
        }
        Mode::Monitor => {
            out.push_str("mode: monitor\n");
            out.push_str(&format!("what they are working on (their own words): {}\n", session.topic));
        }
        Mode::Assist => {
            out.push_str("mode: assist\n");
            out.push_str(&format!("what they are working on (their own words): {}\n", session.topic));
        }
    }
    let stack = if facts.stack.is_empty() { "unknown".to_string() } else { facts.stack.join(", ") };
    out.push_str(&format!("detected stack: {stack}\n"));
    if let Some(cmd) = facts.check_command {
        out.push_str(&format!("suggested check_command: {cmd}\n"));
    }
    out.push_str(&format!("\ngit:\n{}\n", indent(&facts.git)));
    out.push_str(&format!("\nfiles (gitignore-aware, depth 4):\n{}\n", indent(&facts.tree)));
    out.push_str(&format!(
        "\nlearner profile (long-term, across projects; reuse these concept ids when they match):\n{}\n",
        indent(&profile.summary_for_prompt(60))
    ));
    // Les révisions ne se font que si l'apprenant les demande (`vibelearn review`) :
    // une session normale reste concentrée sur ce qu'il veut construire.
    if session.mode == Mode::Review {
        let today = Utc::now().date_naive();
        out.push_str("\ndue for review:\n");
        for (id, rec) in profile.due(today).iter().take(12) {
            out.push_str(&format!("  - {id} | {} | level {}/4\n", rec.label, rec.level));
        }
    }
    if let Some(prev) = previous {
        out.push_str(&format!(
            "\nprevious progress on this goal (the earlier conversation could not be restored; continue from here instead of starting over):\n{}\n",
            indent(prev)
        ));
    }
    out.push_str("</vibelearn_kickoff>");
    out
}

/// Rappel joint aux messages où la structure doit être jugée : sur une longue
/// session, le modèle ne regarde sinon que le diff de l'étape en cours.
const DESIGN_CHECK: &str = "Design checkpoint (Code that lasts): ";

pub fn resumed(session: &SessionState, away_minutes: i64) -> String {
    format!(
        "<session_resumed away_minutes=\"{away_minutes}\">\n{}\nThe developer may have changed their files while vibelearn was not running, and those changes were not sent to you: what you remember of their code is out of date. Read every file you are about to comment on, judge or build a step on.\n{DESIGN_CHECK}audit the code built so far and the remaining plan before continuing.\n</session_resumed>",
        session.progress_summary().trim_end()
    )
}

pub fn learner_command(name: &str) -> String {
    format!("<learner_command name=\"{name}\"/>")
}

pub fn hint(level: u8) -> String {
    format!("<learner_command name=\"hint\" level=\"{level}\"/>")
}

pub fn command_output(cmd: &str, exit: Option<i32>, output: &str) -> String {
    let exit = exit.map(|c| c.to_string()).unwrap_or_else(|| "none".to_string());
    format!("<command_output cmd=\"{}\" exit=\"{exit}\">\n{}\n</command_output>", attr(cmd), output.trim_end())
}

pub struct CheckOutput<'a> {
    pub cmd: &'a str,
    pub exit: Option<i32>,
    pub output: &'a str,
}

/// Message envoyé quand l'apprenant a modifié des fichiers pendant un exercice
/// (ou à chaque sauvegarde en surveillance, sans exercice).
pub fn file_changes(trigger: &str, exercise: Option<&str>, diffs: &str, check: Option<CheckOutput>) -> String {
    let judge = match exercise {
        Some(_) => "before passing this piece, judge the structure of the whole files it touched, not only this diff.",
        None => "judge the structure of the whole files touched, not only this diff.",
    };
    let exercise = exercise.map(|e| format!(" exercise=\"{}\"", attr(e))).unwrap_or_default();
    let mut out = format!("<file_changes trigger=\"{trigger}\"{exercise}>\n{}\n", diffs.trim_end());
    if let Some(c) = check {
        let exit = c.exit.map(|e| e.to_string()).unwrap_or_else(|| "none".to_string());
        out.push_str(&format!(
            "<check_output cmd=\"{}\" exit=\"{exit}\">\n{}\n</check_output>\n",
            attr(c.cmd),
            c.output.trim_end()
        ));
    }
    out.push_str(&format!("{DESIGN_CHECK}{judge}\n</file_changes>"));
    out
}

/// Question de l'apprenant (panneau vibelearn, ou ligne `//?` dans son code),
/// avec la ligne où était son curseur et le code qu'il avait sélectionné.
pub fn code_question(file: &str, line: usize, line_text: Option<&str>, selection: Option<&str>, text: &str) -> String {
    if file.is_empty() {
        return format!("<question>\n{}\n</question>", text.trim());
    }
    let line_text = line_text.map(|a| format!(" line_text=\"{}\"", attr(a))).unwrap_or_default();
    let mut out = format!("<question_in_code file=\"{}\" line=\"{line}\"{line_text}>\n", attr(file));
    if let Some(selected) = selection {
        let selected: String = selected.chars().take(1500).collect();
        out.push_str(&format!("<selected_code>\n{}\n</selected_code>\n", selected.trim_end()));
    }
    out.push_str(&format!("{}\n</question_in_code>", text.trim()));
    out
}

/// Ligne `//n …` : l'apprenant veut savoir par où avancer sur une fonctionnalité.
/// `line_text` est la ligne de code où la réponse s'affichera.
pub fn next_step(file: &str, line: usize, line_text: Option<&str>, feature: &str) -> String {
    let line_text = line_text.map(|a| format!(" line_text=\"{}\"", attr(a))).unwrap_or_default();
    format!("<next_step file=\"{}\" line=\"{line}\"{line_text}>
{}
{DESIGN_CHECK}decide where this feature belongs before naming the step; if it would pile onto a class that already has several jobs, the step is the restructuring.
</next_step>", attr(file), feature.trim())
}

/// Ligne(s) `//// …` (étapes en langage naturel) ou `////! …` (pseudo-code
/// formel) : l'apprenant veut le pseudo-code de ce qu'il veut accomplir.
/// `line_text` est la dernière ligne de la demande, qui reste dans le code.
pub fn pseudo_code(file: &str, line: usize, line_text: Option<&str>, request: &str, style: crate::annotate::PseudoStyle) -> String {
    let line_text = line_text.map(|t| format!(" line_text=\"{}\"", attr(t))).unwrap_or_default();
    let reminder = match style {
        crate::annotate::PseudoStyle::Natural => {
            "Natural style: sentences that explain, one per line, never code (no `=`, no call with parentheses, no cast, no `0.2f`); each still says exactly what is computed from which values."
        }
        crate::annotate::PseudoStyle::Formal => {
            "Formal style: pseudo-code lines (assignments with `<-`, calls, conditions, loops), never the real syntax of their language."
        }
    };
    format!(
        "<pseudo_code style=\"{}\" file=\"{}\" line=\"{line}\"{line_text}>\n{}\n{reminder} Tags: `[where] …` first, only if the code goes somewhere else; `[add] name : type, …` for each field or setting to create. Steps carry no tag and no number.\n</pseudo_code>",
        style.as_str(),
        attr(file),
        request.trim()
    )
}

/// Ligne `//t …` : l'apprenant veut apprendre à faire quelque chose, là.
pub fn teach_notion(file: &str, line: usize, line_text: Option<&str>, notion: &str) -> String {
    let line_text = line_text.map(|t| format!(" line_text=\"{}\"", attr(t))).unwrap_or_default();
    format!(
        "<learner_command name=\"teach\" file=\"{}\" line=\"{line}\"{line_text}>\n<wants_to_learn>{}</wants_to_learn>\n</learner_command>",
        attr(file),
        notion.trim()
    )
}

/// « Apprends-moi » sur une remarque : une mini-leçon sur ce point.
pub fn teach(file: &str, line: usize, line_text: Option<&str>, tone: &str, remark: &str) -> String {
    let line_text = line_text.map(|t| format!(" line_text=\"{}\"", attr(t))).unwrap_or_default();
    format!(
        "<learner_command name=\"teach\" file=\"{}\" line=\"{line}\"{line_text} tone=\"{tone}\">\n<remark>{}</remark>\n</learner_command>",
        attr(file),
        remark.trim()
    )
}

/// « Oublie » sur une remarque : ce genre de remarque ne l'intéresse pas.
pub fn remark_dismissed(file: &str, line: usize, remark: &str) -> String {
    format!(
        "<remark_dismissed file=\"{}\" line=\"{line}\">\n{}\nThe developer does not care about this kind of remark: do not make it again, anywhere.\n</remark_dismissed>",
        attr(file),
        remark.trim()
    )
}

/// Le démarrage, avec les remarques écartées dans ce projet.
pub fn with_dismissed(kickoff: String, dismissed: &[String]) -> String {
    if dismissed.is_empty() {
        return kickoff;
    }
    let list = dismissed.iter().map(|r| format!("  - {}", r.replace('\n', " "))).collect::<Vec<_>>().join("\n");
    let block = format!(
        "\nremarks the developer dismissed in this project (never make these kinds of remarks):\n{list}\n</vibelearn_kickoff>"
    );
    kickoff.replace("</vibelearn_kickoff>", &block)
}

/// « Comment ? » sur une remarque : la question posée à vibelearn.
pub fn how(remark: &str) -> String {
    format!("How do I do what your remark on this line says? Your remark: \"{}\"", remark.trim())
}

/// Le développeur a annulé une correction faite par vibelearn (mode assistance).
pub fn assist_undone(file: &str, line: usize, old: &str, new: &str) -> String {
    format!(
        "<assist_undone file=\"{}\" line=\"{line}\">\nThe developer undid your edit `{old}` → `{new}`. Do not make it again.\n</assist_undone>",
        attr(file)
    )
}

/// Changements faits hors exercice, joints au prochain message de l'apprenant.
pub fn pending_changes(diffs: &str) -> String {
    format!("<pending_changes>\n{}\n</pending_changes>", diffs.trim_end())
}

fn attr(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;")
}

fn indent(text: &str) -> String {
    text.trim_end().lines().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refactor_session_tells_the_tutor_what_to_restructure_and_how() {
        let session = SessionState::new("la boucle de Game1", Mode::Refactor);
        let facts = ProjectFacts { stack: vec![], check_command: None, tree: String::new(), git: String::new() };
        let kick = kickoff(&session, &facts, &Profile::default(), None);
        assert!(kick.contains("mode: refactor\nwhat to refactor (developer's own words): la boucle de Game1"), "{kick}");
        let system = system_prompt(Path::new("."), Mode::Refactor);
        assert!(system.contains("# This is a refactoring session"));
        assert!(!system_prompt(Path::new("."), Mode::Learn).contains("refactoring session"));
    }

    #[test]
    fn a_monitoring_session_drops_the_teaching_and_still_sends_every_save() {
        assert!(system_prompt(Path::new("."), Mode::Monitor).contains("# This is a monitoring session"));
        let session = SessionState::new("le jugement des appuis", Mode::Monitor);
        let facts = ProjectFacts { stack: vec![], check_command: None, tree: String::new(), git: String::new() };
        assert!(kickoff(&session, &facts, &Profile::default(), None).contains("mode: monitor\nwhat they are working on"));
        let changes = file_changes("auto", None, "diff", None);
        assert!(changes.contains(DESIGN_CHECK) && !changes.contains("passing this piece"), "{changes}");
    }

    #[test]
    fn an_assist_session_keeps_the_monitoring_rules_and_adds_the_edits() {
        let system = system_prompt(Path::new("."), Mode::Assist);
        assert!(system.contains("# This is a monitoring session") && system.contains("# This is an assist session"));
        assert!(!system_prompt(Path::new("."), Mode::Monitor).contains("# This is an assist session"));
        assert!(system.contains("Finishing their refactorings is the heart of this."));
        let learn = system_prompt(Path::new("."), Mode::Learn);
        assert!(learn.contains("# Finishing their refactorings") && learn.contains("The current step stays theirs"));
        assert!(learn.contains("Typical cases") && !learn.contains("# This is an assist session"));
        for mode in [Mode::Refactor, Mode::Monitor, Mode::Review] {
            assert!(!system_prompt(Path::new("."), mode).contains("Typical cases"), "{mode:?}");
        }
        let session = SessionState::new("BeatPattern", Mode::Assist);
        let facts = ProjectFacts { stack: vec![], check_command: None, tree: String::new(), git: String::new() };
        assert!(kickoff(&session, &facts, &Profile::default(), None).contains("mode: assist\nwhat they are working on"));
        assert!(assist_undone("src/a.cs", 3, "7", "BeatCount").contains("Do not make it again"));
    }

    #[test]
    fn every_save_and_every_resume_carry_the_design_checkpoint() {
        assert!(file_changes("save", Some("ex1"), "diff", None).contains(DESIGN_CHECK));
        assert!(resumed(&SessionState::new("x", Mode::Learn), 3).contains(DESIGN_CHECK));
        assert!(next_step("Player.cs", 7, None, "wall jump").contains(DESIGN_CHECK));
        let natural = pseudo_code("Player.cs", 3, Some("//// sauter"), "sauter", crate::annotate::PseudoStyle::Natural);
        assert!(natural.starts_with("<pseudo_code style=\"natural\" file=\"Player.cs\" line=\"3\" line_text=\"//// sauter\">\nsauter\n"));
        assert!(natural.contains("never code"));
        let formal = pseudo_code("Player.cs", 3, None, "sauter", crate::annotate::PseudoStyle::Formal);
        assert!(formal.starts_with("<pseudo_code style=\"formal\"") && formal.contains("<-"));
    }

    #[test]
    fn a_lesson_asked_in_the_code_says_what_they_want_to_learn_and_where() {
        let m = teach_notion("Assets/Player.cs", 12, Some("Move();"), "le pattern State");
        assert!(m.starts_with("<learner_command name=\"teach\" file=\"Assets/Player.cs\" line=\"12\" line_text=\"Move();\">"), "{m}");
        assert!(m.contains("<wants_to_learn>le pattern State</wants_to_learn>"));
        assert!(system_prompt(Path::new("."), Mode::Monitor).contains("<wants_to_learn> instead of <remark>"));
        assert!(system_prompt(Path::new("."), Mode::Learn).contains("this is a detour, not a new session"));
    }

    #[test]
    fn watching_flags_structure_early_and_before_answering_what_comes_next() {
        let system = system_prompt(Path::new("."), Mode::Assist);
        assert!(system.contains("never a late summary") && system.contains("decide where it belongs before saying how"));
        assert!(system_prompt(Path::new("."), Mode::Learn).contains("Never point them at a place you will later ask them to restructure."));
    }
}
