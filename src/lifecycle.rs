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
pub const MERGED: &str = "merged";
pub const HANDOFF: &str = "handoff";
pub const DECISION_STATUS: &str = "decision_status";
pub const UNTIL: &str = "until:";
pub const TRACK_KEY: &str = "track";
pub const WRITTEN_AS_KEY: &str = "written_as";
pub const HELD_BY_KEY: &str = "held_by";
pub const HELD_AS_KEY: &str = "held_as";
pub const HELD_SINCE_KEY: &str = "held_since";
/// A handoff with no write for this long says so in the digest, the state it describes may have moved on.
pub const HANDOFF_FRESH_HOURS: i64 = 12;
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

/// `3d`, `12h` or `30m` as a span.
pub fn relative(r: &str) -> Option<Duration> {
    let rel = |suffix: char, unit: fn(i64) -> Duration| r.strip_suffix(suffix).and_then(|x| x.trim().parse::<i64>().ok()).filter(|x| *x > 0).map(unit);
    rel('d', Duration::days).or_else(|| rel('h', Duration::hours)).or_else(|| rel('m', Duration::minutes))
}

/// What a writer may pass as `expires`: a date, an RFC3339 time, `3d` or `12h` from now, or `until:<id>`.
pub fn normalize_expires(v: &Vault, raw: &str) -> Result<String> {
    let r = raw.trim();
    if let Some(id) = r.strip_prefix(UNTIL).or_else(|| r.strip_prefix("until ")) {
        return Ok(format!("{UNTIL}{}", v.resolve_id(id.trim())?));
    }
    if let Some(d) = relative(r) {
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
    merged: HashSet<String>,
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
            if crate::merge::merged_into(n).is_some() {
                out.merged.insert(n.id.clone());
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
        if self.merged.contains(&n.id) {
            Some(MERGED)
        } else if self.expired.contains_key(&n.id) {
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

/// Keeps the writer's session name as it was, names change across restarts and the digest should not credit the new one.
pub fn stamp_writer_name(n: &mut Note) {
    if let Some(name) = crate::session::current().and_then(|id| crate::session::find(&id)).and_then(|s| s.name) {
        n.set_field(WRITTEN_AS_KEY, Value::String(name));
    }
}

/// The writer as named when writing, with today's name beside it when that changed: `neocad-91 (now neocad-50)`.
pub fn writer(n: &Note) -> Option<String> {
    let id = crate::session::created_by(n);
    let then = n.field(WRITTEN_AS_KEY).and_then(yaml_scalar_string).filter(|s| !s.trim().is_empty());
    let now_name = id.as_deref().and_then(crate::session::find).and_then(|s| s.name);
    match (then, now_name) {
        (Some(t), Some(c)) if t != c => Some(format!("{t} (now {c})")),
        (Some(t), _) => Some(t),
        (None, _) => id.map(|id| crate::session::label(&id)),
    }
}

/// Who holds the handoff's work now: the session that took it with `claim`, by its current name, `(gone)` once it ended.
pub fn holder(n: &Note) -> Option<String> {
    let who = n.field(HELD_BY_KEY).and_then(yaml_scalar_string).filter(|s| !s.trim().is_empty())?;
    let then = n.field(HELD_AS_KEY).and_then(yaml_scalar_string).filter(|s| !s.trim().is_empty());
    Some(match crate::session::find(&who) {
        Some(s) => s.name.unwrap_or_else(|| crate::session::short(&s.id)),
        None if crate::session::known_dead(&who) => format!("{} (gone)", then.unwrap_or_else(|| crate::session::short(&who))),
        None => then.unwrap_or(who),
    })
}

/// `track main-worker held by neocad-cb` or `held by ...`, empty when nobody took it and it has no track.
fn holding(n: &Note) -> String {
    match (track(n), holder(n)) {
        (Some(t), Some(h)) => format!(" track {t} held by {h}"),
        (Some(t), None) => format!(" track {t} not taken"),
        (None, Some(h)) => format!(" held by {h}"),
        (None, None) => String::new(),
    }
}

/// The words of a note's title and body that can be an id or a slug.
fn name_words(n: &Note) -> Vec<String> {
    let text = format!("{}\n{}", n.title, n.body).to_ascii_lowercase();
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')).map(|w| w.trim_matches('-').to_string()).filter(|w| !w.is_empty()).collect()
}

/// Backlog items a handoff names by id or slug in its title or body.
pub fn named_items<'a>(h: &Note, all: &'a [Note]) -> Vec<&'a Note> {
    let by_id: HashMap<&str, &Note> = all.iter().map(|n| (n.id.as_str(), n)).collect();
    let by_slug: HashMap<String, &Note> = all.iter().filter_map(|n| n.slug().map(|s| (s, n))).collect();
    let mut out: Vec<&Note> = vec![];
    for word in name_words(h).iter().map(String::as_str) {
        let hit = by_id.get(word).copied().or_else(|| word.contains('-').then(|| by_slug.get(word).copied()).flatten());
        if let Some(n) = hit.filter(|n| is_item(n) && n.id != h.id)
            && !out.iter().any(|o| o.id == n.id)
        {
            out.push(n);
        }
    }
    out
}

/// How many items the handoff names when every one of them landed (review, done, shipped, rejected or archived), so the
/// work it hands over is finished and it no longer leads the digest.
pub fn settled(h: &Note, all: &[Note]) -> Option<usize> {
    let named = named_items(h, all);
    let landed = |n: &&Note| n.is_archived() || matches!(state(n).as_str(), "review" | "done" | "shipped" | "rejected");
    (!named.is_empty() && named.iter().all(landed)).then_some(named.len())
}

/// The newest live handoff created after `h` that names it by id or slug, so the work `h` handed on moved there.
pub fn covered_by<'a>(h: &Note, live: &[&'a Note]) -> Option<&'a Note> {
    let slug = h.slug();
    live.iter().copied().filter(|n| (&n.created, &n.id) > (&h.created, &h.id)).find(|n| {
        name_words(n).iter().any(|w| *w == h.id || (w.contains('-') && slug.as_deref() == Some(w.as_str())))
    })
}

/// Why a live handoff no longer leads, covered by a newer one or settled. Covered wins, it names where to look.
pub fn handoff_mark(h: &Note, live: &[&Note], all: &[Note]) -> Option<String> {
    mark_text(h, covered_by(h, live), all)
}

/// `handoff_mark` of every live handoff by id, each handoff's words split once.
pub fn handoff_marks(live: &[&Note], all: &[Note]) -> HashMap<String, String> {
    let words: Vec<HashSet<String>> = live.iter().map(|n| name_words(n).into_iter().collect()).collect();
    live.iter()
        .filter_map(|h| {
            let slug = h.slug();
            let cover = live.iter().zip(&words).find(|(n, w)| (&n.created, &n.id) > (&h.created, &h.id) && (w.contains(&h.id) || slug.as_ref().is_some_and(|s| w.contains(s))));
            mark_text(h, cover.map(|(n, _)| *n), all).map(|m| (h.id.clone(), m))
        })
        .collect()
}

fn mark_text(h: &Note, cover: Option<&Note>, all: &[Note]) -> Option<String> {
    match cover {
        Some(n) => Some(format!("covered by {}", n.handle())),
        None => settled(h, all).map(|k| format!("settled, the {k} items it names landed")),
    }
}

/// `14 commits since`, nothing for 0.
pub fn commits_since_text(k: usize) -> String {
    match k {
        0 => String::new(),
        1 => "1 commit since".into(),
        k => format!("{k} commits since"),
    }
}

/// `slug id who track age: title`, the one line form of a handoff.
pub fn handoff_line(n: &Note) -> String {
    handoff_line_with(n, "")
}

/// `handoff_line` with `after_age` beside the age, like `, 14 commits since`.
pub fn handoff_line_with(n: &Note, after_age: &str) -> String {
    let who = writer(n).map(|w| format!(" {w}")).unwrap_or_default();
    format!("{}{who}{} {}{after_age}: {}", n.handle(), holding(n), crate::util::short_age(&n.created), crate::format::fit_title(&n.title, n.slug().as_deref()))
}

/// The head line's who part: `, neocad-91 (now neocad-50), track main-worker held by neocad-cb`.
pub fn handoff_who(n: &Note) -> String {
    let who = writer(n).map(|w| format!(", {w}")).unwrap_or_default();
    let hold = holding(n);
    if hold.is_empty() { who } else { format!("{who},{hold}") }
}

/// The live handoff `claim` takes: a handoff id or slug, or `track:<name>` for the newest live handoff on that track.
pub fn find_handoff(v: &Vault, target: &str, project: Option<&str>) -> Result<Option<Note>> {
    if let Some(t) = target.trim().strip_prefix("track:") {
        let t = t.trim().to_lowercase();
        let all = v.load_notes();
        let lapsed = Lapsed::of(&all);
        let live = live_handoffs(&all, project, &lapsed);
        return match live.into_iter().find(|n| track(n).as_deref() == Some(t.as_str())) {
            Some(n) => Ok(Some(n.clone())),
            None => {
                let mut elsewhere: Vec<&str> = all.iter().filter(|n| n.kind == HANDOFF && n.project.as_deref() != project && !n.is_archived() && !lapsed.hides(n) && track(n).as_deref() == Some(t.as_str())).map(|n| n.project.as_deref().unwrap_or("global")).collect();
                elsewhere.sort_unstable();
                elsewhere.dedup();
                let hint = match elsewhere.as_slice() {
                    [] => "pass -p <slug> (project in MCP) for another project's track".to_string(),
                    [p] => format!("{p} has one, pass -p {p} (project in MCP)"),
                    ps => format!("{} have one, pass -p with one of them (project in MCP)", ps.join(", ")),
                };
                bail!("no live handoff on track {t} in {}, {hint}", project.unwrap_or("global"))
            }
        };
    }
    match v.resolve_id(target) {
        Ok(id) => {
            let n = v.get(&id)?;
            Ok((n.kind == HANDOFF).then_some(n))
        }
        Err(_) => Ok(None),
    }
}

/// `claim` and `release` on a handoff or `track:<name>`, None when the target is no handoff and an item claim applies.
pub fn claim_handoff(v: &Vault, target: &str, project: Option<&str>, by: Option<&str>, release: bool, force: bool) -> Result<Option<Note>> {
    match find_handoff(v, target, project)? {
        Some(h) => take_handoff(v, &h.id, by, release, force).map(Some),
        None => Ok(None),
    }
}

/// What claim prints for a handoff.
pub fn take_text(h: &Note, release: bool) -> String {
    let verb = if release { "released" } else { "took" };
    format!("{verb} {}\n{}", handoff_line(h), if release { "nobody holds it now" } else { "the digest names you as its holder, write a new handoff on the track when you hand it on" })
}

/// The session `by` takes over the work a handoff hands on, or with `release` lets it go, only its holder or `force`. Returns the handoff.
pub fn take_handoff(v: &Vault, id: &str, by: Option<&str>, release: bool, force: bool) -> Result<Note> {
    let _g = v.lock()?;
    let mut n = v.get(id)?;
    let named = |s: Option<String>| s.map(|x| x.trim().to_string()).filter(|x| !x.is_empty());
    let who = named(by.map(str::to_string))
        .or_else(|| named(std::env::var(crate::backlog::SESSION_ENV).ok()))
        .or_else(crate::session::current)
        .unwrap_or_else(|| crate::backlog::DEFAULT_ACTOR.to_string());
    if release {
        let held = |k: &str| n.field(k).and_then(yaml_scalar_string).filter(|s| !s.trim().is_empty());
        if let Some(h) = held(HELD_BY_KEY)
            && !force
            && !crate::session::same_holder(&h, &who)
            && !held(HELD_AS_KEY).is_some_and(|a| crate::session::same_holder(&a, &who))
        {
            bail!("{} is held by {}, not {who}, `breadcrumb release {} --force` (MCP force) releases it anyway", n.handle(), holder(&n).unwrap_or(h), n.id);
        }
        for k in [HELD_BY_KEY, HELD_AS_KEY, HELD_SINCE_KEY] {
            n.unset_field(k);
        }
    } else {
        let (who, name) = match crate::session::find(&who) {
            Some(s) => (s.id, s.name),
            None => (who, None),
        };
        n.set_field(HELD_BY_KEY, Value::String(who));
        match name {
            Some(name) => n.set_field(HELD_AS_KEY, Value::String(name)),
            None => {
                n.unset_field(HELD_AS_KEY);
            }
        }
        n.set_field(HELD_SINCE_KEY, Value::String(fmt_time(&now())));
    }
    crate::session::stamp_write(&mut n);
    let p = v.write_note(&mut n)?;
    v.commit(&[p], &format!("breadcrumb: {} {} {}", if release { "release" } else { "take" }, n.id, n.title))?;
    Ok(n)
}

/// What `add` tells the writer about a new handoff: what it superseded, and which other handoffs stay live, each with
/// the command that retires it, in the CLI's or the MCP tool's form.
pub fn handoff_report(v: &Vault, h: &Note, mcp: bool) -> String {
    use std::fmt::Write;
    let all = v.load_notes();
    let by_id: HashMap<&str, &Note> = all.iter().map(|n| (n.id.as_str(), n)).collect();
    let replaced: Vec<String> = h.links.iter().filter(|l| l.rel == SUPERSEDES).map(|l| by_id.get(l.to.as_str()).map_or_else(|| l.to.clone(), |n| handoff_line(n))).collect();
    let lapsed = Lapsed::of(&all);
    let live = live_handoffs(&all, h.project.as_deref(), &lapsed);
    let others: Vec<&&Note> = live.iter().filter(|n| n.id != h.id).collect();
    let mut s = replaced.iter().map(|r| format!("superseded {r}")).collect::<Vec<_>>().join("\n");
    match (s.is_empty(), others.is_empty()) {
        (true, true) => return "superseded nothing, no other live handoff".into(),
        (false, true) => return s,
        (true, false) => s.push_str("superseded nothing, "),
        (false, false) => s.push('\n'),
    }
    s.push_str("other live handoffs, each with the command that retires it if this handoff replaces it:");
    let marks = handoff_marks(&live, &all);
    for n in others.iter().take(REPORT_MAX) {
        let mark = marks.get(&n.id).map(|m| format!(" [{m}]")).unwrap_or_default();
        let retire = if mcp { format!("update {{\"id\": \"{}\", \"supersedes\": [\"{}\"]}}", h.id, n.id) } else { format!("breadcrumb update {} --supersedes {}", h.id, n.id) };
        let _ = write!(s, "\n  {}{mark}\n    {retire}", handoff_line(n));
    }
    let rest: Vec<&Note> = others.iter().skip(REPORT_MAX).map(|n| **n).collect();
    if !rest.is_empty() {
        let retire = if mcp {
            format!("update {{\"id\": \"{}\", \"supersedes\": [{}]}}", h.id, rest.iter().map(|n| format!("\"{}\"", n.id)).collect::<Vec<_>>().join(", "))
        } else {
            format!("breadcrumb update {}{}", h.id, rest.iter().map(|n| format!(" --supersedes {}", n.id)).collect::<String>())
        };
        let _ = write!(s, "\n  +{} more: {}\n    {retire}", rest.len(), rest.iter().map(|n| n.handle()).collect::<Vec<_>>().join(", "));
    }
    s
}

/// `\nsuperseded <line>` for each note `update --supersedes` named, empty for none.
pub fn superseded_text(v: &Vault, targets: &[String]) -> String {
    targets.iter().filter_map(|t| v.resolve_id(t).ok().and_then(|id| v.get(&id).ok())).map(|n| format!("\nsuperseded {}", if n.kind == HANDOFF { handoff_line(&n) } else { n.handle() })).collect()
}

/// A handoff `h` supersedes stops being pinned. Writes them and returns their files for the caller's commit.
pub fn unpin_superseded(v: &Vault, h: &Note, all: Vec<Note>) -> Result<Vec<std::path::PathBuf>> {
    let mut paths = vec![];
    if h.kind != HANDOFF {
        return Ok(paths);
    }
    for mut n in all.into_iter().filter(|n| n.kind == HANDOFF && n.pinned && h.links.iter().any(|l| l.to == n.id && l.rel == SUPERSEDES)) {
        n.pinned = false;
        n.clear_kept("pinned");
        paths.push(v.write_note(&mut n)?);
    }
    Ok(paths)
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
