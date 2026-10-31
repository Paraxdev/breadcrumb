//! Jira-like idea and feature tracking built on ordinary notes with seeded kinds and typed fields.

use crate::kind::{FieldDef, FieldType, KindDef};
use crate::note::{Note, yaml_display, yaml_scalar_string};
use crate::ops::{Filter, Scope, filter_notes};
use crate::util::now;
use crate::vault::Vault;
use anyhow::{Result, bail};
use serde::Serialize;
use serde_yaml::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write;

pub const STATES: [&str; 8] = ["idea", "backlog", "queued", "in-progress", "review", "done", "shipped", "rejected"];
pub const CLOSED: [&str; 3] = ["done", "shipped", "rejected"];
pub const KINDS: [&str; 5] = ["idea", "epic", "feature", "task", "bug"];
pub const PRIORITIES: [&str; 4] = ["p0", "p1", "p2", "p3"];
pub const DEFAULT_ACTOR: &str = "user";

pub fn seed_kinds() -> Vec<KindDef> {
    let enum_f = |name: &str, values: &[&str], default: &str, desc: &str| FieldDef {
        name: name.into(),
        ty: FieldType::Enum,
        required: false,
        description: Some(desc.into()),
        default: Some(toml::Value::String(default.into())),
        values: values.iter().map(|s| s.to_string()).collect(),
    };
    let plain = |name: &str, ty: FieldType, desc: &str| {
        let mut f = FieldDef::new(name, ty);
        f.description = Some(desc.into());
        f
    };
    let shared = |default_state: &str, acceptance: bool| {
        let mut f = vec![
            enum_f("state", &STATES, default_state, "workflow state"),
            enum_f("priority", &PRIORITIES, "p2", "p0 is most urgent"),
            plain("rank", FieldType::Float, "ordering within a state, lower first"),
            plain("parent", FieldType::Ref, "parent epic or feature id"),
            plain("assignee", FieldType::String, "human or agent working on it"),
            plain("estimate", FieldType::String, "rough size, e.g. 2h, 3d, M"),
            plain("resolution", FieldType::String, "outcome when done, shipped or rejected"),
            plain("shipped_in", FieldType::String, "version, commit or PR that shipped it"),
        ];
        if acceptance {
            f.push(plain("acceptance", FieldType::List, "acceptance criteria, prefix met ones with [x]"));
        }
        f
    };
    let mk = |name: &str, desc: &str, state: &str, acc: bool, template: Option<&str>| KindDef {
        name: name.into(),
        description: desc.into(),
        template: template.map(Into::into),
        fields: shared(state, acc),
    };
    vec![
        mk("idea", "Backlog: a raw idea or concept, not yet committed to", "idea", false, None),
        mk("epic", "Backlog: a large body of work grouping features", "backlog", false, Some("## Goal\n\n## Scope\n")),
        mk("feature", "Backlog: a user visible capability to build", "backlog", true, None),
        mk("task", "Backlog: a concrete unit of work", "backlog", true, None),
        mk("bug", "Backlog: a defect to fix", "backlog", false, Some("## Steps to reproduce\n\n## Expected\n\n## Actual\n")),
    ]
}

pub fn is_item(n: &Note) -> bool {
    KINDS.contains(&n.kind.as_str()) || n.field("state").and_then(yaml_scalar_string).is_some_and(|s| STATES.contains(&s.as_str()))
}

pub fn is_closed(state: &str) -> bool {
    CLOSED.contains(&state)
}

fn str_field(n: &Note, key: &str) -> Option<String> {
    n.field(key).and_then(yaml_scalar_string).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn state(n: &Note) -> String {
    str_field(n, "state").unwrap_or_else(|| if n.kind == "idea" { "idea".into() } else { "backlog".into() })
}

/// The state for backlog items, "-" for other notes so nothing pretends they have one.
pub fn state_label(n: &Note) -> String {
    if is_item(n) { state(n) } else { "-".into() }
}

pub fn priority(n: &Note) -> String {
    str_field(n, "priority").filter(|p| PRIORITIES.contains(&p.as_str())).unwrap_or_else(|| "p2".into())
}

pub fn rank(n: &Note) -> Option<f64> {
    match n.field("rank") {
        Some(Value::Number(x)) => x.as_f64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
}

pub fn parent(n: &Note) -> Option<String> {
    str_field(n, "parent")
}

pub fn assignee(n: &Note) -> Option<String> {
    str_field(n, "assignee")
}

pub const CLAIMED_KEY: &str = "claimed";
/// `<machine> <assignee>`, a claim's session can only be judged dead on the machine it ran on. The assignee is in the
/// value because an old binary claiming over it leaves this key behind, a value for another assignee means nothing.
pub const CLAIM_MACHINE_KEY: &str = "claim_machine";
pub const SESSION_ENV: &str = "BREADCRUMB_SESSION";
/// A claim with no write to its item for this long no longer keeps other sessions away.
pub const CLAIM_STALE_HOURS: i64 = 24;

/// Who this process works as: `by`, else BREADCRUMB_SESSION, else the start of the Claude Code session id.
pub fn session_name(by: Option<&str>) -> Option<String> {
    let given = |s: Option<String>| s.map(|x| x.trim().to_string()).filter(|x| !x.is_empty());
    given(by.map(str::to_string))
        .or_else(|| given(std::env::var(SESSION_ENV).ok()))
        .or_else(|| given(std::env::var("CLAUDE_CODE_SESSION_ID").ok()).map(|s| s.chars().take(8).collect()))
}

pub fn claimed_at(n: &Note) -> Option<chrono::DateTime<chrono::Utc>> {
    str_field(n, CLAIMED_KEY).and_then(|s| crate::util::parse_time(&s))
}

fn last_activity(n: &Note) -> chrono::DateTime<chrono::Utc> {
    claimed_at(n).map_or(n.updated, |c| c.max(n.updated))
}

fn quiet_for_a_day(n: &Note) -> bool {
    (now() - last_activity(n)).num_hours() >= CLAIM_STALE_HOURS
}

/// The vault syncs between machines and sessions are only listed locally, so anything but a claim recorded on this
/// machine is left to the one day rule.
fn claim_dead_here(n: &Note) -> bool {
    let (Some(a), Some(rec)) = (assignee(n), str_field(n, CLAIM_MACHINE_KEY)) else { return false };
    let machine = crate::util::machine_name();
    machine != "unknown" && rec.split_once(' ').is_some_and(|(m, who)| m == machine && who.eq_ignore_ascii_case(&a)) && crate::session::known_dead(&a)
}

pub fn claim_stale(n: &Note) -> bool {
    claim_dead_here(n) || quiet_for_a_day(n)
}

fn set_claim(n: &mut Note, by: &str) {
    n.set_field("assignee", Value::String(by.to_string()));
    n.set_field(CLAIMED_KEY, Value::String(crate::util::fmt_time(&now())));
    n.set_field(CLAIM_MACHINE_KEY, Value::String(format!("{} {by}", crate::util::machine_name())));
}

fn clear_claim(n: &mut Note) {
    n.unset_field("assignee");
    n.unset_field(CLAIMED_KEY);
    n.unset_field(CLAIM_MACHINE_KEY);
}

fn is_build_ref(r: &str) -> bool {
    r.starts_with("branch:") || r.starts_with("run:")
}

/// The ref showing work runs for a queued item. Leads start lanes with workflows, so the stored state stays queued.
/// A ref has no time of its own, so a day without a write or a dead session's claim ends it, or a lost lane would
/// hide its item from `next` forever.
pub fn being_built(n: &Note) -> Option<String> {
    if state(n) != "queued" || quiet_for_a_day(n) || claim_dead_here(n) {
        return None;
    }
    n.refs().into_iter().find(|r| is_build_ref(r))
}

/// Going back to the queue ends an earlier build, its refs would read as work still running. The history keeps them.
fn end_build_refs(n: &mut Note, from: &str, keep: &[String], by: Option<&str>) {
    if !matches!(from, "in-progress" | "review" | "done" | "shipped" | "rejected") {
        return;
    }
    let (ended, kept): (Vec<String>, Vec<String>) = n.refs().into_iter().partition(|r| is_build_ref(r) && !keep.contains(r));
    if !ended.is_empty() {
        n.set_refs(kept);
        n.body = append_history(&n.body, &event_line(&format!("build refs ended: {}", ended.join(" ")), &actor(by)));
    }
}

/// The assignee while the claim is fresh, unless that is `who`.
pub fn held_by_other(n: &Note, who: Option<&str>) -> Option<String> {
    let a = assignee(n)?;
    if who.is_some_and(|w| w.eq_ignore_ascii_case(&a)) || claim_stale(n) {
        return None;
    }
    Some(a)
}

fn age(n: &Note) -> String {
    let mins = (now() - n.updated).num_minutes().max(0);
    if mins < 120 { format!("{mins} min") } else { format!("{} h", mins / 60) }
}

pub(crate) fn event_line(what: &str, by: &str) -> String {
    format!("- {}: {what} (by {})", now().format("%Y-%m-%dT%H:%MZ"), crate::session::attribute(Some(by)))
}

/// Sets the assignee to `by` with a timestamp, so `next` in other sessions skips the item until the claim goes stale.
pub fn claim(v: &Vault, id: &str, by: &str, force: bool) -> Result<Note> {
    let by = actor(Some(by));
    let _g = v.lock()?;
    let mut n = get_item(v, id)?;
    if is_closed(&state(&n)) {
        bail!("{} is {}, claims are for open items", n.id, state(&n));
    }
    if let Some(q) = crate::question::open_questions(&n).first() {
        bail!("{} is waiting on the user, question {}: {}. Work on something else, the user answers with `breadcrumb answer {}`", n.id, crate::question::qid(&n, q), q.text, crate::question::qid(&n, q));
    }
    if let Some(h) = held_by_other(&n, Some(&by))
        && !force
    {
        bail!("{} is claimed by {}, last written {} ago, pass force to take it over", n.id, crate::session::holder(&h), age(&n));
    }
    let (what, by) = match assignee(&n) {
        Some(p) if !p.eq_ignore_ascii_case(&by) => (format!("claim taken over from {p}"), by),
        Some(p) => ("claim renewed".to_string(), p),
        None => ("claimed".to_string(), by),
    };
    set_claim(&mut n, &by);
    n.body = append_history(&n.body, &event_line(&what, &by));
    crate::question::ack_in(&mut n, &by);
    save(v, &mut n, "claim")?;
    Ok(n)
}

/// Clears the claim. Releasing another session's fresh claim needs `force`. Ok(None) when nothing was claimed.
pub fn release(v: &Vault, id: &str, by: &str, force: bool) -> Result<Option<Note>> {
    let by = actor(Some(by));
    let _g = v.lock()?;
    let mut n = get_item(v, id)?;
    let Some(holder) = assignee(&n) else { return Ok(None) };
    if let Some(h) = held_by_other(&n, Some(&by))
        && !force
    {
        bail!("{} is claimed by {h}, not {by}, pass force to release it anyway", n.id);
    }
    clear_claim(&mut n);
    let what = if holder.eq_ignore_ascii_case(&by) { "released".to_string() } else { format!("released the claim of {holder}") };
    n.body = append_history(&n.body, &event_line(&what, &by));
    save(v, &mut n, "release")?;
    Ok(Some(n))
}

pub fn key(n: &Note) -> String {
    match &n.project {
        Some(p) => format!("{}-{}", p.to_ascii_uppercase(), n.id),
        None => n.id.clone(),
    }
}

/// `slug KEY-id`, or the key alone for a note without a slug.
pub fn named_key(n: &Note) -> String {
    match n.slug() {
        Some(s) => format!("{s} {}", key(n)),
        None => key(n),
    }
}

pub fn check_state(s: &str) -> Result<String> {
    let s = s.trim().to_ascii_lowercase().replace(['_', ' '], "-");
    let s = if s == "inprogress" || s == "wip" { "in-progress".to_string() } else { s };
    if STATES.contains(&s.as_str()) { Ok(s) } else { bail!("unknown state '{s}', expected one of: {}", STATES.join(", ")) }
}

fn actor(by: Option<&str>) -> String {
    by.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(DEFAULT_ACTOR).to_string()
}

pub fn history_line(what: &str, from: &str, to: &str, by: &str) -> String {
    format!("- {}: {what} {from} -> {to} (by {})", now().format("%Y-%m-%dT%H:%MZ"), crate::session::attribute(Some(by)))
}

fn history_heading(line: &str) -> bool {
    line.trim().eq_ignore_ascii_case("## history")
}

fn section_end(lines: &[&str], start: usize) -> usize {
    lines.iter().enumerate().skip(start + 1).find(|(_, l)| l.starts_with("# ") || l.starts_with("## ")).map(|(i, _)| i).unwrap_or(lines.len())
}

/// Adds a line to the "## History" section, creating it at the end of the body when missing.
pub fn append_history(body: &str, line: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let Some(start) = lines.iter().position(|l| history_heading(l)) else {
        let b = body.trim_end();
        return if b.is_empty() { format!("## History\n\n{line}") } else { format!("{b}\n\n## History\n\n{line}") };
    };
    let mut end = section_end(&lines, start);
    while end > start + 1 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    let mut out: Vec<String> = lines[..end].iter().map(|s| s.to_string()).collect();
    if end == start + 1 {
        out.push(String::new());
    }
    out.push(line.to_string());
    let rest = &lines[section_end(&lines, start)..];
    if !rest.is_empty() {
        out.push(String::new());
        out.extend(rest.iter().map(|s| s.to_string()));
    }
    out.join("\n")
}

/// Inserts text before the History section so the history stays last.
pub fn insert_before_history(body: &str, text: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    match lines.iter().position(|l| history_heading(l)) {
        Some(i) => {
            let head = lines[..i].join("\n");
            let head = head.trim_end();
            let tail = lines[i..].join("\n");
            if head.is_empty() { format!("{text}\n\n{tail}") } else { format!("{head}\n\n{text}\n\n{tail}") }
        }
        None if body.trim().is_empty() => text.to_string(),
        None => format!("{}\n\n{}", body.trim_end(), text),
    }
}

/// Records a state transition in the body. Called by every path that changes state.
pub fn record_state_change(n: &mut Note, from: &str, to: &str, by: Option<&str>) {
    if from != to {
        n.body = append_history(&n.body, &history_line("state", from, to, &crate::session::attribute(by)));
    }
}

pub struct Index<'a> {
    pub by_id: HashMap<&'a str, &'a Note>,
    pub notes: &'a [Note],
}

impl<'a> Index<'a> {
    pub fn new(notes: &'a [Note]) -> Index<'a> {
        Index { by_id: notes.iter().map(|n| (n.id.as_str(), n)).collect(), notes }
    }

    /// Only backlog items block, a link to an ordinary note has no state to finish.
    fn open(&self, id: &str) -> bool {
        self.by_id.get(id).is_some_and(|n| is_item(n) && !n.is_archived() && !is_closed(&state(n)))
    }

    /// Ids that directly block `n`: its depends-on targets and sources of blocks links pointing at it.
    pub fn blocked_by(&self, n: &Note) -> Vec<String> {
        let mut out: Vec<String> = n.links.iter().filter(|l| l.rel == "depends-on").map(|l| l.to.clone()).collect();
        for o in self.notes {
            if o.links.iter().any(|l| l.rel == "blocks" && l.to == n.id) {
                out.push(o.id.clone());
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Ids that `n` directly blocks.
    pub fn blocks(&self, n: &Note) -> Vec<String> {
        let mut out: Vec<String> = n.links.iter().filter(|l| l.rel == "blocks").map(|l| l.to.clone()).collect();
        for o in self.notes {
            if o.links.iter().any(|l| l.rel == "depends-on" && l.to == n.id) {
                out.push(o.id.clone());
            }
        }
        out.sort();
        out.dedup();
        out
    }

    pub fn open_blockers(&self, n: &Note) -> Vec<String> {
        self.blocked_by(n).into_iter().filter(|id| id != &n.id && self.open(id)).collect()
    }

    pub fn is_blocked(&self, n: &Note) -> bool {
        !self.open_blockers(n).is_empty()
    }

    pub fn children(&self, id: &str) -> Vec<&'a Note> {
        let mut c: Vec<&Note> = self.notes.iter().filter(|n| parent(n).as_deref() == Some(id) && n.id != id && !n.is_archived()).collect();
        sort_items(&mut c);
        c
    }

    /// (done or shipped, total excluding rejected) over direct children.
    pub fn progress(&self, id: &str) -> Option<(usize, usize)> {
        let kids = self.children(id);
        if kids.is_empty() {
            return None;
        }
        let states: Vec<String> = kids.iter().map(|k| state(k)).collect();
        let total = states.iter().filter(|s| *s != "rejected").count();
        let done = states.iter().filter(|s| *s == "done" || *s == "shipped").count();
        Some((done, total))
    }
}

fn prio_num(n: &Note) -> usize {
    PRIORITIES.iter().position(|p| *p == priority(n)).unwrap_or(2)
}

/// Priority first, then rank (unranked last), then oldest first.
pub fn sort_items(items: &mut [&Note]) {
    items.sort_by(|a, b| {
        prio_num(a)
            .cmp(&prio_num(b))
            .then_with(|| match (rank(a), rank(b)) {
                (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then(a.created.cmp(&b.created))
            .then(a.id.cmp(&b.id))
    });
}

#[derive(Debug, Clone, Serialize)]
pub struct BoardItem {
    pub id: String,
    pub slug: Option<String>,
    pub key: String,
    pub kind: String,
    pub title: String,
    pub state: String,
    pub priority: String,
    pub rank: Option<f64>,
    pub assignee: Option<String>,
    /// The assignee under its current session name.
    pub holder: Option<String>,
    /// Since the last write or claim, like 45m, 3h, 2d.
    pub age: String,
    /// The branch or run ref of a queued item being built.
    pub building: Option<String>,
    /// When a session claimed it.
    pub claimed: Option<String>,
    /// A session claim on an open item with no write for a day.
    pub claim_stale: bool,
    pub project: Option<String>,
    pub parent: Option<String>,
    pub blocked: bool,
    pub blocked_by: Vec<String>,
    pub progress: Option<(usize, usize)>,
    /// An open question locks the item until the user answers.
    pub waiting: bool,
    /// An answer the agents have not acknowledged yet.
    pub answered: bool,
}

impl BoardItem {
    pub fn from_note(n: &Note, idx: &Index) -> BoardItem {
        BoardItem {
            id: n.id.clone(),
            slug: n.slug(),
            key: key(n),
            kind: n.kind.clone(),
            title: n.title.clone(),
            state: state(n),
            priority: priority(n),
            rank: rank(n),
            assignee: assignee(n),
            holder: assignee(n).map(|a| crate::session::holder(&a)),
            age: crate::util::short_age(&last_activity(n)),
            building: being_built(n),
            claimed: str_field(n, CLAIMED_KEY),
            claim_stale: assignee(n).is_some() && claimed_at(n).is_some() && !is_closed(&state(n)) && claim_stale(n),
            project: n.project.clone(),
            parent: parent(n),
            blocked: idx.is_blocked(n),
            blocked_by: idx.open_blockers(n),
            progress: idx.progress(&n.id),
            waiting: crate::question::locked(n),
            answered: crate::question::answered_unseen(n),
        }
    }

    /// `id kind priority title` plus assignee, blockers and progress, single spaced.
    pub fn short(&self) -> String {
        let name = self.slug.as_ref().map(|x| format!("{x} ")).unwrap_or_default();
        let mut s = format!("{name}{} {} {} {}", self.id, self.kind, self.priority, crate::format::fit_title(&self.title, self.slug.as_deref()));
        if let Some(a) = &self.holder {
            let _ = write!(s, " @{a}{}", if self.claim_stale { " (stale)" } else { "" });
        }
        s.push_str(&self.work_mark());
        if self.blocked {
            let _ = write!(s, " [blocked by {}]", self.blocked_by.join(", "));
        }
        if let Some((d, t)) = self.progress {
            let _ = write!(s, " ({d}/{t} done)");
        }
        s.push_str(self.question_mark());
        s
    }

    pub fn line(&self) -> String {
        let name = self.slug.as_ref().map(|x| format!("{x}  ")).unwrap_or_default();
        let mut s = format!("{name}{}  {:<7} {}  {}", self.key, self.kind, self.priority, self.title);
        if let Some(a) = &self.holder {
            let _ = write!(s, "  @{a}{}", if self.claim_stale { " (stale claim)" } else { "" });
        }
        s.push_str(&self.work_mark());
        if self.blocked {
            let _ = write!(s, "  [blocked by {}]", self.blocked_by.join(", "));
        }
        if let Some((d, t)) = self.progress {
            let _ = write!(s, "  ({d}/{t} done)");
        }
        s.push_str(self.question_mark());
        s
    }

    /// The age for work under way, in progress without an owner says so.
    fn work_mark(&self) -> String {
        match (&self.building, self.state.as_str(), &self.holder) {
            (Some(b), ..) => format!(" [being built {b} {}]", self.age),
            (None, "in-progress", Some(_)) => format!(" {}", self.age),
            (None, "in-progress", None) => format!(" (no owner) {}", self.age),
            _ => String::new(),
        }
    }

    fn question_mark(&self) -> &'static str {
        match (self.waiting, self.answered) {
            (true, _) => " [waiting on user]",
            (_, true) => " [answered]",
            _ => "",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Column {
    pub state: String,
    pub items: Vec<BoardItem>,
}

#[derive(Debug, Clone, Default)]
pub struct BoardFilter {
    pub kind: Option<String>,
    pub assignee: Option<String>,
    pub include_closed: bool,
}

/// Backlog items in scope, active status only. Returns all notes too so links outside the scope still resolve.
fn scoped_items(v: &Vault, scope: &Scope) -> Result<(Vec<Note>, Vec<String>)> {
    let all = v.load_notes();
    let ids: Vec<String> = filter_notes(all.clone(), scope, &Filter::default())?.into_iter().filter(is_item).map(|n| n.id).collect();
    Ok((all, ids))
}

/// Board column for items locked by an open question, whatever their state.
pub const WAITING: &str = "waiting-on-user";
/// Board column for queued items with a branch or run ref. Their stored state stays queued.
pub const BUILDING: &str = "being-built";

fn waiting_items(all: &[Note], ids: &HashSet<&str>, f: &BoardFilter, idx: &Index) -> Vec<BoardItem> {
    let mut items: Vec<&Note> = all
        .iter()
        .filter(|n| ids.contains(n.id.as_str()) && crate::question::locked(n))
        .filter(|n| f.kind.as_ref().is_none_or(|k| n.kind.eq_ignore_ascii_case(k)))
        .filter(|n| f.assignee.as_ref().is_none_or(|a| assignee(n).is_some_and(|x| x.eq_ignore_ascii_case(a))))
        .collect();
    sort_items(&mut items);
    items.iter().map(|n| BoardItem::from_note(n, idx)).collect()
}

pub fn board(v: &Vault, scope: &Scope, f: &BoardFilter) -> Result<Vec<Column>> {
    let (all, ids) = scoped_items(v, scope)?;
    let idx = Index::new(&all);
    let ids: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let mut cols = vec![];
    for st in STATES {
        if !f.include_closed && is_closed(st) {
            continue;
        }
        let mut items: Vec<&Note> = all
            .iter()
            .filter(|n| ids.contains(n.id.as_str()) && state(n) == st && !crate::question::locked(n) && being_built(n).is_none())
            .filter(|n| f.kind.as_ref().is_none_or(|k| n.kind.eq_ignore_ascii_case(k)))
            .filter(|n| f.assignee.as_ref().is_none_or(|a| assignee(n).is_some_and(|x| x.eq_ignore_ascii_case(a))))
            .collect();
        sort_items(&mut items);
        cols.push(Column { state: st.to_string(), items: items.iter().map(|n| BoardItem::from_note(n, &idx)).collect() });
        if st == "in-progress" {
            let mut built: Vec<&Note> = all
                .iter()
                .filter(|n| ids.contains(n.id.as_str()) && being_built(n).is_some() && !crate::question::locked(n))
                .filter(|n| f.kind.as_ref().is_none_or(|k| n.kind.eq_ignore_ascii_case(k)))
                .filter(|n| f.assignee.as_ref().is_none_or(|a| assignee(n).is_some_and(|x| x.eq_ignore_ascii_case(a))))
                .collect();
            sort_items(&mut built);
            if !built.is_empty() {
                cols.push(Column { state: BUILDING.to_string(), items: built.iter().map(|n| BoardItem::from_note(n, &idx)).collect() });
            }
            cols.push(Column { state: WAITING.to_string(), items: waiting_items(&all, &ids, f, &idx) });
        }
    }
    Ok(cols)
}

pub fn board_text(cols: &[Column], scope_label: &str) -> String {
    let mut s = format!("# Board: {scope_label}\n");
    let total: usize = cols.iter().map(|c| c.items.len()).sum();
    if total == 0 {
        s.push_str("\nNo backlog items. Capture one with `breadcrumb idea \"...\"` or add a note of kind idea, epic, feature, task or bug.\n");
        return s;
    }
    for c in cols {
        let _ = writeln!(s, "\n## {} ({})", c.state, c.items.len());
        for it in &c.items {
            let _ = writeln!(s, "- {}", it.line());
        }
    }
    s
}

pub fn board_compact(cols: &[Column]) -> String {
    let mut s = String::new();
    for c in cols.iter().filter(|c| !c.items.is_empty()) {
        let _ = writeln!(s, "{} ({}):", c.state, c.items.len());
        for it in &c.items {
            let _ = writeln!(s, "{}", it.short());
        }
    }
    if s.is_empty() {
        s.push_str("no backlog items, add one with kind idea, epic, feature, task or bug\n");
    }
    s
}

pub fn capture_idea(v: &Vault, title: &str, body: Option<String>, project: Option<String>, tags: Vec<String>, require_existing_project: bool) -> Result<Note> {
    crate::ops::add(
        v,
        crate::ops::AddReq {
            title: title.to_string(),
            kind: Some("idea".into()),
            project,
            tags,
            body,
            fields: vec![("state".into(), crate::ops::FieldInput::Raw("idea".into()))],
            require_existing_project,
            ..Default::default()
        },
    )
}

pub(crate) fn get_item(v: &Vault, id: &str) -> Result<Note> {
    let n = v.get(id)?;
    if !is_item(&n) {
        bail!(
            "{} is a '{}' note, not a backlog item (kinds: {}). Change its kind to one of those first: update with kind (MCP or `breadcrumb update --kind`), or `breadcrumb promote {} --to feature` on the CLI",
            n.id,
            n.kind,
            KINDS.join(", "),
            n.id
        );
    }
    Ok(n)
}

fn queued_ranks(all: &[Note], project: &Option<String>, except: &str) -> Vec<(f64, String)> {
    let mut r: Vec<(f64, String)> = all
        .iter()
        .filter(|n| n.id != except && &n.project == project && is_item(n) && state(n) == "queued" && !n.is_archived())
        .filter_map(|n| rank(n).map(|r| (r, n.id.clone())))
        .collect();
    r.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    r
}

fn end_rank(all: &[Note], project: &Option<String>, except: &str) -> f64 {
    queued_ranks(all, project, except).last().map(|(r, _)| (r + 1.0).floor()).unwrap_or(1.0)
}

#[derive(Debug, Clone)]
pub enum QueuePos {
    End,
    Top,
    After(String),
}

pub(crate) fn save(v: &Vault, n: &mut Note, op: &str) -> Result<()> {
    save_with(v, n, op, vec![])
}

/// Writes `n` and the notes changed along with it in one commit.
fn save_with(v: &Vault, n: &mut Note, op: &str, mut also: Vec<Note>) -> Result<()> {
    let k = v.require_kind(&n.kind)?;
    k.validate(&mut n.extra)?;
    crate::session::stamp_write(n);
    n.updated = now();
    let mut paths = vec![v.write_note(n)?];
    for o in &mut also {
        paths.push(v.write_note(o)?);
    }
    v.commit(&paths, &format!("breadcrumb: {op} {} {}", n.id, crate::util::one_line(&n.title)))?;
    Ok(())
}

pub fn queue(v: &Vault, id: &str, pos: QueuePos, by: Option<&str>) -> Result<Note> {
    let _g = v.lock()?;
    let mut n = get_item(v, id)?;
    let all = v.load_notes();
    let ranks = queued_ranks(&all, &n.project, &n.id);
    let r = match pos {
        QueuePos::End => end_rank(&all, &n.project, &n.id),
        QueuePos::Top => ranks.first().map(|(r, _)| r - 1.0).unwrap_or(1.0),
        QueuePos::After(other) => {
            let oid = v.resolve_id(&other)?;
            let Some(i) = ranks.iter().position(|(_, id)| *id == oid) else {
                bail!("{oid} is not a ranked queued item in the same project, queue it first");
            };
            match ranks.get(i + 1) {
                Some((next, _)) => (ranks[i].0 + next) / 2.0,
                None => ranks[i].0 + 1.0,
            }
        }
    };
    let from = state(&n);
    n.set_field("state", Value::String("queued".into()));
    n.set_field("rank", Value::Number(r.into()));
    record_state_change(&mut n, &from, "queued", by);
    end_build_refs(&mut n, &from, &[], by);
    save(v, &mut n, "queue")?;
    Ok(n)
}

/// Best queued, unblocked item. With `claim`, moves it to in-progress and assigns it, atomically under the lock.
pub fn next(v: &Vault, scope: &Scope, assignee_filter: Option<&str>, claim: Option<&str>) -> Result<Option<Note>> {
    next_as(v, scope, assignee_filter, claim, claim)
}

/// Like `next`, skipping items another session holds a fresh claim on. Without `me` every fresh claim is someone else's.
pub fn next_as(v: &Vault, scope: &Scope, assignee_filter: Option<&str>, claim: Option<&str>, me: Option<&str>) -> Result<Option<Note>> {
    Ok(next_notice(v, scope, assignee_filter, claim, me)?.map(|(n, _)| n))
}

/// Like `next_as`, plus the question notice of the picked item as it was before a claim acknowledged its answers.
/// Items with an unacknowledged answer come before the rest, in any open or built state.
pub fn next_notice(v: &Vault, scope: &Scope, assignee_filter: Option<&str>, claim: Option<&str>, me: Option<&str>) -> Result<Option<(Note, String)>> {
    let who = me.or(assignee_filter);
    let _g = v.lock()?;
    let (all, ids) = scoped_items(v, scope)?;
    let idx = Index::new(&all);
    let ids: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let mut cands: Vec<&Note> = all
        .iter()
        .filter(|n| ids.contains(n.id.as_str()) && !crate::question::locked(n))
        .filter(|n| crate::question::answered_unseen(n) && state(n) != "rejected" || state(n) == "queued" && !idx.is_blocked(n))
        .filter(|n| match (assignee_filter, assignee(n)) {
            (Some(want), Some(have)) => have.eq_ignore_ascii_case(want),
            _ => true,
        })
        .filter(|n| held_by_other(n, who).is_none())
        .filter(|n| being_built(n).is_none() || who.is_some_and(|w| assignee(n).is_some_and(|a| a.eq_ignore_ascii_case(w))))
        .collect();
    sort_items(&mut cands);
    cands.sort_by_key(|n| !crate::question::answered_unseen(n));
    let Some(best) = cands.first() else { return Ok(None) };
    let mut n = (*best).clone();
    let notice = crate::question::notice(&n);
    if let Some(by) = claim {
        let by = actor(Some(by));
        let from = state(&n);
        if from == "queued" {
            n.set_field("state", Value::String("in-progress".into()));
            record_state_change(&mut n, "queued", "in-progress", Some(&by));
        }
        if !is_closed(&from) {
            set_claim(&mut n, &by);
        }
        crate::question::ack_in(&mut n, &by);
        save(v, &mut n, "claim")?;
    }
    Ok(Some((n, notice)))
}

#[derive(Debug, Clone, Default)]
pub struct MoveReq {
    pub state: String,
    pub resolution: Option<String>,
    pub shipped_in: Option<String>,
    pub by: Option<String>,
    /// Acceptance criteria to mark met in the same write.
    pub check: Option<Check>,
    /// `type:value` refs to add, `-type:value` removes.
    pub refs: Vec<String>,
    /// What the user should try, added to the acceptance criteria when moving to review.
    pub steps: Vec<String>,
    /// The user's check of an item in review, refused for items in any other state.
    pub verdict: Option<Verdict>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail(String),
}

#[derive(Debug, Clone)]
pub struct MoveResult {
    pub note: Note,
    pub from: String,
    pub to: String,
    pub unmet: Vec<String>,
    /// False when the item already had the state and nothing else changed, so nothing was written.
    pub changed: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    All,
    /// 1 based positions in the acceptance list.
    Some(Vec<usize>),
}

impl Check {
    /// `all`, or positions like `2,3` or `1 4`.
    pub fn parse(s: &str) -> Result<Check> {
        let s = s.trim();
        if s.is_empty() || s.eq_ignore_ascii_case("all") {
            return Ok(Check::All);
        }
        let mut out = vec![];
        for p in s.split([',', ' ']).map(str::trim).filter(|p| !p.is_empty()) {
            match p.parse::<usize>() {
                Ok(i) if i > 0 => out.push(i),
                _ => bail!("invalid criterion '{p}', expected all or 1 based numbers like 2,3"),
            }
        }
        Ok(Check::Some(out))
    }
}

fn is_met(c: &str) -> bool {
    c.trim_start().to_ascii_lowercase().starts_with("[x]")
}

/// Marks the chosen criteria `[x]`. Returns how many changed.
fn apply_check(n: &mut Note, which: &Check) -> Result<usize> {
    let Some(Value::Sequence(items)) = n.field("acceptance").cloned() else {
        bail!("{} has no acceptance criteria", n.id);
    };
    if let Check::Some(ix) = which
        && let Some(bad) = ix.iter().find(|i| **i > items.len())
    {
        bail!("{} has {} acceptance criteria, there is no {bad}", n.id, items.len());
    }
    let mut changed = 0;
    let out: Vec<Value> = items
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            let want = match which {
                Check::All => true,
                Check::Some(ix) => ix.contains(&(i + 1)),
            };
            match yaml_scalar_string(&v) {
                Some(c) if want && !is_met(&c) => {
                    changed += 1;
                    let t = c.trim_start();
                    let t = t.strip_prefix("[ ]").unwrap_or(t).trim_start();
                    Value::String(format!("[x] {t}"))
                }
                _ => v,
            }
        })
        .collect();
    n.set_field("acceptance", Value::Sequence(out));
    Ok(changed)
}

/// Marks acceptance criteria met without changing the state. Writes nothing when all chosen ones already were.
pub fn check(v: &Vault, id: &str, which: &Check) -> Result<(Note, usize)> {
    let _g = v.lock()?;
    let mut n = get_item(v, id)?;
    let changed = apply_check(&mut n, which)?;
    if changed > 0 {
        save(v, &mut n, "check")?;
    }
    Ok((n, changed))
}

fn criterion_text(c: &str) -> &str {
    let t = c.trim();
    ["[x]", "[X]", "[ ]"].iter().find_map(|p| t.strip_prefix(p)).unwrap_or(t).trim()
}

/// Adds steps as open criteria. A step already listed stays where it is and opens again, the user has to try it again.
fn add_steps(n: &mut Note, steps: &[String]) -> usize {
    let mut items: Vec<Value> = match n.field("acceptance") {
        Some(Value::Sequence(s)) => s.clone(),
        Some(v) => vec![v.clone()],
        None => vec![],
    };
    let mut changed = 0;
    for step in steps.iter().map(|s| crate::util::one_line(s)).filter(|s| !s.trim().is_empty()) {
        let plain = criterion_text(&step).to_string();
        let same = |v: &Value| yaml_scalar_string(v).is_some_and(|c| criterion_text(&c).eq_ignore_ascii_case(&plain));
        match items.iter().position(same) {
            Some(i) if yaml_scalar_string(&items[i]).is_some_and(|c| is_met(&c)) => {
                items[i] = Value::String(plain);
                changed += 1;
            }
            Some(_) => {}
            None => {
                items.push(Value::String(plain));
                changed += 1;
            }
        }
    }
    if changed > 0 {
        n.set_field("acceptance", Value::Sequence(items));
    }
    changed
}

/// Splits `--check` values: `all` and positions tick as before, anything else is a step when moving to review.
pub fn split_checks(values: &[String], state: &str) -> Result<(Option<Check>, Vec<String>)> {
    let review = check_state(state).is_ok_and(|s| s == "review");
    let (mut check, mut steps) = (None, vec![]);
    for v in values {
        match Check::parse(v) {
            Ok(Check::All) => check = Some(Check::All),
            Ok(Check::Some(ix)) => match &mut check {
                Some(Check::Some(have)) => have.extend(ix),
                Some(Check::All) => {}
                None => check = Some(Check::Some(ix)),
            },
            Err(_) if review => steps.push(v.clone()),
            Err(e) => return Err(e),
        }
    }
    Ok((check, steps))
}

/// What is left for the user to try: the acceptance criteria not yet marked met.
pub fn review_steps(n: &Note) -> Vec<String> {
    unmet_acceptance(n).iter().map(|c| criterion_text(c).to_string()).collect()
}

fn move_warnings(n: &Note, from: &str, to: &str) -> Vec<String> {
    let mut w = vec![];
    if from == "queued" && to == "review" && assignee(n).is_none() {
        w.push(format!("{} went from queued to review and was never claimed, `breadcrumb claim {}` when work starts so nobody else picks it up", n.id, n.id));
    }
    if to == "review" && review_steps(n).is_empty() {
        w.push(format!("{} has no steps for the user to check, add them with `breadcrumb move {} review --check \"step\"` (repeatable, MCP move steps)", n.id, n.id));
    }
    w
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewItem {
    pub id: String,
    pub slug: Option<String>,
    pub key: String,
    pub kind: String,
    pub title: String,
    pub project: Option<String>,
    pub priority: String,
    pub created: String,
    pub steps: Vec<String>,
}

/// Items waiting for the user's check, priority first, then oldest.
pub fn review_items(v: &Vault, scope: &Scope) -> Result<Vec<ReviewItem>> {
    let (all, ids) = scoped_items(v, scope)?;
    let ids: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let mut items: Vec<&Note> = all.iter().filter(|n| ids.contains(n.id.as_str()) && state(n) == "review").collect();
    items.sort_by(|a, b| prio_num(a).cmp(&prio_num(b)).then(a.created.cmp(&b.created)).then(a.id.cmp(&b.id)));
    Ok(items
        .iter()
        .map(|n| ReviewItem {
            id: n.id.clone(),
            slug: n.slug(),
            key: key(n),
            kind: n.kind.clone(),
            title: n.title.clone(),
            project: n.project.clone(),
            priority: priority(n),
            created: crate::util::fmt_time(&n.created),
            steps: review_steps(n),
        })
        .collect())
}

pub fn review_text(items: &[ReviewItem], scope_label: &str) -> String {
    if items.is_empty() {
        return format!("nothing in review in {scope_label}, --all shows every project\n");
    }
    let mut s = format!("{} in review in {scope_label}, try each, then `breadcrumb review pass <id>` or `breadcrumb review fail <id> --reason \"...\"`\n", items.len());
    for it in items {
        let name = it.slug.as_ref().map(|x| format!("{x} ")).unwrap_or_default();
        let _ = writeln!(s, "\n{} {name}{} {}", it.priority, it.key, it.title);
        if it.steps.is_empty() {
            let _ = writeln!(s, "  no steps recorded, `breadcrumb get {}` or ask the agent that built it", it.id);
        }
        for (i, step) in it.steps.iter().enumerate() {
            let _ = writeln!(s, "  {}. {step}", i + 1);
        }
    }
    s
}

/// The user's check passed: on to done, with the criteria marked met.
pub fn review_pass(v: &Vault, id: &str, by: Option<String>) -> Result<MoveResult> {
    move_item(v, id, MoveReq { state: "done".into(), by, verdict: Some(Verdict::Pass), ..Default::default() })
}

/// The user's check failed: back to queued, with the reason in the body and the claim gone so the queue can hand it out.
pub fn review_fail(v: &Vault, id: &str, reason: &str, by: Option<String>) -> Result<MoveResult> {
    move_item(v, id, MoveReq { state: "queued".into(), by, verdict: Some(Verdict::Fail(reason.to_string())), ..Default::default() })
}

pub fn check_text(n: &Note, changed: usize) -> String {
    let unmet = unmet_acceptance(n);
    let mut s = if changed == 0 { format!("{}: nothing to tick", n.id) } else { format!("{}: ticked {changed}", n.id) };
    if unmet.is_empty() {
        s.push_str(", all acceptance criteria met");
    } else {
        let _ = write!(s, ", {} still open: {}", unmet.len(), unmet.join("; "));
    }
    s
}

pub fn unmet_acceptance(n: &Note) -> Vec<String> {
    match n.field("acceptance") {
        Some(Value::Sequence(s)) => s
            .iter()
            .filter_map(yaml_scalar_string)
            .filter(|c| !is_met(c))
            .collect(),
        _ => vec![],
    }
}

pub fn move_item(v: &Vault, id: &str, req: MoveReq) -> Result<MoveResult> {
    let to = check_state(&req.state)?;
    if !req.steps.is_empty() && to != "review" {
        bail!("steps are for moving to review, not {to}");
    }
    if matches!(&req.verdict, Some(Verdict::Fail(r)) if r.trim().is_empty()) {
        bail!("a failed review needs a reason");
    }
    let resolution = req.resolution.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let _g = v.lock()?;
    let mut n = get_item(v, id)?;
    let from = state(&n);
    if req.verdict.is_some() && from != "review" {
        bail!("{} is {from}, not in review", n.id);
    }
    if to == "rejected" && resolution.is_none() && (from != "rejected" || str_field(&n, "resolution").is_none()) {
        bail!("moving to rejected requires a resolution explaining why");
    }
    let unchanged = |key: &str, want: &Option<String>| want.as_ref().is_none_or(|w| str_field(&n, key).as_deref() == Some(w.as_str()));
    let shipped_in = req.shipped_in.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let same = from == to && unchanged("resolution", &resolution) && unchanged("shipped_in", &shipped_in);
    let ticked = match &req.check {
        Some(c) => apply_check(&mut n, c)?,
        None => 0,
    };
    let added = add_steps(&mut n, &req.steps);
    let mut ref_edits = req.refs.clone();
    if let Some(sha) = shipped_in.as_deref().filter(|s| (to == "done" || to == "shipped") && is_sha(s)) {
        ref_edits.push(format!("commit:{sha}"));
    }
    let refs_before = n.refs();
    crate::ops::edit_refs(&mut n, &ref_edits)?;
    if same && ticked == 0 && added == 0 && n.refs() == refs_before {
        let unmet = if to == "done" || to == "shipped" { unmet_acceptance(&n) } else { vec![] };
        let warnings = move_warnings(&n, &from, &to);
        return Ok(MoveResult { note: n, from, to, unmet, changed: false, warnings });
    }
    match &req.verdict {
        Some(Verdict::Pass) if n.field("acceptance").is_some() => {
            apply_check(&mut n, &Check::All)?;
        }
        Some(Verdict::Fail(reason)) => {
            let who = crate::session::attribute(req.by.as_deref());
            let date = crate::util::fmt_date(&now());
            n.body = insert_before_history(&n.body, &format!("{date} {who}:\nreview failed: {}", crate::util::normalize_newlines(reason.trim())));
            clear_claim(&mut n);
        }
        _ => {}
    }
    n.set_field("state", Value::String(to.clone()));
    if let Some(r) = &resolution {
        n.set_field("resolution", Value::String(r.clone()));
    }
    if let Some(s) = shipped_in {
        n.set_field("shipped_in", Value::String(s));
    }
    if to == "queued" && from != "queued" {
        let all = v.load_notes();
        n.set_field("rank", Value::Number(end_rank(&all, &n.project, &n.id).into()));
    }
    let unmet = if to == "done" || to == "shipped" { unmet_acceptance(&n) } else { vec![] };
    record_state_change(&mut n, &from, &to, req.by.as_deref());
    if to == "queued" {
        let keep = crate::note::normalize_refs(&req.refs.iter().filter(|r| !r.trim().starts_with('-')).map(|r| r.trim().trim_start_matches('+').to_string()).collect::<Vec<_>>())?;
        end_build_refs(&mut n, &from, &keep, req.by.as_deref());
    }
    // Notes with `expires: until:<this item>` stop holding now.
    let mut waiting = if is_closed(&to) { crate::lifecycle::waiting_for(&v.load_notes(), &n.id) } else { vec![] };
    waiting.iter_mut().for_each(crate::lifecycle::mark);
    save_with(v, &mut n, "move", waiting)?;
    let warnings = move_warnings(&n, &from, &to);
    Ok(MoveResult { note: n, from, to, unmet, changed: true, warnings })
}

fn is_sha(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
}

pub fn move_text(r: &MoveResult) -> String {
    let name = named_key(&r.note);
    let mut s = if r.changed {
        format!("{name} {}: {} -> {}\n", r.note.title, r.from, r.to)
    } else {
        format!("{name} {}: already in {}\n", r.note.title, r.to)
    };
    if !r.unmet.is_empty() {
        let _ = writeln!(s, "warning: {} acceptance criteria not marked met ([x]), {}:", r.unmet.len(), check_hint(&r.note.id));
        for (i, c) in unmet_numbered(&r.note) {
            let _ = writeln!(s, "  {i}. {c}");
        }
    }
    for w in &r.warnings {
        let _ = writeln!(s, "warning: {w}");
    }
    s
}

pub fn check_hint(id: &str) -> String {
    format!("tick them with `breadcrumb check {id} [all|2,3]` or move check")
}

/// Unmet criteria with their 1 based position, as `check` expects it.
pub fn unmet_numbered(n: &Note) -> Vec<(usize, String)> {
    match n.field("acceptance") {
        Some(Value::Sequence(s)) => s.iter().enumerate().filter_map(|(i, v)| yaml_scalar_string(v).filter(|c| !is_met(c)).map(|c| (i + 1, c))).collect(),
        _ => vec![],
    }
}

pub fn promote(v: &Vault, id: &str, to: &str, by: Option<&str>) -> Result<Note> {
    let to = to.trim().to_ascii_lowercase();
    if !["feature", "epic", "task", "bug"].contains(&to.as_str()) {
        bail!("promote target must be feature, epic, task or bug");
    }
    let _g = v.lock()?;
    let mut n = v.get(id)?;
    v.require_kind(&to)?;
    let from_kind = n.kind.clone();
    let from_state = state(&n);
    n.kind = to.clone();
    if from_kind != to {
        n.body = append_history(&n.body, &history_line("kind", &from_kind, &to, &actor(by)));
    }
    if from_state == "idea" {
        n.set_field("state", Value::String("backlog".into()));
        record_state_change(&mut n, "idea", "backlog", by);
    }
    save(v, &mut n, "promote")?;
    Ok(n)
}

fn tree_lines(idx: &Index, n: &Note, depth: usize, seen: &mut HashSet<String>, out: &mut String) {
    let item = BoardItem::from_note(n, idx);
    let blocked = if item.blocked { "  [blocked]" } else { "" };
    let _ = writeln!(out, "{}- {} {} [{}] {}{}", "  ".repeat(depth), item.key, item.kind, state_label(n), item.title, blocked);
    if !seen.insert(n.id.clone()) {
        let _ = writeln!(out, "{}  (cycle)", "  ".repeat(depth));
        return;
    }
    for c in idx.children(&n.id) {
        if seen.contains(&c.id) {
            let _ = writeln!(out, "{}- {} (cycle)", "  ".repeat(depth + 1), c.id);
            continue;
        }
        tree_lines(idx, c, depth + 1, seen, out);
    }
}

pub fn tree(v: &Vault, id: &str) -> Result<String> {
    let root = get_item(v, id)?;
    let all = v.load_notes();
    let idx = Index::new(&all);
    let mut out = String::new();
    let mut chain = vec![];
    let mut cur = parent(&root);
    let mut guard = HashSet::from([root.id.clone()]);
    while let Some(p) = cur {
        if !guard.insert(p.clone()) {
            break;
        }
        match idx.by_id.get(p.as_str()) {
            Some(pn) => {
                chain.push(format!("{} {} [{}] {}", key(pn), pn.kind, state_label(pn), pn.title));
                cur = parent(pn);
            }
            None => {
                chain.push(format!("{p} (missing)"));
                break;
            }
        }
    }
    if !chain.is_empty() {
        chain.reverse();
        let _ = writeln!(out, "parents: {}", chain.join(" > "));
    }
    let mut seen = HashSet::new();
    tree_lines(&idx, &root, 0, &mut seen, &mut out);
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct DepEntry {
    pub id: String,
    pub depth: usize,
    pub title: String,
    pub state: String,
    pub missing: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Deps {
    pub id: String,
    pub blocks: Vec<DepEntry>,
    pub blocked_by: Vec<DepEntry>,
    pub blocked: bool,
}

fn walk(idx: &Index, start: &Note, forward: bool) -> Vec<DepEntry> {
    let mut seen = HashSet::from([start.id.clone()]);
    let mut q = VecDeque::from([(start.id.clone(), 0usize)]);
    let mut out = vec![];
    while let Some((id, depth)) = q.pop_front() {
        let Some(n) = idx.by_id.get(id.as_str()) else { continue };
        let next = if forward { idx.blocks(n) } else { idx.blocked_by(n) };
        for nid in next {
            if !seen.insert(nid.clone()) {
                continue;
            }
            match idx.by_id.get(nid.as_str()) {
                Some(m) => {
                    out.push(DepEntry { id: nid.clone(), depth: depth + 1, title: m.title.clone(), state: state_label(m), missing: false });
                    q.push_back((nid, depth + 1));
                }
                None => out.push(DepEntry { id: nid, depth: depth + 1, title: String::new(), state: String::new(), missing: true }),
            }
        }
    }
    out
}

/// Transitive blockers and dependents, cycle safe through a visited set.
pub fn deps(v: &Vault, id: &str) -> Result<Deps> {
    let n = get_item(v, id)?;
    let all = v.load_notes();
    let idx = Index::new(&all);
    Ok(Deps { id: n.id.clone(), blocks: walk(&idx, &n, true), blocked_by: walk(&idx, &n, false), blocked: idx.is_blocked(&n) })
}

pub fn deps_text(d: &Deps, title: &str) -> String {
    let mut s = format!("{} {}{}\n", d.id, title, if d.blocked { "  [blocked]" } else { "" });
    let section = |s: &mut String, name: &str, list: &[DepEntry]| {
        let _ = writeln!(s, "\n{name}:");
        if list.is_empty() {
            let _ = writeln!(s, "  (none)");
        }
        for e in list {
            if e.missing {
                let _ = writeln!(s, "{}- {} (missing note)", "  ".repeat(e.depth), e.id);
            } else {
                let _ = writeln!(s, "{}- {} [{}] {}", "  ".repeat(e.depth), e.id, e.state, e.title);
            }
        }
    };
    section(&mut s, "blocked by", &d.blocked_by);
    section(&mut s, "blocks", &d.blocks);
    s
}

/// Items the context digest lists in its backlog section.
pub fn in_digest(n: &Note) -> bool {
    is_item(n) && matches!(state(n).as_str(), "in-progress" | "queued")
}

/// Compact backlog lines for the context digest, empty when the scope has no items.
pub fn context_section(scope_notes: &[Note], all: &[Note]) -> String {
    let items: Vec<&Note> = scope_notes.iter().filter(|n| is_item(n)).collect();
    if items.is_empty() {
        return String::new();
    }
    let idx = Index::new(all);
    let counts: Vec<String> = STATES
        .iter()
        .map(|st| (st, items.iter().filter(|n| state(n) == *st).count()))
        .filter(|(_, c)| *c > 0)
        .map(|(st, c)| format!("{st} {c}"))
        .collect();
    let mut built: Vec<&Note> = items.iter().copied().filter(|n| being_built(n).is_some() && !crate::question::locked(n)).collect();
    sort_items(&mut built);
    let mut s = format!("Backlog: {}\n", counts.join(", "));
    let mut wip: Vec<&Note> = items.iter().copied().filter(|n| state(n) == "in-progress").collect();
    sort_items(&mut wip);
    if !wip.is_empty() {
        s.push_str("In progress:\n");
        for n in wip {
            let _ = writeln!(s, "{}", BoardItem::from_note(n, &idx).short());
        }
    }
    if !built.is_empty() {
        s.push_str("Being built (stored as queued):\n");
        for n in built {
            let _ = writeln!(s, "{}", BoardItem::from_note(n, &idx).short());
        }
    }
    let mut queued: Vec<&Note> = items.iter().copied().filter(|n| state(n) == "queued" && !crate::question::locked(n) && being_built(n).is_none()).collect();
    sort_items(&mut queued);
    if !queued.is_empty() {
        s.push_str("Queued (next takes the first unblocked):\n");
        for n in queued.iter().take(5) {
            let _ = writeln!(s, "{}", BoardItem::from_note(n, &idx).short());
        }
    }
    s.push_str(&crate::question::digest_lines(&items));
    s
}

pub fn item_summary(n: &Note) -> String {
    let mut s = format!("{} {} [{}] {} {}", named_key(n), n.kind, state(n), priority(n), n.title);
    if let Some(a) = assignee(n) {
        let _ = write!(s, " @{a}");
    }
    if let Some(r) = n.field("resolution") {
        let _ = write!(s, " (resolution: {})", yaml_display(r));
    }
    s
}
