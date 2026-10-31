//! `ship <range>`: which items a range of commits landed, read from their commit refs and shipped_in values.

use crate::backlog;
use crate::note::{Note, yaml_scalar_string};
use crate::ops::{Filter, Scope};
use crate::vault::Vault;
use anyhow::Result;
use serde_json::{Value as Json, json};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Commit {
    pub sha: String,
    pub merge: bool,
    pub subject: String,
}

/// Git's own message when it cannot read the range.
pub fn commits(repo: &Path, range: &str) -> std::result::Result<Vec<Commit>, String> {
    let log = crate::git::ok(repo, &["log", "--format=%H%x09%P%x09%s", range, "--"]).map_err(|e| e.to_string())?;
    Ok(log
        .lines()
        .filter_map(|l| {
            let mut p = l.splitn(3, '\t');
            let sha = p.next()?.to_ascii_lowercase();
            let merge = p.next()?.split_whitespace().count() > 1;
            Some(Commit { sha, merge, subject: p.next().unwrap_or("").to_string() })
        })
        .collect())
}

/// `shipped_in` is free text in the vault, like `0450c1a5`, `c3b1c7c2 beta` or `beta 1.0.386 (main 19fcf4af)`, so
/// every hex word in it counts. A `commit:` ref is one value.
fn pointers(n: &Note) -> Vec<String> {
    let refs = n.refs().into_iter().filter_map(|r| r.strip_prefix("commit:").map(str::to_string));
    let shipped = n.field("shipped_in").and_then(yaml_scalar_string).unwrap_or_default();
    let words = shipped.split(|c: char| !c.is_ascii_alphanumeric()).map(str::to_string).collect::<Vec<_>>();
    refs.chain(words).map(|s| s.to_ascii_lowercase()).filter(|s| crate::note::is_commit_hex(s)).collect()
}

pub fn has_commit_pointer(n: &Note) -> bool {
    !pointers(n).is_empty()
}

fn records_landing(n: &Note) -> bool {
    n.refs().iter().any(|r| r.starts_with("commit:")) || n.field("shipped_in").and_then(yaml_scalar_string).is_some_and(|s| !s.trim().is_empty())
}

#[derive(Debug, Default)]
pub struct Report {
    /// Matched items still open, they move on --yes.
    pub open: Vec<Note>,
    pub closed: Vec<Note>,
    pub unmatched_merges: Vec<Commit>,
    /// Done items of the project with nothing that says where they landed, no range can ever match them.
    pub unrecorded: Vec<Note>,
    pub commits: usize,
    pub merges: usize,
}

pub fn report(v: &Vault, project: &str, commits: &[Commit]) -> Result<Report> {
    let scope = Scope::Project { slug: project.to_string(), with_global: false };
    let notes = crate::ops::filter_notes(v.load_notes(), &scope, &Filter::default())?;
    let mut hit: HashSet<&str> = HashSet::new();
    let mut r = Report { commits: commits.len(), merges: commits.iter().filter(|c| c.merge).count(), ..Default::default() };
    for n in notes.iter().filter(|n| backlog::is_item(n)) {
        let found: Vec<&str> = pointers(n).iter().filter_map(|p| commits.iter().find(|c| c.sha.starts_with(p.as_str())).map(|c| c.sha.as_str())).collect();
        if found.is_empty() {
            if backlog::state(n) == "done" && !records_landing(n) {
                r.unrecorded.push(n.clone());
            }
            continue;
        }
        hit.extend(found);
        if matches!(backlog::state(n).as_str(), "shipped" | "rejected") {
            r.closed.push(n.clone());
        } else {
            r.open.push(n.clone());
        }
    }
    let order = |n: &Note| (state_order(&backlog::state(n)), n.created);
    r.open.sort_by_key(order);
    r.closed.sort_by_key(order);
    r.unrecorded.sort_by_key(|n| n.created);
    r.unmatched_merges = commits.iter().filter(|c| c.merge && !hit.contains(c.sha.as_str())).cloned().collect();
    Ok(r)
}

fn state_order(s: &str) -> usize {
    backlog::STATES.iter().position(|x| *x == s).unwrap_or(usize::MAX)
}

pub fn release_note(n: &Note) -> Option<String> {
    n.field(backlog::RELEASE_NOTE_KEY).and_then(yaml_scalar_string).map(|s| crate::util::one_line(&s)).filter(|s| !s.trim().is_empty())
}

/// The state, with what is left for the user when the item never passed its check.
fn state_label(n: &Note) -> String {
    let st = backlog::state(n);
    let open = backlog::review_steps(n).len();
    if st == "review" && open > 0 { format!("{st}, {open} steps unchecked") } else { st }
}

fn item_line(n: &Note) -> String {
    format!("{} [{}] {}", backlog::named_key(n), state_label(n), n.title)
}

fn section(s: &mut String, head: &str, lines: Vec<String>) {
    let _ = writeln!(s, "\n{head} ({}):", lines.len());
    for l in lines {
        let _ = writeln!(s, "  {l}");
    }
}

pub fn text(r: &Report, range: &str, repo: &Path) -> String {
    let mut s = format!("{range} in {}: {} commits, {} merges, {} open items match\n", repo.display(), r.commits, r.merges, r.open.len());
    let (noted, plain): (Vec<&Note>, Vec<&Note>) = r.open.iter().partition(|n| release_note(n).is_some());
    section(&mut s, "Release notes", noted.iter().map(|n| format!("- {} [{}, {}]", release_note(n).unwrap_or_default(), n.id, state_label(n))).collect());
    section(&mut s, "No release note", plain.iter().map(|n| item_line(n)).collect());
    section(&mut s, "Already shipped or rejected, skipped", r.closed.iter().map(item_line).collect());
    section(&mut s, "Merges no item points at", r.unmatched_merges.iter().map(|c| format!("{} {}", &c.sha[..c.sha.len().min(9)], c.subject)).collect());
    s
}

/// Empty when every done item says where it landed.
pub fn unrecorded_text(r: &Report) -> String {
    let mut s = String::new();
    if !r.unrecorded.is_empty() {
        section(&mut s, "Done with no commit ref or shipped_in, no range finds these", r.unrecorded.iter().map(item_line).collect());
    }
    s
}

pub fn json(r: &Report, range: &str, repo: &Path) -> Json {
    let item = |n: &Note| json!({"id": n.id, "slug": n.slug(), "title": n.title, "state": backlog::state(n), "release_note": release_note(n), "unchecked_steps": backlog::review_steps(n)});
    json!({
        "range": range,
        "repo": repo.display().to_string(),
        "commits": r.commits,
        "merges": r.merges,
        "open": r.open.iter().map(item).collect::<Vec<_>>(),
        "skipped": r.closed.iter().map(item).collect::<Vec<_>>(),
        "unmatched_merges": r.unmatched_merges.iter().map(|c| json!({"sha": c.sha, "subject": c.subject})).collect::<Vec<_>>(),
        "unrecorded_done": r.unrecorded.iter().map(item).collect::<Vec<_>>(),
    })
}
