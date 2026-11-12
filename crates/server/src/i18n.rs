//! E-mails in each person's language. Texts are written in English in the code (`tr(lang,
//! "Hi {name},", &[("name", …)])`) and looked up in `locales/<lang>.json` (keyed by the English
//! text, like the web app's catalogs); a missing translation falls back to English.
//!
//! A person's language: the one in their profile, else the workspace's default, else the one their
//! browser asked for (recorded when the app loads), else English.
//! People without an account (requesters) get the workspace's default.

use std::collections::HashMap;
use std::sync::OnceLock;

/// The languages e-mails are translated into (besides English).
const CATALOGS: [(&str, &str); 3] =
    [("de", include_str!("../locales/de.json")), ("fr", include_str!("../locales/fr.json")), ("it", include_str!("../locales/it.json"))];

fn catalogs() -> &'static HashMap<&'static str, HashMap<String, String>> {
    static C: OnceLock<HashMap<&'static str, HashMap<String, String>>> = OnceLock::new();
    C.get_or_init(|| CATALOGS.iter().map(|(l, src)| (*l, serde_json::from_str(src).unwrap_or_default())).collect())
}

/// The language to write in: the first of `choices` that's translated (`de-CH` counts as `de`),
/// else English.
pub fn lang<'a>(choices: impl IntoIterator<Item = Option<&'a str>>) -> &'static str {
    for c in choices.into_iter().flatten() {
        let code = c.trim().split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
        if code == "en" {
            return "en";
        }
        if let Some((l, _)) = CATALOGS.iter().find(|(l, _)| *l == code) {
            return l;
        }
    }
    "en"
}

/// A person's e-mail language: their own, else their workspace's default, else their browser's.
pub async fn of_user(db: &sqlx::PgPool, user: uuid::Uuid) -> &'static str {
    let row: Option<(Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT u.locale, w.settings->>'defaultLanguage', u.preferences->>'browserLanguage' FROM users u JOIN workspaces w ON w.id = u.workspace_id WHERE u.id = $1",
    )
    .bind(user)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    let (own, default, browser) = row.unwrap_or((None, None, None));
    lang([own.as_deref(), default.as_deref(), browser.as_deref()])
}

/// The workspace's default language (for people without an account).
pub async fn of_workspace(db: &sqlx::PgPool, workspace: uuid::Uuid) -> &'static str {
    let default: Option<String> = sqlx::query_scalar("SELECT settings->>'defaultLanguage' FROM workspaces WHERE id = $1")
        .bind(workspace)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .flatten();
    lang([default.as_deref()])
}

/// The default language of a project's workspace (for e-mails to its requesters).
pub async fn of_project(db: &sqlx::PgPool, project: uuid::Uuid) -> &'static str {
    let default: Option<String> =
        sqlx::query_scalar("SELECT w.settings->>'defaultLanguage' FROM projects p JOIN workspaces w ON w.id = p.workspace_id WHERE p.id = $1")
            .bind(project)
            .fetch_optional(db)
            .await
            .ok()
            .flatten()
            .flatten();
    lang([default.as_deref()])
}

/// `key` (the English text) in `lang`, with `{name}` placeholders filled from `args`.
pub fn tr(lang: &str, key: &str, args: &[(&str, &str)]) -> String {
    let text = catalogs().get(lang).and_then(|c| c.get(key)).map(String::as_str).unwrap_or(key);
    let mut out = text.to_string();
    for (k, v) in args {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// A short day and time (`Mon 14:05`, `Mo 14:05`, `lun. 14:05`) for digests.
pub fn day_time(lang: &str, at: chrono::NaiveDateTime) -> String {
    use chrono::Datelike;
    let days: [&str; 7] = match lang {
        "de" => ["Mo", "Di", "Mi", "Do", "Fr", "Sa", "So"],
        "fr" => ["lun.", "mar.", "mer.", "jeu.", "ven.", "sam.", "dim."],
        "it" => ["lun", "mar", "mer", "gio", "ven", "sab", "dom"],
        _ => ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
    };
    format!("{} {}", days[at.weekday().num_days_from_monday() as usize], at.format("%H:%M"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_and_fallbacks() {
        assert_eq!(lang([Some("de-CH")]), "de");
        assert_eq!(lang([None, Some("fr")]), "fr");
        assert_eq!(lang([Some("es"), Some("it")]), "it", "a language without e-mails, then the workspace's");
        assert_eq!(lang([Some("en"), Some("de")]), "en");
        assert_eq!(lang([None, None]), "en");
        assert_eq!(tr("de", "Hi {name},", &[("name", "Max")]), "Hallo Max,");
        assert_eq!(tr("xx", "Hi {name},", &[("name", "Max")]), "Hi Max,");
        let at = chrono::NaiveDate::from_ymd_opt(2026, 11, 12).unwrap().and_hms_opt(14, 5, 0).unwrap();
        assert_eq!((day_time("en", at), day_time("de", at)), ("Thu 14:05".to_string(), "Do 14:05".to_string()));
    }

    /// Every text the code passes to `tr` is in every catalog, with its placeholders.
    #[test]
    fn every_text_is_translated() {
        let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let mut keys: Vec<String> = Vec::new();
        let mut stack = vec![std::path::PathBuf::from(src)];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("i18n.rs") {
                    let code = std::fs::read_to_string(&p).unwrap();
                    for (i, _) in code.match_indices("tr(") {
                        // `tr(lang, "…"` with a string literal as the second argument (not `attr(`, `.tr(`)
                        if code[..i].chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.') {
                            continue;
                        }
                        let rest = &code[i + 3..];
                        let Some(comma) = rest.find(',') else { continue };
                        if rest[..comma].contains(['(', ')', '"', '\n']) {
                            continue;
                        }
                        let after = rest[comma + 1..].trim_start();
                        let Some(lit) = after.strip_prefix('"') else { continue };
                        let mut key = String::new();
                        let mut chars = lit.chars();
                        while let Some(c) = chars.next() {
                            match c {
                                '\\' => match chars.next() {
                                    Some('n') => key.push('\n'),
                                    Some(o) => key.push(o),
                                    None => break,
                                },
                                '"' => break,
                                c => key.push(c),
                            }
                        }
                        keys.push(key);
                    }
                }
            }
        }
        assert!(keys.len() > 20, "found {} texts", keys.len());
        let placeholders = |s: &str| {
            let mut v: Vec<String> = s.split('{').skip(1).filter_map(|p| p.split_once('}').map(|(n, _)| n.to_string())).collect();
            v.sort();
            v
        };
        let mut problems = Vec::new();
        for (l, _) in CATALOGS {
            let c = &catalogs()[l];
            for k in &keys {
                match c.get(k) {
                    None => problems.push(format!("{l}: missing {k:?}")),
                    Some(v) if placeholders(v) != placeholders(k) => problems.push(format!("{l}: placeholders of {k:?}")),
                    _ => {}
                }
            }
            for k in c.keys() {
                if !keys.contains(k) {
                    problems.push(format!("{l}: unused {k:?}"));
                }
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }
}
