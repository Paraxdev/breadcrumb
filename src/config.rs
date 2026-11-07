//! `breadcrumb config`: the vault's settings in breadcrumb.toml, listed with what each does and changed one at a time,
//! so the user or an agent can reshape how the vault behaves without editing the file.

use crate::vault::{CONFIG_FILE, Config, Vault};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::fmt::Write;

pub const REVIEW_BY: [&str; 2] = ["user", "agent"];

/// Every setting `config set` accepts, with the line `config` shows for it.
const SETTINGS: [(&str, &str); 8] = [
    ("review_by", "who checks items in review: user, or agent (a session or subagent that did not build the item, a pass needs evidence)"),
    ("feedback_remind_days", "the digest asks agents to collect the user's feedback when the newest feedback note is this many days old, 0 is off"),
    ("stale_days", "notes not verified or updated for this many days are marked possibly stale"),
    ("pin_review_days", "doctor and pins flag a pin not verified for this many days, 0 is off"),
    ("default_kind", "kind of a note added without --kind"),
    ("auto_commit", "commit every write to git"),
    ("lock_timeout_secs", "seconds a write waits for the vault lock"),
    ("remote", "git remote sync pushes to, empty to unset"),
];

#[derive(Debug, Serialize)]
pub struct Setting {
    pub key: String,
    pub value: String,
    pub default: String,
    pub about: String,
}

fn table(c: &Config) -> Result<toml::Table> {
    Ok(toml::Table::try_from(c)?)
}

fn shown(v: Option<&toml::Value>) -> String {
    match v {
        Some(toml::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

pub fn settings(v: &Vault) -> Result<Vec<Setting>> {
    let (now, def) = (table(&v.config)?, table(&Config::default())?);
    Ok(SETTINGS
        .iter()
        .map(|(k, about)| Setting { key: k.to_string(), value: shown(now.get(*k)), default: shown(def.get(*k)), about: about.to_string() })
        .collect())
}

pub fn text(list: &[Setting]) -> String {
    let mut s = format!("Settings in {CONFIG_FILE}, change one with `breadcrumb config set <key> <value>`:\n");
    for it in list {
        let changed = if it.value == it.default { String::new() } else { format!(" (default {})", if it.default.is_empty() { "unset" } else { &it.default }) };
        let value = if it.value.is_empty() { "unset" } else { &it.value };
        let _ = writeln!(s, "  {} = {value}{changed}\n      {}", it.key, it.about);
    }
    s
}

fn parse_like(old: Option<&toml::Value>, key: &str, raw: &str) -> Result<Option<toml::Value>> {
    let raw = raw.trim();
    Ok(Some(match old {
        Some(toml::Value::Integer(_)) => toml::Value::Integer(raw.parse::<u64>().with_context(|| format!("{key} takes a whole number of 0 or more, got '{raw}'"))? as i64),
        Some(toml::Value::Boolean(_)) => toml::Value::Boolean(match raw.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => true,
            "false" | "no" | "off" | "0" => false,
            _ => bail!("{key} takes true or false, got '{raw}'"),
        }),
        _ if raw.is_empty() => return Ok(None),
        _ => toml::Value::String(raw.to_string()),
    }))
}

/// Sets one setting, saves breadcrumb.toml and commits it. Returns the value before.
pub fn set(v: &mut Vault, key: &str, raw: &str) -> Result<String> {
    let key = key.trim();
    if !SETTINGS.iter().any(|(k, _)| *k == key) {
        let keys: Vec<&str> = SETTINGS.iter().map(|(k, _)| *k).collect();
        bail!("unknown setting '{key}', settings are: {}", keys.join(", "));
    }
    let mut t = table(&v.config)?;
    let def = table(&Config::default())?;
    let before = shown(t.get(key));
    match parse_like(t.get(key).or(def.get(key)), key, raw)? {
        Some(val) => {
            t.insert(key.to_string(), val);
        }
        None if key == "remote" => {
            t.remove(key);
        }
        None => bail!("{key} cannot be empty"),
    }
    let cfg: Config = t.try_into().with_context(|| format!("'{raw}' is not a valid {key}"))?;
    if !REVIEW_BY.contains(&cfg.review_by.as_str()) {
        bail!("review_by is user or agent, got '{}'", cfg.review_by);
    }
    let _g = v.lock()?;
    v.config = cfg;
    v.save_config()?;
    v.commit(&[v.root.join(CONFIG_FILE)], &format!("breadcrumb: config {key} = {}", raw.trim()))?;
    Ok(before)
}

pub fn agent_review(v: &Vault) -> bool {
    v.config.review_by == "agent"
}
