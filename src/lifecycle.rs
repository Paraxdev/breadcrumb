//! Notes that no longer hold: superseded by a newer note, a decision marked superseded, or past their `expires`.

use crate::backlog::{is_closed, is_item, state};
use crate::note::{EXPIRES_KEY, Note, yaml_scalar_string};
use crate::util::{fmt_time, now};
use crate::vault::Vault;
use anyhow::{Result, bail};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::Serialize;
use serde_yaml::Value;
use std::collections::{HashMap, HashSet};

pub const SUPERSEDES: &str = "supersedes";
pub const SUPERSEDED: &str = "superseded";
pub const EXPIRED: &str = "expired";
pub const HANDOFF: &str = "handoff";
pub const DECISION_STATUS: &str = "decision_status";
pub const UNTIL: &str = "until:";
pub const TRACK_KEY: &str = "track";
/// Past this age a live handoff that is not the newest leaves the digest unless pinned.
pub const HANDOFF_DIGEST_DAYS: i64 = 7;
/// Doctor keeps naming an expired note for this long after it expired.
pub const RECENTLY_DAYS: i64 = 14;
const REPORT_MAX: usize = 3;

pub fn decision_superseded(n: &Note) -> bool {
    n.kind == "decision" && n.field(DECISION_STATUS).and_then(yaml_scalar_string).is_some_and(|s| s.trim().eq_ignore_ascii_case(SUPERSEDED))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expiry {
    /// A date holds through that whole day (UTC), a time until that moment.
    At(DateTime<Utc>),
    /// Until that note is done, shipped, rejected, archived or gone.
    Until(String),
    Invalid(String),
}

fn parse_expiry(raw: &str) -> Expiry {
    let r = raw.trim();
    if let Some(id) = r.strip_prefix(UNTIL) {
        let id = id.trim().to_ascii_lowercase();
        return if crate::id::is_valid_id(&id) { Expiry::Until(id) } else { Expiry::Invalid(r.to_string()) };
    }
    if let Ok(d) = NaiveDate::parse_from_str(r, "%Y-%m-%d")
        && let Some(t) = (d + Duration::days(1)).and_hms_opt(0, 0, 0)
    {
        return Expiry::At(t.and_utc());
    }
    match DateTime::parse_from_rfc3339(r) {
        Ok(t) => Expiry::At(t.with_timezone(&Utc)),
        Err(_) => Expiry::Invalid(r.to_string()),
    }
}

pub fn expiry(n: &Note) -> Option<Expiry> {
    let raw = n.field(EXPIRES_KEY).and_then(yaml_scalar_string)?;
    (!raw.trim().is_empty()).then(|| parse_expiry(&raw))
}

/// What a writer may pass as `expires`: a date, an RFC3339 time, `3d` or `12h` from now, or `until:<id>`.
pub fn normalize_expires(v: &Vault, raw: &str) -> Result<String> {
    let r = raw.trim();
    if let Some(id) = r.strip_prefix(UNTIL).or_else(|| r.strip_prefix("until ")) {
        return Ok(format!("{UNTIL}{}", v.resolve_id(id.trim())?));
    }
    let rel = |suffix: char, unit: fn(i64) -> Duration| r.strip_suffix(suffix).and_then(|x| x.trim().parse::<i64>().ok()).filter(|x| *x > 0).map(unit);
    if let Some(d) = rel('d', Duration::days).or_else(|| rel('h', Duration::hours)) {
        return Ok(fmt_time(&(now() + d)));
    }
    match parse_expiry(r) {
        Expiry::At(_) => Ok(r.to_string()),
        _ => bail!("expires '{r}' must be a date (the last day it holds), an RFC3339 time, 3d or 12h from now, or until:<id>"),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Expired {
    /// When it stopped holding, unknown when the note it waited for is gone.
    pub when: Option<DateTime<Utc>>,
    pub why: String,
}

pub fn expired(n: &Note, by_id: &HashMap<&str, &Note>) -> Option<Expired> {
    let raw = n.field(EXPIRES_KEY).and_then(yaml_scalar_string).unwrap_or_default();
    match expiry(n)? {
        Expiry::At(t) => (now() >= t).then(|| Expired { when: Some(t), why: format!("expired, expires {}", raw.trim()) }),
        Expiry::Until(id) => match by_id.get(id.as_str()) {
            None => Some(Expired { when: None, why: format!("expired, {id} it waited for is gone") }),
            Some(t) if t.is_archived() => Some(Expired { when: Some(t.updated), why: format!("expired, {id} is archived") }),
            Some(t) if is_item(t) && is_closed(&state(t)) => Some(Expired { when: Some(t.updated), why: format!("expired, {id} is {}", state(t)) }),
            Some(_) => None,
        },
        Expiry::Invalid(_) => None,
    }
}

/// Which notes of a vault no longer hold. Built from every note, so links from other projects count too.
#[derive(Debug, Default, Clone)]
pub struct Lapsed {
    superseded: HashSet<String>,
    expired: HashMap<String, Expired>,
}

impl Lapsed {
    pub fn of(all: &[Note]) -> Lapsed {
        let by_id: HashMap<&str, &Note> = all.iter().map(|n| (n.id.as_str(), n)).collect();
        let mut out = Lapsed::default();
        for n in all {
            if n.has_tag(SUPERSEDED) || decision_superseded(n) {
                out.superseded.insert(n.id.clone());
            }
            if let Some(e) = expired(n, &by_id) {
                out.expired.insert(n.id.clone(), e);
            }
            if n.is_archived() {
                continue;
            }
            for l in n.links.iter().filter(|l| l.rel == SUPERSEDES && l.to != n.id) {
                out.superseded.insert(l.to.clone());
            }
        }
        out
    }

    pub fn is_superseded(&self, n: &Note) -> bool {
        self.superseded.contains(&n.id)
    }

    pub fn expired(&self, n: &Note) -> Option<&Expired> {
        self.expired.get(&n.id)
    }

    /// Why the note no longer holds, None while it does.
    pub fn label(&self, n: &Note) -> Option<&'static str> {
        if self.expired.contains_key(&n.id) {
            Some(EXPIRED)
        } else if self.is_superseded(n) {
            Some(SUPERSEDED)
        } else {
            None
        }
    }

    pub fn hides(&self, n: &Note) -> bool {
        self.label(n).is_some()
    }
}

/// The live handoffs of a project (None is global), newest created first.
/// Created, not updated, so tagging or appending to an old handoff never makes it current again.
pub fn live_handoffs<'a>(notes: &'a [Note], project: Option<&str>, lapsed: &Lapsed) -> Vec<&'a Note> {
    let mut live: Vec<&Note> = notes.iter().filter(|n| n.kind == HANDOFF && n.project.as_deref() == project && !n.is_archived() && !lapsed.hides(n)).collect();
    live.sort_by(|a, b| b.created.cmp(&a.created).then(b.id.cmp(&a.id)));
    live
}

pub fn track(n: &Note) -> Option<String> {
    n.field(TRACK_KEY).and_then(yaml_scalar_string).map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty())
}

/// Whether a new handoff replaces `old` without being asked: the same track, or with no track on either the same session.
/// A handoff with neither a track nor a session stamp belongs to nobody, so it is never replaced by itself.
pub fn same_track(new: &Note, old: &Note) -> bool {
    match (track(new), track(old)) {
        (Some(a), Some(b)) => a == b,
        (None, None) => crate::session::created_by(new).is_some_and(|s| crate::session::created_by(old).as_deref() == Some(s.as_str())),
        _ => false,
    }
}

/// `slug id who track age: title`, the one line form of a handoff.
pub fn handoff_line(n: &Note) -> String {
    let who = crate::session::created_by(n).map(|id| format!(" {}", crate::session::label(&id))).unwrap_or_default();
    let track = track(n).map(|t| format!(" track {t}")).unwrap_or_default();
    format!("{}{who}{track} {}: {}", n.handle(), crate::util::short_age(&n.created), crate::format::fit_title(&n.title, n.slug().as_deref()))
}

/// What `add` tells the writer about a new handoff: what it superseded, or that it left the others alone and which they are.
pub fn handoff_report(v: &Vault, h: &Note) -> String {
    use std::fmt::Write;
    let all = v.load_notes();
    let by_id: HashMap<&str, &Note> = all.iter().map(|n| (n.id.as_str(), n)).collect();
    let replaced: Vec<String> = h.links.iter().filter(|l| l.rel == SUPERSEDES).map(|l| by_id.get(l.to.as_str()).map_or_else(|| l.to.clone(), |n| handoff_line(n))).collect();
    if !replaced.is_empty() {
        return replaced.iter().map(|r| format!("superseded {r}")).collect::<Vec<_>>().join("\n");
    }
    let lapsed = Lapsed::of(&all);
    let live = live_handoffs(&all, h.project.as_deref(), &lapsed);
    let others: Vec<&&Note> = live.iter().filter(|n| n.id != h.id).collect();
    if others.is_empty() {
        return "superseded nothing, no other live handoff".into();
    }
    let mut s = "superseded nothing, other live handoffs (--supersedes <id> replaces one):".to_string();
    for n in others.iter().take(REPORT_MAX) {
        let _ = write!(s, "\n  {}", handoff_line(n));
    }
    if others.len() > REPORT_MAX {
        let _ = write!(s, "\n  +{} more", others.len() - REPORT_MAX);
    }
    s
}

/// An expired note that still reads as current: pinned, or a decision not marked superseded.
pub fn needs_mark(n: &Note) -> bool {
    !n.is_archived() && (n.pinned || (n.kind == "decision" && !decision_superseded(n)))
}

/// Unpins it and marks a decision superseded. `updated` stays, nobody edited the note.
pub fn mark(n: &mut Note) {
    n.pinned = false;
    n.clear_kept("pinned");
    if n.kind == "decision" {
        n.set_field(DECISION_STATUS, Value::String(SUPERSEDED.into()));
    }
}

/// Active notes that wait for `id` with `expires: until:<id>` and still read as current.
pub fn waiting_for(all: &[Note], id: &str) -> Vec<Note> {
    all.iter().filter(|n| needs_mark(n) && expiry(n) == Some(Expiry::Until(id.to_string()))).cloned().collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct Due {
    pub id: String,
    pub title: String,
    pub why: String,
}

/// Expired notes `sweep` would mark.
pub fn sweep_plan(v: &Vault) -> Vec<Due> {
    let all = v.load_notes();
    let lapsed = Lapsed::of(&all);
    all.iter()
        .filter(|n| needs_mark(n))
        .filter_map(|n| lapsed.expired(n).map(|e| Due { id: n.id.clone(), title: n.title.clone(), why: e.why.clone() }))
        .collect()
}

/// Marks every expired note that still reads as current, in one commit.
pub fn sweep(v: &Vault) -> Result<Vec<Due>> {
    let _g = v.lock()?;
    let due = sweep_plan(v);
    let mut touched = vec![];
    for d in &due {
        let mut n = v.get(&d.id)?;
        mark(&mut n);
        touched.push(v.write_note(&mut n)?);
    }
    if !touched.is_empty() {
        v.commit(&touched, &format!("breadcrumb: expire {} notes", touched.len()))?;
    }
    Ok(due)
}

pub fn sweep_text(due: &[Due], applied: bool) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    if due.is_empty() {
        return s;
    }
    for d in due {
        let _ = writeln!(s, "{}: {}, unpin it and mark a decision superseded", d.id, d.why);
    }
    if applied {
        let _ = writeln!(s, "marked {} expired notes", due.len());
    } else {
        let _ = writeln!(s, "dry run: {} expired notes to mark, apply with --yes", due.len());
    }
    s
}
