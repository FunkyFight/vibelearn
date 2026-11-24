//! Langue de l'interface (français / anglais). Le tuteur, lui, répond dans la
//! langue de l'apprenant quelle qu'elle soit.

use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Fr,
    En,
}

static LANG: OnceLock<Lang> = OnceLock::new();

pub fn init(explicit: Option<&str>) {
    let lang = match explicit.map(str::to_lowercase).as_deref() {
        Some(l) if l.starts_with("fr") => Lang::Fr,
        Some(_) => Lang::En,
        None => match sys_locale::get_locale() {
            Some(l) if l.to_lowercase().starts_with("fr") => Lang::Fr,
            Some(_) => Lang::En,
            None => Lang::Fr,
        },
    };
    let _ = LANG.set(lang);
}

pub fn lang() -> Lang {
    *LANG.get().unwrap_or(&Lang::Fr)
}

/// Les deux-points entre un intitulé et sa suite, avec l'espace que le français met devant.
pub fn colon() -> &'static str {
    tr(" : ", ": ")
}

/// Choisit la chaîne correspondant à la langue de l'interface.
pub fn tr(fr: &'static str, en: &'static str) -> &'static str {
    match lang() {
        Lang::Fr => fr,
        Lang::En => en,
    }
}

/// Nom de la langue, tel qu'on le donne au tuteur.
pub fn language_name() -> &'static str {
    match lang() {
        Lang::Fr => "French",
        Lang::En => "English",
    }
}
