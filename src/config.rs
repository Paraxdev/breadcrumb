//! `breadcrumb config`: the vault's settings in breadcrumb.toml, listed with what each does and changed one at a time,
//! so the user or an agent can reshape how the vault behaves without editing the file. A project overrides some of them
//! in `projects/<slug>/settings.toml`, a file of its own so an older binary rewriting project.toml cannot drop them.

use crate::vault::{CONFIG_FILE, Config, Vault};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Write;

pub const REVIEW_BY: [&str; 2] = ["user", "agent"];
pub const JUDGE_NUDGE: [&str; 3] = ["off", "session", "daily"];
pub const SETTINGS_FILE: &str = "settings.toml";

struct Def {
    key: &'static str,
    about: &'static str,
    words: &'static [&'static str],
    /// A project may override it, otherwise it holds for the whole vault only.
    project: bool,
}

/// Every setting `config set` accepts.
const SETTINGS: &[Def] = &[
    Def { key: "review_by", about: "who checks items in review: user, or agent (a session or subagent that did not build the item, a pass needs evidence)", words: &REVIEW_BY, project: true },
    Def { key: "judge_nudge", about: "how often the digest asks agents to judge breadcrumb itself with `breadcrumb judge`: off, session or daily", words: &JUDGE_NUDGE, project: true },
    Def { key: "judge_project", about: "project that judgments of breadcrumb are filed in, wherever the agent works", words: &[], project: false },
    Def { key: "stale_days", about: "notes not verified or updated for this many days are marked possibly stale", words: &[], project: true },
    Def { key: "pin_review_days", about: "doctor and pins flag a pin not verified for this many days, 0 is off", words: &[], project: true },
    Def { key: "default_kind", about: "kind of a note added without --kind", words: &[], project: true },
    Def { key: "recall_notes", about: "notes the prompt hook names for a session's first prompt, 0 silences it", words: &[], project: true },
    Def { key: "file_notes", about: "notes the file hook names for a file being read or edited, 0 silences it", words: &[], project: true },
    Def { key: "auto_commit", about: "commit every write to git", words: &[], project: false },
    Def { key: "lock_timeout_secs", about: "seconds a write waits for the vault lock", words: &[], project: false },
    Def { key: "remote", about: "git remote sync pushes to, empty to unset", words: &[], project: false },
];

#[derive(Debug, Serialize)]
pub struct Setting {
    pub key: String,
    pub value: String,
    pub default: String,
    pub about: String,
    /// Where the value comes from: default, vault or project.
    pub source: String,
    pub overridable: bool,
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

fn def(key: &str) -> Result<&'static Def> {
    let key = key.trim();
    SETTINGS.iter().find(|d| d.key == key).ok_or_else(|| {
        let keys: Vec<&str> = SETTINGS.iter().map(|d| d.key).collect();
        anyhow::anyhow!("unknown setting '{key}', settings are: {}", keys.join(", "))
    })
}

fn settings_path(v: &Vault, slug: &str) -> std::path::PathBuf {
    v.project_dir(slug).join(SETTINGS_FILE)
}

/// The overrides a project's settings.toml holds as written, empty when it has none.
pub fn overrides(v: &Vault, slug: &str) -> Result<toml::Table> {
    let p = settings_path(v, slug);
    if !p.exists() {
        return Ok(toml::Table::new());
    }
    crate::util::read_text(&p)?.parse::<toml::Table>().with_context(|| format!("parsing {}", p.display()))
}

/// `base` with the overrides that name an overridable setting and hold a valid value, a bad one is skipped.
fn apply(base: &Config, over: &toml::Table) -> Config {
    let Ok(mut t) = table(base) else { return base.clone() };
    for (k, val) in over {
        let Some(d) = SETTINGS.iter().find(|d| d.key == k && d.project) else { continue };
        if !d.words.is_empty() && !val.as_str().is_some_and(|s| d.words.contains(&s)) {
            continue;
        }
        let mut next = t.clone();
        next.insert(k.clone(), val.clone());
        if next.clone().try_into::<Config>().is_ok() {
            t = next;
        }
    }
    t.try_into().unwrap_or_else(|_| base.clone())
}

/// The settings that hold in `project`: its override, else the vault's value, else the default. A hook must not fail
/// on a broken settings.toml, so one that does not parse counts as no overrides.
pub fn effective(v: &Vault, project: Option<&str>) -> Config {
    match project {
        Some(slug) if crate::util::is_valid_slug(slug) => apply(&v.config, &overrides(v, slug).unwrap_or_default()),
        Some(_) => v.config.clone(),
        None => v.config.clone(),
    }
}

/// The vault with the settings of `project` in place, for code that reads `v.config` throughout one project's work.
pub fn scoped(v: &Vault, project: Option<&str>) -> Vault {
    Vault { root: v.root.clone(), config: effective(v, project) }
}

/// Effective settings of every project at once, for code that walks notes of many projects.
pub struct Effective {
    vault: Config,
    projects: HashMap<String, Config>,
}

impl Effective {
    pub fn load(v: &Vault) -> Effective {
        let mut projects = HashMap::new();
        if let Ok(rd) = std::fs::read_dir(v.root.join("projects")) {
            for e in rd.flatten() {
                let slug = e.file_name().to_string_lossy().to_string();
                if e.path().join(SETTINGS_FILE).exists() {
                    projects.insert(slug.clone(), effective(v, Some(&slug)));
                }
            }
        }
        Effective { vault: v.config.clone(), projects }
    }

    pub fn of(&self, project: Option<&str>) -> &Config {
        project.and_then(|p| self.projects.get(p)).unwrap_or(&self.vault)
    }
}

pub fn agent_review(v: &Vault, project: Option<&str>) -> bool {
    effective(v, project).review_by == "agent"
}

fn known_project(v: &Vault, slug: &str) -> Result<String> {
    let slug = crate::ops::check_slug(slug)?;
    if v.load_project(&slug)?.is_none() {
        return Err(crate::ops::unknown_project_error(v, &slug));
    }
    Ok(slug)
}

/// Every setting with the value that holds for `project`, or for the vault without one.
pub fn settings(v: &Vault, project: Option<&str>) -> Result<Vec<Setting>> {
    let over = match project {
        Some(p) => overrides(v, &known_project(v, p)?)?,
        None => toml::Table::new(),
    };
    let eff = apply(&v.config, &over);
    let (vault, now, def) = (table(&v.config)?, table(&eff)?, table(&Config::default())?);
    Ok(SETTINGS
        .iter()
        .map(|d| {
            let (value, default) = (shown(now.get(d.key)), shown(def.get(d.key)));
            let source = if d.project && over.contains_key(d.key) && value == shown(over.get(d.key)) {
                "project"
            } else if shown(vault.get(d.key)) != default {
                "vault"
            } else {
                "default"
            };
            Setting { key: d.key.to_string(), value, default, about: d.about.to_string(), source: source.into(), overridable: d.project }
        })
        .collect())
}

fn value_text(it: &Setting) -> String {
    if it.value.is_empty() { "unset".into() } else { it.value.clone() }
}

fn marks(it: &Setting, project: bool) -> String {
    let mut m: Vec<String> = vec![];
    if project && it.source != "default" {
        m.push(it.source.clone());
    }
    if it.value != it.default {
        m.push(format!("default {}", if it.default.is_empty() { "unset" } else { &it.default }));
    }
    if !it.overridable {
        m.push("vault only".into());
    }
    if m.is_empty() { String::new() } else { format!(" ({})", m.join(", ")) }
}

/// One setting as `key = value (marks)`.
pub fn line(it: &Setting, project: bool) -> String {
    format!("{} = {}{}", it.key, value_text(it), marks(it, project))
}

/// The listing, worded for the CLI or for the MCP `project` tool.
pub fn text(list: &[Setting], project: Option<&str>, mcp: bool) -> String {
    let mut s = match (project, mcp) {
        (None, false) => format!("Settings in {CONFIG_FILE}, change one with `breadcrumb config set <key> <value>`, a project overrides any not marked vault only with `--project <slug>`:\n"),
        (None, true) => "Vault settings, set one with key and value, a project overrides any not marked vault only when slug is given:\n".to_string(),
        (Some(p), false) => format!("Settings for project {p}, `breadcrumb config set <key> <value> --project {p}` overrides one, `config unset <key> --project {p}` drops it:\n"),
        (Some(p), true) => format!("Settings for project {p}, set one with key and value to override it here, an empty value drops the override:\n"),
    };
    for it in list {
        let _ = writeln!(s, "  {}\n      {}", line(it, project.is_some()), it.about);
    }
    s
}

/// The closing line of a vault listing when the detected project overrides anything.
pub fn overrides_line(v: &Vault, slug: &str, mcp: bool) -> Option<String> {
    let over = overrides(v, slug).ok()?;
    let set: Vec<String> = SETTINGS.iter().filter(|d| d.project).filter_map(|d| over.get(d.key).map(|x| format!("{} = {}", d.key, shown(Some(x))))).collect();
    if set.is_empty() {
        return None;
    }
    let how = if mcp { format!("slug {slug} lists its settings") } else { format!("`breadcrumb config --project {slug}` lists its settings") };
    Some(format!("Project {slug} (detected here) overrides {}, {how}\n", set.join(", ")))
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

/// Checks `raw` for `key` and returns the vault config with it in place, or None for an empty remote.
fn checked(base: &Config, key: &str, raw: &str) -> Result<(Config, Option<toml::Value>)> {
    let d = def(key)?;
    if !d.words.is_empty() && !d.words.contains(&raw.trim()) {
        bail!("{} is one of {}, got '{}'", d.key, d.words.join(", "), raw.trim());
    }
    let mut t = table(base)?;
    let dflt = table(&Config::default())?;
    let val = parse_like(t.get(d.key).or(dflt.get(d.key)), d.key, raw)?;
    match &val {
        Some(x) => {
            t.insert(d.key.to_string(), x.clone());
        }
        None if d.key == "remote" => {
            t.remove(d.key);
        }
        None => bail!("{} cannot be empty", d.key),
    }
    let cfg: Config = t.try_into().with_context(|| format!("'{raw}' is not a valid {}", d.key))?;
    if d.key == "judge_project" {
        crate::ops::check_slug(&cfg.judge_project)?;
    }
    Ok((cfg, val))
}

fn save_vault(v: &mut Vault, cfg: Config, msg: &str) -> Result<()> {
    let _g = v.lock()?;
    v.config = cfg;
    v.save_config()?;
    v.commit(&[v.root.join(CONFIG_FILE)], msg)?;
    Ok(())
}

/// Sets one setting for the whole vault, saves breadcrumb.toml and commits it. Returns the value before.
pub fn set(v: &mut Vault, key: &str, raw: &str) -> Result<String> {
    let key = def(key)?.key;
    let before = shown(table(&v.config)?.get(key));
    let (cfg, _) = checked(&v.config, key, raw)?;
    save_vault(v, cfg, &format!("breadcrumb: config {key} = {}", raw.trim()))?;
    Ok(before)
}

fn project_key(key: &str) -> Result<&'static str> {
    let d = def(key)?;
    if !d.project {
        bail!("{} is vault only, a project cannot override it, set it for every project with `breadcrumb config set {} <value>`", d.key, d.key);
    }
    Ok(d.key)
}

fn get(c: &Config, key: &str) -> String {
    table(c).map(|t| shown(t.get(key))).unwrap_or_default()
}

fn write_overrides(v: &Vault, slug: &str, over: &toml::Table, msg: &str) -> Result<()> {
    let p = settings_path(v, slug);
    if over.is_empty() {
        if p.exists() {
            std::fs::remove_file(&p).with_context(|| format!("removing {}", p.display()))?;
        }
    } else {
        crate::util::atomic_write(&p, toml::to_string_pretty(over)?.as_bytes())?;
    }
    v.commit(&[p], msg)?;
    Ok(())
}

/// Overrides one setting for a project, an empty value drops the override. Returns the effective value before and after.
pub fn set_in(v: &Vault, slug: &str, key: &str, raw: &str) -> Result<(String, String)> {
    let key = project_key(key)?;
    let slug = known_project(v, slug)?;
    if raw.trim().is_empty() {
        return unset(&mut v.clone(), key, Some(&slug));
    }
    let val = checked(&v.config, key, raw)?.1.with_context(|| format!("{key} cannot be empty"))?;
    let _g = v.lock()?;
    let mut over = overrides(v, &slug)?;
    let before = get(&apply(&v.config, &over), key);
    over.insert(key.to_string(), val);
    write_overrides(v, &slug, &over, &format!("breadcrumb: config {key} = {} in {slug}", raw.trim()))?;
    Ok((before, get(&apply(&v.config, &over), key)))
}

/// Drops a project's override, or without a project sets the vault value back to its default. Returns the value before and after.
pub fn unset(v: &mut Vault, key: &str, project: Option<&str>) -> Result<(String, String)> {
    match project {
        Some(slug) => {
            let key = project_key(key)?;
            let slug = known_project(v, slug)?;
            let _g = v.lock()?;
            let mut over = overrides(v, &slug)?;
            let before = get(&apply(&v.config, &over), key);
            if over.remove(key).is_some() {
                write_overrides(v, &slug, &over, &format!("breadcrumb: config unset {key} in {slug}"))?;
            }
            Ok((before, get(&apply(&v.config, &over), key)))
        }
        None => {
            let key = def(key)?.key;
            let before = get(&v.config, key);
            let mut t = table(&v.config)?;
            t.remove(key);
            let cfg: Config = t.try_into()?;
            save_vault(v, cfg, &format!("breadcrumb: config unset {key}"))?;
            Ok((before, get(&v.config, key)))
        }
    }
}

/// `before -> after` for a change, `unset` for an empty value.
pub fn change_text(key: &str, project: Option<&str>, before: &str, after: &str) -> String {
    let at = project.map(|p| format!(" in {p}")).unwrap_or_default();
    let show = |s: &str| if s.is_empty() { "unset".to_string() } else { s.to_string() };
    format!("{key}{at}: {} -> {}\n", show(before), show(after))
}

/// The MCP `project` tool's action config: list, read one, or set one, for `slug` when given.
pub fn mcp(v: &Vault, slug: Option<&str>, key: Option<&str>, value: Option<&str>, detected: Option<&str>) -> Result<String> {
    let slug = slug.map(str::trim).filter(|s| !s.is_empty());
    match (key.map(str::trim).filter(|k| !k.is_empty()), value) {
        (None, _) => {
            let mut s = text(&settings(v, slug)?, slug, true);
            if slug.is_none()
                && let Some(l) = detected.and_then(|d| overrides_line(v, d, true))
            {
                s.push_str(&l);
            }
            Ok(s)
        }
        (Some(k), None) => {
            let d = def(k)?;
            let list = settings(v, slug)?;
            let it = list.iter().find(|s| s.key == d.key).context("unknown setting")?;
            Ok(format!("{}\n", line(it, slug.is_some())))
        }
        (Some(k), Some(raw)) => match slug {
            Some(p) => {
                let (before, after) = set_in(v, p, k, raw)?;
                Ok(change_text(def(k)?.key, Some(p), &before, &after))
            }
            None => {
                let mut v = v.clone();
                let before = set(&mut v, k, raw)?;
                let key = def(k)?.key;
                Ok(change_text(key, None, &before, &get(&v.config, key)))
            }
        },
    }
}
