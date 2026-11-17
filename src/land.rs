//! The landing queue: the merge commits two sessions push to a branch one by one, each with its review, its CI run
//! and the session that holds the next step. It lives in the vault as `projects/<slug>/land.jsonl`, one entry per
//! line so git merges concurrent edits of different entries, and syncs like notes do.

use crate::backlog::{self, MoveReq};
use crate::ci;
use crate::note::is_commit_hex;
use crate::session;
use crate::util::{atomic_write, now, one_line, short_age, truncate_chars};
use crate::vault::Vault;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

pub const FILE: &str = "land.jsonl";
/// Approvals given before their merge was queued, apart from the queue so old binaries never see them.
pub const PARKED_FILE: &str = "land-parked.jsonl";
const KEEP_PARKED: usize = 16;
/// A pushed entry with no read for this long shows as stale, a CI run of the usual kind is long over by then.
pub const STALE_MINUTES: i64 = 45;
/// Green entries kept for the record, older ones leave the file.
const KEEP_GREEN: usize = 8;
const DIGEST_ROWS: usize = 3;
const DIGEST_MAX_ROWS: usize = 5;
/// Where `land list` cuts a note, `land show` prints it whole.
const NOTE_MAX: usize = 120;
const NOTE_KEEP: usize = 2000;
const DIGEST_NOTE: usize = 60;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Step {
    pub state: String,
    pub at: DateTime<Utc>,
    pub by: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub sha: String,
    /// Backlog item ids this merge lands.
    #[serde(default)]
    pub items: Vec<String>,
    /// built, approved, verified, pushed, green or red.
    pub state: String,
    /// When the entry reached its state.
    pub at: DateTime<Utc>,
    /// The condition on an approval, cleared by `verify`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// What the session that added the entry wants known with it, apart from `note` so no step clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memo: Option<String>,
    /// The session that holds the next step, as `name id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(default)]
    pub steps: Vec<Step>,
    /// The entry this one replaced, the same work merged again on a newer base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<String>,
    /// Approval notes a later approve, verify, push or replace took off, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub earlier_notes: Vec<OldNote>,
    /// Reads of the run with an attempt, a note or failed gate jobs, and infra reds, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<ReadRec>,
    /// Keys a newer binary wrote stay as they are.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
    /// What gh said of the run just now, `list` and the digest ask it before marking the entry stale. Never stored.
    #[serde(skip)]
    pub live: Option<ci::RunRow>,
}

/// One read of an entry's run. `result` is green, red or infra, an earlier attempt found when a rerun is read may also be
/// cancelled or going.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadRec {
    pub result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// The gate jobs that failed, with how.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub by: String,
    pub at: DateTime<Utc>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OldNote {
    pub note: String,
    pub by: String,
    pub at: DateTime<Utc>,
}

/// An approval given before its merge was queued, `land add` of that sha takes it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Parked {
    pub sha: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    pub at: DateTime<Utc>,
    pub by: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn same_sha(a: &str, b: &str) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

impl Entry {
    pub fn short(&self) -> &str {
        &self.sha[..self.sha.len().min(7)]
    }

    fn is_open(&self) -> bool {
        matches!(self.state.as_str(), "built" | "approved" | "verified" | "pushed")
    }

    fn late(&self) -> bool {
        self.state == "pushed" && now() - self.at >= Duration::minutes(STALE_MINUTES)
    }

    /// Pushed with no read for STALE_MINUTES, unless gh said just now that its run is still going.
    pub fn stale(&self) -> bool {
        self.late() && !self.live.as_ref().is_some_and(|r| r.status != "completed")
    }

    /// The last read of the run that counts against a rerun: its attempt when known, and when it was made.
    fn last_read(&self) -> Option<(Option<u32>, DateTime<Utc>)> {
        let rec = self.reads.iter().rev().find(|r| matches!(r.result.as_str(), "green" | "red" | "infra")).map(|r| (r.attempt, r.at));
        let step = self.steps.iter().rev().find(|s| matches!(s.state.as_str(), "green" | "red" | "infra")).map(|s| (None, s.at));
        match (rec, step) {
            (Some(r), Some(s)) if s.1 > r.1 + Duration::seconds(1) => Some(s),
            (r, s) => r.or(s),
        }
    }

    /// The run's current attempt was read already, so a check records nothing until it is rerun.
    fn covers(&self, run: &ci::Run) -> bool {
        match self.last_read() {
            None => false,
            Some((Some(a), _)) => run.attempt <= a,
            Some((None, at)) => run.started_at.is_none_or(|s| s <= at),
        }
    }

    /// Pushed with an infra red as the last read, waiting for its failed jobs to be rerun.
    fn infra(&self) -> Option<&ReadRec> {
        self.reads.last().filter(|r| r.result == "infra" && self.state == "pushed" && self.steps.last().is_some_and(|s| s.state == "infra"))
    }

    fn by(&self, state: &str) -> Option<&str> {
        self.steps.iter().rev().find(|s| s.state == state).map(|s| s.by.as_str())
    }

    /// Moves the approval's note to the earlier notes.
    fn retire_note(&mut self) -> Option<OldNote> {
        let note = self.note.take()?;
        let (by, at) = self.steps.iter().rev().find(|s| s.state == "approved").map(|s| (s.by.clone(), s.at)).unwrap_or_else(|| (String::new(), self.at));
        let old = OldNote { note, by, at };
        self.earlier_notes.push(old.clone());
        Some(old)
    }

    /// What is due next, None once the entry is green.
    fn due(&self) -> Option<&'static str> {
        match self.state.as_str() {
            "built" => Some("approve"),
            "approved" => Some("verify or push"),
            "verified" => Some("push"),
            "pushed" if self.infra().is_some() => Some("rerun the failed jobs, then read the run"),
            "pushed" => Some("read the run"),
            "red" => Some("fix, or drop it"),
            _ => None,
        }
    }
}

fn is_me(who: &str) -> bool {
    let id = session::current().map(|id| session::short(&id));
    id.is_some_and(|id| who.contains(&id)) || std::env::var(crate::backlog::SESSION_ENV).ok().is_some_and(|s| !s.trim().is_empty() && who == s.trim())
}

fn path(v: &Vault, slug: &str) -> PathBuf {
    v.project_dir(slug).join(FILE)
}

fn parked_path(v: &Vault, slug: &str) -> PathBuf {
    v.project_dir(slug).join(PARKED_FILE)
}

fn read_text(p: &Path) -> Result<String> {
    match std::fs::read_to_string(p) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", p.display())),
    }
}

fn read(v: &Vault, slug: &str) -> Result<Vec<Entry>> {
    let p = path(v, slug);
    read_text(&p)?
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| serde_json::from_str(l).with_context(|| format!("{} line {} is not a queue entry, a merge conflict may have left markers in it", p.display(), i + 1)))
        .collect()
}

/// Parked approvals and the lines that do not parse, which stay in the file as they are.
struct ParkedFile {
    parked: Vec<Parked>,
    bad: Vec<String>,
}

fn read_parked(v: &Vault, slug: &str) -> Result<ParkedFile> {
    let mut f = ParkedFile { parked: vec![], bad: vec![] };
    for l in read_text(&parked_path(v, slug))?.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str(l) {
            Ok(p) => f.parked.push(p),
            Err(_) => f.bad.push(l.to_string()),
        }
    }
    Ok(f)
}

/// Lines of the queue and parked files that do not parse, as (file, line number, problem) for `doctor`.
pub fn bad_lines(v: &Vault, slug: &str) -> Vec<(PathBuf, usize, &'static str)> {
    let mut out = vec![];
    let files: [(PathBuf, fn(&str) -> bool, &'static str); 2] = [
        (path(v, slug), |l| serde_json::from_str::<Entry>(l).is_ok(), "is not a queue entry, every land step fails until it is fixed, a merge conflict may have left markers in it"),
        (parked_path(v, slug), |l| serde_json::from_str::<Parked>(l).is_ok(), "is not a parked approval, land steps skip it and keep it in the file"),
    ];
    for (p, parses, what) in files {
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        for (i, l) in text.lines().enumerate() {
            if !l.trim().is_empty() && !parses(l) {
                out.push((p.clone(), i + 1, what));
            }
        }
    }
    out
}

fn lines<T: Serialize>(xs: &[T]) -> Result<String> {
    let mut text = String::new();
    for x in xs {
        text.push_str(&serde_json::to_string(x)?);
        text.push('\n');
    }
    Ok(text)
}

fn prune(entries: &mut Vec<Entry>, parked: &mut Vec<Parked>) {
    let mut greens = entries.iter().filter(|e| e.state == "green").count();
    entries.retain(|e| {
        let drop = e.state == "green" && greens > KEEP_GREEN;
        if drop {
            greens -= 1;
        }
        !drop
    });
    let cut = parked.len().saturating_sub(KEEP_PARKED);
    parked.drain(..cut);
}

fn slug_of(v: &Vault, project: Option<&str>) -> Result<String> {
    let slug = project.context("the landing queue belongs to a project, pass --project (the project argument in MCP) or run from its directory")?;
    if v.load_project(slug)?.is_none() {
        bail!("unknown project '{slug}', the landing queue belongs to a registered project");
    }
    Ok(slug.to_string())
}

fn find(entries: &[Entry], sha: &str) -> Result<usize> {
    let q = sha.trim().to_ascii_lowercase();
    if q.len() < 4 || !q.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("{sha:?} is not a commit sha, give at least 4 hex characters");
    }
    let hits: Vec<usize> = entries.iter().enumerate().filter(|(_, e)| e.sha.starts_with(&q) || q.starts_with(&e.sha)).map(|(i, _)| i).collect();
    match hits.as_slice() {
        [i] => Ok(*i),
        [] => bail!("no queue entry for {q}, `crumb land list` shows the queue"),
        many => bail!("{q} matches {}, give more of the sha", many.iter().map(|i| entries[*i].short()).collect::<Vec<_>>().join(", ")),
    }
}

/// A session given by name or id becomes its `name id` form, anything else stays free text.
fn holder(to: &str) -> String {
    session::find(to).map(|s| session::signature(&s.id)).unwrap_or_else(|| to.trim().to_string())
}

/// A stored `name id` under the session's current name while it runs, so a renamed session reads as it is now.
fn named(who: &str) -> String {
    let id = who.rsplit(' ').next().unwrap_or(who);
    if id.len() < 8 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return who.to_string();
    }
    session::find(id).map(|s| session::signature(&s.id)).unwrap_or_else(|| who.to_string())
}

#[derive(Clone, Debug)]
pub enum Op {
    List { all: bool },
    Show { sha: String },
    Add { sha: String, items: Vec<String>, note: Option<String>, to: Option<String>, replaces: Option<String>, clear_approval: bool },
    Approve { sha: String, note: Option<String>, to: Option<String> },
    Verify { sha: String, to: Option<String> },
    Pushed { shas: Vec<String>, run: Option<String>, to: Option<String> },
    Read { shas: Vec<String>, green: bool, to: Option<String>, attempt: Option<u32>, note: Option<String> },
    Infra { shas: Vec<String>, note: Option<String>, attempt: Option<u32>, to: Option<String> },
    Check { shas: Vec<String>, wait: Option<Wait>, to: Option<String> },
    Ci { repo: Option<String>, workflow: Option<String>, branch: Option<String>, gate: Option<Vec<String>>, clear: bool },
    Drop { sha: String },
    Move { sha: String, after: Option<String> },
}

/// How long `land wait` waits for the gate and how often it looks.
#[derive(Clone, Debug)]
pub struct Wait {
    pub minutes: u64,
    pub every: u64,
}

#[derive(Clone, Debug)]
pub struct Req {
    pub op: Op,
    pub by: Option<String>,
    pub force: bool,
    /// Where the call runs, an approval of a sha not queued yet looks for the commit in this checkout first.
    pub cwd: Option<PathBuf>,
}

pub struct Outcome {
    pub text: String,
    pub data: Value,
}

fn names(v: &Vault, e: &Entry) -> String {
    if e.items.is_empty() {
        return "no items".to_string();
    }
    e.items.iter().map(|id| v.get(id).map(|n| n.handle()).unwrap_or_else(|_| id.clone())).collect::<Vec<_>>().join(", ")
}

/// Other unread entries pushed on the same run, so a reader sees one push.
fn peers(entries: &[Entry], e: &Entry) -> String {
    let Some(r) = &e.run else { return String::new() };
    let o: Vec<&str> = entries.iter().filter(|x| x.sha != e.sha && x.state == "pushed" && e.state == "pushed" && x.run.as_ref() == Some(r)).map(Entry::short).collect();
    if o.is_empty() { String::new() } else { format!(" (with {})", o.join(", ")) }
}

fn memo_tail(e: &Entry, max: usize) -> String {
    e.memo.as_ref().map(|m| format!(", note: \"{}\"", truncate_chars(m, max))).unwrap_or_default()
}

fn head(entries: &[Entry], e: &Entry) -> String {
    let mut s = format!("{} {}", e.short(), e.state);
    if let Some(n) = &e.note {
        s.push_str(&format!(" ({})", truncate_chars(n, NOTE_MAX)));
    }
    if let Some(t) = read_tag(e) {
        s.push_str(&format!(" ({t})"));
    }
    s + &run_age(entries, e)
}

fn attempt_text(a: Option<u32>) -> String {
    a.map(|a| format!(" on attempt {a}")).unwrap_or_default()
}

/// What the last read says beyond the state: an infra red waiting for a rerun, a read after a rerun, a note.
fn read_tag(e: &Entry) -> Option<String> {
    let note = |r: &ReadRec| r.note.as_ref().map(|n| truncate_chars(n, DIGEST_NOTE));
    if let Some(r) = e.infra() {
        return Some(format!("infra red{}{}", attempt_text(r.attempt), note(r).map(|n| format!(": {n}")).unwrap_or_default()));
    }
    let r = e.reads.iter().rev().find(|r| r.result == e.state)?;
    if e.steps.last().is_none_or(|s| s.state != e.state) {
        return None;
    }
    let mut parts = vec![];
    if let Some(a) = r.attempt.filter(|a| *a > 1) {
        let mut by_attempt = std::collections::BTreeMap::new();
        for x in &e.reads {
            if let Some(k) = x.attempt.filter(|k| *k < a) {
                by_attempt.insert(k, if x.result == "infra" { "infra red" } else { x.result.as_str() });
            }
        }
        let earlier: Vec<String> = by_attempt.iter().map(|(k, r)| format!("attempt {k} {r}")).collect();
        parts.push(if earlier.is_empty() { format!("attempt {a}") } else { format!("attempt {a}, after {}", earlier.join(", ")) });
    }
    if !r.failed.is_empty() {
        parts.push(r.failed.join(", "));
    }
    parts.extend(note(r));
    Some(parts.join(", ")).filter(|t| !t.is_empty())
}

fn run_age(entries: &[Entry], e: &Entry) -> String {
    let mut s = String::new();
    if let Some(r) = &e.run {
        s.push_str(&format!(", run {r}{}", peers(entries, e)));
    }
    s.push_str(&format!(", {} ago", short_age(&e.at)));
    match &e.live {
        Some(r) if e.late() && r.status != "completed" => s.push_str(", run still going"),
        Some(r) if e.stale() && e.infra().is_some() => s.push_str(&format!(", STALE, run finished {}, no rerun yet", r.conclusion)),
        Some(r) if e.stale() => s.push_str(&format!(", STALE, run finished {}, no read yet, `crumb land check {}` reads it", r.conclusion, e.short())),
        _ if e.stale() => s.push_str(", STALE, no read yet"),
        _ => {}
    }
    s
}

fn when(t: &DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M").to_string()
}

/// One entry whole, `land show`.
fn show_text(v: &Vault, entries: &[Entry], i: usize) -> String {
    let e = &entries[i];
    let mut s = format!("{} {}{}, #{} in the queue\n  next: {}\n", e.sha, e.state, run_age(entries, e), i + 1, due_text(e));
    if e.items.is_empty() {
        s.push_str("  items: none\n");
    } else {
        s.push_str("  items:\n");
        for id in &e.items {
            match v.get(id) {
                Ok(n) => s.push_str(&format!("    {} [{}] {}\n", n.handle(), backlog::state(&n), n.title)),
                Err(_) => s.push_str(&format!("    {id}, not in the vault any more\n")),
            }
        }
    }
    if let Some(m) = &e.memo {
        s.push_str(&format!("  note: {m}\n"));
    }
    if let Some(n) = &e.note {
        s.push_str(&format!("  approval note: {n} (by {})\n", e.by("approved").map(named).unwrap_or_default()));
    }
    if !e.earlier_notes.is_empty() {
        s.push_str("  earlier approval notes:\n");
        for o in &e.earlier_notes {
            s.push_str(&format!("    {} {}: {}\n", when(&o.at), named(&o.by), o.note));
        }
    }
    if let Some(r) = &e.replaces {
        s.push_str(&format!("  replaces: {r}\n"));
    }
    if !e.reads.is_empty() {
        s.push_str("  reads:\n");
        for r in &e.reads {
            let failed = if r.failed.is_empty() { String::new() } else { format!(", {}", r.failed.join(", ")) };
            let note = r.note.as_ref().map(|n| format!(": {n}")).unwrap_or_default();
            s.push_str(&format!("    {}{} {} by {}{failed}{note}\n", r.result, attempt_text(r.attempt), when(&r.at), named(&r.by)));
        }
    }
    s.push_str("  steps:\n");
    for st in &e.steps {
        s.push_str(&format!("    {} {} by {}\n", st.state, when(&st.at), named(&st.by)));
    }
    s.trim_end().to_string()
}

fn parked_line(p: &Parked, max: usize) -> String {
    let note = p.note.as_ref().map(|n| format!(" ({})", truncate_chars(n, max))).unwrap_or_default();
    format!("{} approved by {} {} ago{note}", &p.sha[..p.sha.len().min(7)], named(&p.by), short_age(&p.at))
}

fn due_text(e: &Entry) -> String {
    match (e.due(), &e.next) {
        (Some(d), Some(w)) => format!("{d}: {}{}", named(w), if is_me(w) { " (you)" } else { "" }),
        (Some(d), None) => d.to_string(),
        (None, _) => "landed".to_string(),
    }
}

fn row(v: &Vault, entries: &[Entry], i: usize, e: &Entry) -> String {
    format!("{}. {}, {}, items: {}{}", i + 1, head(entries, e), due_text(e), names(v, e), memo_tail(e, NOTE_MAX))
}

fn list_text(v: &Vault, entries: &[Entry], parked: &[Parked], all: bool) -> String {
    let shown: Vec<(usize, &Entry)> = entries.iter().enumerate().filter(|(_, e)| all || e.state != "green").collect();
    let mut s = if shown.is_empty() { "the landing queue is empty".to_string() } else { shown.iter().map(|(i, e)| row(v, entries, *i, e)).collect::<Vec<_>>().join("\n") };
    if !parked.is_empty() {
        s.push_str("\nparked approvals, each attaches when `crumb land add` queues its sha:");
        for p in parked {
            s.push_str(&format!("\n  {}", parked_line(p, NOTE_MAX)));
        }
    }
    s
}

/// Runs `f` on the queue under the vault lock and saves what changed. Parked approvals whose sha is queued by now are
/// settled first, the second value is what the reply says about them.
fn mutate<T>(v: &Vault, slug: &str, f: impl FnOnce(&mut Vec<Entry>, &mut Vec<Parked>) -> Result<(String, T)>) -> Result<(T, String)> {
    let _g = v.lock()?;
    let mut entries = read(v, slug)?;
    let ParkedFile { mut parked, bad } = read_parked(v, slug)?;
    let before = [lines(&entries)?, lines(&parked)?];
    let said = settle(&mut entries, &mut parked);
    let (mut msg, out) = f(&mut entries, &mut parked)?;
    if msg.is_empty() && !said.is_empty() {
        msg = "land settle parked approvals".to_string();
    }
    if msg.is_empty() {
        return Ok((out, said));
    }
    prune(&mut entries, &mut parked);
    let mut paths = vec![];
    for ((p, after, keep), was) in [(path(v, slug), lines(&entries)?, String::new()), (parked_path(v, slug), lines(&parked)?, bad.iter().map(|l| format!("{l}\n")).collect())].into_iter().zip(before) {
        if after != was {
            atomic_write(&p, (keep + &after).as_bytes())?;
            paths.push(p);
        }
    }
    if !paths.is_empty() {
        v.commit(&paths, &format!("breadcrumb: {msg}"))?;
    }
    Ok((out, said))
}

/// The queued entry a parked approval is for, None when no entry or more than one matches.
fn parked_for(entries: &[Entry], p: &Parked) -> Option<usize> {
    let hits: Vec<usize> = entries.iter().enumerate().filter(|(_, e)| same_sha(&e.sha, &p.sha)).map(|(i, _)| i).collect();
    match hits.as_slice() {
        [i] => Some(*i),
        _ => None,
    }
}

/// Parked approvals whose sha is queued, perhaps by an older binary or on another machine: one for a built entry
/// with no approval attaches, any other is dropped. Returns the lines for the reply.
fn settle(entries: &mut [Entry], parked: &mut Vec<Parked>) -> String {
    let mut said = String::new();
    parked.retain(|p| {
        let Some(i) = parked_for(entries, p) else { return true };
        let e = &mut entries[i];
        if e.state == "built" && e.by("approved").is_none() {
            said.push_str(&attach(e, p));
        } else {
            said.push_str(&format!("\n  dropped the approval {} parked for {} {} ago, {} is {} already", named(&p.by), e.short(), short_age(&p.at), e.short(), e.state));
        }
        false
    });
    said
}

fn attach(e: &mut Entry, p: &Parked) -> String {
    e.retire_note();
    e.state = "approved".into();
    e.at = now();
    e.steps.push(Step { state: "approved".into(), at: p.at, by: p.by.clone() });
    e.note = p.note.clone();
    e.next = p.next.clone().or_else(|| e.by("built").map(str::to_string));
    format!("\n  {} approved by {} {} ago before it was queued", e.short(), named(&p.by), short_age(&p.at))
}

/// A replacing entry takes the old one's place, its items and note where it names none, and its approval unless
/// `clear` is given or the old one was not approved. Returns the line for the reply.
fn carry(e: &mut Entry, mut old: Entry, clear: bool, to_given: bool) -> String {
    if e.items.is_empty() {
        e.items = old.items.clone();
    }
    if e.memo.is_none() {
        e.memo = old.memo.clone();
    }
    e.replaces = Some(old.sha.clone());
    let approver = old.by("approved").map(named);
    let approved = matches!(old.state.as_str(), "approved" | "verified");
    if approved && !clear {
        e.steps.extend(old.steps.iter().filter(|s| matches!(s.state.as_str(), "approved" | "verified")).cloned());
        e.state = old.state.clone();
        e.note = old.note.clone();
        e.earlier_notes = old.earlier_notes.clone();
        if !to_given {
            e.next = e.by("built").map(str::to_string);
        }
        return format!(
            "\n  replaces {}, its approval by {} carries over as {}, `git diff {} {}` shows what changed, `--clear-approval` asks for a new one",
            old.short(),
            approver.unwrap_or_default(),
            old.state,
            old.short(),
            e.short()
        );
    }
    old.retire_note();
    e.earlier_notes = old.earlier_notes.clone();
    if !to_given {
        e.next = old.by("approved").map(str::to_string);
    }
    let why = if approved { "its approval was cleared".to_string() } else { format!("it was {}", old.state) };
    format!("\n  replaces {}, starts built since {why}", old.short())
}

fn set_state(e: &mut Entry, state: &str, by: &str) {
    let t = now();
    e.state = state.to_string();
    e.at = t;
    e.steps.push(Step { state: state.to_string(), at: t, by: by.to_string() });
}

fn step_error(e: &Entry, verb: &str, want: &str) -> anyhow::Error {
    anyhow::anyhow!("{} is {}, {verb} needs {want}, `--force` does it anyway", e.short(), e.state)
}

/// The queue runs in order, so an earlier entry that is not pushed yet holds this one back unless this call pushes it
/// too. One pushed already is on the branch, read or not, so pushing after it keeps the order.
fn blocker<'a>(entries: &'a [Entry], i: usize, call: &[usize]) -> Option<&'a Entry> {
    entries[..i].iter().enumerate().find(|(j, e)| matches!(e.state.as_str(), "built" | "approved" | "verified") && !call.contains(j)).map(|(_, e)| e)
}

/// Refuses a push of an entry that is not approved or verified, or that an earlier unpushed entry holds back. With a run
/// id, a pushed entry that has none takes it.
fn push_check(entries: &[Entry], idx: &[usize], run_known: bool) -> Result<()> {
    for &i in idx {
        let e = &entries[i];
        let fill = run_known && e.state == "pushed" && e.run.is_none();
        if !fill && !matches!(e.state.as_str(), "approved" | "verified") {
            return Err(step_error(e, "pushed", "an approved entry"));
        }
        if let Some(b) = blocker(entries, i, idx) {
            let name = if b.state == "built" { "" } else { ", or name it in this call if the push carries it" };
            bail!("{} is ahead in the queue and still {}, push in order{name}, or `--force`, or `crumb land move`/`drop`", b.short(), b.state);
        }
    }
    Ok(())
}

/// How long one gh call of a land step may take.
const GH_LIMIT: std::time::Duration = std::time::Duration::from_secs(20);
/// How long `list` and the digest wait on gh for the state of a run that looks stale.
const GH_LIST_LIMIT: std::time::Duration = std::time::Duration::from_secs(4);

/// The id of the newest run for one of `shas`, a few looks apart since GitHub starts a run some seconds after the push.
fn find_run(c: &ci::Ci, shas: &[&str]) -> std::result::Result<Option<String>, ci::GhError> {
    for i in 0..4 {
        if i > 0 {
            std::thread::sleep(ci::pause());
        }
        if let Some(r) = ci::run_for(&ci::runs(c, GH_LIMIT)?, shas) {
            return Ok(Some(r.database_id.to_string()));
        }
    }
    Ok(None)
}

/// `build run on dev`, less when the workflow or branch is unset.
fn ci_where(c: &ci::Ci) -> String {
    let mut s = if c.workflow.is_empty() { "run".to_string() } else { format!("{} run", c.workflow) };
    if !c.branch.is_empty() {
        s.push_str(&format!(" on {}", c.branch));
    }
    s
}

/// The entries a call names, in queue order.
fn find_all(entries: &[Entry], shas: &[String]) -> Result<Vec<usize>> {
    let mut idx = shas.iter().map(|s| find(entries, s)).collect::<Result<Vec<_>>>()?;
    idx.sort_unstable();
    idx.dedup();
    if idx.is_empty() {
        bail!("give at least one commit sha");
    }
    Ok(idx)
}

/// How long finding a sha in a checkout of the project may take.
const GIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(3);

/// The full sha of `q` in a checkout of the project on this machine, else the checkouts it looked in.
fn resolve(v: &Vault, slug: &str, cwd: Option<&Path>, q: &str) -> std::result::Result<String, Vec<PathBuf>> {
    let mut dirs: Vec<PathBuf> = vec![];
    if let Some(c) = cwd.filter(|c| v.detect_project(c).as_deref() == Some(slug)) {
        dirs.push(c.to_path_buf());
        dirs.extend(crate::git::main_checkout(c).map(|(_, main)| main));
    }
    if let Ok(Some(p)) = v.load_project(slug) {
        dirs.extend(p.paths.iter().map(PathBuf::from));
    }
    let mut seen: Vec<PathBuf> = vec![];
    for d in dirs.into_iter().filter(|d| d.is_dir()) {
        if seen.contains(&d) {
            continue;
        }
        if let Some(full) = crate::git::resolve_commit(&d, q, GIT_LIMIT) {
            return Ok(full);
        }
        seen.push(d);
    }
    Err(seen)
}

/// The queue and the parked approvals, settled first when a parked sha is queued by now.
fn current(v: &Vault, slug: &str) -> Result<(Vec<Entry>, Vec<Parked>, String)> {
    let entries = read(v, slug)?;
    let parked = read_parked(v, slug)?.parked;
    if !parked.iter().any(|p| parked_for(&entries, p).is_some()) {
        return Ok((entries, parked, String::new()));
    }
    let ((entries, parked), said) = mutate(v, slug, |es, ps| Ok((String::new(), (es.clone(), ps.clone()))))?;
    Ok((entries, parked, said))
}

/// One entry after a step, with what is due next.
fn state_out(v: &Vault, slug: &str, e: &Entry, extra: &str) -> Outcome {
    let all = read(v, slug).unwrap_or_default();
    let memo = e.memo.as_ref().map(|m| format!("\n  note: {m}")).unwrap_or_default();
    Outcome { text: format!("{}\n  next: {}{memo}{extra}", head(&all, e), due_text(e)), data: json!({"entry": e}) }
}

pub fn run(v: &Vault, project: Option<&str>, req: Req) -> Result<Outcome> {
    let slug = slug_of(v, project)?;
    let by = session::attribute(req.by.as_deref());
    let force = req.force;
    let state_out = |e: &Entry, extra: &str| state_out(v, &slug, e, extra);
    match req.op {
        Op::List { all } => {
            let (mut entries, parked, settled) = current(v, &slug)?;
            ask_live(v, &slug, &mut entries);
            let shown: Vec<&Entry> = entries.iter().filter(|e| all || e.state != "green").collect();
            let mut data = json!({"entries": shown});
            if !parked.is_empty() {
                data["parked"] = json!(parked);
            }
            Ok(Outcome { text: list_text(v, &entries, &parked, all) + &settled, data })
        }
        Op::Show { sha } => {
            let (entries, parked, settled) = current(v, &slug)?;
            match find(&entries, &sha) {
                Ok(i) => Ok(Outcome { text: show_text(v, &entries, i) + &settled, data: json!({"entry": entries[i]}) }),
                Err(err) => {
                    let q = sha.trim().to_ascii_lowercase();
                    let p = parked.iter().find(|p| same_sha(&p.sha, &q)).ok_or(err)?;
                    Ok(Outcome { text: format!("{}, parked, it attaches when `crumb land add {}` queues it", parked_line(p, NOTE_KEEP), p.sha), data: json!({"parked": p}) })
                }
            }
        }
        Op::Add { sha, items, note, to, replaces, clear_approval } => {
            let memo = note.map(|n| truncate_chars(&one_line(&n), NOTE_KEEP)).filter(|n| !n.is_empty());
            let sha = sha.trim().to_ascii_lowercase();
            if !is_commit_hex(&sha) {
                bail!("{sha:?} is not a commit sha, give 7 to 40 hex characters");
            }
            let mut ids: Vec<String> = vec![];
            let mut stays = String::new();
            for item in items.iter().flat_map(|s| s.split(',')).map(str::trim).filter(|s| !s.is_empty()) {
                let n = backlog::get_item(v, item)?;
                if !ids.contains(&n.id) {
                    if !goes_to_review(&n) {
                        let st = backlog::state(&n);
                        let claim = if st == "backlog" { format!(" and unclaimed, a green read leaves it there, `crumb claim {}` when its builder holds it", n.id) } else { ", a green read leaves it there".to_string() };
                        stays.push_str(&format!("\n  {} is {st}{claim}", n.handle()));
                    }
                    ids.push(n.id);
                }
            }
            let ((e, said, gone, all), settled) = mutate(v, &slug, |entries, parked| {
                if let Some(old) = entries.iter().find(|e| same_sha(&e.sha, &sha)) {
                    bail!("{} is in the queue already as {}", old.short(), old.state);
                }
                let t = now();
                let mut e = Entry {
                    sha: sha.clone(),
                    items: ids,
                    state: "built".into(),
                    at: t,
                    note: None,
                    run: None,
                    memo,
                    next: to.as_deref().map(holder),
                    steps: vec![Step { state: "built".into(), at: t, by: by.clone() }],
                    replaces: None,
                    earlier_notes: vec![],
                    reads: vec![],
                    extra: Map::new(),
                    live: None,
                };
                let mut said = String::new();
                let mut at = entries.len();
                let mut gone = None;
                if let Some(r) = replaces.as_deref() {
                    let i = find(entries, r).map_err(|err| anyhow::anyhow!("{err}, it is the entry this one replaces"))?;
                    if matches!(entries[i].state.as_str(), "pushed" | "green") {
                        bail!("{} is {}, it is on the branch already, a replace is for an entry not pushed yet or read red", entries[i].short(), entries[i].state);
                    }
                    at = i;
                    let old = entries.remove(i);
                    said = carry(&mut e, old.clone(), clear_approval, to.is_some());
                    gone = Some(old);
                }
                let (mine, rest): (Vec<Parked>, Vec<Parked>) = std::mem::take(parked).into_iter().partition(|p| same_sha(&p.sha, &sha));
                *parked = rest;
                if let Some(p) = mine.first() {
                    let carried = e.state != "built";
                    match &gone {
                        // The replace decided the approval, a parked one would undo that.
                        Some(old) if carried || clear_approval => {
                            let why = if carried { format!("the approval of {} carries over", old.short()) } else { "`--clear-approval` asks for a new one".to_string() };
                            said.push_str(&format!("\n  dropped the approval {} parked for {} {} ago, {why}", named(&p.by), e.short(), short_age(&p.at)));
                        }
                        _ => {
                            said.push_str(&attach(&mut e, p));
                            if let Some(t) = to.as_deref() {
                                e.next = Some(holder(t));
                            }
                        }
                    }
                }
                entries.insert(at, e.clone());
                let msg = match &e.replaces {
                    Some(o) => format!("land replace {} {}", &o[..o.len().min(7)], &sha[..7]),
                    None => format!("land add {}", &sha[..7]),
                };
                Ok((msg, (e, said, gone, entries.clone())))
            })?;
            let hint = if e.items.is_empty() { "\n  no items given, none move to review when it is read green".to_string() } else { stays };
            let (left, moved) = match &gone {
                Some(old) => land_left(v, old, &all, req.by.clone()),
                None => (String::new(), vec![]),
            };
            let mut out = state_out(&e, &format!("{said}{hint}{left}{settled}"));
            if !moved.is_empty() {
                out.data["items"] = json!(moved);
            }
            Ok(out)
        }
        Op::Approve { sha, note, to } => {
            let note = note.map(|n| truncate_chars(&one_line(&n), NOTE_MAX)).filter(|n| !n.is_empty());
            let q = sha.trim().to_ascii_lowercase();
            let (done, settled) = mutate(v, &slug, |entries, parked| {
                let i = match find(entries, &sha) {
                    Ok(i) => i,
                    Err(err) if is_commit_hex(&q) && !entries.iter().any(|e| same_sha(&e.sha, &q)) => {
                        if let Some(n) = entries.iter().find(|e| e.replaces.as_deref().is_some_and(|r| same_sha(r, &q))) {
                            let (o, n) = (&q[..7], n.short());
                            bail!("{o} was replaced by {n}, `crumb land approve {n}` approves the new merge, `git diff {o} {n}` shows what changed");
                        }
                        let full = match resolve(v, &slug, req.cwd.as_deref(), &q) {
                            Ok(full) => full,
                            Err(dirs) if dirs.is_empty() => bail!("{err}, and {slug} has no checkout on this machine to find the commit in, so the approval is not parked"),
                            Err(dirs) => {
                                let dirs = dirs.iter().map(|d| crate::util::path_str(d)).collect::<Vec<_>>().join(", ");
                                bail!("{err}, and no checkout of {slug} here has that commit ({dirs}), so the approval is not parked, check the sha and the project")
                            }
                        };
                        let earlier = parked.iter().position(|p| same_sha(&p.sha, &full)).map(|i| parked.remove(i));
                        let p = Parked { sha: full, note: note.clone(), next: to.as_deref().map(holder), at: now(), by: by.clone(), extra: Map::new() };
                        parked.push(p.clone());
                        return Ok((format!("land approve {} before it is queued", &p.sha[..7]), Err((p, earlier))));
                    }
                    Err(err) => return Err(err),
                };
                let e = &mut entries[i];
                if !force && !matches!(e.state.as_str(), "built" | "approved") {
                    return Err(step_error(e, "approve", "a built entry"));
                }
                let mut said = String::new();
                let had = e.note.clone();
                match (&note, had) {
                    (Some(n), had) if had.as_ref() != Some(n) => {
                        if let Some(old) = e.retire_note() {
                            said = format!("\n  the earlier note \"{}\" by {} stays with the entry, `crumb land show {}` lists it", truncate_chars(&old.note, NOTE_MAX), named(&old.by), e.short());
                        }
                        e.note = Some(n.clone());
                    }
                    (None, Some(n)) => said = format!("\n  keeps the note \"{}\", a new `--note` replaces it and keeps this one", truncate_chars(&n, NOTE_MAX)),
                    _ => {}
                }
                set_state(e, "approved", &by);
                e.next = to.as_deref().map(holder).or_else(|| e.by("built").map(str::to_string));
                Ok((format!("land approve {}", e.short()), Ok((e.clone(), said))))
            })?;
            match done {
                Ok((e, said)) => Ok(state_out(&e, &format!("{settled}{said}"))),
                Err((p, earlier)) => {
                    let was = earlier.map(|o| format!("\n  it replaces the parked {}", parked_line(&o, NOTE_MAX))).unwrap_or_default();
                    let text = format!("parked: {}\n  no queue entry has that sha yet, the approval attaches when `crumb land add {}` queues it{was}{settled}", parked_line(&p, NOTE_MAX), p.sha);
                    Ok(Outcome { text, data: json!({"parked": p}) })
                }
            }
        }
        Op::Verify { sha, to } => {
            let (e, settled) = mutate(v, &slug, |entries, _| {
                let i = find(entries, &sha)?;
                let e = &mut entries[i];
                if !force && e.state != "approved" {
                    return Err(step_error(e, "verify", "an approved entry"));
                }
                set_state(e, "verified", &by);
                e.retire_note();
                e.next = to.as_deref().map(holder).or_else(|| e.by("built").map(str::to_string));
                Ok((format!("land verify {}", e.short()), e.clone()))
            })?;
            Ok(state_out(&e, &settled))
        }
        Op::Pushed { shas, run, to } => {
            let given = run.map(|r| one_line(&r).trim().to_string()).filter(|r| !r.is_empty());
            let (run, why) = match given {
                Some(r) => (Some(r), String::new()),
                None => {
                    let ci = ci::load(v, &slug)?.filter(|c| !c.repo.is_empty()).context("pushed needs the CI run id, `--run <id>`, or `crumb land ci --repo <owner/name>` once so pushed finds the run itself")?;
                    // Check the call first, a refused push should not wait on gh.
                    let entries = read(v, &slug)?;
                    let idx = find_all(&entries, &shas)?;
                    if !force {
                        push_check(&entries, &idx, false)?;
                    }
                    let want: Vec<&str> = idx.iter().rev().map(|&i| entries[i].sha.as_str()).collect();
                    let shorts = idx.iter().map(|&i| entries[i].short()).collect::<Vec<_>>().join(" ");
                    match find_run(&ci, &want) {
                        Ok(Some(id)) => (Some(id), String::new()),
                        Ok(None) => (None, format!("\n  GitHub has no {} for it yet, `crumb land check {shorts}` finds it once one starts", ci_where(&ci))),
                        Err(e) => (None, format!("\n  the run is not known, {e}, `crumb land pushed {shorts} --run <id>` adds it")),
                    }
                }
            };
            let (es, settled) = mutate(v, &slug, |entries, _| {
                let idx = find_all(entries, &shas)?;
                if !force {
                    push_check(entries, &idx, run.is_some())?;
                }
                for &i in &idx {
                    let e = &mut entries[i];
                    if e.state == "pushed" && e.run.is_none() && run.is_some() && !force {
                        e.run = run.clone();
                        continue;
                    }
                    set_state(e, "pushed", &by);
                    e.run = run.clone();
                    e.retire_note();
                    e.next = to.as_deref().map(holder).or_else(|| e.by("approved").map(str::to_string));
                }
                let es: Vec<Entry> = idx.iter().map(|&i| entries[i].clone()).collect();
                Ok((format!("land pushed {}", es.iter().map(Entry::short).collect::<Vec<_>>().join(" ")), es))
            })?;
            if let [e] = es.as_slice() {
                return Ok(state_out(e, &(why + &settled)));
            }
            let all = read(v, &slug)?;
            let text = es.iter().map(|e| format!("{}, next: {}", head(&all, e), due_text(e))).collect::<Vec<_>>().join("\n");
            Ok(Outcome { text: text + &why + &settled, data: json!({"entries": es}) })
        }
        Op::Read { shas, green, to, attempt, note } => {
            let note = note.map(|n| truncate_chars(&one_line(&n), NOTE_MAX)).filter(|n| !n.is_empty());
            read_entries(v, &slug, &by, req.by.clone(), force, &shas, green, to, ReadInfo { attempt, note, ..Default::default() })
        }
        Op::Infra { shas, note, attempt, to } => {
            let note = note.map(|n| truncate_chars(&one_line(&n), NOTE_MAX)).filter(|n| !n.is_empty());
            let attempt = attempt.or_else(|| {
                let c = ci::load(v, &slug).ok().flatten().filter(|c| !c.repo.is_empty())?;
                let entries = read(v, &slug).ok()?;
                let run = find_all(&entries, &shas).ok()?.iter().find_map(|&i| entries[i].run.clone())?;
                ci::view(&c, &run, None, GH_LIMIT).ok().map(|r| r.attempt)
            });
            let (es, settled) = mutate(v, &slug, |entries, _| {
                let idx = find_all(entries, &shas)?;
                if let Some(e) = idx.iter().map(|&i| &entries[i]).find(|e| !force && !matches!(e.state.as_str(), "pushed" | "red")) {
                    return Err(step_error(e, "infra", "a pushed or red entry"));
                }
                for &i in &idx {
                    let e = &mut entries[i];
                    let was_red = e.state == "red";
                    e.reads.push(ReadRec { result: "infra".to_string(), attempt, failed: vec![], note: note.clone(), by: by.clone(), at: now(), extra: Map::new() });
                    set_state(e, "infra", &by);
                    e.state = "pushed".to_string();
                    if to.is_some() || was_red {
                        e.next = to.as_deref().map(holder).or_else(|| e.by("approved").map(str::to_string));
                    }
                }
                let es: Vec<Entry> = idx.iter().map(|&i| entries[i].clone()).collect();
                Ok((format!("land infra {}", es.iter().map(Entry::short).collect::<Vec<_>>().join(" ")), es))
            })?;
            let shorts = es.iter().map(Entry::short).collect::<Vec<_>>().join(" ");
            let all = read(v, &slug)?;
            let mut text = es.iter().map(|e| format!("{}, next: {}", head(&all, e), due_text(e))).collect::<Vec<_>>().join("\n");
            text.push_str(&format!("\n  it stays pushed and its items stay where they are, `crumb land check {shorts}` reads the rerun{settled}"));
            Ok(Outcome { text, data: json!({"entries": es}) })
        }
        Op::Check { shas, wait, to } => check(v, &slug, &by, req.by.clone(), &shas, wait, to),
        Op::Ci { repo, workflow, branch, gate, clear } => {
            if clear {
                ci::save(v, &slug, None)?;
                return Ok(Outcome { text: format!("{slug} has no CI gate now, runs are read by hand"), data: json!({"ci": null}) });
            }
            let old = ci::load(v, &slug)?;
            if repo.is_none() && workflow.is_none() && branch.is_none() && gate.is_none() {
                return Ok(match old {
                    Some(c) => Outcome { text: format!("CI gate of {slug}: {}", c.text()), data: json!({"ci": c}) },
                    None => Outcome { text: format!("{slug} has no CI gate, `crumb land ci --repo <owner/name> --workflow <name> --branch <branch> --gate <jobs>` sets one"), data: json!({"ci": null}) },
                });
            }
            let mut c = old.unwrap_or_default();
            let clean = |s: String| one_line(&s).trim().to_string();
            if let Some(r) = repo {
                let r = clean(r);
                let n = crate::project::normalize_remote(&r);
                c.repo = n.strip_prefix("github.com/").unwrap_or(&n).to_string();
                if !c.repo.is_empty() && c.repo.split('/').filter(|p| !p.is_empty()).count() != 2 {
                    bail!("the repo is owner/name, like Paraxdev/FundaCAD, got {r:?}");
                }
            }
            if let Some(w) = workflow {
                c.workflow = clean(w);
            }
            if let Some(b) = branch {
                c.branch = clean(b);
            }
            if let Some(g) = gate {
                c.gate = g.into_iter().map(clean).filter(|g| !g.is_empty()).collect();
            }
            ci::save(v, &slug, Some(&c))?;
            let missing = if c.repo.is_empty() || c.gate.is_empty() { ", check and wait need a repo and gate jobs" } else { "" };
            Ok(Outcome { text: format!("CI gate of {slug}: {}{missing}", c.text()), data: json!({"ci": c}) })
        }
        Op::Drop { sha } => {
            let ((gone, all), settled) = mutate(v, &slug, |entries, parked| match find(entries, &sha) {
                Ok(i) => {
                    let e = entries.remove(i);
                    Ok((format!("land drop {}", e.short()), (Ok(e), entries.clone())))
                }
                Err(err) => {
                    let q = sha.trim().to_ascii_lowercase();
                    let i = parked.iter().position(|p| same_sha(&p.sha, &q)).ok_or(err)?;
                    let p = parked.remove(i);
                    Ok((format!("land drop the parked approval of {}", &p.sha[..7]), (Err(p), entries.clone())))
                }
            })?;
            match gone {
                Ok(e) => {
                    let (left, moved) = land_left(v, &e, &all, req.by.clone());
                    let mut data = json!({"dropped": e});
                    if !moved.is_empty() {
                        data["items"] = json!(moved);
                    }
                    Ok(Outcome { text: format!("dropped {} ({}) from the queue{left}{settled}", e.short(), e.state), data })
                }
                Err(p) => Ok(Outcome { text: format!("dropped the parked approval {}{settled}", parked_line(&p, NOTE_MAX)), data: json!({"dropped_parked": p}) }),
            }
        }
        Op::Move { sha, after } => {
            let (e, settled) = mutate(v, &slug, |entries, _| {
                let i = find(entries, &sha)?;
                let e = entries.remove(i);
                let at = match after.as_deref() {
                    Some(a) => {
                        let j = find(entries, a).map_err(|err| anyhow::anyhow!("{err}, it is the entry to go after"))?;
                        j + 1
                    }
                    None => 0,
                };
                entries.insert(at, e.clone());
                Ok((format!("land move {}", e.short()), e))
            })?;
            let entries = read(v, &slug)?;
            Ok(Outcome { text: list_text(v, &entries, &[], false) + &settled, data: json!({"moved": e.sha}) })
        }
    }
}

/// What a read records beyond green or red.
#[derive(Default)]
struct ReadInfo {
    attempt: Option<u32>,
    note: Option<String>,
    failed: Vec<String>,
    /// Earlier attempts of the run, found when a rerun is read.
    earlier: Vec<ReadRec>,
    /// The run a check found for entries pushed without one.
    run: Option<String>,
    /// The check saw a newer attempt than the last read, so a red entry reads again.
    rerun: bool,
}

/// Reads entries green or red: saves the read, then a green read moves the items.
#[allow(clippy::too_many_arguments)]
fn read_entries(v: &Vault, slug: &str, by: &str, req_by: Option<String>, force: bool, shas: &[String], green: bool, to: Option<String>, info: ReadInfo) -> Result<Outcome> {
    let word = if green { "green" } else { "red" };
    let ((es, retried, others, all), settled) = mutate(v, slug, |entries, _| {
        let idx = find_all(entries, &shas)?;
        let again = |e: &Entry| green && e.state == "green";
        // A red entry reads again for a rerun of its run, a newer attempt than the one read red.
        let rerun = |e: &Entry| e.state == "red" && (info.rerun || info.attempt.is_some_and(|a| a > e.last_read().and_then(|r| r.0).unwrap_or(1)));
        if let Some(e) = idx.iter().map(|&i| &entries[i]).find(|e| !force && e.state != "pushed" && !again(e) && !rerun(e)) {
            return Err(step_error(e, "read", "a pushed entry"));
        }
        let mut changed = vec![];
        let mut retried = vec![];
        for &i in &idx {
            let e = &mut entries[i];
            if again(e) {
                retried.push(e.sha.clone());
                continue;
            }
            if e.run.is_none() {
                e.run = info.run.clone();
            }
            let known: Vec<Option<u32>> = e.reads.iter().map(|r| r.attempt).collect();
            e.reads.extend(info.earlier.iter().filter(|r| !known.contains(&r.attempt)).cloned());
            if info.attempt.is_some() || info.note.is_some() || !info.failed.is_empty() {
                e.reads.push(ReadRec { result: word.to_string(), attempt: info.attempt, failed: info.failed.clone(), note: info.note.clone(), by: by.to_string(), at: now(), extra: Map::new() });
            }
            set_state(e, word, by);
            e.next = if green { None } else { to.as_deref().map(holder).or_else(|| e.by("built").map(str::to_string)) };
            changed.push(e.short().to_string());
        }
        let mut others: Vec<(String, Vec<String>)> = vec![];
        for r in idx.iter().filter_map(|&i| entries[i].run.as_ref()) {
            let unread: Vec<String> = entries.iter().enumerate().filter(|(j, e)| !idx.contains(j) && e.state == "pushed" && e.run.as_ref() == Some(r)).map(|(_, e)| e.short().to_string()).collect();
            if !unread.is_empty() && !others.iter().any(|(o, _)| o == r) {
                others.push((r.clone(), unread));
            }
        }
        let msg = if changed.is_empty() { String::new() } else { format!("land read {} {word}", changed.join(" ")) };
        Ok((msg, (idx.iter().map(|&i| entries[i].clone()).collect::<Vec<Entry>>(), retried, others, entries.clone())))
    })?;
    let multi = es.len() > 1;
    let mut blocks = vec![];
    let mut moved_all = vec![];
    let mut done = vec![];
    for e in &es {
        let mut out = state_out(v, slug, e, "");
        if green {
            let (text, moved) = land_items(v, e, &all, req_by.clone(), &mut done);
            out.text.push_str(&text);
            if retried.contains(&e.sha) {
                out.text.push_str(&format!("\n  {} green already, moved what was left", if multi { format!("{} was", e.short()) } else { "it was".to_string() }));
            }
            moved_all.extend(moved);
        }
        blocks.push(out.text);
    }
    let mut text = blocks.join("\n");
    if !green {
        text.push_str(if multi { "\n  their items stay where they are" } else { "\n  its items stay where they are" });
    }
    for (r, unread) in &others {
        text.push_str(&format!("\n  run {r} also holds {} unread, `crumb land read {} {word}` reads {} too", unread.join(", "), unread.join(" "), if unread.len() > 1 { "them" } else { "it" }));
    }
    text.push_str(&settled);
    let mut data = if multi { json!({"entries": es}) } else { json!({"entry": es[0]}) };
    if green {
        data["items"] = json!(moved_all);
    }
    Ok(Outcome { text, data })
}

/// Asks gh once for the runs of entries that look stale, so one whose run is still going is not marked stale.
fn ask_live(v: &Vault, slug: &str, entries: &mut [Entry]) {
    if !entries.iter().any(Entry::late) {
        return;
    }
    let Some(c) = ci::load(v, slug).ok().flatten().filter(|c| !c.repo.is_empty()) else { return };
    let Ok(rows) = ci::runs(&c, GH_LIST_LIMIT) else { return };
    for e in entries.iter_mut().filter(|e| e.late()) {
        e.live = match &e.run {
            Some(r) => rows.iter().find(|x| x.database_id.to_string() == *r),
            None => ci::run_for(&rows, &[&e.sha]),
        }
        .cloned();
    }
}

/// One look at the run of the named entries.
struct Looked {
    text: String,
    /// The gate is decided and read, or says nothing more until someone acts.
    done: bool,
    data: Value,
}

fn need_ci(v: &Vault, slug: &str) -> Result<ci::Ci> {
    let c = ci::load(v, slug)?.with_context(|| format!("{slug} has no CI gate, `crumb land ci --repo <owner/name> --workflow <name> --branch <branch> --gate <jobs>` sets one, `crumb land read <sha> green|red` reads a run by hand"))?;
    if c.repo.is_empty() || c.gate.is_empty() {
        bail!("the CI gate of {slug} needs a repo and gate jobs, `crumb land ci --repo <owner/name> --gate <jobs>` sets them ({})", c.text());
    }
    Ok(c)
}

/// Looks at the run of `shas` once: prints each gate job, and reads every entry pushed on the run once the gate is
/// decided for an attempt not read yet. A red entry whose run was rerun goes back to pushed.
#[allow(clippy::too_many_arguments)]
fn look(v: &Vault, slug: &str, c: &ci::Ci, by: &str, req_by: Option<String>, shas: &[String], to: &Option<String>) -> Result<std::result::Result<Looked, ci::GhError>> {
    let entries = read(v, slug)?;
    let idx = find_all(&entries, shas)?;
    for &i in &idx {
        let e = &entries[i];
        if !matches!(e.state.as_str(), "pushed" | "red" | "green") {
            return Err(step_error(e, "check", "a pushed entry"));
        }
    }
    let mut runs: Vec<&String> = idx.iter().filter_map(|&i| entries[i].run.as_ref()).collect();
    runs.dedup();
    if runs.len() > 1 {
        let names = idx.iter().map(|&i| entries[i].short()).collect::<Vec<_>>().join(", ");
        bail!("{names} are on different runs ({}), check the entries of one run", runs.iter().map(|r| r.as_str()).collect::<Vec<_>>().join(", "));
    }
    let shorts = |ix: &[usize]| ix.iter().map(|&i| entries[i].short().to_string()).collect::<Vec<_>>().join(" ");
    let (id, found) = match runs.first() {
        Some(r) => (r.to_string(), false),
        None => {
            let want: Vec<&str> = idx.iter().rev().map(|&i| entries[i].sha.as_str()).collect();
            let rows = match ci::runs(c, GH_LIMIT) {
                Ok(r) => r,
                Err(e) => return Ok(Err(e)),
            };
            match ci::run_for(&rows, &want) {
                Some(r) => (r.database_id.to_string(), true),
                None => return Ok(Ok(Looked { text: format!("GitHub has no {} for {} yet", ci_where(c), shorts(&idx)), done: false, data: json!({"run": null}) })),
            }
        }
    };
    let run = match ci::view(c, &id, None, GH_LIMIT) {
        Ok(r) => r,
        Err(e) => return Ok(Err(e)),
    };
    let gate = ci::gate(&c.gate, &run);
    let status = if run.status == "completed" { format!("finished {}", run.conclusion) } else { run.status.replace('_', " ") };
    let mut text = format!("run {id} attempt {}, {status}, gate {}:\n  {}", run.attempt, gate.verdict.word(), gate.lines.join("\n  "));
    let data = json!({"run": id, "attempt": run.attempt, "gate": gate.verdict.word(), "jobs": gate.lines});
    let group: Vec<usize> = entries.iter().enumerate().filter(|(i, e)| idx.contains(i) || (matches!(e.state.as_str(), "pushed" | "red") && e.run.as_deref() == Some(id.as_str()))).map(|(i, _)| i).collect();
    let open: Vec<usize> = group.iter().copied().filter(|&i| entries[i].state != "green" && !entries[i].covers(&run)).collect();
    let decided = matches!(gate.verdict, ci::Verdict::Green | ci::Verdict::Red(_));
    if !decided || open.is_empty() {
        let back: Vec<String> = open.iter().filter(|&&i| entries[i].state == "red").map(|&i| entries[i].sha.clone()).collect();
        let fill: Vec<String> = if found { group.iter().map(|&i| entries[i].sha.clone()).collect() } else { vec![] };
        if !back.is_empty() || !fill.is_empty() {
            mutate(v, slug, |es, _| {
                let mut n = vec![];
                for e in es.iter_mut() {
                    if fill.contains(&e.sha) && e.run.is_none() {
                        e.run = Some(id.clone());
                        n.push(e.short().to_string());
                    }
                    if back.contains(&e.sha) && e.state == "red" {
                        set_state(e, "rerun", by);
                        e.state = "pushed".to_string();
                        e.next = to.as_deref().map(holder).or_else(|| e.by("approved").map(str::to_string));
                        n.push(e.short().to_string());
                    }
                }
                n.dedup();
                Ok((if n.is_empty() { String::new() } else { format!("land check {}", n.join(" ")) }, ()))
            })?;
        }
        if !back.is_empty() {
            text.push_str(&format!("\n{} pushed again, run {id} was rerun", back.iter().map(|s| &s[..s.len().min(7)]).collect::<Vec<_>>().join(", ")));
        }
        let waiting = group.iter().find_map(|&i| entries[i].infra().filter(|_| !open.contains(&i)).map(|r| (i, r)));
        let (tail, done) = match (&gate.verdict, waiting) {
            (ci::Verdict::Going, _) => ("the gate is still going, nothing read yet".to_string(), false),
            (ci::Verdict::Cancelled, _) => (format!("run {id} was cancelled, so it says nothing of the code, a later push's run carries it, read {} by hand once that one is read, or drop it", shorts(&group)), true),
            (_, Some((i, r))) => (format!("{} has an infra red{}, waiting for a rerun of the failed jobs", entries[i].short(), attempt_text(r.attempt)), false),
            (verdict, None) => (format!("{} read {} for this attempt already", shorts(&group), verdict.word()), true),
        };
        text.push_str(&format!("\n{tail}"));
        return Ok(Ok(Looked { text, done, data }));
    }
    let failed = match &gate.verdict {
        ci::Verdict::Red(f) => f.clone(),
        _ => vec![],
    };
    let mut earlier = vec![];
    let known: Vec<u32> = open.iter().flat_map(|&i| entries[i].reads.iter().filter_map(|r| r.attempt)).collect();
    for k in run.attempt.saturating_sub(3).max(1)..run.attempt {
        if known.contains(&k) {
            continue;
        }
        if let Ok(r) = ci::view(c, &id, Some(k), GH_LIMIT) {
            let g = ci::gate(&c.gate, &r);
            let failed = if let ci::Verdict::Red(f) = &g.verdict { f.clone() } else { vec![] };
            earlier.push(ReadRec { result: g.verdict.word().to_string(), attempt: Some(k), failed, note: None, by: by.to_string(), at: now(), extra: Map::new() });
        }
    }
    let green = gate.verdict == ci::Verdict::Green;
    let read_shas: Vec<String> = open.iter().map(|&i| entries[i].sha.clone()).collect();
    let info = ReadInfo { attempt: Some(run.attempt), failed, earlier, run: Some(id.clone()), rerun: true, ..Default::default() };
    let out = read_entries(v, slug, by, req_by, false, &read_shas, green, to.clone(), info)?;
    let mut data = data;
    data["read"] = out.data;
    Ok(Ok(Looked { text: format!("{text}\n{}", out.text), done: true, data }))
}

/// `land check` looks once, `land wait` looks every so often until the gate is decided or the time is up.
fn check(v: &Vault, slug: &str, by: &str, req_by: Option<String>, shas: &[String], wait: Option<Wait>, to: Option<String>) -> Result<Outcome> {
    let c = need_ci(v, slug)?;
    let Some(w) = wait else {
        let l = look(v, slug, &c, by, req_by, shas, &to)?.map_err(|e| anyhow::anyhow!("{e}, `crumb land read <sha> green|red` reads the run by hand"))?;
        return Ok(Outcome { text: l.text, data: l.data });
    };
    let start = std::time::Instant::now();
    let limit = std::time::Duration::from_secs(w.minutes * 60);
    let every = std::time::Duration::from_secs(w.every.max(1));
    let (mut shown, mut fails) = (String::new(), 0);
    loop {
        match look(v, slug, &c, by, req_by.clone(), shas, &to)? {
            Ok(l) if l.done => return Ok(Outcome { text: l.text, data: l.data }),
            Ok(l) => {
                fails = 0;
                if l.text != shown {
                    eprintln!("{} {}", chrono::Local::now().format("%H:%M"), l.text);
                    shown = l.text;
                }
            }
            Err(e @ (ci::GhError::Missing | ci::GhError::Auth)) => bail!("{e}, `crumb land read <sha> green|red` reads the run by hand"),
            Err(e) => {
                fails += 1;
                if fails >= 5 {
                    bail!("{e}, five looks in a row failed, `crumb land wait` again or read the run by hand");
                }
                eprintln!("{e}, looking again");
            }
        }
        if start.elapsed() + every > limit {
            let last = if shown.is_empty() { String::new() } else { format!(", last look:\n{shown}") };
            return Ok(Outcome { text: format!("the gate is not decided after {} minutes, nothing read{last}", w.minutes), data: json!({"decided": false}) });
        }
        std::thread::sleep(every);
    }
}

const REVIEWABLE: [&str; 3] = ["queued", "in-progress", "review"];

/// Whether a green read moves the item: queued, in progress or in review, or still in backlog with a claim, since a
/// claim leaves the state alone.
fn goes_to_review(n: &crate::note::Note) -> bool {
    let st = backlog::state(n);
    REVIEWABLE.contains(&st.as_str()) || st == "backlog" && backlog::assignee(n).is_some()
}

/// The session id in a step's `name id`, the whole text when it has none, empty for the user.
fn session_of(by: &str) -> String {
    let by = by.trim();
    let last = by.rsplit(' ').next().unwrap_or(by);
    let id = if last.len() == 8 && last.chars().all(|c| c.is_ascii_hexdigit()) { last } else { by };
    if id.eq_ignore_ascii_case(backlog::DEFAULT_ACTOR) { String::new() } else { id.to_string() }
}

/// Moves the items of a green entry to review through the code `crumb move` runs. An item another open or red entry
/// carries waits for the last of them, with this commit added to its refs at once so the move has every piece even
/// after older green entries left the file. `built_by` is the item's claim holder, else the session that added the
/// entry, never the reader.
fn land_items(v: &Vault, e: &Entry, all: &[Entry], by: Option<String>, done: &mut Vec<String>) -> (String, Vec<Value>) {
    let mut text = String::new();
    let mut reqs = vec![];
    let mut waiting = vec![];
    for id in &e.items {
        if done.contains(id) {
            continue;
        }
        let n = match v.get(id) {
            Ok(n) => n,
            Err(_) => {
                text.push_str(&format!("\n  {id} is not in the vault any more, left alone"));
                continue;
            }
        };
        if !goes_to_review(&n) {
            text.push_str(&format!("\n  left {} where it is, it is {} and only queued, in-progress, review or claimed backlog items go to review", n.handle(), backlog::state(&n)));
            continue;
        }
        let open: Vec<String> = all.iter().filter(|x| x.sha != e.sha && x.items.contains(id) && (x.is_open() || x.state == "red")).map(|x| format!("{} {}", x.short(), x.state)).collect();
        if !open.is_empty() {
            let verb = if open.len() > 1 { "carry" } else { "carries" };
            text.push_str(&format!("\n  left {} where it is, {} still {verb} it, the last of them read green moves it", n.handle(), open.join(", ")));
            if !n.refs().contains(&format!("commit:{}", e.sha)) {
                waiting.push(id.clone());
            }
            continue;
        }
        done.push(id.clone());
        reqs.push((id.clone(), review_move(&n, e, all, by.clone())));
    }
    add_ref(v, &waiting, e, by.clone(), &mut text);
    let moved = apply_moves(v, &reqs, &mut text);
    (text, moved)
}

/// The move to review that the green read of `last` makes, with every green carrier's commit as a ref.
fn review_move(n: &crate::note::Note, last: &Entry, all: &[Entry], by: Option<String>) -> MoveReq {
    let mut refs = vec![format!("commit:{}", last.sha)];
    for x in all.iter().filter(|x| x.sha != last.sha && x.state == "green" && x.items.contains(&n.id)) {
        refs.push(format!("commit:{}", x.sha));
    }
    let built_by = backlog::assignee(n).or_else(|| last.by("built").map(session_of)).unwrap_or_default();
    MoveReq { state: "review".into(), refs, by, built_by: Some(built_by), ..Default::default() }
}

/// Adds `commit:<sha of e>` to the refs of items that wait for another carrier, in one vault commit.
fn add_ref(v: &Vault, ids: &[String], e: &Entry, by: Option<String>, text: &mut String) {
    if ids.is_empty() {
        return;
    }
    let mut quiet = v.clone();
    quiet.config.auto_commit = false;
    let mut touched = vec![];
    for id in ids {
        match crate::ops::update(&quiet, id, crate::ops::UpdateReq { refs: vec![format!("commit:{}", e.sha)], by: by.clone(), ..Default::default() }) {
            Ok(n) => touched.extend(n.path),
            Err(err) => text.push_str(&format!("\n  adding commit:{} to {id} failed: {}", e.short(), one_line(&format!("{err:#}")))),
        }
    }
    if !touched.is_empty()
        && let Err(err) = v.lock().and_then(|_g| v.commit(&touched, &format!("breadcrumb: land ref {} on {} waiting items", e.short(), touched.len())))
    {
        text.push_str(&format!("\n  committing the refs failed: {}", one_line(&format!("{err:#}"))));
    }
}

fn apply_moves(v: &Vault, reqs: &[(String, MoveReq)], text: &mut String) -> Vec<Value> {
    let mut moved = vec![];
    if reqs.is_empty() {
        return moved;
    }
    match backlog::move_each(v, reqs) {
        Ok(results) => {
            for (id, r) in results {
                match r {
                    Ok(r) => {
                        text.push('\n');
                        text.push_str(backlog::move_text(&r).trim_end());
                        moved.push(json!({"id": r.note.id, "from": r.from, "to": r.to, "warnings": r.warnings}));
                    }
                    Err(err) => text.push_str(&format!("\n  failed {id}: {}", one_line(&format!("{err:#}")))),
                }
            }
        }
        Err(err) => text.push_str(&format!("\n  moving the items failed: {}", one_line(&format!("{err:#}")))),
    }
    moved
}

/// Items of `gone`, an open or red entry dropped or replaced, that only green entries still carry: they waited for
/// `gone`, so they move now as the last green read would have moved them. Items in review already stay.
fn land_left(v: &Vault, gone: &Entry, all: &[Entry], by: Option<String>) -> (String, Vec<Value>) {
    let mut text = String::new();
    let mut reqs = vec![];
    if !(gone.is_open() || gone.state == "red") {
        return (text, vec![]);
    }
    for id in &gone.items {
        if all.iter().any(|x| x.items.contains(id) && (x.is_open() || x.state == "red")) {
            continue;
        }
        let Some(last) = all.iter().filter(|x| x.state == "green" && x.items.contains(id)).max_by_key(|x| x.at) else { continue };
        let Ok(n) = v.get(id) else { continue };
        if !goes_to_review(&n) || backlog::state(&n) == "review" {
            continue;
        }
        text.push_str(&format!("\n  {} waited for {}, {} read green carries it, so it moves now", n.handle(), gone.short(), last.short()));
        reqs.push((id.clone(), review_move(&n, last, all, by.clone())));
    }
    let moved = apply_moves(v, &reqs, &mut text);
    (text, moved)
}

/// The queue's head for the digest: what is open, whose move it is, and any pushed entry nobody read.
pub fn digest_lines(v: &Vault, project: Option<&str>) -> Option<String> {
    let mut entries = read(v, project?).ok()?;
    ask_live(v, project?, &mut entries);
    let open: Vec<&Entry> = entries.iter().filter(|e| e.is_open() || e.state == "red").collect();
    if open.is_empty() {
        return None;
    }
    let mut s = format!("Landing queue, {} to land, `crumb land list`:\n", open.len());
    let mut shown = 0;
    for (i, e) in open.iter().enumerate() {
        if i >= DIGEST_ROWS && !e.stale() || shown >= DIGEST_MAX_ROWS {
            continue;
        }
        shown += 1;
        s.push_str(&format!("{}, {}{}\n", head(&entries, e), due_text(e), memo_tail(e, DIGEST_NOTE)));
    }
    if open.len() > shown {
        s.push_str(&format!("+{} more\n", open.len() - shown));
    }
    Some(s)
}

#[derive(Args, Debug)]
pub struct LandArgs {
    /// Project slug (default: detected from the current directory)
    #[arg(long, short = 'p', global = true)]
    pub project: Option<String>,
    /// Who does the step (default: $BREADCRUMB_SESSION, else the Claude Code session)
    #[arg(long, global = true)]
    pub by: Option<String>,
    /// Skip the state and order checks
    #[arg(long, global = true)]
    pub force: bool,
    #[command(subcommand)]
    pub cmd: Option<LandCmd>,
}

#[derive(Subcommand, Debug)]
pub enum LandCmd {
    /// Queue a merge commit at the end, or in the place of the one --replaces names
    Add {
        /// The merge commit, 7 to 40 hex characters
        sha: String,
        /// Backlog ids or slugs this merge lands, comma separated
        #[arg(long, value_delimiter = ',')]
        items: Vec<String>,
        /// A note shown with the entry, like a condition or the worktree and branch it sits in
        #[arg(long)]
        note: Option<String>,
        /// Session that holds the next step
        #[arg(long)]
        to: Option<String>,
        /// A queued entry this merge replaces, the same work merged again: it keeps its place, items, note and approval
        #[arg(long)]
        replaces: Option<String>,
        /// With --replaces: start built, the old approval does not carry over
        #[arg(long, requires = "replaces")]
        clear_approval: bool,
    },
    /// Queue <new> in the place of <old>, the same as `add <new> --replaces <old>`
    Replace {
        /// The queued merge commit it replaces
        old: String,
        /// The new merge commit, 7 to 40 hex characters
        new: String,
        /// Backlog ids or slugs, the old entry's when left out
        #[arg(long, value_delimiter = ',')]
        items: Vec<String>,
        /// A note, the old entry's when left out
        #[arg(long)]
        note: Option<String>,
        /// Session that holds the next step
        #[arg(long)]
        to: Option<String>,
        /// Start built, the old approval does not carry over
        #[arg(long)]
        clear_approval: bool,
    },
    /// The review approves it, --note names a condition like "pending the alone harness run". A sha not queued yet parks
    /// the approval until `add` queues it
    Approve {
        /// The merge commit, 4 or more leading hex characters find a queued one
        sha: String,
        /// A condition the approval waits on
        #[arg(long)]
        note: Option<String>,
        /// Session that holds the next step
        #[arg(long)]
        to: Option<String>,
    },
    /// The condition is met, clears the approval's note
    Verify {
        /// The merge commit, 4 or more leading hex characters find a queued one
        sha: String,
        /// Session that holds the next step
        #[arg(long)]
        to: Option<String>,
    },
    /// It was pushed and started a CI run, several commits for one push that carries them all
    Pushed {
        /// The merge commits, 4 or more leading hex characters find a queued one
        #[arg(required = true, value_delimiter = ',')]
        shas: Vec<String>,
        /// The CI run id, found by the commit when left out and the project has a CI gate (`land ci`)
        #[arg(long)]
        run: Option<String>,
        /// Session that holds the next step
        #[arg(long)]
        to: Option<String>,
    },
    /// The CI run was read, green moves the entry's items to review with commit:<sha> as a ref
    Read {
        /// The merge commits of one run, 4 or more leading hex characters find a queued one
        #[arg(required = true, value_delimiter = ',')]
        shas: Vec<String>,
        /// green, red, or infra for a red from outside the code
        result: String,
        /// Session that holds the next step
        #[arg(long)]
        to: Option<String>,
        /// The run attempt read, a rerun's is 2 or more
        #[arg(long)]
        attempt: Option<u32>,
        /// What the read found, like the job that flaked
        #[arg(long)]
        note: Option<String>,
    },
    /// The CI run was read green, the same as `read <sha> green`
    Green {
        /// The merge commits of one run, 4 or more leading hex characters find a queued one
        #[arg(required = true, value_delimiter = ',')]
        shas: Vec<String>,
        /// Session that holds the next step
        #[arg(long)]
        to: Option<String>,
        /// The run attempt read, a rerun's is 2 or more
        #[arg(long)]
        attempt: Option<u32>,
        /// What the read found, like the job that flaked
        #[arg(long)]
        note: Option<String>,
    },
    /// The CI run was read red, the same as `read <sha> red`
    Red {
        /// The merge commits of one run, 4 or more leading hex characters find a queued one
        #[arg(required = true, value_delimiter = ',')]
        shas: Vec<String>,
        /// Session that holds the next step
        #[arg(long)]
        to: Option<String>,
        /// The run attempt read, a rerun's is 2 or more
        #[arg(long)]
        attempt: Option<u32>,
        /// What the read found, like the job that flaked
        #[arg(long)]
        note: Option<String>,
    },
    /// The run failed outside the code, like a runner out of disk: the entry stays pushed, waiting for a rerun
    Infra {
        /// The merge commits of one run, 4 or more leading hex characters find a queued one
        #[arg(required = true, value_delimiter = ',')]
        shas: Vec<String>,
        /// What failed, like "runner disk full"
        #[arg(long)]
        note: Option<String>,
        /// The run attempt that failed, asked of gh when left out
        #[arg(long)]
        attempt: Option<u32>,
        /// Session that holds the next step
        #[arg(long)]
        to: Option<String>,
    },
    /// One look at the run of pushed entries: each gate job, and a green or red read once the gate is decided
    Check {
        /// The merge commits of one run, 4 or more leading hex characters find a queued one
        #[arg(required = true, value_delimiter = ',')]
        shas: Vec<String>,
        /// Session that holds the next step
        #[arg(long)]
        to: Option<String>,
    },
    /// Looks at the run every so often until the gate is decided, then reads it green or red
    Wait {
        /// The merge commits of one run, 4 or more leading hex characters find a queued one
        #[arg(required = true, value_delimiter = ',')]
        shas: Vec<String>,
        /// Minutes to wait at most
        #[arg(long, default_value_t = 180)]
        timeout: u64,
        /// Seconds between looks
        #[arg(long, default_value_t = 60)]
        every: u64,
        /// Session that holds the next step
        #[arg(long)]
        to: Option<String>,
    },
    /// The project's CI gate: the repo, workflow and branch pushes run on and the jobs that must pass. Shows it, or sets what is given
    Ci {
        /// The GitHub repo, owner/name
        #[arg(long)]
        repo: Option<String>,
        /// The workflow, by name or file
        #[arg(long)]
        workflow: Option<String>,
        /// The branch the queue pushes to
        #[arg(long)]
        branch: Option<String>,
        /// Gate jobs, comma separated: a job name, which takes a matrix job's legs too, or a prefix ending in *
        #[arg(long, value_delimiter = ',')]
        gate: Option<Vec<String>>,
        /// Remove the project's CI gate
        #[arg(long)]
        clear: bool,
    },
    /// The queue in order, finished entries only with --all
    List {
        /// Show green entries too
        #[arg(long)]
        all: bool,
    },
    /// One entry whole: its notes uncut, items, steps and earlier approval notes
    Show {
        /// The merge commit, 4 or more leading hex characters find a queued one
        sha: String,
    },
    /// Remove an entry
    Drop {
        /// The merge commit, 4 or more leading hex characters find a queued one
        sha: String,
    },
    /// Put an entry first, or after another one
    Move {
        /// The merge commit, 4 or more leading hex characters find a queued one
        sha: String,
        /// Goes right after this entry, first when left out
        #[arg(long)]
        after: Option<String>,
    },
}

impl LandArgs {
    pub fn into_req(self) -> Result<Req> {
        let op = match self.cmd.unwrap_or(LandCmd::List { all: false }) {
            LandCmd::Add { sha, items, note, to, replaces, clear_approval } => Op::Add { sha, items, note, to, replaces, clear_approval },
            LandCmd::Replace { old, new, items, note, to, clear_approval } => Op::Add { sha: new, items, note, to, replaces: Some(old), clear_approval },
            LandCmd::Approve { sha, note, to } => Op::Approve { sha, note, to },
            LandCmd::Verify { sha, to } => Op::Verify { sha, to },
            LandCmd::Pushed { shas, run, to } => Op::Pushed { shas, run, to },
            LandCmd::Read { mut shas, mut result, to, attempt, note } => {
                let word = |s: &str| s.trim().eq_ignore_ascii_case("infra") || parse_result(s).is_ok();
                // `read green <sha>` reads as meant too.
                if !word(&result) && shas.first().is_some_and(|s| word(s)) {
                    std::mem::swap(&mut shas[0], &mut result);
                }
                let shas = shas.iter().flat_map(|s| s.split(',')).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();
                if result.trim().eq_ignore_ascii_case("infra") {
                    Op::Infra { shas, note, attempt, to }
                } else {
                    Op::Read { shas, green: parse_result(&result)?, to, attempt, note }
                }
            }
            LandCmd::Green { shas, to, attempt, note } => Op::Read { shas, green: true, to, attempt, note },
            LandCmd::Red { shas, to, attempt, note } => Op::Read { shas, green: false, to, attempt, note },
            LandCmd::Infra { shas, note, attempt, to } => Op::Infra { shas, note, attempt, to },
            LandCmd::Check { shas, to } => Op::Check { shas, wait: None, to },
            LandCmd::Wait { shas, timeout, every, to } => Op::Check { shas, wait: Some(Wait { minutes: timeout, every }), to },
            LandCmd::Ci { repo, workflow, branch, gate, clear } => Op::Ci { repo, workflow, branch, gate, clear },
            LandCmd::List { all } => Op::List { all },
            LandCmd::Show { sha } => Op::Show { sha },
            LandCmd::Drop { sha } => Op::Drop { sha },
            LandCmd::Move { sha, after } => Op::Move { sha, after },
        };
        Ok(Req { op, by: self.by, force: self.force, cwd: std::env::current_dir().ok() })
    }
}

fn parse_result(s: &str) -> Result<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "green" => Ok(true),
        "red" => Ok(false),
        other => bail!("a CI read is green or red, got {other:?}"),
    }
}

/// The MCP `land` tool.
pub fn mcp(v: &Vault, project: Option<&str>, a: &Map<String, Value>, cwd: &Path) -> Result<String> {
    let s = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_string).filter(|x| !x.trim().is_empty());
    let need = |k: &str| s(k).with_context(|| format!("{k} is required for this action"));
    let flag = |k: &str| a.get(k).and_then(Value::as_bool).unwrap_or(false);
    let items: Vec<String> = a.get("items").and_then(Value::as_array).map(|xs| xs.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    let to = s("to");
    let shas = || -> Result<Vec<String>> { Ok(need("sha")?.split(|c: char| c == ',' || c.is_whitespace()).filter(|x| !x.is_empty()).map(str::to_string).collect()) };
    let op = match s("action").as_deref().unwrap_or("list") {
        "list" => Op::List { all: flag("all") },
        "show" => Op::Show { sha: need("sha")? },
        "add" => Op::Add { sha: need("sha")?, items, note: s("note"), to, replaces: s("replaces"), clear_approval: flag("clear_approval") },
        "replace" => Op::Add { sha: need("sha")?, items, note: s("note"), to, replaces: Some(need("replaces")?), clear_approval: flag("clear_approval") },
        "approve" => Op::Approve { sha: need("sha")?, note: s("note"), to },
        "verify" => Op::Verify { sha: need("sha")?, to },
        "pushed" => Op::Pushed { shas: shas()?, run: s("run"), to },
        read @ ("green" | "red") => Op::Read { shas: shas()?, green: read == "green", to, attempt: None, note: s("note") },
        "read" => Op::Read { shas: shas()?, green: parse_result(&need("result")?)?, to, attempt: None, note: s("note") },
        "infra" => Op::Infra { shas: shas()?, note: s("note"), attempt: None, to },
        "check" => Op::Check { shas: shas()?, wait: None, to },
        "drop" => Op::Drop { sha: need("sha")? },
        "move" => Op::Move { sha: need("sha")?, after: s("after") },
        other => bail!("unknown land action {other:?}, use list, show, add, replace, approve, verify, pushed, check, green, red, infra, read, drop or move"),
    };
    Ok(run(v, project, Req { op, by: s("by"), force: flag("force"), cwd: Some(cwd.to_path_buf()) })?.text)
}
