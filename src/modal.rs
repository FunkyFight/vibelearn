//! Interactions ouvertes par le tuteur : checklist de connaissances, question,
//! demande de permission. Ce module ne contient que leur état et la réponse
//! renvoyée au tuteur ; l'extension VS Code les affiche (listes, boutons).

use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Instant;

use crate::session::Claim;

pub enum Modal {
    Checklist(Checklist),
    Ask(Ask),
    Permission(PermissionPrompt),
}

impl Modal {
    /// Identifiant de l'appel en attente côté moteur.
    pub fn reply_id(&self) -> u64 {
        match self {
            Modal::Checklist(c) => c.reply,
            Modal::Ask(a) => a.reply,
            Modal::Permission(p) => p.reply,
        }
    }
}

// ---------------------------------------------------------------------------
// Checklist
// ---------------------------------------------------------------------------

pub struct CheckItem {
    pub id: String,
    pub label: String,
    pub why: Option<String>,
    pub evidence: Option<String>,
    pub kind: String,
    /// Pré-coché par le tuteur d'après le code ou le profil.
    pub assumed: bool,
    pub state: Claim,
}

pub struct Checklist {
    pub reply: u64,
    pub title: String,
    pub intro: Option<String>,
    pub items: Vec<CheckItem>,
}

impl Checklist {
    pub fn from_args(reply: u64, args: &Value) -> Result<Self, String> {
        let items: Vec<CheckItem> = args["items"]
            .as_array()
            .ok_or("`items` must be an array")?
            .iter()
            .filter_map(|it| {
                let id = it["id"].as_str()?.to_string();
                let label = it["label"].as_str()?.to_string();
                let assumed = it["assumed_known"].as_bool().unwrap_or(false);
                Some(CheckItem {
                    id,
                    label,
                    why: it["why"].as_str().map(str::to_string),
                    evidence: it["evidence"].as_str().map(str::to_string),
                    kind: it["kind"].as_str().unwrap_or("core").to_string(),
                    assumed,
                    state: if assumed { Claim::Known } else { Claim::Unknown },
                })
            })
            .collect();
        if items.is_empty() {
            return Err("`items` needs at least one item with `id` and `label`".into());
        }
        Ok(Self {
            reply,
            title: args["title"].as_str().unwrap_or("Checklist").to_string(),
            intro: args["intro"].as_str().map(str::to_string),
            items,
        })
    }

    #[cfg_attr(not(feature = "window"), allow(dead_code))]
    pub fn set_all(&mut self, claim: Claim) {
        self.items.iter_mut().for_each(|i| i.state = claim);
    }

    pub fn count(&self, claim: Claim) -> usize {
        self.items.iter().filter(|i| i.state == claim).count()
    }

    fn ids(&self, claim: Claim) -> Vec<&str> {
        self.items.iter().filter(|i| i.state == claim).map(|i| i.id.as_str()).collect()
    }

    /// Réponse renvoyée au tuteur.
    pub fn result(&self) -> Value {
        let corrected: Vec<&str> = self
            .items
            .iter()
            .filter(|i| i.assumed && i.state != Claim::Known)
            .map(|i| i.id.as_str())
            .collect();
        json!({
            "known": self.ids(Claim::Known),
            "unsure": self.ids(Claim::Unsure),
            "unknown": self.ids(Claim::Unknown),
            "unticked_assumptions": corrected,
        })
    }
}

// ---------------------------------------------------------------------------
// Question
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskKind {
    Choice,
    Multi,
    Predict,
    Free,
    Confidence,
}

pub struct Ask {
    pub reply: u64,
    pub kind: AskKind,
    pub question: String,
    pub code: Option<String>,
    pub options: Vec<String>,
    /// Option retenue (question à choix unique).
    pub choice: Option<usize>,
    /// Options cochées (question à choix multiples).
    pub selected: Vec<bool>,
    /// Réponse libre.
    pub text: String,
    pub confidence: u8,
    /// « Je ne sais pas » : toujours permis, jamais pénalisé.
    pub dont_know: bool,
    opened: Instant,
}

impl Ask {
    pub fn from_args(reply: u64, args: &Value) -> Result<Self, String> {
        let kind = match args["kind"].as_str().unwrap_or("") {
            "choice" => AskKind::Choice,
            "multi" => AskKind::Multi,
            "predict" => AskKind::Predict,
            "free" => AskKind::Free,
            "confidence" => AskKind::Confidence,
            other => return Err(format!("unknown kind `{other}`")),
        };
        let question = args["question"].as_str().ok_or("`question` is required")?.to_string();
        let options: Vec<String> = args["options"]
            .as_array()
            .map(|a| a.iter().filter_map(|o| o.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        if matches!(kind, AskKind::Choice | AskKind::Multi) && options.len() < 2 {
            return Err("`options` needs at least 2 entries for choice/multi".into());
        }
        Ok(Self {
            reply,
            kind,
            question,
            code: args["code"].as_str().map(str::to_string),
            selected: vec![false; options.len()],
            options,
            choice: None,
            text: String::new(),
            confidence: 3,
            dont_know: false,
            opened: Instant::now(),
        })
    }

    /// Vrai quand une réponse est prête à être envoyée.
    #[cfg_attr(not(feature = "window"), allow(dead_code))]
    pub fn can_submit(&self) -> bool {
        match self.kind {
            AskKind::Choice => self.choice.is_some(),
            AskKind::Multi => self.selected.iter().any(|s| *s),
            AskKind::Predict | AskKind::Free => !self.text.trim().is_empty(),
            AskKind::Confidence => true,
        }
    }

    /// Réponse en clair (pour le fil de conversation).
    pub fn answer_text(&self) -> String {
        if self.dont_know {
            return crate::i18n::tr("je ne sais pas", "I don't know").to_string();
        }
        match self.kind {
            AskKind::Choice => self.choice.and_then(|i| self.options.get(i)).cloned().unwrap_or_default(),
            AskKind::Multi => self
                .options
                .iter()
                .zip(&self.selected)
                .filter(|(_, s)| **s)
                .map(|(o, _)| o.as_str())
                .collect::<Vec<_>>()
                .join(" ; "),
            AskKind::Predict | AskKind::Free => self.text.trim().to_string(),
            AskKind::Confidence => format!("{}/5", self.confidence),
        }
    }

    /// Réponse renvoyée au tuteur.
    pub fn result(&self) -> Value {
        let seconds = self.opened.elapsed().as_secs();
        if self.dont_know {
            return json!({ "answer": null, "dont_know": true, "seconds": seconds });
        }
        match self.kind {
            AskKind::Choice => json!({
                "answer": self.choice.and_then(|i| self.options.get(i)),
                "selected_index": self.choice,
                "seconds": seconds,
            }),
            AskKind::Multi => {
                let idx: Vec<usize> = self.selected.iter().enumerate().filter(|(_, s)| **s).map(|(i, _)| i).collect();
                let answers: Vec<&String> = idx.iter().map(|i| &self.options[*i]).collect();
                json!({ "answers": answers, "selected_indices": idx, "seconds": seconds })
            }
            AskKind::Predict | AskKind::Free => json!({ "answer": self.text.trim(), "seconds": seconds }),
            AskKind::Confidence => json!({ "confidence": self.confidence, "scale": "1-5", "seconds": seconds }),
        }
    }
}

// ---------------------------------------------------------------------------
// Permission
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Autoriser, et ne plus demander pour la même chose pendant la session.
    Always,
    Deny,
}

pub struct PermissionPrompt {
    pub reply: u64,
    pub tool: String,
    pub headline: String,
    /// Commande, ou aperçu du contenu à écrire.
    pub detail: String,
    /// Clé mémorisée si l'apprenant choisit « toujours ».
    pub always_key: String,
    /// Fichier que le tuteur écrirait hors du bac à sable.
    pub write_path: Option<PathBuf>,
    pub decision: Option<Decision>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checklist() -> Checklist {
        Checklist::from_args(
            1,
            &json!({ "title": "T", "items": [
                { "id": "a", "label": "A", "assumed_known": true },
                { "id": "b", "label": "B", "assumed_known": false },
                { "id": "c", "label": "C", "assumed_known": true },
            ]}),
        )
        .unwrap()
    }

    #[test]
    fn checklist_starts_prechecked_and_reports_corrections() {
        let mut c = checklist();
        assert_eq!(c.ids(Claim::Known), vec!["a", "c"]);
        c.items[0].state = Claim::Unknown;
        c.items[1].state = Claim::Unsure;
        let r = c.result();
        assert_eq!(r["known"], json!(["c"]));
        assert_eq!(r["unsure"], json!(["b"]));
        assert_eq!(r["unknown"], json!(["a"]));
        assert_eq!(r["unticked_assumptions"], json!(["a"]));
        c.set_all(Claim::Known);
        assert_eq!(c.count(Claim::Known), 3);
    }

    #[test]
    fn choice_question_needs_a_pick_unless_the_learner_does_not_know() {
        let args = json!({ "kind": "choice", "question": "Q ?", "options": ["x", "y"] });
        let mut a = Ask::from_args(2, &args).unwrap();
        assert!(!a.can_submit());
        a.choice = Some(1);
        assert!(a.can_submit());
        assert_eq!(a.result()["answer"], "y");
        assert_eq!(a.answer_text(), "y");

        let mut a = Ask::from_args(2, &args).unwrap();
        a.dont_know = true;
        assert_eq!(a.result()["dont_know"], true);
    }

    #[test]
    fn free_answer_requires_text_and_keeps_it() {
        let mut a = Ask::from_args(3, &json!({ "kind": "free", "question": "Explique" })).unwrap();
        assert!(!a.can_submit());
        a.text = "  un trait \n".into();
        assert!(a.can_submit());
        assert_eq!(a.result()["answer"], "un trait");
    }

    #[test]
    fn multi_choice_reports_every_ticked_option() {
        let mut a = Ask::from_args(4, &json!({ "kind": "multi", "question": "Q", "options": ["a", "b", "c"] })).unwrap();
        a.selected[0] = true;
        a.selected[2] = true;
        assert_eq!(a.result()["selected_indices"], json!([0, 2]));
        assert_eq!(a.answer_text(), "a ; c");
    }

    #[test]
    fn malformed_tool_arguments_are_rejected_with_a_reason() {
        assert!(Ask::from_args(1, &json!({ "kind": "choice", "question": "Q", "options": ["seul"] })).is_err());
        assert!(Ask::from_args(1, &json!({ "kind": "essay", "question": "Q" })).is_err());
        assert!(Checklist::from_args(1, &json!({ "title": "T", "items": [] })).is_err());
    }
}
