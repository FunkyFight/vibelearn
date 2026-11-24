//! Outils du tuteur, exposés à Claude par un serveur MCP tenu en mémoire par
//! vibelearn (nom du serveur : `tutor`, donc `mcp__tutor__<outil>`).
//! Ce sont eux qui rendent la session interactive : checklist, questions,
//! plan, exercices surveillés, suivi de la maîtrise.

use serde_json::{Value, json};

pub const SERVER: &str = "tutor";

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        },
    })
}

pub fn definitions() -> Vec<Value> {
    vec![
        tool(
            "knowledge_checklist",
            "Show the learner a checklist of the concepts needed for the goal and wait for their answer. It opens as a list \
             in their editor, pre-ticked items included: they adjust it and press Enter. \
             Pre-check (assumed_known=true) what the project code, git history or learner profile suggests they already know, \
             and say why in `evidence`. The learner can untick an item or mark it \"unsure\". \
             Returns JSON {known:[ids], unsure:[ids], unknown:[ids]}. Use it once, after reconnaissance is finished and the \
             full list is decided, and again later only if the scope changes. Call it alone, never alongside other tool \
             calls. 4 to 12 items, ordered from most basic to most advanced.",
            json!({
                "title": { "type": "string", "description": "Short title, in the learner's language" },
                "intro": { "type": "string", "description": "One sentence explaining what to do" },
                "items": {
                    "type": "array", "minItems": 1, "maxItems": 16,
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string", "description": "Stable concept id, lowercase dotted slug, e.g. \"rust.traits.basics\". Reuse ids from the learner profile when the concept matches." },
                            "label": { "type": "string", "description": "Concept name as shown to the learner, on one line: under 45 characters" },
                            "why": { "type": "string", "description": "Why this concept is needed for the goal (one sentence)" },
                            "evidence": { "type": "string", "description": "What makes you think they know it (file, commit, profile). Omit if nothing." },
                            "assumed_known": { "type": "boolean", "description": "true = pre-checked" },
                            "kind": { "type": "string", "enum": ["prerequisite", "core", "bonus"] },
                        },
                        "required": ["id", "label", "assumed_known"],
                        "additionalProperties": false,
                    },
                },
            }),
            &["title", "items"],
        ),
        tool(
            "set_plan",
            "Publish (or replace) the plan. The developer sees \"piece k/n\" in each piece's instructions. Call it right after the checklist, and again \
             whenever the plan changes. The plan is the implementation itself, in their own project, cut into pieces in \
             build order: each step is one piece of real code (kind \"implementation\"), small enough to hold one idea. \
             Notions are explained inside the piece that needs them, not in separate lesson steps. \
             No capstone, quiz or invented challenge step.",
            json!({
                "goal": { "type": "string", "description": "What will exist in their project at the end (one sentence)" },
                "steps": {
                    "type": "array", "minItems": 1, "maxItems": 12,
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "title": { "type": "string", "description": "Short step title: what will exist in their code after this piece" },
                            "kind": {
                                "type": "string",
                                "enum": ["implementation"],
                                "description": "Always \"implementation\": a piece of the real code, in their project",
                            },
                        },
                        "required": ["id", "title", "kind"],
                        "additionalProperties": false,
                    },
                },
            }),
            &["goal", "steps"],
        ),
        tool(
            "update_step",
            "Change the status of a plan step. Mark a step \"active\" when you start it and \"done\" \
             when the learner has shown they got it.",
            json!({
                "id": { "type": "string" },
                "status": { "type": "string", "enum": ["active", "done", "skipped"] },
            }),
            &["id", "status"],
        ),
        tool(
            "ask",
            "Ask the developer ONE question (a pick list in their editor) and wait for the answer. Use it rarely: only when you cannot \
             continue without their choice (two reasonable designs, a preference), or when they asked to be quizzed. \
             Do NOT use it to check understanding, to have them predict a result or to make them restate an explanation: \
             they answer by writing code. Kinds: \"choice\" (pick one option), \"multi\" (pick several), \"free\" (open answer), \
             \"predict\" (show `code`, free-text prediction), \"confidence\" (1-5). Never mark the right answer in the options. \
             They may answer \"I don't know\". Returns JSON with the answer. One question per call, called alone.",
            json!({
                "kind": { "type": "string", "enum": ["choice", "multi", "predict", "free", "confidence"] },
                "question": { "type": "string", "description": "The question, in the learner's language. Light markdown allowed." },
                "options": { "type": "array", "maxItems": 8, "items": { "type": "string" }, "description": "For choice/multi: 2 to 6 plausible options, including common misconceptions" },
                "code": { "type": "string", "description": "Code snippet shown with the question" },
                "concept_id": { "type": "string" },
            }),
            &["kind", "question"],
        ),
        tool(
            "show_demo",
            "Point the learner to a demo you just wrote under .vibelearn/demos/. The application opens the files in their \
             editor; they run it by typing /run in the panel and you receive the output. Use a \
             different domain than the learner's real task so the demo never contains their answer.",
            json!({
                "title": { "type": "string" },
                "files": { "type": "array", "items": { "type": "string" }, "description": "Paths relative to the project root" },
                "run_command": { "type": "string", "description": "Shell command that runs the demo from the project root" },
                "observe": { "type": "string", "description": "What the learner should look for" },
            }),
            &["title", "files"],
        ),
        tool(
            "start_exercise",
            "Start the next step: a piece to write IN THEIR REAL CODE. The vibelearn panel shows the step (title, notes, \
             goal, criteria), and the line where they write is highlighted in their editor with `here` at its end; the \
             editor jumps there. This clears everything shown for the previous step. State the problem as a goal or a \
             question, never as the list of edits to make. After this call, end your turn with no text: you will receive a \
             <file_changes> message with diffs (and the check command output) each time they save.",
            json!({
                "id": { "type": "string" },
                "title": { "type": "string" },
                "goal": { "type": "string", "description": "The outcome to reach, one short sentence" },
                "notes": { "type": "string", "description": "The facts and the classic pitfall they need for this piece, given up front: one fact per line, two to four short lines (shown as a list). Never the code of the piece." },
                "anchor": { "type": "string", "description": "The line of the first target file where they will write, copied exactly: an empty line, a TODO, or the line right before the new code. It is highlighted. Omit for a new file." },
                "here": { "type": "string", "description": "Very short label shown at the end of that line, e.g. \"écris ici : la vitesse, puis la position\" (under 50 characters)" },
                "success_criteria": {
                    "type": "array", "minItems": 1, "maxItems": 6, "items": { "type": "string" },
                    "description": "Observable in the code; shown to the learner; must not reveal the solution",
                },
                "target_files": { "type": "array", "minItems": 1, "items": { "type": "string" }, "description": "Where the work happens, relative paths; the first holds the highlighted line. A new file may be named here: it is created empty." },
                "check_command": { "type": "string", "description": "Fast command run automatically on each change, e.g. \"cargo check\" or \"npm test -- foo\". Its output is attached to <file_changes>." },
            }),
            &["id", "title", "goal", "success_criteria", "target_files"],
        ),
        tool(
            "exercise_feedback",
            "Record your verdict on the learner's latest changes for the current exercise. \"passed\" closes the exercise; \
             \"progress\" = right direction, not done; \"wip\" = unfinished or unrelated edit, nothing to say yet; \
             \"off_track\" = wrong approach. Call it every time you receive <file_changes>. On \"passed\", the panel celebrates \
             the step with your summary and clears its annotations: no comment needed, start the next step.",
            json!({
                "id": { "type": "string" },
                "verdict": { "type": "string", "enum": ["passed", "progress", "wip", "off_track"] },
                "summary": { "type": "string", "description": "One line for the session history, in the learner's language" },
            }),
            &["id", "verdict", "summary"],
        ),
        tool(
            "comment",
            "Annotate one line of the developer's code: a short remark shown at the end of that line (with an icon for its \
             tone, full text on hover) and listed in the vibelearn panel, where clicking it jumps to the line. Nothing is \
             written into their file. Use it for feedback on what they just wrote: what is right (tone ok), what is broken \
             or wrong (tone fix, the line is tinted), code that works but is badly structured (tone design: duplication, \
             a function or type doing several jobs, a case hard-coded where the next one will need an edit, a dependency \
             on a concrete type where an abstraction is needed), a fact they need on that line (tone info). For design, \
             recommend the structure (the class to create, the pattern to use, the principle) and the pain it saves when \
             the project grows, never the refactored code. `text` is the headline shown at the end of the line: under about \
             70 characters, the problem in plain words. `detail` is what they read on hover and in the panel: the full \
             explanation, clear to someone who has not followed your reasoning. Several annotations in one turn when they \
             are about different lines. Your earlier annotations are removed when you annotate again. Never the code of \
             their piece.",
            json!({
                "file": { "type": "string", "description": "Relative path of the file" },
                "anchor": { "type": "string", "description": "One line of their code, copied exactly as it is in the file" },
                "line": { "type": "integer", "description": "Line number of the anchor if you know it (picks the right one when the text appears several times), or the line to annotate when there is no anchor" },
                "tone": { "type": "string", "enum": crate::threads::TONES, "description": "ok: this line is right; fix: broken or wrong here; design: works, but the structure will resist change (not SOLID, not DRY, not extensible); info: a fact about this line" },
                "text": { "type": "string", "description": "Headline, under about 70 characters: what is wrong (or right) on this line, in plain words, e.g. \"`conductor` is still null when LoadContent runs\". Inline `code` allowed" },
                "detail": { "type": "string", "description": "Two to four short sentences, for fix and design always: what happens, in which order, and with which consequence (the exception, the wrong value, the next feature that will hurt), naming their own variables and methods; then what to do about it (in a learning session: as a fact or a question, never the code). For ok and info, optional." },
            }),
            &["file", "tone", "text"],
        ),
        tool(
            "assist_edit",
            "Assist sessions only. Replace a short passage of the developer's code (up to 8 lines) to finish a change they made \
             and left incomplete: a leftover old name, a literal they turned into a constant, the leftovers of their refactoring \
             (code still using the old form where their new class, record, method or constant now does the job), a call on a \
             signature they changed, a typo with a single possible fix. Never a decision of your own: no name or type they did \
             not create, no choice between options, no behaviour change, no deleting code. The edit is applied in their editor \
             (undoable), shown on the line with your reason and listed in the panel; it is refused if the file has unsaved \
             changes or `old` does not start on that line. One passage per call; several calls for several places.",
            json!({
                "file": { "type": "string", "description": "Relative path of the file" },
                "line": { "type": "integer", "description": "Line number (from 1) where `old` starts, as in the file you last read" },
                "old": { "type": "string", "description": "The exact text to replace, copied from the file (lines separated by \\n, indentation included); long enough to start only once on that line" },
                "new": { "type": "string", "description": "The replacement (lines separated by \\n, indentation included)" },
                "reason": { "type": "string", "description": "A few words: what of theirs it follows from, e.g. \"renamed on line 4\", \"the constant on line 3\"" },
            }),
            &["file", "line", "old", "new", "reason"],
        ),
        tool(
            "record_mastery",
            "Update the learner's long-term profile. Levels: 0 unknown, 1 exposed (saw an explanation or demo), \
             2 understood (answered a question correctly), 3 applied (used it correctly in real code without being told how), \
             4 mastered (explained it back or transferred it to a new situation). Only raise a level on evidence you observed; \
             lower it if a review shows they forgot.",
            json!({
                "items": {
                    "type": "array", "minItems": 1, "maxItems": 12,
                    "items": {
                        "type": "object",
                        "properties": {
                            "concept_id": { "type": "string" },
                            "label": { "type": "string" },
                            "level": { "type": "integer", "minimum": 0, "maximum": 4 },
                            "evidence": { "type": "string" },
                        },
                        "required": ["concept_id", "label", "level"],
                        "additionalProperties": false,
                    },
                },
            }),
            &["items"],
        ),
        tool(
            "finish_session",
            "Close the session once the goal is reached (or the learner asks to stop). Saves a revision sheet to \
             .vibelearn/notes/ and shows the summary.",
            json!({
                "summary": { "type": "string", "description": "2-4 sentences: what the learner can now do" },
                "notes_markdown": { "type": "string", "description": "Revision sheet in markdown: key ideas, pitfalls they hit, mini-examples from the demos (never the solution to their exercises verbatim)" },
                "next_steps": { "type": "array", "maxItems": 6, "items": { "type": "string" }, "description": "What to learn next" },
            }),
            &["summary", "notes_markdown"],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_a_valid_object_schema() {
        let defs = definitions();
        assert_eq!(defs.len(), 11);
        for d in &defs {
            assert!(d["name"].as_str().is_some_and(|n| !n.is_empty()));
            assert_eq!(d["inputSchema"]["type"], "object");
            let props = d["inputSchema"]["properties"].as_object().unwrap();
            for req in d["inputSchema"]["required"].as_array().unwrap() {
                assert!(props.contains_key(req.as_str().unwrap()), "{} requires unknown {req}", d["name"]);
            }
        }
    }
}
