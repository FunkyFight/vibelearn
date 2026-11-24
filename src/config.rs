//! Réglages persistants de vibelearn (`vibelearn config`), dans
//! `~/.vibelearn/config.json`. Une option de la ligne de commande l'emporte
//! toujours sur le réglage enregistré.
//!
//! Ajouter un réglage : un champ dans [`Config`] et une entrée dans [`SETTINGS`].

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::i18n::tr;
use crate::paths;

/// Secondes d'inactivité avant l'envoi automatique, sans réglage ni option.
pub const DEFAULT_AUTO_DELAY: u64 = 6;
const AUTO_DELAY_RANGE: std::ops::RangeInclusive<u64> = 1..=600;

/// Effort de réflexion sans réglage ni option : un tuteur doit répondre vite.
pub const DEFAULT_EFFORT: &str = "medium";
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_delay: Option<u64>,
    /// Modèle Claude (alias ou nom complet) ; sans lui, celui de l'abonnement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Install newer GitHub releases by itself; on without the setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_update: Option<bool>,
}

/// Un réglage modifiable par `vibelearn config <clé> <valeur>`.
pub struct Setting {
    pub key: &'static str,
    /// Description (français, anglais).
    about: (&'static str, &'static str),
    default: fn() -> String,
    get: fn(&Config) -> Option<String>,
    /// `None` remet la valeur par défaut.
    set: fn(&mut Config, Option<&str>) -> Result<()>,
}

pub const SETTINGS: &[Setting] = &[Setting {
    key: "auto-delay",
    about: (
        "secondes sans modification avant d'envoyer tes changements à vibelearn",
        "seconds without changes before your changes are sent to vibelearn",
    ),
    default: || DEFAULT_AUTO_DELAY.to_string(),
    get: |c| c.auto_delay.map(|s| s.to_string()),
    set: |c, value| {
        c.auto_delay = match value {
            None => None,
            Some(v) => match v.trim().parse::<u64>() {
                Ok(n) if AUTO_DELAY_RANGE.contains(&n) => Some(n),
                _ => bail!(
                    "{} {} {} {}",
                    tr("auto-delay attend un nombre de secondes entre", "auto-delay expects a number of seconds between"),
                    AUTO_DELAY_RANGE.start(),
                    tr("et", "and"),
                    AUTO_DELAY_RANGE.end()
                ),
            },
        };
        Ok(())
    },
},
Setting {
    key: "model",
    about: (
        "modèle Claude : opus, sonnet, haiku ou un nom complet (claude-opus-5-5…)",
        "Claude model: opus, sonnet, haiku or a full name (claude-opus-5-5…)",
    ),
    default: || tr("celui de ton abonnement", "your subscription's default").to_string(),
    get: |c| c.model.clone(),
    set: |c, value| {
        c.model = match value.map(str::trim) {
            None => None,
            Some(v) if v.is_empty() || v.contains(char::is_whitespace) => {
                bail!("{}", tr("model attend un nom de modèle, sans espace", "model expects a model name, without spaces"))
            }
            Some(v) => Some(v.to_string()),
        };
        Ok(())
    },
},
Setting {
    key: "effort",
    about: ("effort de réflexion : low, medium, high, xhigh ou max", "thinking effort: low, medium, high, xhigh or max"),
    default: || DEFAULT_EFFORT.to_string(),
    get: |c| c.effort.clone(),
    set: |c, value| {
        c.effort = match value.map(|v| v.trim().to_lowercase()) {
            None => None,
            Some(v) if EFFORTS.contains(&v.as_str()) => Some(v),
            Some(_) => bail!("{} {}", tr("effort attend une de ces valeurs :", "effort expects one of:"), EFFORTS.join(", ")),
        };
        Ok(())
    },
},
Setting {
    key: "auto-update",
    about: (
        "installe seul la dernière release GitHub, en arrière-plan : on ou off",
        "installs the latest GitHub release by itself, in the background: on or off",
    ),
    default: || "on".to_string(),
    get: |c| c.auto_update.map(|on| if on { "on" } else { "off" }.to_string()),
    set: |c, value| {
        c.auto_update = match value.map(|v| v.trim().to_lowercase()) {
            None => None,
            Some(v) if ["on", "true", "yes", "oui", "1"].contains(&v.as_str()) => Some(true),
            Some(v) if ["off", "false", "no", "non", "0"].contains(&v.as_str()) => Some(false),
            Some(_) => bail!("{}", tr("auto-update attend on ou off", "auto-update expects on or off")),
        };
        Ok(())
    },
}];

impl Setting {
    pub fn about(&self) -> &'static str {
        tr(self.about.0, self.about.1)
    }

    /// Valeur en vigueur, et si elle vient du fichier de réglages.
    pub fn current(&self, config: &Config) -> (String, bool) {
        match (self.get)(config) {
            Some(v) => (v, true),
            None => ((self.default)(), false),
        }
    }
}

pub fn find(key: &str) -> Result<&'static Setting> {
    SETTINGS.iter().find(|s| s.key.eq_ignore_ascii_case(key.trim())).with_context(|| {
        let keys: Vec<&str> = SETTINGS.iter().map(|s| s.key).collect();
        format!("{} « {key} » ({} {})", tr("réglage inconnu", "unknown setting"), tr("réglages :", "settings:"), keys.join(", "))
    })
}

impl Config {
    /// Le fichier de réglages, ou les valeurs par défaut s'il n'existe pas (ou est illisible).
    pub fn load() -> Self {
        paths::config_file()
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = paths::config_file()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Change un réglage ; `default` (ou `défaut`) le remet à sa valeur par défaut.
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let setting = find(key)?;
        let reset = ["default", "défaut", "defaut"].contains(&value.trim().to_lowercase().as_str());
        (setting.set)(self, (!reset).then_some(value))
    }

    /// Délai d'envoi automatique : l'option de la ligne de commande, sinon le réglage, sinon le défaut.
    pub fn auto_delay(&self, flag: Option<u64>) -> u64 {
        flag.or(self.auto_delay).unwrap_or(DEFAULT_AUTO_DELAY).max(1)
    }

    /// Modèle : l'option, sinon le réglage ; `None` laisse celui de l'abonnement.
    pub fn model(&self, flag: Option<String>) -> Option<String> {
        flag.or_else(|| self.model.clone())
    }

    /// Whether newer releases are installed in the background: the setting, otherwise yes.
    pub fn auto_update(&self) -> bool {
        self.auto_update.unwrap_or(true)
    }

    /// Effort : l'option, sinon le réglage, sinon moyen.
    pub fn effort(&self, flag: Option<String>) -> String {
        flag.or_else(|| self.effort.clone()).unwrap_or_else(|| DEFAULT_EFFORT.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setting_is_validated_reset_and_overridden_by_the_flag() {
        let mut c = Config::default();
        assert_eq!(c.auto_delay(None), DEFAULT_AUTO_DELAY);
        c.set("auto-delay", "12").unwrap();
        assert_eq!(c.auto_delay(None), 12);
        assert_eq!(c.auto_delay(Some(3)), 3, "l'option de la ligne de commande l'emporte");
        assert!(c.set("auto-delay", "0").is_err());
        assert!(c.set("auto-delay", "beaucoup").is_err());
        assert_eq!(c.auto_delay, Some(12), "une valeur refusée ne change rien");
        c.set("AUTO-DELAY", "défaut").unwrap();
        assert_eq!(c, Config::default());
        assert!(c.set("couleur", "rose").is_err());
    }

    #[test]
    fn model_and_effort_are_saved_and_the_flag_still_wins() {
        let mut c = Config::default();
        assert_eq!((c.model(None), c.effort(None).as_str()), (None, DEFAULT_EFFORT));
        c.set("model", "opus").unwrap();
        c.set("effort", "HIGH").unwrap();
        assert_eq!((c.model(None).as_deref(), c.effort(None).as_str()), (Some("opus"), "high"));
        assert_eq!(c.model(Some("haiku".into())).as_deref(), Some("haiku"), "l'option l'emporte");
        assert!(c.set("effort", "énorme").is_err());
        assert!(c.set("model", "claude opus").is_err());
        c.set("model", "default").unwrap();
        assert_eq!(c.model(None), None);
    }

    #[test]
    fn auto_update_is_on_until_turned_off() {
        let mut c = Config::default();
        assert!(c.auto_update());
        c.set("auto-update", "off").unwrap();
        assert!(!c.auto_update());
        assert!(c.set("auto-update", "parfois").is_err());
        c.set("auto-update", "default").unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn the_file_only_keeps_what_was_set() {
        assert_eq!(serde_json::to_string(&Config::default()).unwrap(), "{}");
        let c: Config = serde_json::from_str(r#"{"auto_delay": 9, "unknown": true}"#).unwrap();
        assert_eq!(c.auto_delay, Some(9));
    }
}
