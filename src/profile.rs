//! Profil d'apprenant global (`~/.vibelearn/profile.json`) : ce que tu sais,
//! à quel niveau, et quand le réviser (répétition espacée).

use anyhow::Result;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::paths;

/// 0 inconnu, 1 vu, 2 compris, 3 appliqué, 4 maîtrisé
pub const MAX_LEVEL: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Coché par l'apprenant dans la checklist, pas encore vérifié.
    SelfReported,
    /// Observé par le tuteur (réponse, code, explication).
    Demonstrated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConceptRecord {
    pub label: String,
    pub level: u8,
    pub source: Source,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub next_review: Option<NaiveDate>,
    #[serde(default)]
    pub interval_days: u32,
    #[serde(default)]
    pub reviews: u32,
    #[serde(default)]
    pub lapses: u32,
    #[serde(default)]
    pub evidence: Option<String>,
    #[serde(default)]
    pub topics: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub topic: String,
    pub project: String,
    pub date: NaiveDate,
    pub minutes: u32,
    pub exercises_passed: u32,
    pub hints_used: u32,
    pub completed: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub concepts: BTreeMap<String, ConceptRecord>,
    #[serde(default)]
    pub sessions: Vec<SessionRecord>,
    /// Fichier où le profil est enregistré. Absent pour un profil en mémoire
    /// (tests) : `save` ne fait alors rien.
    #[serde(skip)]
    path: Option<std::path::PathBuf>,
}

pub fn level_name(level: u8) -> &'static str {
    use crate::i18n::tr;
    match level {
        0 => tr("inconnu", "unknown"),
        1 => tr("vu", "seen"),
        2 => tr("compris", "understood"),
        3 => tr("appliqué", "applied"),
        _ => tr("maîtrisé", "mastered"),
    }
}

impl Profile {
    pub fn load() -> Self {
        let path = paths::profile_file().ok();
        let mut profile: Profile = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        profile.path = path;
        profile
    }

    pub fn save(&self) -> Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Enregistre une observation et replanifie la prochaine révision.
    pub fn record(
        &mut self,
        id: &str,
        label: &str,
        level: u8,
        source: Source,
        evidence: Option<&str>,
        topic: &str,
    ) {
        self.record_at(id, label, level, source, evidence, topic, Utc::now());
    }

    #[allow(clippy::too_many_arguments)]
    fn record_at(
        &mut self,
        id: &str,
        label: &str,
        level: u8,
        source: Source,
        evidence: Option<&str>,
        topic: &str,
        now: DateTime<Utc>,
    ) {
        let level = level.min(MAX_LEVEL);
        let today = now.date_naive();
        let rec = self.concepts.entry(id.to_string()).or_insert_with(|| ConceptRecord {
            label: label.to_string(),
            level: 0,
            source,
            first_seen: now,
            last_seen: now,
            next_review: None,
            interval_days: 0,
            reviews: 0,
            lapses: 0,
            evidence: None,
            topics: Vec::new(),
        });

        // Une auto-déclaration ne rétrograde jamais ce qui a été démontré.
        if source == Source::SelfReported && rec.source == Source::Demonstrated && level <= rec.level {
            rec.last_seen = now;
            return;
        }

        let previous = rec.level;
        let was_due = rec.next_review.is_some_and(|d| d <= today);
        rec.label = label.to_string();
        rec.last_seen = now;
        if let Some(e) = evidence.filter(|e| !e.trim().is_empty()) {
            rec.evidence = Some(e.to_string());
        }
        if !topic.is_empty() && !rec.topics.iter().any(|t| t == topic) {
            rec.topics.push(topic.to_string());
            if rec.topics.len() > 8 {
                rec.topics.remove(0);
            }
        }

        if level >= 2 {
            let first_interval = match (source, level) {
                (Source::SelfReported, _) => 10,
                (_, 2) => 2,
                (_, 3) => 4,
                _ => 7,
            };
            rec.interval_days = if previous >= 2 && was_due && rec.interval_days > 0 {
                // Révision réussie : l'intervalle s'allonge selon la solidité.
                let factor = match level {
                    2 => 1.6,
                    3 => 2.2,
                    _ => 2.8,
                };
                ((rec.interval_days as f32 * factor).round() as u32).clamp(1, 180)
            } else {
                rec.interval_days.max(first_interval)
            };
            rec.reviews += 1;
        } else {
            if previous >= 2 {
                rec.lapses += 1;
            }
            rec.interval_days = 1;
        }
        rec.level = level;
        rec.source = source;
        rec.next_review = Some(today + Duration::days(rec.interval_days as i64));
    }

    /// Concepts à réviser aujourd'hui, les plus en retard d'abord.
    pub fn due(&self, today: NaiveDate) -> Vec<(&String, &ConceptRecord)> {
        let mut due: Vec<_> = self
            .concepts
            .iter()
            .filter(|(_, r)| r.level >= 1 && r.next_review.is_some_and(|d| d <= today))
            .collect();
        due.sort_by_key(|(_, r)| r.next_review);
        due
    }

    /// Nombre de jours consécutifs (jusqu'à aujourd'hui ou hier) avec une session.
    pub fn streak(&self, today: NaiveDate) -> u32 {
        let mut days: Vec<NaiveDate> = self.sessions.iter().map(|s| s.date).collect();
        days.sort();
        days.dedup();
        let mut streak = 0;
        let mut cursor = today;
        if !days.contains(&cursor) {
            cursor -= Duration::days(1);
        }
        while days.contains(&cursor) {
            streak += 1;
            cursor -= Duration::days(1);
        }
        streak
    }

    /// Résumé compact donné au tuteur en début de session.
    pub fn summary_for_prompt(&self, limit: usize) -> String {
        if self.concepts.is_empty() {
            return "(empty: first session with this learner)".to_string();
        }
        let today = Utc::now().date_naive();
        let mut rows: Vec<_> = self.concepts.iter().collect();
        rows.sort_by_key(|(_, r)| std::cmp::Reverse(r.last_seen));
        let mut out = String::new();
        for (id, r) in rows.iter().take(limit) {
            let due = if r.next_review.is_some_and(|d| d <= today) { " DUE_FOR_REVIEW" } else { "" };
            let src = if r.source == Source::SelfReported { " self-reported" } else { "" };
            out.push_str(&format!(
                "- {id} | {} | level {}/4{src}{due} | last seen {}\n",
                r.label,
                r.level,
                r.last_seen.format("%Y-%m-%d"),
            ));
        }
        if rows.len() > limit {
            out.push_str(&format!("(+{} older concepts not listed)\n", rows.len() - limit));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(day: u32) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("2026-01-{day:02}T10:00:00Z"))
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn successful_reviews_stretch_the_interval() {
        let mut p = Profile::default();
        p.record_at("rust.traits", "Traits", 2, Source::Demonstrated, None, "traits", at(1));
        let r = &p.concepts["rust.traits"];
        assert_eq!(r.interval_days, 2);
        assert_eq!(r.next_review, Some(at(3).date_naive()));

        // Révisé le jour prévu, niveau « appliqué » : 2 j × 2,2 ≈ 4 j.
        p.record_at("rust.traits", "Traits", 3, Source::Demonstrated, None, "traits", at(3));
        let r = &p.concepts["rust.traits"];
        assert_eq!(r.interval_days, 4);
        assert_eq!(r.reviews, 2);
    }

    #[test]
    fn forgetting_resets_and_counts_a_lapse() {
        let mut p = Profile::default();
        p.record_at("c", "C", 3, Source::Demonstrated, None, "t", at(1));
        p.record_at("c", "C", 1, Source::Demonstrated, None, "t", at(9));
        let r = &p.concepts["c"];
        assert_eq!((r.level, r.lapses, r.interval_days), (1, 1, 1));
        assert_eq!(p.due(at(10).date_naive()).len(), 1);
    }

    #[test]
    fn self_report_never_downgrades_demonstrated_knowledge() {
        let mut p = Profile::default();
        p.record_at("c", "C", 4, Source::Demonstrated, None, "t", at(1));
        p.record_at("c", "C", 2, Source::SelfReported, None, "t", at(2));
        assert_eq!(p.concepts["c"].level, 4);
        assert_eq!(p.concepts["c"].source, Source::Demonstrated);
    }

    #[test]
    fn streak_counts_consecutive_days() {
        let mut p = Profile::default();
        for day in [5, 6, 7] {
            p.sessions.push(SessionRecord {
                topic: "t".into(),
                project: "p".into(),
                date: at(day).date_naive(),
                minutes: 10,
                exercises_passed: 0,
                hints_used: 0,
                completed: true,
            });
        }
        assert_eq!(p.streak(at(7).date_naive()), 3);
        assert_eq!(p.streak(at(8).date_naive()), 3);
        assert_eq!(p.streak(at(10).date_naive()), 0);
    }
}
