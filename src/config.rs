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
    Def { key: "digest_sections", about: "blocks the session digest shows, in this order, a block left out is not shown", words: &[], project: true },
    Def { key: "digest_budget", about: "chars the session digest may take, 1000 or more", words: &[], project: true },
    Def { key: "digest_recent", about: "recent notes the digest lists, unset is 10 at session start and 15 from context", words: &[], project: true },
    Def { key: "handoff_digest_days", about: "other live handoffs older than this many days leave the digest unless pinned", words: &[], project: true },
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
    /// The project's override when it does not hold, with why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignored: Option<String>,
    /// The value in breadcrumb.toml when it does not hold, with why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub broken: Option<String>,
}

fn table(c: &Config) -> Result<toml::Table> {
    let mut t = toml::Table::try_from(c)?;
    t.entry("digest_sections").or_insert_with(|| c.digest_sections.clone().into());
    Ok(t)
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
    crate::util::read_text(&p)?
        .parse::<toml::Table>()
        .map_err(|e| anyhow::anyhow!("{} is not valid TOML ({}), fix it or delete it to drop the project's overrides", crate::util::path_str(&p), crate::util::one_line(e.message())))
}

/// Why `apply` skips the override `k = val` on top of the settings `t`, None when it holds.
fn skip_reason(t: &toml::Table, k: &str, val: &toml::Value) -> Option<String> {
    let Some(d) = SETTINGS.iter().find(|d| d.key == k) else { return Some(format!("{k} is not a setting")) };
    if !d.project {
        return Some(format!("{k} is vault only"));
    }
    if !d.words.is_empty() && !val.as_str().is_some_and(|s| d.words.contains(&s)) {
        return Some(format!("{k} is one of {}", d.words.join(", ")));
    }
    if let Err(e) = check(k, val) {
        return Some(why(e));
    }
    let mut next = t.clone();
    next.insert(k.to_string(), val.clone());
    if next.try_into::<Config>().is_ok() {
        return None;
    }
    Some(match t.get(k) {
        Some(toml::Value::Integer(_)) => format!("{k} takes a whole number of {} or more", floor(k)),
        None if k == "digest_recent" => format!("{k} takes a whole number of 1 or more"),
        Some(toml::Value::Boolean(_)) => format!("{k} takes true or false"),
        _ => format!("{k} takes text"),
    })
}

/// `base` with the overrides that name an overridable setting and hold a valid value, a bad one is skipped.
fn apply(base: &Config, over: &toml::Table) -> Config {
    let Ok(mut t) = table(base) else { return base.clone() };
    for (k, val) in over {
        if skip_reason(&t, k, val).is_none() {
            t.insert(k.clone(), val.clone());
        }
    }
    t.try_into().unwrap_or_else(|_| base.clone())
}

/// What is wrong with a project's settings.toml, as `(key, problem)`, the key empty when the file does not parse.
pub fn override_problems(v: &Vault, slug: &str) -> Vec<(String, String)> {
    let p = settings_path(v, slug);
    let over = match crate::util::read_text(&p).map(|t| t.parse::<toml::Table>()) {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return vec![(String::new(), format!("not valid TOML ({}), none of its overrides hold, fix it or delete it", crate::util::one_line(e.message())))],
        Err(_) => return vec![],
    };
    let Ok(base) = table(&v.config) else { return vec![] };
    let mut out = vec![];
    for (k, val) in &over {
        if let Some(why) = skip_reason(&base, k, val) {
            out.push((k.clone(), format!("{k} = {val} is ignored, {why}")));
        } else if k == "default_kind" && let Some(name) = val.as_str() && v.require_kind(name).is_err() {
            out.push((k.clone(), format!("default_kind = {val} names no kind, so add without a kind fails in {slug}")));
        }
    }
    out
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
            let (mut value, default) = (shown(now.get(d.key)), shown(def.get(d.key)));
            let broken = vault.get(d.key).and_then(|x| check(d.key, x).err().map(|e| format!("{} ignored, {}", shown(Some(x)), why(e))));
            let source = if d.project && over.contains_key(d.key) && value == shown(over.get(d.key)) {
                "project"
            } else if broken.is_some() {
                value = default.clone();
                "default"
            } else if shown(vault.get(d.key)) != default {
                "vault"
            } else {
                "default"
            };
            let ignored = over.get(d.key).and_then(|x| skip_reason(&vault, d.key, x).map(|why| format!("{} ignored, {why}", shown(Some(x)))));
            Setting { key: d.key.to_string(), value, default, about: d.about.to_string(), source: source.into(), overridable: d.project, ignored, broken }
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
    if let Some(b) = &it.broken {
        m.push(format!("vault value {b}"));
    }
    if let Some(i) = &it.ignored {
        m.push(format!("project value {i}"));
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
    let over = match overrides(v, slug) {
        Ok(o) => o,
        Err(e) => return Some(format!("Project {slug} (detected here) overrides nothing, {e}\n")),
    };
    let base = table(&v.config).ok()?;
    let set: Vec<String> = SETTINGS.iter().filter_map(|d| over.get(d.key).filter(|x| skip_reason(&base, d.key, x).is_none()).map(|x| format!("{} = {}", d.key, shown(Some(x))))).collect();
    let ignored: Vec<String> = over.iter().filter_map(|(k, x)| skip_reason(&base, k, x).map(|why| format!("{k} = {} ({why})", shown(Some(x))))).collect();
    if set.is_empty() && ignored.is_empty() {
        return None;
    }
    let how = if mcp { format!("slug {slug} lists its settings") } else { format!("`breadcrumb config --project {slug}` lists its settings") };
    let holds = if set.is_empty() { "nothing".to_string() } else { set.join(", ") };
    let skips = if ignored.is_empty() { String::new() } else { format!(" and ignores {}", ignored.join(", ")) };
    Some(format!("Project {slug} (detected here) overrides {holds}{skips}, {how}\n"))
}

/// A `check` refusal without the value, for lines that already name it.
fn why(e: anyhow::Error) -> String {
    e.to_string().split(", got ").next().unwrap_or_default().to_string()
}

/// Values in breadcrumb.toml that parse but do not hold, so the default does: a line per key, for doctor.
pub fn vault_problems(v: &Vault) -> Vec<String> {
    let (Ok(vault), Ok(def)) = (table(&v.config), table(&Config::default())) else { return vec![] };
    SETTINGS
        .iter()
        .filter_map(|d| {
            let x = vault.get(d.key)?;
            let e = check(d.key, x).err()?;
            let dflt = shown(def.get(d.key));
            Some(format!("{} = {x} is ignored, {}, the default {} holds", d.key, why(e), if dflt.is_empty() { "unset" } else { &dflt }))
        })
        .collect()
}

/// The least value of a number setting.
fn floor(key: &str) -> u64 {
    match key {
        "digest_budget" => crate::digest::BUDGET_MIN as u64,
        "digest_recent" => 1,
        _ => 0,
    }
}

/// What a value's type does not catch: a number below its floor, a digest_sections list that names a section twice or
/// one that does not exist.
fn check(key: &str, val: &toml::Value) -> Result<()> {
    if key == "digest_sections" {
        let Some(names) = val.as_str() else { bail!("digest_sections takes text, section names separated by commas") };
        crate::digest::parse_sections(names)?;
    }
    if let Some(n) = val.as_integer()
        && n < floor(key) as i64
    {
        let hint = if key == "digest_recent" { ", leave recent out of digest_sections to hide the list" } else { "" };
        bail!("{key} takes a whole number of {} or more, got {n}{hint}", floor(key));
    }
    Ok(())
}

/// The value `checked` stores: digest_sections as its names joined by commas, digest_recent as a number though it has no default.
fn refine(key: &str, raw: &str, val: Option<toml::Value>) -> Result<Option<toml::Value>> {
    let val = match key {
        "digest_sections" => Some(toml::Value::String(crate::digest::parse_sections(raw)?.join(","))),
        "digest_recent" => parse_like(Some(&toml::Value::Integer(0)), key, raw)?,
        _ => val,
    };
    if let Some(x) = &val {
        check(key, x)?;
    }
    Ok(val)
}

fn parse_like(old: Option<&toml::Value>, key: &str, raw: &str) -> Result<Option<toml::Value>> {
    let raw = raw.trim();
    Ok(Some(match old {
        Some(toml::Value::Integer(_)) => toml::Value::Integer(raw.parse::<u64>().with_context(|| format!("{key} takes a whole number of {} or more, got '{raw}'", floor(key)))? as i64),
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
fn checked(v: &Vault, key: &str, raw: &str) -> Result<(Config, Option<toml::Value>)> {
    let base = &v.config;
    let d = def(key)?;
    if !d.words.is_empty() && !d.words.contains(&raw.trim()) {
        bail!("{} is one of {}, got '{}'", d.key, d.words.join(", "), raw.trim());
    }
    let mut t = table(base)?;
    let dflt = table(&Config::default())?;
    let val = parse_like(t.get(d.key).or(dflt.get(d.key)), d.key, raw)?;
    let val = refine(d.key, raw, val)?;
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
    if d.key == "default_kind" && v.require_kind(&cfg.default_kind).is_err() {
        let names: Vec<String> = v.load_kinds().into_iter().map(|k| k.name).collect();
        bail!("default_kind takes a kind the vault has, '{}' is none, kinds: {}", cfg.default_kind, names.join(", "));
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
    let (cfg, _) = checked(v, key, raw)?;
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
    let val = checked(v, key, raw)?.1.with_context(|| format!("{key} cannot be empty"))?;
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
                let d = def(k)?;
                if !d.project {
                    bail!("{} is vault only, a project cannot override it, call the same action without slug to set it for every project", d.key);
                }
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
