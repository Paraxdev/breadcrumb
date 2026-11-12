//! Merging a duplicate note into the one that stays, and taking it back.

use crate::backlog::{is_item, state};
use crate::note::{Link, Note, yaml_scalar_string};
use crate::ops::{commit_msg, push_tag, set_paths};
use crate::util::{fmt_date, fmt_time, now, one_line, parse_time, truncate_chars};
use crate::vault::Vault;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::path::PathBuf;

pub const MERGED_INTO: &str = "merged_into";
pub const MERGED_ON: &str = "merged_on";
pub const UNDO_KEY: &str = "merge_undo";
pub const KEYS: [&str; 3] = [MERGED_INTO, MERGED_ON, UNDO_KEY];
const LIKELY_MAX: usize = 10;

pub fn merged_into(n: &Note) -> Option<String> {
    n.field(MERGED_INTO).and_then(yaml_scalar_string).map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty())
}

/// What writing to a merged note would silently lose, since nothing reads it any more.
pub fn refuse_merged(n: &Note) -> Result<()> {
    match merged_into(n) {
        Some(to) => bail!("{} was merged into {to}, write to that note instead, or `breadcrumb merge --undo {}` to restore it", n.id, n.id),
        None => Ok(()),
    }
}

/// The line `get` leads with for a merged note.
pub fn notice(n: &Note, all: &[Note]) -> Option<String> {
    let to = merged_into(n)?;
    let target = match all.iter().find(|o| o.id == to) {
        Some(k) => label(k),
        None => format!("{to} (gone)"),
    };
    Some(format!("merged into {target}, read that note instead, `breadcrumb merge --undo {}` restores this one", n.id))
}

#[derive(Serialize, Deserialize, Default)]
struct Retarget {
    note: String,
    rel: String,
    /// Where the link sat in the note's list, so an undo puts it back there.
    #[serde(default)]
    at: usize,
    /// The merge created a link to the kept note, so an undo removes it again.
    #[serde(default)]
    created: bool,
}

#[derive(Serialize, Deserialize, Default)]
struct Undo {
    /// The exact text the merge added to the kept note.
    text: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    links: Vec<Link>,
    #[serde(default)]
    retargeted: Vec<Retarget>,
    /// The merge pinned the kept note.
    #[serde(default)]
    pinned: bool,
    /// The duplicate was pinned before.
    #[serde(default)]
    unpinned: bool,
    #[serde(default)]
    was_archived: bool,
    #[serde(default)]
    keep_updated: String,
    /// `updated` of the kept note right after the merge, an undo restores `keep_updated` only while it still reads this.
    #[serde(default)]
    merge_updated: String,
}

pub struct Merged {
    pub dup: Note,
    pub keep: Note,
    /// Why the duplicate's text was not added to the kept note, when it was not.
    pub skipped: Option<&'static str>,
}

fn check(dup: &Note, keep: &Note) -> Result<()> {
    if dup.id == keep.id {
        bail!("a note cannot be merged into itself");
    }
    if let Some(to) = merged_into(dup) {
        bail!("{} is already merged into {to}, `breadcrumb merge --undo {}` restores it", dup.id, dup.id);
    }
    if let Some(to) = merged_into(keep) {
        bail!("{} was itself merged into {to}, merge into that note instead", keep.id);
    }
    if keep.is_archived() {
        bail!("{} is archived, unarchive it first, what is merged into it would leave search with it", keep.id);
    }
    if is_item(dup) && !is_item(keep) {
        bail!(
            "{} is a backlog item in state {} and {} is a '{}' note, merging would lose the state, merge it into another backlog item instead",
            dup.id,
            state(dup),
            keep.id,
            keep.kind
        );
    }
    if dup.project.is_some() && dup.project != keep.project {
        bail!("{} belongs to project {} and {} to {}, paths and links only make sense inside one project", dup.id, dup.project.as_deref().unwrap_or("-"), keep.id, keep.project.as_deref().unwrap_or("global"));
    }
    Ok(())
}

/// The dup's own History heading would become the kept note's, so it is demoted one level.
fn carried_body(dup: &Note) -> String {
    dup.body.lines().map(|l| if l.trim().eq_ignore_ascii_case("## history") { "### History" } else { l }).collect::<Vec<_>>().join("\n").trim().to_string()
}

fn squashed(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn skip_reason(carried: &str, keep_body: &str) -> Option<&'static str> {
    let c = squashed(carried);
    if c.is_empty() {
        Some("empty")
    } else if format!(" {} ", squashed(keep_body)).contains(&format!(" {c} ")) {
        Some("already there")
    } else {
        None
    }
}

fn to_value<T: Serialize>(t: &T) -> Result<Value> {
    Ok(serde_yaml::to_value(t)?)
}

/// Appends the duplicate's body to `keep`, moves links, tags and path globs over, and hides the duplicate.
/// Nothing is deleted, the duplicate keeps its file with a record that `unmerge` reads.
pub fn merge(v: &Vault, dup_ref: &str, keep_ref: &str) -> Result<Merged> {
    let _g = v.lock()?;
    let mut dup = v.get(dup_ref)?;
    let mut keep = v.get(keep_ref)?;
    check(&dup, &keep)?;
    let all = v.load_notes();
    let mut undo = Undo { was_archived: dup.is_archived(), keep_updated: fmt_time(&keep.updated), ..Default::default() };

    let what = if is_item(&dup) { format!("{} {}, ", dup.kind, state(&dup)) } else { String::new() };
    let carried = carried_body(&dup);
    let skipped = skip_reason(&carried, &keep.body);
    if skipped.is_none() {
        let text = format!("{}\nMerged from {} \"{}\" ({what}created {})\n\n{carried}", crate::session::append_head(), dup.id, one_line(&dup.title), fmt_date(&dup.created));
        keep.body = if is_item(&keep) {
            crate::backlog::insert_before_history(&keep.body, &text)
        } else if keep.body.trim().is_empty() {
            text.clone()
        } else {
            format!("{}\n\n{text}", keep.body.trim_end())
        };
        undo.text = text;
    }

    for t in &dup.tags {
        if !keep.has_tag(t) {
            push_tag(&mut keep.tags, t);
            undo.tags.push(t.clone());
        }
    }
    let mut globs = keep.path_globs();
    for g in dup.path_globs() {
        if !globs.contains(&g) {
            globs.push(g.clone());
            undo.paths.push(g);
        }
    }
    if !undo.paths.is_empty() {
        set_paths(&mut keep, globs);
    }
    for l in &dup.links {
        if l.to != keep.id && l.to != dup.id && !keep.links.contains(l) {
            keep.links.push(l.clone());
            undo.links.push(l.clone());
        }
    }
    if dup.pinned && !keep.pinned {
        keep.pinned = true;
        undo.pinned = true;
    }

    let mut others: Vec<Note> = vec![];
    for o in all.iter().filter(|o| o.id != dup.id && o.id != keep.id && o.links.iter().any(|l| l.to == dup.id)) {
        others.push(o.clone());
    }
    let keep_id = keep.id.clone();
    for o in others.iter_mut().chain(std::iter::once(&mut keep)) {
        let is_keep = o.id == keep_id;
        let pointing: Vec<(usize, String)> = o.links.iter().enumerate().filter(|(_, l)| l.to == dup.id).map(|(i, l)| (i, l.rel.clone())).collect();
        for (at, rel) in pointing {
            o.links.retain(|l| !(l.to == dup.id && l.rel == rel));
            let there = Link { to: keep_id.clone(), rel: rel.clone() };
            let created = !is_keep && !o.links.contains(&there);
            if created {
                o.links.push(there);
            }
            undo.retargeted.push(Retarget { note: o.id.clone(), rel, at, created });
        }
    }

    keep.mark_verified();
    crate::session::stamp_write(&mut keep);
    keep.updated = now();
    undo.merge_updated = fmt_time(&keep.updated);

    if dup.pinned {
        dup.pinned = false;
        dup.clear_kept("pinned");
        undo.unpinned = true;
    }
    dup.status = "archived".into();
    dup.clear_kept("status");
    dup.set_field(MERGED_INTO, Value::String(keep.id.clone()));
    dup.set_field(MERGED_ON, Value::String(fmt_date(&now())));
    dup.set_field(UNDO_KEY, to_value(&undo)?);

    let mut touched: Vec<PathBuf> = vec![];
    for o in &mut others {
        touched.push(v.write_note(o)?);
    }
    touched.push(v.write_note(&mut keep)?);
    touched.push(v.write_note(&mut dup)?);
    v.commit(&touched, &format!("breadcrumb: merge {} into {} {}", dup.id, keep.id, one_line(&dup.title)))?;
    Ok(Merged { dup, keep, skipped })
}

/// The recorded text where it ends at a line break or the end, so a line someone kept writing on is not cut in two.
fn find_whole(body: &str, text: &str) -> Option<usize> {
    body.match_indices(text).map(|(i, _)| i).find(|i| body[i + text.len()..].chars().next().is_none_or(|c| c == '\n'))
}

pub struct Unmerged {
    pub dup: Note,
    pub keep: Option<Note>,
    /// What an undo could not take back because the note was edited or deleted meanwhile.
    pub left: Vec<String>,
}

/// Restores both notes from the record the merge kept on the duplicate.
pub fn unmerge(v: &Vault, dup_ref: &str) -> Result<Unmerged> {
    let _g = v.lock()?;
    let mut dup = v.get(dup_ref)?;
    let Some(keep_id) = merged_into(&dup) else {
        bail!("{} is not merged into another note", dup.id);
    };
    let undo: Undo = dup.field(UNDO_KEY).cloned().map(serde_yaml::from_value).transpose().context("the merge record on the note is unreadable")?.context("the note has no merge record, restore it with `breadcrumb update --unset merged_into`")?;
    let mut left: Vec<String> = vec![];
    let mut touched: Vec<PathBuf> = vec![];

    let mut keep = v.get(&keep_id).ok();
    let mut others: Vec<Note> = vec![];
    for r in &undo.retargeted {
        if r.note == keep_id || others.iter().any(|o| o.id == r.note) {
            continue;
        }
        match v.get(&r.note) {
            Ok(n) => others.push(n),
            Err(_) => left.push(format!("{} is gone, its link to {} stays as it was", r.note, dup.id)),
        }
    }
    if let Some(k) = keep.as_mut() {
        k.links.retain(|l| !undo.links.contains(l));
    }
    for r in &undo.retargeted {
        let target = if r.note == keep_id { keep.as_mut() } else { others.iter_mut().find(|o| o.id == r.note) };
        let Some(o) = target else { continue };
        if r.created {
            o.links.retain(|l| !(l.to == keep_id && l.rel == r.rel));
        }
        let back = Link { to: dup.id.clone(), rel: r.rel.clone() };
        if !o.links.contains(&back) {
            o.links.insert(r.at.min(o.links.len()), back);
        }
    }
    if let Some(k) = keep.as_mut() {
        match find_whole(&k.body, &undo.text) {
            _ if undo.text.is_empty() => {}
            Some(i) => {
                let (before, after) = (k.body[..i].trim_end(), k.body[i + undo.text.len()..].trim_start());
                k.body = [before, after].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("\n\n");
            }
            None => left.push(format!("the merged text was edited in {keep_id}, it stays there")),
        }
        k.tags.retain(|t| !undo.tags.iter().any(|u| u.eq_ignore_ascii_case(t)));
        let globs: Vec<String> = k.path_globs().into_iter().filter(|g| !undo.paths.contains(g)).collect();
        if !undo.paths.is_empty() {
            set_paths(k, globs);
        }
        if undo.pinned {
            k.pinned = false;
            k.clear_kept("pinned");
        }
        if fmt_time(&k.updated) == undo.merge_updated
            && let Some(t) = parse_time(&undo.keep_updated)
        {
            k.updated = t;
        }
    } else {
        left.push(format!("{keep_id} is gone, only {} was restored", dup.id));
    }

    dup.unset_field(MERGED_INTO);
    dup.unset_field(MERGED_ON);
    dup.unset_field(UNDO_KEY);
    if !undo.was_archived {
        dup.status = "active".into();
        dup.clear_kept("status");
    }
    if undo.unpinned {
        dup.pinned = true;
    }

    for o in &mut others {
        touched.push(v.write_note(o)?);
    }
    if let Some(k) = keep.as_mut() {
        touched.push(v.write_note(k)?);
    }
    touched.push(v.write_note(&mut dup)?);
    v.commit(&touched, &commit_msg("unmerge", &dup))?;
    Ok(Unmerged { dup, keep, left })
}

fn label(n: &Note) -> String {
    format!("{} \"{}\"", n.handle(), crate::format::short_title(&n.title))
}

pub fn merged_text(m: &Merged) -> String {
    let body = m.skipped.map(|why| format!(", body not added ({why})")).unwrap_or_default();
    format!("merged {} into {}{body}, `breadcrumb merge --undo {}` restores it", label(&m.dup), label(&m.keep), m.dup.id)
}

pub fn unmerged_text(u: &Unmerged) -> String {
    let mut s = match &u.keep {
        Some(k) => format!("restored {} and {}", label(&u.dup), label(k)),
        None => format!("restored {}", label(&u.dup)),
    };
    for l in &u.left {
        s.push_str(&format!("\nnote: {l}"));
    }
    s
}

/// Share of body words two notes have in common, below which lookalike titles are taken as separate facts.
const BODY_ALIKE: f64 = 0.3;
/// Bodies shorter than this many words say too little to tell notes apart, the title decides.
const BODY_MIN_WORDS: usize = 12;
/// A body with fewer distinct words than this proves nothing by overlap, only an identical body or "duplicate of" lists the pair.
const BODY_TINY_WORDS: usize = 8;

/// Pairs of active notes that read like one fact, `(dup, keep)`, strongest first.
/// A note that says it is a duplicate of the other always counts. Otherwise the titles look alike (or the path globs
/// match with a shared title word) and the bodies share enough words, or are identical when either has under 8 words. Code maps, workflows, and pairs linked to each
/// other (someone looked and kept both), are left out.
pub fn likely_duplicates(all: &[Note], lapsed: &crate::lifecycle::Lapsed) -> Vec<(usize, usize)> {
    let live: Vec<usize> = (0..all.len())
        .filter(|&i| !matches!(all[i].kind.as_str(), crate::lifecycle::HANDOFF | "map" | "workflow") && !all[i].is_archived() && !lapsed.hides(&all[i]))
        .collect();
    let terms: Vec<Vec<String>> = all.iter().map(|n| crate::ops::title_terms(&n.title)).collect();
    let words: Vec<std::collections::HashSet<String>> = all.iter().map(|n| body_words(&n.body)).collect();
    let mut out: Vec<(f64, usize, usize)> = vec![];
    for (a, &i) in live.iter().enumerate() {
        for &j in &live[a + 1..] {
            let (x, y) = (&all[i], &all[j]);
            if x.project != y.project {
                continue;
            }
            if says_duplicate_of(x, y) {
                out.push((2.0, i, j));
                continue;
            }
            if says_duplicate_of(y, x) {
                out.push((2.0, j, i));
                continue;
            }
            if x.links.iter().any(|l| l.to == y.id) || y.links.iter().any(|l| l.to == x.id) {
                continue;
            }
            let (gx, gy) = (sorted(x.path_globs()), sorted(y.path_globs()));
            let shared_title = terms[i].iter().any(|t| terms[j].contains(t));
            let same_paths = !gx.is_empty() && gx == gy && shared_title;
            let title = one_line(&x.title).to_lowercase();
            let alike = crate::ops::look_alike(x, &terms[i], &title, y, &terms[j]);
            if !(same_paths || alike.is_some()) {
                continue;
            }
            if distinct_words(&x.body).min(distinct_words(&y.body)) < BODY_TINY_WORDS {
                let norm = |b: &str| b.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
                if !x.body.trim().is_empty() && norm(&x.body) == norm(&y.body) {
                    let (dup, keep) = if (x.created, &x.id) > (y.created, &y.id) { (i, j) } else { (j, i) };
                    out.push((1.5, dup, keep));
                }
                continue;
            }
            let exact = alike.is_some_and(|(e, _)| e);
            let body = overlap(&words[i], &words[j]);
            let short = words[i].len().min(words[j].len()) < BODY_MIN_WORDS;
            if exact || short || body >= BODY_ALIKE {
                let (dup, keep) = if (x.created, &x.id) > (y.created, &y.id) { (i, j) } else { (j, i) };
                out.push((if exact { 1.5 } else { body }, dup, keep));
            }
        }
    }
    out.sort_by(|a, b| b.0.total_cmp(&a.0));
    out.into_iter().map(|(_, d, k)| (d, k)).collect()
}

fn distinct_words(body: &str) -> usize {
    body.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect::<std::collections::HashSet<_>>().len()
}

fn body_words(body: &str) -> std::collections::HashSet<String> {
    body.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 4).map(str::to_lowercase).collect()
}

fn overlap(a: &std::collections::HashSet<String>, b: &std::collections::HashSet<String>) -> f64 {
    let union = a.union(b).count();
    if union == 0 { 0.0 } else { a.intersection(b).count() as f64 / union as f64 }
}

/// Whether `x`'s text or resolution says "duplicate of" naming `y` by id or slug.
fn says_duplicate_of(x: &Note, y: &Note) -> bool {
    let resolution = x.field("resolution").and_then(yaml_scalar_string).unwrap_or_default();
    let text = format!("{}\n{}", x.body, resolution).to_lowercase();
    let names: Vec<String> = std::iter::once(y.id.clone()).chain(y.slug()).collect();
    text.match_indices("duplicate of ").any(|(at, m)| {
        let after: String = text[at + m.len()..].chars().take(60).collect();
        names.iter().any(|name| after.contains(name.as_str()))
    })
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

/// Doctor lines for `likely_duplicates`, capped so one messy vault does not drown the report.
pub fn likely_lines(all: &[Note], lapsed: &crate::lifecycle::Lapsed) -> Vec<(usize, String)> {
    let pairs = likely_duplicates(all, lapsed);
    let mut out: Vec<(usize, String)> = pairs
        .iter()
        .take(LIKELY_MAX)
        .map(|&(d, k)| {
            let keep = &all[k];
            (d, format!("may duplicate {} \"{}\", if it is the same fact run `breadcrumb merge {} {}`, if not link them", keep.handle(), truncate_chars(&one_line(&keep.title), 50), all[d].id, keep.id))
        })
        .collect();
    if pairs.len() > LIKELY_MAX
        && let Some(&(d, _)) = pairs.get(LIKELY_MAX)
    {
        out.push((d, format!("and {} more likely duplicate pairs", pairs.len() - LIKELY_MAX)));
    }
    out
}
