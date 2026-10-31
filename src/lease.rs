//! Leases on things that are not items, like a build dir or a runner. They live in .local, which never syncs, so a
//! lease stays on its machine, adds no commit and no note, and old binaries never see it.

use crate::hooks::HookInput;
use crate::session;
use crate::util::{atomic_write, fmt_time, now, one_line, path_str, truncate_chars};
use crate::vault::{LockGuard, Vault, lock_file};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};

pub const DEFAULT_TTL: &str = "4h";
/// In a command it lets that one command past the Bash hook.
pub const HOOK_OK: &str = "BREADCRUMB_LEASE_OK=1";
const LOCK_SECS: u64 = 10;
const DIGEST_NAMES: usize = 4;
const NAME_MAX: usize = 80;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lease {
    pub name: String,
    /// The 8 character session id when a live session holds it, otherwise free text.
    pub holder: String,
    /// The session that ran the command, so `--by ci` still counts as its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The holder had a session file when taking it, only then can its absence mean it ended.
    #[serde(default)]
    pub seen: bool,
    /// That file's process, `/clear` changes the session id under it. Early leases have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub machine: String,
    pub taken: DateTime<Utc>,
    pub expires: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Default)]
pub struct TakeReq<'a> {
    pub name: &'a str,
    pub by: Option<&'a str>,
    pub ttl: Option<&'a str>,
    pub note: Option<&'a str>,
    pub path: Option<&'a Path>,
    pub force: bool,
}

#[derive(Debug)]
pub struct Taken {
    pub lease: Lease,
    pub renewed: bool,
    pub took_from: Option<String>,
}

fn file(root: &Path) -> PathBuf {
    root.join(".local").join("leases.json")
}

fn lock(root: &Path) -> Result<LockGuard> {
    Vault::open(root)?;
    let dir = root.join(".local");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    lock_file(&dir.join("leases.lock"), LOCK_SECS)
}

fn read(root: &Path) -> Result<Vec<Lease>> {
    let p = file(root);
    match std::fs::read_to_string(&p) {
        Ok(text) => serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(e).with_context(|| format!("reading {}", p.display())),
    }
}

fn save(root: &Path, leases: &[Lease]) -> Result<()> {
    atomic_write(&file(root), serde_json::to_string_pretty(leases)?.as_bytes())
}

fn on_this_machine(l: &Lease) -> bool {
    l.machine.eq_ignore_ascii_case(&crate::util::machine_name())
}

fn unexpired(l: &Lease) -> bool {
    l.expires > now() && on_this_machine(l)
}

fn ended(l: &Lease) -> bool {
    match l.pid {
        Some(pid) => session::process_gone(pid, &l.holder),
        None => l.seen && session::known_dead(&l.holder),
    }
}

fn holds(l: &Lease) -> bool {
    unexpired(l) && !ended(l)
}

/// Whether session `id` is the one holding the lease, also under a new id in the same process.
fn is_session(l: &Lease, id: &str) -> bool {
    l.session.as_deref() == Some(id) || (l.holder.len() >= session::SHORT_LEN && id.starts_with(&l.holder)) || l.pid.is_some_and(|pid| session::is_process(id, pid))
}

/// `neocad-4b a03e0d0a` for a live session, otherwise the holder as stored.
pub fn who(l: &Lease) -> String {
    match l.pid.and_then(session::by_pid).or_else(|| session::find(&l.holder)) {
        Some(s) => session::signature(&s.id),
        None => l.holder.clone(),
    }
}

fn same_name(a: &str, b: &str) -> bool {
    norm(a) == norm(b)
}

/// `4h`, `30m`, `2d` from now.
pub fn parse_ttl(s: &str) -> Result<Duration> {
    let s = s.trim();
    let bad = || format!("ttl must look like 30m, 4h or 2d, got {s:?}");
    let (num, unit) = s.split_at(s.char_indices().next_back().map_or(0, |(i, _)| i));
    let n: i64 = num.parse().with_context(bad)?;
    let d = match unit {
        "m" => Duration::try_minutes(n),
        "h" => Duration::try_hours(n),
        "d" => Duration::try_days(n),
        _ => None,
    };
    d.filter(|d| *d > Duration::zero()).with_context(bad)
}

fn looks_absolute(s: &str) -> bool {
    Path::new(s).is_absolute() || s.starts_with('/')
}

fn resource_path(name: &str, path: Option<&Path>) -> Result<Option<String>> {
    let p = match path {
        Some(p) => path_str(&crate::util::absolutize(p)),
        None if looks_absolute(name) => name.replace('\\', "/"),
        None => return Ok(None),
    };
    let p = p.trim_end_matches('/').to_string();
    if !norm(&p).contains('/') {
        bail!("{p} is a whole drive, a lease needs a directory under it");
    }
    Ok(Some(p))
}

/// Takes or renews a lease. Refused while another holder has it unless `force`.
pub fn take(root: &Path, r: TakeReq) -> Result<Taken> {
    let name = r.name.trim();
    if name.is_empty() || name.contains(['\n', '\r']) || name.chars().count() > NAME_MAX {
        bail!("a lease name is one line of at most {NAME_MAX} characters");
    }
    let expires = now() + parse_ttl(r.ttl.unwrap_or(DEFAULT_TTL))?;
    let path = resource_path(name, r.path)?;
    let asked = crate::backlog::session_name(r.by).unwrap_or_else(|| crate::backlog::DEFAULT_ACTOR.to_string());
    let live = session::find(&asked);
    let me = session::current();

    let _g = lock(root)?;
    let mut all = read(root)?;
    let old = all.iter().position(|l| same_name(&l.name, name)).map(|i| all.remove(i));
    let holder = live.as_ref().map_or_else(|| asked.clone(), |s| session::short(&s.id));
    let held = old.as_ref().filter(|l| holds(l));
    let mine = held.is_some_and(|l| l.holder.eq_ignore_ascii_case(&holder) || is_session(l, &holder) || me.as_deref().is_some_and(|id| is_session(l, id)));
    let other = held.filter(|_| !mine).map(who);
    if let Some(w) = &other
        && !r.force
    {
        bail!("{name} is leased by {w} until {}, `crumb lease {name} --force` takes it over", fmt_time(&held.map_or(expires, |l| l.expires)));
    }
    let kept = held.filter(|_| mine);
    let lease = Lease {
        name: name.to_string(),
        session: if live.is_some() { None } else { me },
        seen: live.is_some(),
        pid: live.as_ref().and_then(|s| s.pid),
        machine: crate::util::machine_name(),
        taken: kept.map_or_else(now, |l| l.taken),
        expires,
        path: path.or_else(|| kept.and_then(|l| l.path.clone())),
        note: r.note.map(one_line).filter(|n| !n.is_empty()).or_else(|| kept.and_then(|l| l.note.clone())),
        holder,
    };
    all.retain(unexpired);
    all.push(lease.clone());
    all.sort_by(|a, b| a.name.cmp(&b.name));
    save(root, &all)?;
    Ok(Taken { lease, renewed: mine, took_from: other })
}

/// False when nothing holds that name.
pub fn release(root: &Path, name: &str, by: Option<&str>, force: bool) -> Result<bool> {
    let name = name.trim();
    let asked = crate::backlog::session_name(by).unwrap_or_else(|| crate::backlog::DEFAULT_ACTOR.to_string());
    let holder = session::find(&asked).map_or(asked, |s| session::short(&s.id));
    let me = session::current();
    let _g = lock(root)?;
    let mut all = read(root)?;
    let Some(i) = all.iter().position(|l| same_name(&l.name, name)) else { return Ok(false) };
    let l = &all[i];
    let mine = l.holder.eq_ignore_ascii_case(&holder) || is_session(l, &holder) || me.as_deref().is_some_and(|id| is_session(l, id));
    if holds(l) && !mine && !force {
        bail!("{name} is leased by {}, `crumb lease --release {name} --force` releases it anyway", who(l));
    }
    let was_held = holds(l);
    all.remove(i);
    all.retain(unexpired);
    save(root, &all)?;
    Ok(was_held)
}

/// Leases that hold on this machine right now, by name.
pub fn active(root: &Path) -> Result<Vec<Lease>> {
    let mut all = read(root)?;
    all.retain(holds);
    Ok(all)
}

/// One line for the digest, None when no lease holds.
pub fn digest_line(root: &Path) -> Option<String> {
    let all = active(root).ok()?;
    if all.is_empty() {
        return None;
    }
    let shown: Vec<String> = all.iter().take(DIGEST_NAMES).map(|l| format!("{} ({})", truncate_chars(&l.name, 40), truncate_chars(&who(l), 40))).collect();
    let more = all.len().saturating_sub(DIGEST_NAMES);
    let more = if more > 0 { format!(", +{more} more") } else { String::new() };
    Some(format!("Leases on this machine: {}{more}, `crumb leases`", shown.join(", ")))
}

pub fn line(l: &Lease) -> String {
    let path = l.path.as_deref().map(|p| format!(" path {p}")).unwrap_or_default();
    let note = l.note.as_deref().map(|n| format!(": {n}")).unwrap_or_default();
    format!("{} held by {} until {}{path}{note}", l.name, who(l), fmt_time(&l.expires))
}

/// Slashes one way, doubled ones merged, and no case on Windows, so spellings of one path compare equal.
fn norm(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().map(|c| if c == '\\' { '/' } else { c }) {
        if c != '/' || !out.ends_with('/') {
            out.push(c);
        }
    }
    let out = if out.len() > 1 { out.trim_end_matches('/').to_string() } else { out };
    if cfg!(windows) { out.to_lowercase() } else { out }
}

fn path_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.')
}

fn names_at(text: &str, path: &str) -> bool {
    text.match_indices(path).any(|(i, m)| {
        let before = text[..i].chars().next_back();
        let after = text[i + m.len()..].chars().next();
        !before.is_some_and(|c| path_char(c) || matches!(c, '/' | ':')) && !after.is_some_and(path_char)
    })
}

/// Whether normalized `text` names `path` or something under it, also in the /d/dir form Git Bash uses.
fn names(text: &str, path: &str) -> bool {
    if names_at(text, path) {
        return true;
    }
    let mut c = path.chars();
    match (c.next(), c.next(), c.next()) {
        (Some(d), Some(':'), Some('/')) => names_at(text, &format!("/{d}{}", &path[d.len_utf8() + 1..])),
        _ => false,
    }
}

fn heredoc_end(line: &str) -> Option<String> {
    if line.contains("<<<") {
        return None;
    }
    let rest = &line[line.rfind("<<")? + 2..];
    let word = rest.trim_start_matches('-').split_whitespace().next()?;
    let word = word.trim_matches(['\'', '"', '\\']);
    (!word.is_empty()).then(|| word.to_string())
}

/// The command's parts between unquoted `;`, `&`, `|` and line ends. A heredoc body stays with the part that opened it.
fn segments(cmd: &str) -> Vec<String> {
    let mut parts = vec![];
    let mut cur = String::new();
    let mut quote = None;
    let mut chars = cmd.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match (quote, c) {
            (Some('"'), '\\') => {
                cur.push(c);
                if let Some((_, n)) = chars.next() {
                    cur.push(n);
                }
            }
            (Some(q), _) => {
                cur.push(c);
                if c == q {
                    quote = None;
                }
            }
            (None, '\'' | '"') => {
                cur.push(c);
                quote = Some(c);
            }
            (None, '\n') => {
                let start = cur.rfind('\n').map_or(0, |n| n + 1);
                if let Some(end) = heredoc_end(&cur[start..]) {
                    let mut pos = i + 1;
                    cur.push('\n');
                    for l in cmd[pos..].split_inclusive('\n') {
                        pos += l.len();
                        cur.push_str(l);
                        if l.trim() == end {
                            break;
                        }
                    }
                    while chars.peek().is_some_and(|(j, _)| *j < pos) {
                        chars.next();
                    }
                }
                parts.push(std::mem::take(&mut cur));
            }
            (None, ';' | '&' | '|') => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    parts
}

/// A crumb call, whose text args are notes, or `git commit`, whose message is prose. Naming a path there touches nothing.
fn only_text(segment: &str) -> bool {
    let mut words = segment.split_whitespace().skip_while(|w| w.contains('=') || matches!(*w, "(" | "{"));
    let Some(first) = words.next() else { return true };
    let first = first.trim_matches(['"', '\'', '(', '{']).replace('\\', "/");
    let exe = first.rsplit('/').next().unwrap_or("");
    let exe = exe.strip_suffix(".exe").unwrap_or(exe);
    matches!(exe, "crumb" | "breadcrumb") || (exe == "git" && words.next() == Some("commit"))
}

/// The deny reason when the Bash command would touch a resource another session leases. None lets it through.
pub fn check_bash(root: &Path, input: &HookInput) -> Option<String> {
    let text = std::fs::read_to_string(file(root)).ok()?;
    let mut leases: Vec<Lease> = serde_json::from_str(&text).ok()?;
    leases.retain(|l| l.path.is_some() && unexpired(l));
    if leases.is_empty() {
        return None;
    }
    let cmd = input.command.as_deref()?;
    if cmd.contains(HOOK_OK) {
        return None;
    }
    let parts: Vec<String> = segments(cmd).into_iter().filter(|s| !only_text(s)).collect();
    if parts.is_empty() {
        return None;
    }
    let hay = norm(&parts.join("\n"));
    let env = std::env::var("CARGO_TARGET_DIR").ok().map(|p| norm(&p));
    let me = input.session_id.clone().or_else(session::current)?;
    let my_name = || session::find(&me).and_then(|s| s.name);
    leases.into_iter().find_map(|l| {
        let path = l.path.as_deref()?;
        let p = norm(path);
        if !names(&hay, &p) && !env.as_deref().is_some_and(|e| names(e, &p)) {
            return None;
        }
        if is_session(&l, &me) || my_name().is_some_and(|n| n.eq_ignore_ascii_case(&l.holder)) || !holds(&l) {
            return None;
        }
        let note = l.note.as_deref().map(|n| format!(": {}", truncate_chars(n, 80))).unwrap_or_default();
        Some(format!(
            "{path} is leased by {} until {}{note}, wait for it, or prefix this one command with {HOOK_OK}, or take it over with `crumb lease {} --force`",
            who(&l),
            fmt_time(&l.expires),
            l.name
        ))
    })
}

/// The PreToolUse answer that refuses the call.
pub fn deny_json(reason: &str) -> String {
    json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}}).to_string()
}
