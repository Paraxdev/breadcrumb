//! Commits that touched the files under a note's `paths` globs since it was last verified, for get, brief and doctor.

use crate::note::Note;
use crate::util::{fmt_date, truncate_chars};
use crate::vault::Vault;
use chrono::{DateTime, Utc};
use std::path::Path;
use std::time::{Duration, Instant};

/// How long doctor may spend on all notes together before it stops and counts the rest.
pub const DOCTOR_BUDGET: Duration = Duration::from_secs(3);
const SUBJECT_MAX: usize = 60;

pub struct Drift {
    pub count: usize,
    pub sha: String,
    pub subject: String,
    /// `verified 2026-11-01` or `updated 2026-11-01`.
    pub since: String,
}

impl Drift {
    pub fn line(&self) -> String {
        format!("paths changed: {} since {}, newest {} \"{}\"", commits(self.count), self.since, self.sha, truncate_chars(&self.subject, SUBJECT_MAX))
    }

    /// The form brief puts after a note's title.
    pub fn mark(&self) -> String {
        format!("[{} since {}]", commits(self.count), self.since.split(' ').next().unwrap_or("verified"))
    }
}

fn commits(k: usize) -> String {
    if k == 1 { "1 commit".into() } else { format!("{k} commits") }
}

/// Commits after this time count: `verified_at`, else the start (UTC) of the `verified` day, which is all notes from
/// older binaries have, so commits from earlier that day count rather than hiding ones made after a verify later that
/// day, though never before the note was created. A note without `verified` counts from `updated`.
pub fn since(n: &Note) -> (DateTime<Utc>, String) {
    match n.verified() {
        Some(day) => (n.verified_at().unwrap_or(day.max(n.created)), format!("verified {}", fmt_date(&day))),
        None => (n.updated, format!("updated {}", fmt_date(&n.updated))),
    }
}

/// Note globs as git pathspecs with the same meaning as `recall::glob_match`: relative to the checkout, a glob without a
/// slash matches the file name at any depth, a path without `*` or `?` also matches everything below it, ASCII case is
/// ignored and backslashes are slashes.
pub fn pathspecs(globs: &[String]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for g in globs {
        let p = g.trim().replace('\\', "/");
        let p = p.trim_start_matches("./").trim_start_matches('/');
        let mut segs: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
        if segs.is_empty() {
            continue;
        }
        if !p.contains('/') {
            segs.insert(0, "**");
        }
        let joined = segs.join("/");
        let mut specs = vec![format!(":(glob,icase){joined}")];
        if !p.contains(['*', '?']) {
            specs.push(format!(":(glob,icase){joined}/**"));
        }
        for s in specs {
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out
}

/// One git call on the checkout at `dir`, None for a note without globs, 0 commits, or git missing, failing or slower than `limit`.
pub fn check(dir: &Path, n: &Note, limit: Duration) -> Option<Drift> {
    let specs = pathspecs(&n.path_globs());
    if specs.is_empty() {
        return None;
    }
    let (after, label) = since(n);
    let found: Vec<(i64, String, String)> = crate::git::commit_subjects(dir, after, &specs, limit)?.into_iter().filter(|c| c.0 > after.timestamp()).collect();
    let (_, sha, subject) = found.first()?.clone();
    Some(Drift { count: found.len(), sha, subject, since: label })
}

/// The line `get` ends with, from the checkout of the note's project the digest would pick for `cwd`.
pub fn get_line(v: &Vault, n: &Note, cwd: &Path) -> Option<String> {
    if n.path_globs().is_empty() {
        return None;
    }
    let p = v.load_project(n.project.as_deref()?).ok()??;
    let dir = crate::digest::checkout(&p, Some(cwd))?;
    check(&dir, n, crate::digest::GIT_LIMIT).map(|d| d.line())
}

/// Code maps carry their own stale marks, closed items and lapsed notes record the past.
fn checked(n: &Note, lapsed: &crate::lifecycle::Lapsed) -> bool {
    !n.is_archived()
        && n.kind != crate::map::MAP
        && !lapsed.hides(n)
        && !(crate::backlog::is_item(n) && crate::backlog::is_closed(&crate::backlog::state(n)))
        && !n.path_globs().is_empty()
}

/// Doctor's warnings for project `slug`, one per note, until `budget` runs out, then one info line counting the rest.
pub fn doctor_problems(v: &Vault, slug: &str, cwd: &Path, budget: Duration) -> Vec<crate::doctor::Problem> {
    let Some(dir) = v.load_project(slug).ok().flatten().and_then(|p| crate::digest::checkout(&p, Some(cwd))) else { return vec![] };
    if !crate::git::available() {
        return vec![];
    }
    let all = v.load_notes();
    let lapsed = crate::lifecycle::Lapsed::of(&all);
    let notes: Vec<&Note> = all.iter().filter(|n| n.project.as_deref() == Some(slug) && checked(n, &lapsed)).collect();
    let start = Instant::now();
    let mut out = vec![];
    for (i, n) in notes.iter().enumerate() {
        let left = budget.saturating_sub(start.elapsed());
        if left.is_zero() {
            let rest = notes.len() - i;
            let message = format!(
                "paths check stopped after {}s, {rest} note{} with paths not checked, `breadcrumb get <id>` checks one",
                budget.as_secs(),
                if rest == 1 { "" } else { "s" }
            );
            out.push(crate::doctor::Problem { path: v.rel(&v.project_dir(slug)), id: None, severity: "info", message });
            break;
        }
        if let Some(d) = check(&dir, n, left.min(crate::digest::GIT_LIMIT)) {
            let message = format!("{}, reread it, then `breadcrumb verify {}` or update the note", d.line(), n.id);
            out.push(crate::doctor::Problem { path: n.path.as_ref().map(|p| v.rel(p)).unwrap_or_default(), id: Some(n.id.clone()), severity: "warning", message });
        }
    }
    out
}
