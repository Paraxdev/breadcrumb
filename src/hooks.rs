//! Claude Code hooks: session context injection, background sync on exit, and settings.json management.

use crate::util::{atomic_write, path_str, read_text};
use crate::vault::Vault;
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const START_SUFFIX: &str = "hook session-start";
pub const END_SUFFIX: &str = "hook session-end";
pub const START_MATCHER: &str = "startup|resume|clear|compact";
pub const CONTEXT_CAP: usize = 3000;
const RECENT: usize = 10;

#[derive(Debug, Default)]
pub struct HookInput {
    pub cwd: Option<PathBuf>,
    pub source: Option<String>,
}

pub fn parse_input(stdin: &str) -> HookInput {
    let v: Value = serde_json::from_str(stdin.trim()).unwrap_or(Value::Null);
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::trim).filter(|x| !x.is_empty()).map(str::to_string);
    HookInput { cwd: s("cwd").map(PathBuf::from).filter(|p| p.is_dir()), source: s("source") }
}

/// The SessionStart hook JSON printed to stdout.
pub fn session_start(root: &Path, input: &HookInput, fallback_cwd: &Path) -> Result<String> {
    let v = Vault::open(root)?;
    let cwd = input.cwd.as_deref().unwrap_or(fallback_cwd);
    let project = v.detect_project_fast(cwd);
    let body = crate::ops::context_with(&v, project.as_deref(), crate::ops::ContextOpts { recent: RECENT, kinds: false })?;
    let text = cap(body.trim_end(), CONTEXT_CAP);
    Ok(serde_json::to_string(&json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": text}}))?)
}

/// Cuts at a line boundary so the total, including the trailing hint, stays within `max` chars.
pub fn cap(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    const HINT: &str = "\n[truncated, use search or get for more]";
    let budget = max.saturating_sub(HINT.chars().count());
    let mut out = String::new();
    for line in text.lines() {
        if out.chars().count() + line.chars().count() + 1 > budget {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    if out.is_empty() {
        out = text.chars().take(budget).collect();
    }
    format!("{}{HINT}", out.trim_end())
}

pub fn needs_sync(v: &Vault) -> bool {
    v.is_git() && (crate::git::is_dirty(&v.root) || crate::git::ahead_of_origin(&v.root) > 0)
}

/// Starts `breadcrumb sync` detached from the console with output appended to <vault>/.local/sync.log.
pub fn spawn_background_sync(root: &Path) -> Result<()> {
    let exe = std::env::current_exe().context("cannot determine the breadcrumb executable path")?;
    let log_dir = root.join(".local");
    std::fs::create_dir_all(&log_dir)?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(log_dir.join("sync.log"))?;
    let mut c = Command::new(exe);
    c.arg("--vault").arg(root).arg("sync").stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
    }
    c.spawn().context("starting background sync")?;
    Ok(())
}

pub fn default_settings_path() -> Result<PathBuf> {
    Ok(dirs::home_dir().context("cannot determine home directory, pass --settings")?.join(".claude").join("settings.json"))
}

pub fn exe_command(vault: Option<&Path>, suffix: &str) -> Result<String> {
    let exe = std::env::current_exe().context("cannot determine the breadcrumb executable path")?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let mut cmd = format!("\"{}\"", path_str(&exe));
    if let Some(v) = vault {
        cmd.push_str(&format!(" --vault \"{}\"", path_str(v)));
    }
    cmd.push(' ');
    cmd.push_str(suffix);
    Ok(cmd)
}

fn load_settings(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let text = read_text(path)?;
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).with_context(|| format!("parsing {}", path.display()))?;
    if !v.is_object() {
        bail!("{} is not a JSON object", path.display());
    }
    Ok(v)
}

fn save_settings(path: &Path, v: &Value) -> Result<()> {
    if path.exists() {
        let bak = PathBuf::from(format!("{}.breadcrumb.bak", path.display()));
        if !bak.exists() {
            std::fs::copy(path, &bak).with_context(|| format!("writing backup {}", bak.display()))?;
        }
    }
    let mut text = serde_json::to_string_pretty(v)?;
    text.push('\n');
    atomic_write(path, text.as_bytes())
}

fn is_breadcrumb(hook: &Value, suffix: &str) -> bool {
    hook.get("command").and_then(Value::as_str).is_some_and(|c| c.trim_end().ends_with(suffix))
}

/// Points every breadcrumb hook with `suffix` at `command`. Returns false when none exists.
fn update_existing(entries: &mut [Value], suffix: &str, command: &str) -> bool {
    let mut found = false;
    for e in entries.iter_mut() {
        if let Some(hs) = e.get_mut("hooks").and_then(Value::as_array_mut) {
            for h in hs.iter_mut().filter(|h| is_breadcrumb(h, suffix)) {
                h["command"] = json!(command);
                found = true;
            }
        }
    }
    found
}

/// Drops breadcrumb hooks with `suffix`, then entries and event arrays left empty by that.
fn remove_from(hooks: &mut Map<String, Value>, event: &str, suffix: &str) -> bool {
    let Some(entries) = hooks.get_mut(event).and_then(Value::as_array_mut) else { return false };
    let mut removed = false;
    for e in entries.iter_mut() {
        if let Some(hs) = e.get_mut("hooks").and_then(Value::as_array_mut) {
            let before = hs.len();
            hs.retain(|h| !is_breadcrumb(h, suffix));
            removed |= hs.len() != before;
        }
    }
    if removed {
        entries.retain(|e| e.get("hooks").and_then(Value::as_array).is_none_or(|hs| !hs.is_empty()));
        if entries.is_empty() {
            hooks.shift_remove(event);
        }
    }
    removed
}

fn ensure(hooks: &mut Map<String, Value>, event: &str, matcher: Option<&str>, suffix: &str, command: &str) -> Result<()> {
    let entries = hooks.entry(event.to_string()).or_insert_with(|| json!([]));
    let Some(entries) = entries.as_array_mut() else { bail!("hooks.{event} in settings is not an array") };
    if update_existing(entries, suffix, command) {
        return Ok(());
    }
    let mut entry = Map::new();
    if let Some(m) = matcher {
        entry.insert("matcher".into(), json!(m));
    }
    entry.insert("hooks".into(), json!([{"type": "command", "command": command, "timeout": 10}]));
    entries.push(Value::Object(entry));
    Ok(())
}

fn hooks_obj(settings: &mut Value) -> Result<&mut Map<String, Value>> {
    let root = settings.as_object_mut().context("settings is not a JSON object")?;
    let h = root.entry("hooks").or_insert_with(|| json!({}));
    h.as_object_mut().context("\"hooks\" in settings is not an object")
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HookChange {
    pub settings: String,
    pub changed: bool,
    pub session_start: Option<String>,
    pub session_end: Option<String>,
}

pub fn install(path: &Path, vault: Option<&Path>, sync: bool) -> Result<HookChange> {
    let mut s = load_settings(path)?;
    let before = s.clone();
    let start = exe_command(vault, START_SUFFIX)?;
    let end = exe_command(vault, END_SUFFIX)?;
    let hooks = hooks_obj(&mut s)?;
    ensure(hooks, "SessionStart", Some(START_MATCHER), START_SUFFIX, &start)?;
    if sync {
        ensure(hooks, "SessionEnd", None, END_SUFFIX, &end)?;
    } else {
        remove_from(hooks, "SessionEnd", END_SUFFIX);
    }
    let changed = s != before;
    if changed {
        save_settings(path, &s)?;
    }
    Ok(HookChange { settings: path_str(path), changed, session_start: Some(start), session_end: sync.then_some(end) })
}

pub fn uninstall(path: &Path) -> Result<HookChange> {
    let mut s = load_settings(path)?;
    let mut changed = false;
    if let Some(root) = s.as_object_mut()
        && let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut)
    {
        changed |= remove_from(hooks, "SessionStart", START_SUFFIX);
        changed |= remove_from(hooks, "SessionEnd", END_SUFFIX);
        if changed && hooks.is_empty() {
            root.shift_remove("hooks");
        }
    }
    if changed {
        save_settings(path, &s)?;
    }
    Ok(HookChange { settings: path_str(path), changed, session_start: None, session_end: None })
}

pub fn status(path: &Path) -> Result<HookChange> {
    let s = load_settings(path)?;
    let find = |event: &str, suffix: &str| -> Option<String> {
        s.get("hooks")?.get(event)?.as_array()?.iter().find_map(|e| {
            e.get("hooks")?.as_array()?.iter().find(|h| is_breadcrumb(h, suffix)).and_then(|h| h.get("command")?.as_str().map(str::to_string))
        })
    };
    Ok(HookChange { settings: path_str(path), changed: false, session_start: find("SessionStart", START_SUFFIX), session_end: find("SessionEnd", END_SUFFIX) })
}
