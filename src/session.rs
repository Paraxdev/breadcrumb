//! Which Claude Code session wrote what. The id is what gets stored, the name can change across a restart.

use crate::note::{Note, yaml_scalar_string};
use serde_yaml::Value;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub const SESSION_KEY: &str = "session";
pub const LAST_SESSION_KEY: &str = "last_session";
pub const ID_ENV: &str = "CLAUDE_CODE_SESSION_ID";
pub const SHORT_LEN: usize = 8;
const RETRY_MS: u64 = 30;

static DISABLED: AtomicBool = AtomicBool::new(false);

/// For long running processes that write for people, like `serve`, which may inherit a session's environment.
pub fn disable() {
    DISABLED.store(true, Ordering::Relaxed);
}

/// The session id of this process, set for Bash children, hooks and the MCP server alike.
pub fn current() -> Option<String> {
    if DISABLED.load(Ordering::Relaxed) {
        return None;
    }
    std::env::var(ID_ENV).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn short(id: &str) -> String {
    id.chars().take(SHORT_LEN).collect()
}

#[derive(Clone, Debug)]
pub struct Live {
    pub id: String,
    pub name: Option<String>,
}

struct SessionList {
    live: Vec<Live>,
    /// False when some file could not be read, then the session it holds may be any of them.
    whole: bool,
}

/// Sessions with a file under <claude dir>/sessions whose process still runs. A file without `pid` counts as running.
pub fn live_sessions() -> Vec<Live> {
    read_sessions().map(|l| l.live).unwrap_or_default()
}

enum SessionFile {
    Gone,
    Bad,
    Good(serde_json::Value),
}

fn try_session_file(path: &Path) -> SessionFile {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_or(SessionFile::Bad, SessionFile::Good),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SessionFile::Gone,
        Err(_) => SessionFile::Bad,
    }
}

/// Note: Claude Code rewrites these files in place, so a read can meet half a file or a Windows sharing violation.
fn session_file(path: &Path) -> SessionFile {
    match try_session_file(path) {
        SessionFile::Bad => {
            std::thread::sleep(std::time::Duration::from_millis(RETRY_MS));
            try_session_file(path)
        }
        read => read,
    }
}

/// None when this machine has no session list to ask, so nothing can be said about who is dead.
fn read_sessions() -> Option<SessionList> {
    let dir = crate::importer::default_claude_dir()?.join("sessions");
    let mut list = SessionList { live: vec![], whole: true };
    for entry in std::fs::read_dir(dir).ok()? {
        let Ok(entry) = entry else {
            list.whole = false;
            continue;
        };
        if entry.path().extension().is_none_or(|x| x != "json") {
            continue;
        }
        let j = match session_file(&entry.path()) {
            SessionFile::Gone => continue,
            SessionFile::Bad => {
                list.whole = false;
                continue;
            }
            SessionFile::Good(j) => j,
        };
        if j["pid"].as_u64().is_some_and(|pid| !process_running(pid as u32, j["procStart"].as_str())) {
            continue;
        }
        let Some(id) = j["sessionId"].as_str() else {
            list.whole = false;
            continue;
        };
        let name = j["name"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        list.live.push(Live { id: id.to_string(), name });
    }
    Some(list)
}

/// A live session by full id, an id prefix of at least 8 characters (what claims store) or its current name.
pub fn find(who: &str) -> Option<Live> {
    let who = who.trim();
    if who.is_empty() {
        return None;
    }
    live_sessions().into_iter().find(|s| matches(s, who))
}

fn matches(s: &Live, who: &str) -> bool {
    s.id == who || (who.chars().count() >= SHORT_LEN && s.id.starts_with(who)) || s.name.as_deref() == Some(who)
}

/// Whether two ways to name a holder, an id, an id prefix or a live name, are one holder.
pub fn same_holder(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim(), b.trim());
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    let live = live_sessions();
    let find = |who: &str| live.iter().find(|s| matches(s, who)).map(|s| s.id.as_str());
    find(a).is_some_and(|id| find(b) == Some(id))
}

/// The 8 character id claims store or a whole session id. Anything else is free text no session file can match.
fn looks_like_id(who: &str) -> bool {
    let hex = |s: &str| s.chars().all(|c| c.is_ascii_hexdigit());
    (who.len() == SHORT_LEN && hex(who)) || (who.len() == 36 && hex(&who.replace('-', "")))
}

/// Whether this machine's session list says the session ended. Only means something for a session of this machine.
/// A list with an unreadable file says nothing, that file may be this session's.
pub fn known_dead(who: &str) -> bool {
    let who = who.trim();
    if !looks_like_id(who) || current().is_some_and(|c| c.starts_with(who)) {
        return false;
    }
    read_sessions().is_some_and(|l| l.whole && !l.live.iter().any(|s| matches(s, who)))
}

/// The current name of a claim's assignee when it is a live session, otherwise the assignee as stored.
pub fn holder(who: &str) -> String {
    find(who).and_then(|s| s.name).unwrap_or_else(|| who.to_string())
}

pub fn is_alive(who: &str) -> bool {
    find(who).is_some()
}

/// The current name when the session is live and named, otherwise the id's first 8 characters.
pub fn label(id: &str) -> String {
    find(id).and_then(|s| s.name).unwrap_or_else(|| short(id))
}

/// `neocad-4b a03e0d0a`, the name beside the id because the name can change, or just the id when it has no name.
pub fn signature(id: &str) -> String {
    let (name, id) = (label(id), short(id));
    if name == id { id } else { format!("{name} {id}") }
}

/// Who a body line is attributed to: the free text `by` when given, always with the session id when there is one.
pub fn attribute(by: Option<&str>) -> String {
    let by = by.map(str::trim).filter(|b| !b.is_empty());
    let Some(id) = current() else {
        let env = std::env::var(crate::backlog::SESSION_ENV).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        return by.map(str::to_string).or(env).unwrap_or_else(|| crate::backlog::DEFAULT_ACTOR.to_string());
    };
    let (sig, id) = (signature(&id), short(&id));
    match by {
        Some(b) if b == sig || b.ends_with(&id) => b.to_string(),
        Some(b) if sig.split(' ').next() != Some(b) => format!("{b} {id}"),
        _ => sig,
    }
}

/// The line above an appended paragraph, like `2026-10-31 neocad-4b a03e0d0a:`.
pub fn append_head() -> String {
    let date = crate::util::fmt_date(&crate::util::now());
    let who = current().map(|id| signature(&id)).or_else(|| std::env::var(crate::backlog::SESSION_ENV).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
    match who {
        Some(w) => format!("{date} {w}:"),
        None => format!("{date}:"),
    }
}

pub fn stamp_new(n: &mut Note) {
    if let Some(id) = current() {
        n.set_field(SESSION_KEY, Value::String(id));
    }
}

/// `last_session` stays away while the creator is the only writer.
pub fn stamp_write(n: &mut Note) {
    let Some(id) = current() else { return };
    if created_by(n).as_deref() == Some(id.as_str()) {
        n.unset_field(LAST_SESSION_KEY);
    } else {
        n.set_field(LAST_SESSION_KEY, Value::String(id));
    }
}

pub fn created_by(n: &Note) -> Option<String> {
    n.field(SESSION_KEY).and_then(yaml_scalar_string)
}

/// The session behind the note's latest stamped write. Writes by old binaries leave no stamp.
pub fn last_writer(n: &Note) -> Option<String> {
    n.field(LAST_SESSION_KEY).and_then(yaml_scalar_string).or_else(|| created_by(n))
}

/// `written by neocad-4b a03e0d0a, last by neocad-39 151cdeae`, None for notes without a stamp.
pub fn written_line(n: &Note) -> Option<String> {
    let last = n.field(LAST_SESSION_KEY).and_then(yaml_scalar_string);
    match (created_by(n), last) {
        (Some(c), Some(l)) => Some(format!("written by {}, last by {}", signature(&c), signature(&l))),
        (Some(c), None) => Some(format!("written by {}", signature(&c))),
        (None, Some(l)) => Some(format!("last written by {}", signature(&l))),
        (None, None) => None,
    }
}

#[cfg(windows)]
fn process_running(pid: u32, proc_start: Option<&str>) -> bool {
    use std::ffi::c_void;
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn GetExitCodeProcess(h: *mut c_void, code: *mut u32) -> i32;
        fn GetProcessTimes(h: *mut c_void, created: *mut FileTime, exited: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> i32;
        fn CloseHandle(h: *mut c_void) -> i32;
        fn GetLastError() -> u32;
    }
    const QUERY_LIMITED: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    const ACCESS_DENIED: u32 = 5;
    unsafe {
        let h = OpenProcess(QUERY_LIMITED, 0, pid);
        if h.is_null() {
            return GetLastError() == ACCESS_DENIED;
        }
        let mut code = 0;
        let mut t = [FileTime::default(); 4];
        let times = GetProcessTimes(h, &mut t[0], &mut t[1], &mut t[2], &mut t[3]) != 0;
        let active = GetExitCodeProcess(h, &mut code) != 0 && code == STILL_ACTIVE;
        CloseHandle(h);
        // Note: procStart is the FILETIME of the session's process, a different value means the pid was reused.
        let created = (t[0].high as u64) << 32 | t[0].low as u64;
        active && (!times || proc_start.and_then(|s| s.parse::<u64>().ok()).is_none_or(|want| want == created))
    }
}

#[cfg(not(windows))]
fn process_running(pid: u32, _proc_start: Option<&str>) -> bool {
    let proc = std::path::Path::new("/proc");
    !proc.is_dir() || proc.join(pid.to_string()).exists()
}
