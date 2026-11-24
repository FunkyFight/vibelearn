//! Connexion de vibelearn à un modèle (`vibelearn login`), dans
//! `~/.vibelearn/login.json`. Quatre façons de se connecter :
//!
//! - l'abonnement Claude, par le binaire `claude` (sans clé) ;
//! - une API compatible Anthropic (Messages) ;
//! - une API compatible OpenAI (Chat Completions) ;
//! - l'API OpenAI Responses, celle des modèles Codex.
//!
//! Avec une clé, il n'y a pas de binaire : c'est vibelearn qui tient la
//! conversation et les outils (voir `api.rs`).

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::time::Duration;

use crate::i18n::tr;
use crate::paths;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// Abonnement Claude, par le binaire `claude`.
    #[default]
    Claude,
    /// API compatible Anthropic (Messages), conversation tenue par vibelearn.
    Anthropic,
    /// API compatible OpenAI (Chat Completions), conversation tenue par vibelearn.
    Openai,
    /// API OpenAI Responses (modèles Codex), conversation tenue par vibelearn.
    Codex,
}

/// Les choix du menu de `vibelearn login` : l'API Claude officielle est l'API
/// Anthropic avec l'URL d'Anthropic, sans la demander.
pub struct Choice {
    pub provider: Provider,
    /// URL imposée (sinon : demandée).
    pub url: Option<&'static str>,
    names: &'static [&'static str],
    label: (&'static str, &'static str),
}

pub const CHOICES: &[Choice] = &[
    Choice {
        provider: Provider::Claude,
        url: None,
        names: &["claude", "abo", "abonnement", "subscription"],
        label: ("abonnement Claude (claude.ai, sans clé)", "Claude subscription (claude.ai, no key)"),
    },
    Choice {
        provider: Provider::Anthropic,
        url: Some(ANTHROPIC_URL),
        names: &["claude-api", "official", "officielle"],
        label: ("API Claude officielle (clé API Anthropic)", "official Claude API (Anthropic API key)"),
    },
    Choice {
        provider: Provider::Anthropic,
        url: None,
        names: &["anthropic", "anthropic-compatible"],
        label: ("API compatible Anthropic (URL et clé API)", "Anthropic-compatible API (URL and API key)"),
    },
    Choice {
        provider: Provider::Openai,
        url: None,
        names: &["openai", "openai-compatible", "chat"],
        label: ("API compatible OpenAI, Chat Completions (clé API)", "OpenAI-compatible API, Chat Completions (API key)"),
    },
    Choice {
        provider: Provider::Codex,
        url: None,
        names: &["codex", "responses"],
        label: ("API OpenAI Responses, modèles Codex (clé API)", "OpenAI Responses API, Codex models (API key)"),
    },
];

impl Choice {
    pub fn label(&self) -> &'static str {
        tr(self.label.0, self.label.1)
    }

    /// Par son nom (`claude-api`, `openai`…) ou son numéro dans le menu.
    pub fn parse(text: &str) -> Option<&'static Choice> {
        let text = text.trim().to_lowercase();
        if let Ok(n) = text.parse::<usize>() {
            return CHOICES.get(n.checked_sub(1)?);
        }
        CHOICES.iter().find(|c| c.names.contains(&text.as_str()))
    }

    /// Le choix qui correspond à une connexion enregistrée.
    pub fn of(login: &Login) -> &'static Choice {
        let official = login.provider == Provider::Anthropic && is_official_anthropic(&login.url());
        CHOICES
            .iter()
            .find(|c| c.provider == login.provider && (c.provider != Provider::Anthropic || c.url.is_some() == official))
            .unwrap_or(&CHOICES[0])
    }

    pub fn names() -> String {
        CHOICES.iter().map(|c| c.names[0]).collect::<Vec<_>>().join(", ")
    }
}

impl Provider {
    pub fn default_url(self) -> &'static str {
        match self {
            Self::Claude | Self::Anthropic => ANTHROPIC_URL,
            Self::Openai | Self::Codex => OPENAI_URL,
        }
    }

    /// Passe par le binaire `claude`.
    pub fn uses_claude(self) -> bool {
        self == Self::Claude
    }
}

pub const ANTHROPIC_URL: &str = "https://api.anthropic.com";
pub const OPENAI_URL: &str = "https://api.openai.com/v1";

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Login {
    #[serde(default)]
    pub provider: Provider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl Login {
    /// La connexion enregistrée ; l'abonnement Claude s'il n'y en a pas.
    pub fn load() -> Self {
        paths::login_file()
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = paths::login_file()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Revient à l'abonnement Claude (oublie la clé).
    pub fn forget() -> Result<()> {
        let path = paths::login_file()?;
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    pub fn url(&self) -> String {
        trim_url(self.url.as_deref().unwrap_or(self.provider.default_url()))
    }

    /// La clé, montrée sans la dévoiler : `sk-…a1b2`.
    pub fn masked_key(&self) -> String {
        match &self.key {
            None => "-".into(),
            Some(k) => {
                let chars: Vec<char> = k.chars().collect();
                if chars.len() <= 8 {
                    "…".into()
                } else {
                    let head: String = chars[..3].iter().collect();
                    let tail: String = chars[chars.len() - 4..].iter().collect();
                    format!("{head}…{tail}")
                }
            }
        }
    }
}

pub fn trim_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

/// L'API officielle d'Anthropic (clé en `x-api-key`) plutôt qu'un service compatible.
pub fn is_official_anthropic(url: &str) -> bool {
    trim_url(url) == ANTHROPIC_URL
}

// ---------------------------------------------------------------------------
// Vérification de la clé
// ---------------------------------------------------------------------------

pub enum Check {
    /// La clé est acceptée ; les modèles que le service annonce, s'il les liste.
    Ok(Vec<String>),
    Refused(String),
    /// Le service ne permet pas de vérifier (pas de liste de modèles) : on garde quand même.
    Unknown(String),
}

/// Demande la liste des modèles : le moyen le moins cher de savoir si la clé passe.
pub async fn check(provider: Provider, url: &str, key: &str) -> Check {
    let client = match reqwest::Client::builder().connect_timeout(Duration::from_secs(15)).timeout(Duration::from_secs(30)).build() {
        Ok(c) => c,
        Err(e) => return Check::Unknown(e.to_string()),
    };
    let url = trim_url(url);
    let request = if provider == Provider::Anthropic {
        let mut r = client.get(crate::api::anthropic_endpoint(&url, "models")).header("anthropic-version", "2023-06-01").header("x-api-key", key);
        if !is_official_anthropic(&url) {
            r = r.bearer_auth(key);
        }
        r
    } else {
        client.get(format!("{url}/models")).bearer_auth(key)
    };
    let response = match request.send().await {
        Ok(r) => r,
        Err(e) => return Check::Unknown(format!("{} : {e}", tr("service injoignable", "service unreachable"))),
    };
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Check::Refused(format!("{status} {}", crate::api::error_message(&body)));
    }
    if !status.is_success() {
        return Check::Unknown(format!("{status} {}", crate::api::error_message(&body)));
    }
    let json: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    let mut models: Vec<String> =
        json["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(str::to_string)).collect();
    models.sort();
    Check::Ok(models)
}

// ---------------------------------------------------------------------------
// `vibelearn login`
// ---------------------------------------------------------------------------

fn prompt_line(question: &str, default: Option<&str>) -> Result<String> {
    match default {
        Some(d) if !d.is_empty() => print!("{question} [{d}] : "),
        _ => print!("{question} : "),
    }
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).context("lecture du terminal")?;
    let line = line.trim().to_string();
    Ok(if line.is_empty() { default.unwrap_or("").to_string() } else { line })
}

pub fn choose() -> Result<&'static Choice> {
    let current = Choice::of(&Login::load()) as *const Choice;
    println!("{}", tr("Comment vibelearn se connecte-t-il au modèle ?", "How should vibelearn reach the model?"));
    for (i, c) in CHOICES.iter().enumerate() {
        let mark = if std::ptr::eq(c, current) { tr("  (actuel)", "  (current)") } else { "" };
        println!("  {}. {}{mark}", i + 1, c.label());
    }
    loop {
        let answer = prompt_line(tr("Choix", "Choice"), Some("1"))?;
        if let Some(c) = Choice::parse(&answer) {
            return Ok(c);
        }
        println!("{} 1-{}.", tr("Tape un numéro de", "Type a number from"), CHOICES.len());
    }
}

/// Connexion par clé API : URL, clé, vérification, modèle, puis enregistrement.
pub async fn login_with_key(provider: Provider, url: Option<String>, model: Option<String>) -> Result<Login> {
    let previous = Login::load();
    let same = previous.provider == provider;
    let url = match url {
        Some(u) => trim_url(&u),
        None => {
            let default = if same { previous.url() } else { provider.default_url().to_string() };
            trim_url(&prompt_line(tr("URL de l'API", "API URL"), Some(&default))?)
        }
    };
    if !url.starts_with("http://") && !url.starts_with("https://") {
        bail!("{} : {url}", tr("l'URL doit commencer par http:// ou https://", "the URL must start with http:// or https://"));
    }
    let key = match std::env::var("VIBELEARN_API_KEY").ok().filter(|k| !k.trim().is_empty()) {
        Some(k) => k.trim().to_string(),
        None => {
            let keep = same && previous.key.is_some() && previous.url() == url;
            let question = if keep {
                format!("{} ({} {})", tr("Clé API", "API key"), tr("Entrée garde", "Enter keeps"), previous.masked_key())
            } else {
                tr("Clé API (elle ne s'affiche pas)", "API key (hidden)").to_string()
            };
            let typed = rpassword::prompt_password(format!("{question} : ")).context("lecture de la clé")?;
            match typed.trim() {
                "" if keep => previous.key.clone().unwrap_or_default(),
                "" => bail!("{}", tr("aucune clé saisie", "no key entered")),
                k => k.to_string(),
            }
        }
    };

    println!("{}", tr("Vérification de la clé…", "Checking the key…"));
    let models = match check(provider, &url, &key).await {
        Check::Ok(models) => {
            println!("  [ok] {}", tr("clé acceptée", "key accepted"));
            models
        }
        Check::Refused(why) => bail!("{} ({why})", tr("clé refusée par le service", "the service refused the key")),
        Check::Unknown(why) => {
            println!(
                "  [?] {} ({why}). {}",
                tr("impossible de vérifier la clé", "could not check the key"),
                tr("Je l'enregistre quand même.", "Saving it anyway.")
            );
            Vec::new()
        }
    };

    let model = match model.filter(|m| !m.trim().is_empty()) {
        Some(m) => Some(m.trim().to_string()),
        None => {
            if !models.is_empty() {
                let shown: Vec<&str> = models.iter().map(String::as_str).take(40).collect();
                println!("{} {}", tr("Modèles proposés :", "Available models:"), shown.join(", "));
            }
            let default = if same { previous.model.clone() } else { None };
            let typed = prompt_line(tr("Modèle", "Model"), default.as_deref())?;
            let typed = typed.trim().to_string();
            if typed.is_empty() {
                bail!("{}", tr("il faut un nom de modèle", "a model name is required"));
            }
            Some(typed)
        }
    };
    if model.as_deref().is_some_and(|m| m.contains(char::is_whitespace)) {
        bail!("{}", tr("le nom du modèle ne contient pas d'espace", "a model name has no spaces"));
    }
    let login = Login { provider, url: Some(url), key: Some(key), model };
    login.save()?;
    Ok(login)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_are_parsed_by_name_or_number() {
        let p = |t: &str| Choice::parse(t).map(|c| (c.provider, c.url));
        assert_eq!(p("Codex"), Some((Provider::Codex, None)));
        assert_eq!(p("2"), Some((Provider::Anthropic, Some(ANTHROPIC_URL))));
        assert_eq!(p("claude-api"), Some((Provider::Anthropic, Some(ANTHROPIC_URL))));
        assert_eq!(p("anthropic"), Some((Provider::Anthropic, None)));
        assert_eq!(p("4"), Some((Provider::Openai, None)));
        assert_eq!(p(" abonnement "), Some((Provider::Claude, None)));
        assert_eq!(p("gemini"), None);
        assert_eq!(p("0"), None);
        let official = Login { provider: Provider::Anthropic, url: Some("https://api.anthropic.com/".into()), ..Login::default() };
        assert_eq!(Choice::of(&official).names[0], "claude-api");
        let other = Login { provider: Provider::Anthropic, url: Some("https://api.z.ai/api/anthropic".into()), ..Login::default() };
        assert_eq!(Choice::of(&other).names[0], "anthropic");
        assert!(Provider::Claude.uses_claude() && !Provider::Anthropic.uses_claude() && !Provider::Codex.uses_claude());
    }

    #[test]
    fn the_file_defaults_to_the_subscription_and_hides_the_key() {
        let login: Login = serde_json::from_str("{}").unwrap();
        assert_eq!(login.provider, Provider::Claude);
        let login: Login =
            serde_json::from_str(r#"{"provider":"openai","url":"http://localhost:1234/v1/","key":"sk-abcdefgh1234","model":"qwen"}"#)
                .unwrap();
        assert_eq!(login.url(), "http://localhost:1234/v1");
        assert_eq!(login.masked_key(), "sk-…1234");
        assert!(is_official_anthropic("https://api.anthropic.com/"));
        assert!(!is_official_anthropic("https://api.z.ai/api/anthropic"));
    }
}
