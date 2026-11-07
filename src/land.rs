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
use std::path::PathBuf;

pub const FILE: &str = "land.jsonl";
/// A pushed entry with no read for this long shows as stale, a CI run of the usual kind is long over by then.
pub const STALE_MINUTES: i64 = 45;
/// Green entries kept for the record, older ones leave the file.
const KEEP_GREEN: usize = 8;
const DIGEST_ROWS: usize = 3;
const DIGEST_MAX_ROWS: usize = 5;
const NOTE_MAX: usize = 120;

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
    /// The session that holds the next step, as `name id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Keys a newer binary wrote stay as they are.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
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

fn read(v: &Vault, slug: &str) -> Result<Vec<Entry>> {
    let p = path(v, slug);
    let text = match std::fs::read_to_string(&p) {
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

fn write(v: &Vault, slug: &str, entries: &mut Vec<Entry>) -> Result<PathBuf> {
    let mut greens = entries.iter().filter(|e| e.state == "green").count();
    entries.retain(|e| {
        let drop = e.state == "green" && greens > KEEP_GREEN;
        if drop {
            greens -= 1;
        }
        !drop
    });
    let mut text = String::new();
    for e in entries.iter() {
        text.push_str(&serde_json::to_string(e)?);
        text.push('\n');
    }
    let p = path(v, slug);
    atomic_write(&p, text.as_bytes())?;
    Ok(p)
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

#[derive(Clone, Debug)]
pub enum Op {
    List { all: bool },
    Add { sha: String, items: Vec<String>, to: Option<String> },
    Approve { sha: String, note: Option<String>, to: Option<String> },
    Verify { sha: String, to: Option<String> },
    Pushed { sha: String, run: String, to: Option<String> },
    Read { sha: String, green: bool, to: Option<String> },
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

fn head(e: &Entry) -> String {
    let mut s = format!("{} {}", e.short(), e.state);
    if let Some(n) = &e.note {
        s.push_str(&format!(" ({n})"));
    }
    if let Some(r) = &e.run {
        s.push_str(&format!(", run {r}"));
    }
    s.push_str(&format!(", {} ago", short_age(&e.at)));
    if e.stale() {
        s.push_str(", STALE, no read yet");
    }
    s
}

fn due_text(e: &Entry) -> String {
    match (e.due(), &e.next) {
        (Some(d), Some(w)) => format!("{d}: {w}{}", if is_me(w) { " (you)" } else { "" }),
        (Some(d), None) => d.to_string(),
        (None, _) => "landed".to_string(),
    }
}

fn row(v: &Vault, i: usize, e: &Entry) -> String {
    format!("{}. {}, {}, items: {}", i + 1, head(e), due_text(e), names(v, e))
}

pub fn list_text(v: &Vault, entries: &[Entry], all: bool) -> String {
    let shown: Vec<(usize, &Entry)> = entries.iter().enumerate().filter(|(_, e)| all || e.state != "green").collect();
    if shown.is_empty() {
        return "the landing queue is empty".to_string();
    }
    shown.iter().map(|(i, e)| row(v, *i, e)).collect::<Vec<_>>().join("\n")
}

fn mutate<T>(v: &Vault, slug: &str, f: impl FnOnce(&mut Vec<Entry>) -> Result<(String, T)>) -> Result<T> {
    let _g = v.lock()?;
    let mut entries = read(v, slug)?;
    let (msg, out) = f(&mut entries)?;
    let p = write(v, slug, &mut entries)?;
    v.commit(&[p], &format!("breadcrumb: {msg}"))?;
    Ok(out)
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

/// The queue runs in order, so an earlier entry that is not landed yet holds this one back.
fn blocker(entries: &[Entry], i: usize) -> Option<&Entry> {
    entries[..i].iter().find(|e| e.is_open())
}

pub fn run(v: &Vault, project: Option<&str>, req: Req) -> Result<Outcome> {
    let slug = slug_of(v, project)?;
    let by = session::attribute(req.by.as_deref());
    let force = req.force;
    let state_out = |e: &Entry, extra: &str| Outcome { text: format!("{}\n  next: {}{extra}", head(e), due_text(e)), data: json!({"entry": e}) };
    match req.op {
        Op::List { all } => {
            let entries = read(v, &slug)?;
            let shown: Vec<&Entry> = entries.iter().filter(|e| all || e.state != "green").collect();
            Ok(Outcome { text: list_text(v, &entries, all), data: json!({"entries": shown}) })
        }
        Op::Add { sha, items, to } => {
            let sha = sha.trim().to_ascii_lowercase();
            if !is_commit_hex(&sha) {
                bail!("{sha:?} is not a commit sha, give 7 to 40 hex characters");
            }
            let mut ids: Vec<String> = vec![];
            for item in items.iter().flat_map(|s| s.split(',')).map(str::trim).filter(|s| !s.is_empty()) {
                let id = backlog::get_item(v, item)?.id;
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            let e = mutate(v, &slug, |entries| {
                if let Some(old) = entries.iter().find(|e| e.sha.starts_with(&sha) || sha.starts_with(&e.sha)) {
                    bail!("{} is in the queue already as {}", old.short(), old.state);
                }
                let t = now();
                let e = Entry {
                    sha: sha.clone(),
                    items: ids,
                    state: "built".into(),
                    at: t,
                    note: None,
                    run: None,
                    next: to.as_deref().map(holder),
                    steps: vec![Step { state: "built".into(), at: t, by: by.clone() }],
                    extra: Map::new(),
                };
                entries.push(e.clone());
                Ok((format!("land add {}", &sha[..7]), e))
            })?;
            let hint = if e.items.is_empty() { "\n  no items given, none move to review when it is read green" } else { "" };
            Ok(state_out(&e, hint))
        }
        Op::Approve { sha, note, to } => {
            let note = note.map(|n| truncate_chars(&one_line(&n), NOTE_MAX)).filter(|n| !n.is_empty());
            let e = mutate(v, &slug, |entries| {
                let i = find(entries, &sha)?;
                let e = &mut entries[i];
                if !force && !matches!(e.state.as_str(), "built" | "approved") {
                    return Err(step_error(e, "approve", "a built entry"));
                }
                set_state(e, "approved", &by);
                e.note = note;
                e.next = to.as_deref().map(holder).or_else(|| e.by("built").map(str::to_string));
                Ok((format!("land approve {}", e.short()), e.clone()))
            })?;
            Ok(state_out(&e, ""))
        }
        Op::Verify { sha, to } => {
            let e = mutate(v, &slug, |entries| {
                let i = find(entries, &sha)?;
                let e = &mut entries[i];
                if !force && e.state != "approved" {
                    return Err(step_error(e, "verify", "an approved entry"));
                }
                set_state(e, "verified", &by);
                e.note = None;
                e.next = to.as_deref().map(holder).or_else(|| e.by("built").map(str::to_string));
                Ok((format!("land verify {}", e.short()), e.clone()))
            })?;
            Ok(state_out(&e, ""))
        }
        Op::Pushed { sha, run, to } => {
            let run = one_line(&run).trim().to_string();
            if run.is_empty() {
                bail!("pushed needs the CI run id, `--run <id>`");
            }
            let e = mutate(v, &slug, |entries| {
                let i = find(entries, &sha)?;
                if !force {
                    let e = &entries[i];
                    if !matches!(e.state.as_str(), "approved" | "verified") {
                        return Err(step_error(e, "pushed", "an approved entry"));
                    }
                    if let Some(b) = blocker(entries, i) {
                        bail!("{} is ahead in the queue and still {}, push in order, or `--force`, or `crumb land move`/`drop`", b.short(), b.state);
                    }
                }
                let e = &mut entries[i];
                set_state(e, "pushed", &by);
                e.run = Some(run);
                e.note = None;
                e.next = to.as_deref().map(holder).or_else(|| e.by("approved").map(str::to_string));
                Ok((format!("land pushed {}", e.short()), e.clone()))
            })?;
            Ok(state_out(&e, ""))
        }
        Op::Read { sha, green, to } => {
            let mut retry = false;
            let e = mutate(v, &slug, |entries| {
                let i = find(entries, &sha)?;
                let e = &mut entries[i];
                if green && e.state == "green" {
                    retry = true;
                    return Ok((String::new(), e.clone()));
                }
                if !force && e.state != "pushed" {
                    return Err(step_error(e, "read", "a pushed entry"));
                }
                set_state(e, if green { "green" } else { "red" }, &by);
                e.next = if green { None } else { to.as_deref().map(holder).or_else(|| e.by("built").map(str::to_string)) };
                Ok((format!("land read {} {}", e.short(), e.state), e.clone()))
            })?;
            let mut out = state_out(&e, "");
            if !green {
                out.text.push_str("\n  its items stay where they are");
                return Ok(out);
            }
            let (text, moved) = land_items(v, &e, req.by.clone());
            out.text.push_str(&text);
            out.data["items"] = json!(moved);
            if retry {
                out.text.push_str("\n  it was green already, moved what was left");
            }
            Ok(out)
        }
        Op::Drop { sha } => {
            let e = mutate(v, &slug, |entries| {
                let i = find(entries, &sha)?;
                let e = entries.remove(i);
                Ok((format!("land drop {}", e.short()), e))
            })?;
            Ok(Outcome { text: format!("dropped {} ({}) from the queue", e.short(), e.state), data: json!({"dropped": e}) })
        }
        Op::Move { sha, after } => {
            let e = mutate(v, &slug, |entries| {
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
            Ok(Outcome { text: list_text(v, &entries, false), data: json!({"moved": e.sha}) })
        }
    }
}

const REVIEWABLE: [&str; 3] = ["queued", "in-progress", "review"];

/// Moves the items of a green entry to review with the merge as a ref, through the code `crumb move` runs.
fn land_items(v: &Vault, e: &Entry, by: Option<String>) -> (String, Vec<Value>) {
    let mut text = String::new();
    let mut ids = vec![];
    for id in &e.items {
        match v.get(id) {
            Ok(n) if REVIEWABLE.contains(&backlog::state(&n).as_str()) => ids.push(id.clone()),
            Ok(n) => text.push_str(&format!("\n  left {} where it is, it is {} and only queued, in-progress or review items go to review", n.handle(), backlog::state(&n))),
            Err(_) => text.push_str(&format!("\n  {id} is not in the vault any more, left alone")),
        }
    }
    let mut moved = vec![];
    if ids.is_empty() {
        return (text, moved);
    }
    let req = MoveReq { state: "review".into(), refs: vec![format!("commit:{}", e.sha)], by, ..Default::default() };
    match backlog::move_items(v, &ids, &req) {
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
        s.push_str(&format!("{}, {}\n", head(e), due_text(e)));
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
    /// Queue a merge commit at the end
    Add {
        /// The merge commit, 7 to 40 hex characters
        sha: String,
        /// Backlog ids or slugs this merge lands, comma separated
        #[arg(long, value_delimiter = ',')]
        items: Vec<String>,
        /// Session that holds the next step
        #[arg(long)]
        to: Option<String>,
    },
    /// The review approves it, --note names a condition like "pending the alone harness run"
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
    /// It was pushed and started a CI run
    Pushed {
        /// The merge commit, 4 or more leading hex characters find a queued one
        sha: String,
        /// The CI run id
        #[arg(long)]
        run: String,
        /// Session that holds the next step
        #[arg(long)]
        to: Option<String>,
    },
    /// The CI run was read, green moves the entry's items to review with commit:<sha> as a ref
    Read {
        /// The merge commit, 4 or more leading hex characters find a queued one
        sha: String,
        /// green or red
        result: String,
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
            LandCmd::Add { sha, items, to } => Op::Add { sha, items, to },
            LandCmd::Approve { sha, note, to } => Op::Approve { sha, note, to },
            LandCmd::Verify { sha, to } => Op::Verify { sha, to },
            LandCmd::Pushed { sha, run, to } => Op::Pushed { sha, run, to },
            LandCmd::Read { sha, result, to } => Op::Read { sha, green: parse_result(&result)?, to },
            LandCmd::List { all } => Op::List { all },
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
    let op = match s("action").as_deref().unwrap_or("list") {
        "list" => Op::List { all: false },
        "add" => Op::Add { sha: need("sha")?, items, to },
        "approve" => Op::Approve { sha: need("sha")?, note: s("note"), to },
        "verify" => Op::Verify { sha: need("sha")?, to },
        "pushed" => Op::Pushed { sha: need("sha")?, run: need("run")?, to },
        read @ ("green" | "red") => Op::Read { sha: need("sha")?, green: read == "green", to },
        "drop" => Op::Drop { sha: need("sha")? },
        "move" => Op::Move { sha: need("sha")?, after: s("after") },
        other => bail!("unknown land action {other:?}, use list, add, approve, verify, pushed, green, red, drop or move"),
    };
    Ok(run(v, project, Req { op, by: s("by"), force: flag("force") })?.text)
}
