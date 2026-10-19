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

pub fn key(n: &Note) -> String {
    match &n.project {
        Some(p) => format!("{}-{}", p.to_ascii_uppercase(), n.id),
        None => n.id.clone(),
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
    format!("- {}: {what} {from} -> {to} (by {by})", now().format("%Y-%m-%dT%H:%MZ"))
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
        n.body = append_history(&n.body, &history_line("state", from, to, &actor(by)));
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
    pub key: String,
    pub kind: String,
    pub title: String,
    pub state: String,
    pub priority: String,
    pub rank: Option<f64>,
    pub assignee: Option<String>,
    pub project: Option<String>,
    pub parent: Option<String>,
    pub blocked: bool,
    pub blocked_by: Vec<String>,
    pub progress: Option<(usize, usize)>,
}

impl BoardItem {
    pub fn from_note(n: &Note, idx: &Index) -> BoardItem {
        BoardItem {
            id: n.id.clone(),
            key: key(n),
            kind: n.kind.clone(),
            title: n.title.clone(),
            state: state(n),
            priority: priority(n),
            rank: rank(n),
            assignee: assignee(n),
            project: n.project.clone(),
            parent: parent(n),
            blocked: idx.is_blocked(n),
            blocked_by: idx.open_blockers(n),
            progress: idx.progress(&n.id),
        }
    }

    /// `id kind priority title` plus assignee, blockers and progress, single spaced.
    pub fn short(&self) -> String {
        let mut s = format!("{} {} {} {}", self.id, self.kind, self.priority, crate::format::short_title(&self.title));
        if let Some(a) = &self.assignee {
            let _ = write!(s, " @{a}");
        }
        if self.blocked {
            let _ = write!(s, " [blocked by {}]", self.blocked_by.join(", "));
        }
        if let Some((d, t)) = self.progress {
            let _ = write!(s, " ({d}/{t} done)");
        }
        s
    }

    pub fn line(&self) -> String {
        let mut s = format!("{}  {:<7} {}  {}", self.key, self.kind, self.priority, self.title);
        if let Some(a) = &self.assignee {
            let _ = write!(s, "  @{a}");
        }
        if self.blocked {
            let _ = write!(s, "  [blocked by {}]", self.blocked_by.join(", "));
        }
        if let Some((d, t)) = self.progress {
            let _ = write!(s, "  ({d}/{t} done)");
        }
        s
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
            .filter(|n| ids.contains(n.id.as_str()) && state(n) == st)
            .filter(|n| f.kind.as_ref().is_none_or(|k| n.kind.eq_ignore_ascii_case(k)))
            .filter(|n| f.assignee.as_ref().is_none_or(|a| assignee(n).is_some_and(|x| x.eq_ignore_ascii_case(a))))
            .collect();
        sort_items(&mut items);
        cols.push(Column { state: st.to_string(), items: items.iter().map(|n| BoardItem::from_note(n, &idx)).collect() });
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

fn get_item(v: &Vault, id: &str) -> Result<Note> {
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

fn save(v: &Vault, n: &mut Note, op: &str) -> Result<()> {
    let k = v.require_kind(&n.kind)?;
    k.validate(&mut n.extra)?;
    n.updated = now();
    let p = v.write_note(n)?;
    v.commit(&[p], &format!("breadcrumb: {op} {} {}", n.id, crate::util::one_line(&n.title)))?;
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
    save(v, &mut n, "queue")?;
    Ok(n)
}

/// Best queued, unblocked item. With `claim`, moves it to in-progress and assigns it, atomically under the lock.
pub fn next(v: &Vault, scope: &Scope, assignee_filter: Option<&str>, claim: Option<&str>) -> Result<Option<Note>> {
    let _g = v.lock()?;
    let (all, ids) = scoped_items(v, scope)?;
    let idx = Index::new(&all);
    let ids: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let mut cands: Vec<&Note> = all
        .iter()
        .filter(|n| ids.contains(n.id.as_str()) && state(n) == "queued" && !idx.is_blocked(n))
        .filter(|n| match (assignee_filter, assignee(n)) {
            (Some(want), Some(have)) => have.eq_ignore_ascii_case(want),
            _ => true,
        })
        .collect();
    sort_items(&mut cands);
    let Some(best) = cands.first() else { return Ok(None) };
    let mut n = (*best).clone();
    if let Some(by) = claim {
        let by = actor(Some(by));
        n.set_field("state", Value::String("in-progress".into()));
        n.set_field("assignee", Value::String(by.clone()));
        record_state_change(&mut n, "queued", "in-progress", Some(&by));
        save(v, &mut n, "claim")?;
    }
    Ok(Some(n))
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
}

#[derive(Debug, Clone)]
pub struct MoveResult {
    pub note: Note,
    pub from: String,
    pub to: String,
    pub unmet: Vec<String>,
    /// False when the item already had the state and nothing else changed, so nothing was written.
    pub changed: bool,
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
    let resolution = req.resolution.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let _g = v.lock()?;
    let mut n = get_item(v, id)?;
    let from = state(&n);
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
    let mut ref_edits = req.refs.clone();
    if let Some(sha) = shipped_in.as_deref().filter(|s| (to == "done" || to == "shipped") && is_sha(s)) {
        ref_edits.push(format!("commit:{sha}"));
    }
    let refs_before = n.refs();
    crate::ops::edit_refs(&mut n, &ref_edits)?;
    if same && ticked == 0 && n.refs() == refs_before {
        let unmet = if to == "done" || to == "shipped" { unmet_acceptance(&n) } else { vec![] };
        return Ok(MoveResult { note: n, from, to, unmet, changed: false });
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
    save(v, &mut n, "move")?;
    Ok(MoveResult { note: n, from, to, unmet, changed: true })
}

fn is_sha(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
}

pub fn move_text(r: &MoveResult) -> String {
    let mut s = if r.changed {
        format!("{} {}: {} -> {}\n", key(&r.note), r.note.title, r.from, r.to)
    } else {
        format!("{} {}: already in {}\n", key(&r.note), r.note.title, r.to)
    };
    if !r.unmet.is_empty() {
        let _ = writeln!(s, "warning: {} acceptance criteria not marked met ([x]), {}:", r.unmet.len(), check_hint(&r.note.id));
        for (i, c) in unmet_numbered(&r.note) {
            let _ = writeln!(s, "  {i}. {c}");
        }
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
    let mut s = format!("Backlog: {}\n", counts.join(", "));
    let mut wip: Vec<&Note> = items.iter().copied().filter(|n| state(n) == "in-progress").collect();
    sort_items(&mut wip);
    if !wip.is_empty() {
        s.push_str("In progress:\n");
        for n in wip {
            let _ = writeln!(s, "{}", BoardItem::from_note(n, &idx).short());
        }
    }
    let mut queued: Vec<&Note> = items.iter().copied().filter(|n| state(n) == "queued").collect();
    sort_items(&mut queued);
    if !queued.is_empty() {
        s.push_str("Queued (next takes the first unblocked):\n");
        for n in queued.iter().take(5) {
            let _ = writeln!(s, "{}", BoardItem::from_note(n, &idx).short());
        }
    }
    s
}

pub fn item_summary(n: &Note) -> String {
    let mut s = format!("{} {} [{}] {} {}", key(n), n.kind, state(n), priority(n), n.title);
    if let Some(a) = assignee(n) {
        let _ = write!(s, " @{a}");
    }
    if let Some(r) = n.field("resolution") {
        let _ = write!(s, " (resolution: {})", yaml_display(r));
    }
    s
}
