//! Claude Code hooks: session context injection, background sync on exit, and settings.json management.

use crate::recall;
use crate::util::{atomic_write, path_str, read_text};
use crate::vault::Vault;
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const START_SUFFIX: &str = "hook session-start";
pub const END_SUFFIX: &str = "hook session-end";
pub const PROMPT_SUFFIX: &str = "hook prompt";
pub const FILE_SUFFIX: &str = "hook file-touch";
pub const START_MATCHER: &str = "startup|resume|clear|compact";
pub const FILE_MATCHER: &str = "Read|Edit|Write|MultiEdit|NotebookEdit";
pub const FILE_EVENT: &str = "PreToolUse";
pub const PROMPT_EVENT: &str = "UserPromptSubmit";
const SESSION_TIMEOUT: u64 = 10;
const RECALL_TIMEOUT: u64 = 5;
pub const CONTEXT_CAP: usize = 3000;
const RECENT: usize = 10;

#[derive(Debug, Default)]
pub struct HookInput {
    pub cwd: Option<PathBuf>,
    pub source: Option<String>,
    pub session_id: Option<String>,
    pub prompt: Option<String>,
    pub file_path: Option<String>,
}

pub fn parse_input(stdin: &str) -> HookInput {
    let v: Value = serde_json::from_str(stdin.trim()).unwrap_or(Value::Null);
    let get = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::trim).filter(|x| !x.is_empty()).map(str::to_string);
    let tool = v.get("tool_input").cloned().unwrap_or(Value::Null);
    HookInput {
        cwd: get(&v, "cwd").map(PathBuf::from).filter(|p| p.is_dir()),
        source: get(&v, "source"),
        session_id: get(&v, "session_id"),
        prompt: get(&v, "prompt"),
        file_path: get(&tool, "file_path").or_else(|| get(&tool, "notebook_path")),
    }
}

fn output(event: &str, text: &str) -> Result<String> {
    Ok(serde_json::to_string(&json!({"hookSpecificOutput": {"hookEventName": event, "additionalContext": text}}))?)
}

/// The SessionStart hook JSON printed to stdout. Records the digest's note ids in the session state.
pub fn session_start(root: &Path, input: &HookInput, fallback_cwd: &Path) -> Result<String> {
    let v = Vault::open(root)?;
    let cwd = input.cwd.as_deref().unwrap_or(fallback_cwd);
    let project = v.detect_project_fast(cwd);
    let body = crate::ops::context_with(&v, project.as_deref(), crate::ops::ContextOpts { recent: RECENT, kinds: false })?;
    let text = cap(body.trim_end(), CONTEXT_CAP);
    if let Some(sid) = &input.session_id {
        recall::prune_sessions(root);
        let mut st = recall::load_session(root, sid);
        // After a compact or clear the earlier injections are gone from the context, so they may show again.
        match input.source.as_deref() {
            Some("clear") => st = recall::Session::default(),
            Some("compact") => st.shown.clear(),
            _ => {}
        }
        st.add_shown(recall::ids_in(&text));
        let _ = recall::save_session(root, sid, &st);
    }
    output("SessionStart", &text)
}

/// UserPromptSubmit: on the first real prompt of a session, a one line pointer to notes that match it.
pub fn prompt(root: &Path, input: &HookInput, fallback_cwd: &Path) -> Result<Option<String>> {
    let (Some(sid), Some(text)) = (&input.session_id, &input.prompt) else { return Ok(None) };
    if text.starts_with('/') || recall::prompt_words(text) < recall::RECALL_MIN_WORDS {
        return Ok(None);
    }
    let mut st = recall::load_session(root, sid);
    if st.prompted {
        return Ok(None);
    }
    st.prompted = true;
    let v = Vault::open(root)?;
    let cwd = input.cwd.as_deref().unwrap_or(fallback_cwd);
    let project = v.detect_project_fast(cwd);
    let hits = recall::recall(&v, project.as_deref(), text, &st.shown_set());
    st.add_shown(hits.iter().map(|n| n.id.clone()));
    recall::save_session(root, sid, &st)?;
    if hits.is_empty() {
        return Ok(None);
    }
    Ok(Some(output(PROMPT_EVENT, &recall::recall_line(&v, &hits))?))
}

/// PreToolUse on file tools: notes whose path globs match the file, each id at most once per session.
pub fn file_touch(root: &Path, input: &HookInput, fallback_cwd: &Path) -> Result<Option<String>> {
    let Some(file) = &input.file_path else { return Ok(None) };
    let v = Vault::open(root)?;
    let cwd = input.cwd.as_deref().unwrap_or(fallback_cwd);
    let mut st = input.session_id.as_deref().map(|sid| recall::load_session(root, sid)).unwrap_or_default();
    let Some((line, ids)) = recall::file_notes(&v, cwd, Path::new(file), &st.shown_set()) else { return Ok(None) };
    if let Some(sid) = &input.session_id {
        st.add_shown(ids);
        let _ = recall::save_session(root, sid, &st);
    }
    Ok(Some(output(FILE_EVENT, &line)?))
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

fn import_sig_file(root: &Path) -> PathBuf {
    root.join(".local").join("claude-import.sig")
}

/// Names, sizes and mtimes of the Claude memory and plan files, empty when there are none.
pub fn claude_signature(claude_dir: &Path) -> String {
    let mut parts: Vec<String> = vec![];
    let mut add_dir = |d: &Path| {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.ends_with(".md") {
                continue;
            }
            if let Ok(m) = e.metadata() {
                let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
                parts.push(format!("{}/{name}:{}:{t}", path_str(d), m.len()));
            }
        }
    };
    if let Ok(rd) = std::fs::read_dir(claude_dir.join("projects")) {
        for e in rd.flatten() {
            add_dir(&e.path().join("memory"));
        }
    }
    add_dir(&claude_dir.join("plans"));
    if parts.is_empty() {
        return String::new();
    }
    parts.sort();
    crate::util::sha256_hex(parts.join("\n").as_bytes())
}

/// True when Claude memory files changed since the last background import.
pub fn needs_import(root: &Path) -> bool {
    let Some(dir) = crate::importer::default_claude_dir() else { return false };
    let sig = claude_signature(&dir);
    !sig.is_empty() && read_text(&import_sig_file(root)).map(|s| s.trim().to_string()).unwrap_or_default() != sig
}

/// What the detached SessionEnd child does: import Claude memory quietly, then sync when needed.
pub fn background(root: &Path, import: bool) -> Result<()> {
    let v = Vault::open(root)?;
    if import && let Some(dir) = crate::importer::default_claude_dir().filter(|d| d.is_dir()) {
        let sig = claude_signature(&dir);
        let opts = crate::importer::ClaudeOpts { claude_dir: dir, dry_run: false, project_map: vec![] };
        match crate::importer::import_claude(&v, &opts) {
            Ok(rep) => {
                if let Some(line) = rep.quiet_line() {
                    println!("{} import claude: {line}", crate::util::fmt_time(&crate::util::now()));
                }
                let _ = atomic_write(&import_sig_file(root), sig.as_bytes());
            }
            Err(e) => eprintln!("import claude failed: {e:#}"),
        }
    }
    if needs_sync(&v) {
        let r = crate::export::sync(&v, None)?;
        println!("{}", r.message);
    }
    Ok(())
}

/// Starts `breadcrumb hook background` detached from the console with output appended to <vault>/.local/sync.log.
pub fn spawn_background(root: &Path, import: bool) -> Result<()> {
    let exe = std::env::current_exe().context("cannot determine the breadcrumb executable path")?;
    let log_dir = root.join(".local");
    std::fs::create_dir_all(&log_dir)?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(log_dir.join("sync.log"))?;
    let mut c = Command::new(exe);
    c.arg("--vault").arg(root).args(["hook", "background"]);
    if !import {
        c.arg("--no-import");
    }
    c.stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
    }
    c.spawn().context("starting background import and sync")?;
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

/// The command ends with `suffix`, optionally followed by flags.
fn is_breadcrumb(hook: &Value, suffix: &str) -> bool {
    hook.get("command").and_then(Value::as_str).is_some_and(|c| {
        let c = c.trim_end();
        c.ends_with(suffix) || c.find(&format!("{suffix} --")).is_some_and(|i| c[i + suffix.len()..].split_whitespace().all(|w| w.starts_with("--")))
    })
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

fn ensure(hooks: &mut Map<String, Value>, event: &str, matcher: Option<&str>, suffix: &str, command: &str, timeout: u64) -> Result<()> {
    let entries = hooks.entry(event.to_string()).or_insert_with(|| json!([]));
    let Some(entries) = entries.as_array_mut() else { bail!("hooks.{event} in settings is not an array") };
    if update_existing(entries, suffix, command) {
        return Ok(());
    }
    let mut entry = Map::new();
    if let Some(m) = matcher {
        entry.insert("matcher".into(), json!(m));
    }
    entry.insert("hooks".into(), json!([{"type": "command", "command": command, "timeout": timeout}]));
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
    pub prompt: Option<String>,
    pub file_touch: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct InstallOpts {
    pub sync: bool,
    pub import: bool,
    pub recall: bool,
    pub file_notes: bool,
}

impl Default for InstallOpts {
    fn default() -> Self {
        InstallOpts { sync: true, import: true, recall: true, file_notes: true }
    }
}

struct Spec<'a> {
    event: &'a str,
    matcher: Option<&'a str>,
    suffix: &'a str,
    command: String,
    timeout: u64,
}

fn toggle(hooks: &mut Map<String, Value>, on: bool, s: Spec) -> Result<Option<String>> {
    if on {
        ensure(hooks, s.event, s.matcher, s.suffix, &s.command, s.timeout)?;
        Ok(Some(s.command))
    } else {
        remove_from(hooks, s.event, s.suffix);
        Ok(None)
    }
}

pub fn install(path: &Path, vault: Option<&Path>, o: InstallOpts) -> Result<HookChange> {
    let mut s = load_settings(path)?;
    let before = s.clone();
    let start = exe_command(vault, START_SUFFIX)?;
    let mut end = exe_command(vault, END_SUFFIX)?;
    if !o.import {
        end.push_str(" --no-import");
    }
    let prompt = exe_command(vault, PROMPT_SUFFIX)?;
    let file = exe_command(vault, FILE_SUFFIX)?;
    let hooks = hooks_obj(&mut s)?;
    ensure(hooks, "SessionStart", Some(START_MATCHER), START_SUFFIX, &start, SESSION_TIMEOUT)?;
    let session_end = toggle(hooks, o.sync, Spec { event: "SessionEnd", matcher: None, suffix: END_SUFFIX, command: end, timeout: SESSION_TIMEOUT })?;
    let prompt = toggle(hooks, o.recall, Spec { event: PROMPT_EVENT, matcher: None, suffix: PROMPT_SUFFIX, command: prompt, timeout: RECALL_TIMEOUT })?;
    let file_touch = toggle(hooks, o.file_notes, Spec { event: FILE_EVENT, matcher: Some(FILE_MATCHER), suffix: FILE_SUFFIX, command: file, timeout: RECALL_TIMEOUT })?;
    let changed = s != before;
    if changed {
        save_settings(path, &s)?;
    }
    Ok(HookChange { settings: path_str(path), changed, session_start: Some(start), session_end, prompt, file_touch })
}

pub fn uninstall(path: &Path) -> Result<HookChange> {
    let mut s = load_settings(path)?;
    let mut changed = false;
    if let Some(root) = s.as_object_mut()
        && let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut)
    {
        changed |= remove_from(hooks, "SessionStart", START_SUFFIX);
        changed |= remove_from(hooks, "SessionEnd", END_SUFFIX);
        changed |= remove_from(hooks, PROMPT_EVENT, PROMPT_SUFFIX);
        changed |= remove_from(hooks, FILE_EVENT, FILE_SUFFIX);
        if changed && hooks.is_empty() {
            root.shift_remove("hooks");
        }
    }
    if changed {
        save_settings(path, &s)?;
    }
    Ok(HookChange { settings: path_str(path), changed, session_start: None, session_end: None, prompt: None, file_touch: None })
}

pub fn status(path: &Path) -> Result<HookChange> {
    let s = load_settings(path)?;
    let find = |event: &str, suffix: &str| -> Option<String> {
        s.get("hooks")?.get(event)?.as_array()?.iter().find_map(|e| {
            e.get("hooks")?.as_array()?.iter().find(|h| is_breadcrumb(h, suffix)).and_then(|h| h.get("command")?.as_str().map(str::to_string))
        })
    };
    Ok(HookChange {
        settings: path_str(path),
        changed: false,
        session_start: find("SessionStart", START_SUFFIX),
        session_end: find("SessionEnd", END_SUFFIX),
        prompt: find(PROMPT_EVENT, PROMPT_SUFFIX),
        file_touch: find(FILE_EVENT, FILE_SUFFIX),
    })
}
