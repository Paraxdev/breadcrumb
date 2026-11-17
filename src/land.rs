//! The landing queue: the merge commits two sessions push to a branch one by one, each with its review, its CI run
//! and the session that holds the next step. It lives in the vault as `projects/<slug>/land.jsonl`, one entry per
//! line so git merges concurrent edits of different entries, and syncs like notes do.

use crate::backlog::{self, MoveReq};
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
    /// Keys a newer binary wrote stay as they are.
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

    pub fn stale(&self) -> bool {
        self.state == "pushed" && now() - self.at >= Duration::minutes(STALE_MINUTES)
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

fn read_lines<T: serde::de::DeserializeOwned>(p: &Path) -> Result<Vec<T>> {
    let text = match std::fs::read_to_string(p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e).with_context(|| format!("reading {}", p.display())),
    };
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| serde_json::from_str(l).with_context(|| format!("{} line {} is not a queue entry, a merge conflict may have left markers in it", p.display(), i + 1)))
        .collect()
}

fn read(v: &Vault, slug: &str) -> Result<Vec<Entry>> {
    read_lines(&path(v, slug))
}

fn read_parked(v: &Vault, slug: &str) -> Result<Vec<Parked>> {
    read_lines(&parked_path(v, slug))
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
    Pushed { shas: Vec<String>, run: String, to: Option<String> },
    Read { shas: Vec<String>, green: bool, to: Option<String> },
    Drop { sha: String },
    Move { sha: String, after: Option<String> },
}

#[derive(Clone, Debug)]
pub struct Req {
    pub op: Op,
    pub by: Option<String>,
    pub force: bool,
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
    s + &run_age(entries, e)
}

fn run_age(entries: &[Entry], e: &Entry) -> String {
    let mut s = String::new();
    if let Some(r) = &e.run {
        s.push_str(&format!(", run {r}{}", peers(entries, e)));
    }
    s.push_str(&format!(", {} ago", short_age(&e.at)));
    if e.stale() {
        s.push_str(", STALE, no read yet");
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

fn mutate<T>(v: &Vault, slug: &str, f: impl FnOnce(&mut Vec<Entry>, &mut Vec<Parked>) -> Result<(String, T)>) -> Result<T> {
    let _g = v.lock()?;
    let mut entries = read(v, slug)?;
    let mut parked = read_parked(v, slug)?;
    let before = [lines(&entries)?, lines(&parked)?];
    let (msg, out) = f(&mut entries, &mut parked)?;
    if msg.is_empty() {
        return Ok(out);
    }
    prune(&mut entries, &mut parked);
    let mut paths = vec![];
    for ((p, after), was) in [(path(v, slug), lines(&entries)?), (parked_path(v, slug), lines(&parked)?)].into_iter().zip(before) {
        if after != was {
            atomic_write(&p, after.as_bytes())?;
            paths.push(p);
        }
    }
    if !paths.is_empty() {
        v.commit(&paths, &format!("breadcrumb: {msg}"))?;
    }
    Ok(out)
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

pub fn run(v: &Vault, project: Option<&str>, req: Req) -> Result<Outcome> {
    let slug = slug_of(v, project)?;
    let by = session::attribute(req.by.as_deref());
    let force = req.force;
    let state_out = |e: &Entry, extra: &str| {
        let all = read(v, &slug).unwrap_or_default();
        let memo = e.memo.as_ref().map(|m| format!("\n  note: {m}")).unwrap_or_default();
        Outcome { text: format!("{}\n  next: {}{memo}{extra}", head(&all, e), due_text(e)), data: json!({"entry": e}) }
    };
    match req.op {
        Op::List { all } => {
            let entries = read(v, &slug)?;
            let parked = read_parked(v, &slug)?;
            let shown: Vec<&Entry> = entries.iter().filter(|e| all || e.state != "green").collect();
            let mut data = json!({"entries": shown});
            if !parked.is_empty() {
                data["parked"] = json!(parked);
            }
            Ok(Outcome { text: list_text(v, &entries, &parked, all), data })
        }
        Op::Show { sha } => {
            let entries = read(v, &slug)?;
            match find(&entries, &sha) {
                Ok(i) => Ok(Outcome { text: show_text(v, &entries, i), data: json!({"entry": entries[i]}) }),
                Err(err) => {
                    let q = sha.trim().to_ascii_lowercase();
                    let parked = read_parked(v, &slug)?;
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
            let (e, said) = mutate(v, &slug, |entries, parked| {
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
                    extra: Map::new(),
                };
                let mut said = String::new();
                let mut at = entries.len();
                if let Some(r) = replaces.as_deref() {
                    let i = find(entries, r).map_err(|err| anyhow::anyhow!("{err}, it is the entry this one replaces"))?;
                    if matches!(entries[i].state.as_str(), "pushed" | "green") {
                        bail!("{} is {}, it is on the branch already, a replace is for an entry not pushed yet or read red", entries[i].short(), entries[i].state);
                    }
                    at = i;
                    said = carry(&mut e, entries.remove(i), clear_approval, to.is_some());
                }
                if let Some(i) = parked.iter().position(|p| same_sha(&p.sha, &sha)) {
                    let p = parked.remove(i);
                    e.retire_note();
                    e.state = "approved".into();
                    e.steps.push(Step { state: "approved".into(), at: p.at, by: p.by.clone() });
                    e.note = p.note.clone();
                    e.next = to.as_deref().map(holder).or(p.next.clone()).or_else(|| Some(by.clone()));
                    said.push_str(&format!("\n  approved by {} {} ago before it was queued", named(&p.by), short_age(&p.at)));
                }
                entries.insert(at, e.clone());
                let msg = match &e.replaces {
                    Some(o) => format!("land replace {} {}", &o[..o.len().min(7)], &sha[..7]),
                    None => format!("land add {}", &sha[..7]),
                };
                Ok((msg, (e, said)))
            })?;
            let hint = if e.items.is_empty() { "\n  no items given, none move to review when it is read green".to_string() } else { stays };
            Ok(state_out(&e, &format!("{said}{hint}")))
        }
        Op::Approve { sha, note, to } => {
            let note = note.map(|n| truncate_chars(&one_line(&n), NOTE_KEEP)).filter(|n| !n.is_empty());
            let done = mutate(v, &slug, |entries, parked| {
                let q = sha.trim().to_ascii_lowercase();
                let i = match find(entries, &sha) {
                    Ok(i) => i,
                    Err(_) if is_commit_hex(&q) && !entries.iter().any(|e| same_sha(&e.sha, &q)) => {
                        let earlier = parked.iter().position(|p| same_sha(&p.sha, &q)).map(|i| parked.remove(i));
                        let p = Parked { sha: q.clone(), note: note.clone(), next: to.as_deref().map(holder), at: now(), by: by.clone(), extra: Map::new() };
                        parked.push(p.clone());
                        return Ok((format!("land approve {} before it is queued", &q[..7]), Err((p, earlier))));
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
                Ok((e, said)) => Ok(state_out(&e, &said)),
                Err((p, earlier)) => {
                    let was = earlier.map(|o| format!("\n  it replaces the parked {}", parked_line(&o, NOTE_MAX))).unwrap_or_default();
                    let text = format!("parked: {}\n  no queue entry has that sha yet, the approval attaches when `crumb land add {}` queues it{was}", parked_line(&p, NOTE_MAX), p.sha);
                    Ok(Outcome { text, data: json!({"parked": p}) })
                }
            }
        }
        Op::Verify { sha, to } => {
            let e = mutate(v, &slug, |entries, _| {
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
            Ok(state_out(&e, ""))
        }
        Op::Pushed { shas, run, to } => {
            let run = one_line(&run).trim().to_string();
            if run.is_empty() {
                bail!("pushed needs the CI run id, `--run <id>`");
            }
            let es = mutate(v, &slug, |entries, _| {
                let idx = find_all(entries, &shas)?;
                if !force {
                    for &i in &idx {
                        let e = &entries[i];
                        if !matches!(e.state.as_str(), "approved" | "verified") {
                            return Err(step_error(e, "pushed", "an approved entry"));
                        }
                        if let Some(b) = blocker(entries, i, &idx) {
                            let name = if b.state == "built" { "" } else { ", or name it in this call if the push carries it" };
                            bail!("{} is ahead in the queue and still {}, push in order{name}, or `--force`, or `crumb land move`/`drop`", b.short(), b.state);
                        }
                    }
                }
                for &i in &idx {
                    let e = &mut entries[i];
                    set_state(e, "pushed", &by);
                    e.run = Some(run.clone());
                    e.retire_note();
                    e.next = to.as_deref().map(holder).or_else(|| e.by("approved").map(str::to_string));
                }
                let es: Vec<Entry> = idx.iter().map(|&i| entries[i].clone()).collect();
                Ok((format!("land pushed {}", es.iter().map(Entry::short).collect::<Vec<_>>().join(" ")), es))
            })?;
            if let [e] = es.as_slice() {
                return Ok(state_out(e, ""));
            }
            let all = read(v, &slug)?;
            let text = es.iter().map(|e| format!("{}, next: {}", head(&all, e), due_text(e))).collect::<Vec<_>>().join("\n");
            Ok(Outcome { text, data: json!({"entries": es}) })
        }
        Op::Read { shas, green, to } => {
            let word = if green { "green" } else { "red" };
            let (es, retried, others, all) = mutate(v, &slug, |entries, _| {
                let idx = find_all(entries, &shas)?;
                let again = |e: &Entry| green && e.state == "green";
                if let Some(e) = idx.iter().map(|&i| &entries[i]).find(|e| !force && e.state != "pushed" && !again(e)) {
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
                    set_state(e, word, &by);
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
                let mut out = state_out(e, "");
                if green {
                    let (text, moved) = land_items(v, e, &all, req.by.clone(), &mut done);
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
            let mut data = if multi { json!({"entries": es}) } else { json!({"entry": es[0]}) };
            if green {
                data["items"] = json!(moved_all);
            }
            Ok(Outcome { text, data })
        }
        Op::Drop { sha } => {
            let gone = mutate(v, &slug, |entries, parked| match find(entries, &sha) {
                Ok(i) => {
                    let e = entries.remove(i);
                    Ok((format!("land drop {}", e.short()), Ok(e)))
                }
                Err(err) => {
                    let q = sha.trim().to_ascii_lowercase();
                    let i = parked.iter().position(|p| same_sha(&p.sha, &q)).ok_or(err)?;
                    let p = parked.remove(i);
                    Ok((format!("land drop the parked approval of {}", &p.sha[..7]), Err(p)))
                }
            })?;
            match gone {
                Ok(e) => Ok(Outcome { text: format!("dropped {} ({}) from the queue", e.short(), e.state), data: json!({"dropped": e}) }),
                Err(p) => Ok(Outcome { text: format!("dropped the parked approval {}", parked_line(&p, NOTE_MAX)), data: json!({"dropped_parked": p}) }),
            }
        }
        Op::Move { sha, after } => {
            let e = mutate(v, &slug, |entries, _| {
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
            Ok(Outcome { text: list_text(v, &entries, &[], false), data: json!({"moved": e.sha}) })
        }
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
/// carries waits for the last of them, which moves it with every green carrier's commit as a ref. `built_by` is the
/// item's claim holder, else the session that added the entry, never the reader.
fn land_items(v: &Vault, e: &Entry, all: &[Entry], by: Option<String>, done: &mut Vec<String>) -> (String, Vec<Value>) {
    let mut text = String::new();
    let mut reqs = vec![];
    for id in &e.items {
        if done.contains(id) {
            continue;
        }
        let n = match v.get(id) {
            Ok(n) => n,
            Err(_) => {
                text.push_str(&format!("
  {id} is not in the vault any more, left alone"));
                continue;
            }
        };
        if !goes_to_review(&n) {
            text.push_str(&format!("
  left {} where it is, it is {} and only queued, in-progress, review or claimed backlog items go to review", n.handle(), backlog::state(&n)));
            continue;
        }
        let open: Vec<String> = all.iter().filter(|x| x.sha != e.sha && x.items.contains(id) && (x.is_open() || x.state == "red")).map(|x| format!("{} {}", x.short(), x.state)).collect();
        if !open.is_empty() {
            let verb = if open.len() > 1 { "carry" } else { "carries" };
            text.push_str(&format!("
  left {} where it is, {} still {verb} it, the last of them read green moves it", n.handle(), open.join(", ")));
            continue;
        }
        done.push(id.clone());
        let mut refs = vec![format!("commit:{}", e.sha)];
        for x in all.iter().filter(|x| x.sha != e.sha && x.state == "green" && x.items.contains(id)) {
            refs.push(format!("commit:{}", x.sha));
        }
        let built_by = backlog::assignee(&n).or_else(|| e.by("built").map(session_of)).unwrap_or_default();
        reqs.push((id.clone(), MoveReq { state: "review".into(), refs, by: by.clone(), built_by: Some(built_by), ..Default::default() }));
    }
    let mut moved = vec![];
    if reqs.is_empty() {
        return (text, moved);
    }
    match backlog::move_each(v, &reqs) {
        Ok(results) => {
            for (id, r) in results {
                match r {
                    Ok(r) => {
                        text.push('\n');
                        text.push_str(backlog::move_text(&r).trim_end());
                        moved.push(json!({"id": r.note.id, "from": r.from, "to": r.to, "warnings": r.warnings}));
                    }
                    Err(err) => text.push_str(&format!("
  failed {id}: {}", one_line(&format!("{err:#}")))),
                }
            }
        }
        Err(err) => text.push_str(&format!("
  moving the items failed: {}", one_line(&format!("{err:#}")))),
    }
    (text, moved)
}

/// The queue's head for the digest: what is open, whose move it is, and any pushed entry nobody read.
pub fn digest_lines(v: &Vault, project: Option<&str>) -> Option<String> {
    let entries = read(v, project?).ok()?;
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
        /// The CI run id
        #[arg(long)]
        run: String,
        /// Session that holds the next step
        #[arg(long)]
        to: Option<String>,
    },
    /// The CI run was read, green moves the entry's items to review with commit:<sha> as a ref
    Read {
        /// The merge commits of one run, 4 or more leading hex characters find a queued one
        #[arg(required = true, value_delimiter = ',')]
        shas: Vec<String>,
        /// green or red
        result: String,
        /// Session that holds the next step
        #[arg(long)]
        to: Option<String>,
    },
    /// The CI run was read green, the same as `read <sha> green`
    Green {
        /// The merge commits of one run, 4 or more leading hex characters find a queued one
        #[arg(required = true, value_delimiter = ',')]
        shas: Vec<String>,
        /// Session that holds the next step
        #[arg(long)]
        to: Option<String>,
    },
    /// The CI run was read red, the same as `read <sha> red`
    Red {
        /// The merge commits of one run, 4 or more leading hex characters find a queued one
        #[arg(required = true, value_delimiter = ',')]
        shas: Vec<String>,
        /// Session that holds the next step
        #[arg(long)]
        to: Option<String>,
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
            LandCmd::Read { mut shas, mut result, to } => {
                // `read green <sha>` reads as meant too.
                if parse_result(&result).is_err() && shas.first().is_some_and(|s| parse_result(s).is_ok()) {
                    std::mem::swap(&mut shas[0], &mut result);
                }
                Op::Read { shas, green: parse_result(&result)?, to }
            }
            LandCmd::Green { shas, to } => Op::Read { shas, green: true, to },
            LandCmd::Red { shas, to } => Op::Read { shas, green: false, to },
            LandCmd::List { all } => Op::List { all },
            LandCmd::Show { sha } => Op::Show { sha },
            LandCmd::Drop { sha } => Op::Drop { sha },
            LandCmd::Move { sha, after } => Op::Move { sha, after },
        };
        Ok(Req { op, by: self.by, force: self.force })
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
pub fn mcp(v: &Vault, project: Option<&str>, a: &Map<String, Value>) -> Result<String> {
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
        "pushed" => Op::Pushed { shas: shas()?, run: need("run")?, to },
        read @ ("green" | "red") => Op::Read { shas: shas()?, green: read == "green", to },
        "read" => Op::Read { shas: shas()?, green: parse_result(&need("result")?)?, to },
        "drop" => Op::Drop { sha: need("sha")? },
        "move" => Op::Move { sha: need("sha")?, after: s("after") },
        other => bail!("unknown land action {other:?}, use list, show, add, replace, approve, verify, pushed, green, red, read, drop or move"),
    };
    Ok(run(v, project, Req { op, by: s("by"), force: flag("force") })?.text)
}
