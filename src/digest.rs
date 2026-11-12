//! The digest agents read at session start: pins as one line each, the live handoff, prefs, backlog and recent notes,
//! composed to fit one budget so nothing important is cut off at the end.

use crate::format::{row, short_title};
use crate::kind::ENV;
use crate::lifecycle::{self, HANDOFF, Lapsed, SUPERSEDES};
use crate::note::{Note, yaml_display, yaml_scalar_string};
use crate::ops::{Filter, Scope, SortKey, filter_notes, is_pref, prefs_line, sort_notes};
use crate::util::{one_line, truncate_chars};
use crate::vault::Vault;
use anyhow::Result;
use std::collections::HashMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};

/// Chars the whole digest may take. The SessionStart hook injects it as is.
pub const DIGEST_BUDGET: usize = 6000;
pub const SUMMARY_KEY: &str = "summary";
pub const SUMMARY_MAX: usize = 120;
/// The live handoff gets what the other sections leave, within these bounds.
const HANDOFF_MIN: usize = 800;
const HANDOFF_MAX: usize = 3500;
const RECENT_RESERVE: usize = 800;
/// Prefs from before `said` existed are never marked. Most of them quote the user in their body, in forms no check can
/// tell from a paraphrase, so a mark on them would be a false claim. Bump this to the day the feature lands.
const SAID_SINCE: &str = "2026-11-01";
const OTHER_HANDOFFS_MAX: usize = 3;
const PINS_HEAD: &str = "Pinned (get an id for its body):\n";

/// How long the digest waits for git to count commits since a handoff, a slower answer shows no count.
const GIT_LIMIT: std::time::Duration = std::time::Duration::from_millis(500);

#[derive(Clone, Debug)]
pub struct ContextOpts {
    pub recent: usize,
    pub kinds: bool,
    pub budget: usize,
    /// Where the session runs, it picks the project checkout commits are counted on. None is the process's cwd.
    pub cwd: Option<PathBuf>,
}

impl Default for ContextOpts {
    fn default() -> Self {
        ContextOpts { recent: 15, kinds: false, budget: DIGEST_BUDGET, cwd: None }
    }
}

const ABBREVIATIONS: [&str; 7] = ["e.g", "i.e", "etc", "vs", "cf", "approx", "incl"];

fn first_sentence(s: &str) -> &str {
    for (i, c) in s.char_indices() {
        if !matches!(c, '.' | '!' | '?') || !s[i + 1..].starts_with(' ') {
            continue;
        }
        let word = s[..i].rsplit(' ').next().unwrap_or("").to_ascii_lowercase();
        if word.chars().count() > 1 && !ABBREVIATIONS.contains(&word.as_str()) {
            return &s[..i + c.len_utf8()];
        }
    }
    s
}

/// The `summary` field, else the title's first sentence, on one line and at most SUMMARY_MAX chars.
pub fn summary(n: &Note) -> String {
    let own = n.field(SUMMARY_KEY).and_then(yaml_scalar_string).map(|s| one_line(&s)).filter(|s| !s.is_empty());
    let s = own.unwrap_or_else(|| first_sentence(&one_line(&n.title)).to_string());
    truncate_chars(&s, SUMMARY_MAX)
}

fn is_open_todo(n: &Note) -> bool {
    n.kind == "todo" && !matches!(n.field("done"), Some(serde_yaml::Value::Bool(true)))
}

/// Standing rules come before other pins.
fn is_rule(n: &Note) -> bool {
    n.kind == "decision" || is_pref(n)
}

fn inferred_mark(n: &Note) -> &'static str {
    if crate::ops::lacks_quote(n) && crate::util::fmt_date(&n.created).as_str() >= SAID_SINCE { " (inferred)" } else { "" }
}

fn scope_suffix(n: &Note, project: Option<&str>) -> String {
    if n.project.as_deref() != project { format!(" ({})", n.scope_label()) } else { String::new() }
}

/// Decisions and preferences first, newest first within each group. Pins past `max` chars are named by id only.
fn pins_section(notes: &[Note], project: Option<&str>, stale_days: u64, max: usize) -> String {
    let mut pins: Vec<&Note> = notes.iter().filter(|n| n.pinned && n.kind != HANDOFF).collect();
    if pins.is_empty() {
        return String::new();
    }
    pins.sort_by(|a, b| is_rule(b).cmp(&is_rule(a)).then(b.updated.cmp(&a.updated)).then(a.id.cmp(&b.id)));
    let mut s = PINS_HEAD.to_string();
    let mut rest: Vec<&str> = vec![];
    for n in &pins {
        let line = format!("{} {}{} {}{}{}\n", n.id, n.kind, inferred_mark(n), summary(n), scope_suffix(n, project), crate::recall::stale_mark(n, stale_days));
        if rest.is_empty() && s.chars().count() + line.chars().count() <= max {
            s.push_str(&line);
        } else {
            rest.push(&n.id);
        }
    }
    if !rest.is_empty() {
        let _ = writeln!(s, "+{} more pinned: {}", rest.len(), rest.join(" "));
    }
    s
}

/// Text lines without blank lines, headings as `Heading:`, cut to `max` chars at a sentence end where one is close.
fn compact_text(text: &str, id: &str, max: usize) -> String {
    let more = format!(" ... (get {id} for more)");
    let mut out = String::new();
    for l in text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()) {
        let l = match l.trim_start().strip_prefix('#') {
            Some(h) => format!("{}:", h.trim_start_matches('#').trim()),
            None => l.to_string(),
        };
        let used = out.chars().count();
        if used + l.chars().count() < max {
            out.push_str(&l);
            out.push('\n');
            continue;
        }
        let room = max.saturating_sub(used + more.chars().count());
        if room >= 40 {
            out.push_str(&cut_at_sentence(&l, room));
        }
        out = out.trim_end().to_string();
        out.push_str(&more);
        out.push('\n');
        break;
    }
    out
}

/// The first `room` chars of a line, ending after the last full sentence in them or else at a word.
fn cut_at_sentence(l: &str, room: usize) -> String {
    let head: String = l.chars().take(room).collect();
    let sentence = head.char_indices().filter(|&(i, c)| matches!(c, '.' | '!' | '?') && head[i + 1..].starts_with(' ')).map(|(i, _)| i + 1).next_back();
    let cut = match sentence {
        Some(i) if i >= head.len() / 2 => i,
        _ => head.rfind(' ').filter(|&i| i >= head.len() / 2).unwrap_or(head.len()),
    };
    head[..cut].trim_end().to_string()
}

/// A line append writes above its paragraph, `2026-11-07 neocad-cb 8e60d517:`.
fn is_stamp(l: &str) -> bool {
    let l = l.trim();
    l.len() >= 11 && l.len() <= 90 && l.ends_with(':') && l.is_char_boundary(10) && chrono::NaiveDate::parse_from_str(&l[..10], "%Y-%m-%d").is_ok() && (l.len() == 11 || l[10..].starts_with(' '))
}

/// The handoff's text for the digest: the writer's summary, then appended updates newest first, then what it first said,
/// so a cut drops the oldest state and not the newest.
fn handoff_text(h: &Note) -> String {
    let mut blocks: Vec<String> = vec![String::new()];
    for l in h.body.lines() {
        if is_stamp(l) {
            blocks.push(String::new());
        }
        let b = blocks.last_mut().expect("starts with one");
        b.push_str(l);
        b.push('\n');
    }
    let first = blocks.remove(0);
    let mut out = String::new();
    if let Some(s) = h.field(SUMMARY_KEY).and_then(yaml_scalar_string).map(|s| one_line(&s)).filter(|s| !s.is_empty()) {
        let _ = writeln!(out, "Summary: {s}");
    }
    for b in blocks.iter().rev() {
        out.push_str(b);
    }
    if !blocks.is_empty() && !first.trim().is_empty() {
        out.push_str("First written:
");
    }
    out.push_str(&first);
    out
}

fn handoff_section(h: &Note, commits: &str, stale_days: u64, max: usize) -> String {
    let prev = h.links.iter().find(|l| l.rel == SUPERSEDES).map(|l| format!(", previous {}", l.to)).unwrap_or_default();
    let quiet = crate::util::now() - h.updated;
    let fresh = if quiet > chrono::Duration::hours(lifecycle::HANDOFF_FRESH_HOURS) {
        format!(", last written {} ago{commits}, check it still holds before resuming", crate::util::short_age(&h.updated))
    } else {
        commits.to_string()
    };
    let head = format!(
        "Latest handoff {} ({}{}{prev}{fresh}): {}{}
",
        h.handle(),
        crate::util::fmt_date(&h.created),
        lifecycle::handoff_who(h),
        short_title(&h.title),
        crate::recall::stale_mark(h, stale_days)
    );
    let body = compact_text(&handoff_text(h), &h.id, max.saturating_sub(head.chars().count()));
    format!("{head}{body}")
}

/// The live handoffs beside the latest that the digest names. Old ones leave unless pinned, or legacy notes would pile up.
fn shown_others<'a>(others: &[&'a Note]) -> Vec<&'a Note> {
    let cutoff = crate::util::now() - chrono::Duration::days(lifecycle::HANDOFF_DIGEST_DAYS);
    others.iter().copied().filter(|n| n.pinned || n.created >= cutoff).collect()
}

/// One line each, with the commits since it was written and why it does not lead.
fn other_handoffs(shown: &[&Note], commits: &dyn Fn(&Note) -> String, mark: &dyn Fn(&Note) -> Option<String>) -> String {
    if shown.is_empty() {
        return String::new();
    }
    let mut s = "Other live handoffs:
".to_string();
    for n in shown.iter().take(OTHER_HANDOFFS_MAX) {
        s.push_str(&lifecycle::handoff_line_with(n, &commits(n)));
        if let Some(m) = mark(n) {
            let _ = write!(s, " [{m}]");
        }
        s.push('\n');
    }
    if shown.len() > OTHER_HANDOFFS_MAX {
        let _ = writeln!(s, "+{} more", shown.len() - OTHER_HANDOFFS_MAX);
    }
    s
}

/// The project's checkout on this machine: its path holding `cwd`, else the first path inside a git checkout.
fn checkout(p: &crate::project::Project, cwd: Option<&Path>) -> Option<PathBuf> {
    use crate::project::{normalize_path, path_has_prefix};
    let in_git = |x: &&String| Path::new(x.as_str()).is_dir() && Path::new(x.as_str()).ancestors().any(|d| d.join(".git").exists());
    let here = cwd.map(|c| normalize_path(&c.to_string_lossy()));
    let holding = here.and_then(|c| p.paths.iter().filter(|x| path_has_prefix(&c, &normalize_path(x))).max_by_key(|x| x.len())).filter(in_git);
    holding.or_else(|| p.paths.iter().find(in_git)).map(PathBuf::from)
}

/// Commits on the checkout since each handoff was last written, one git call for all of them. Empty when unknown.
fn commits_since(dir: Option<PathBuf>, hs: &[&Note]) -> HashMap<String, usize> {
    let (Some(dir), Some(oldest)) = (dir, hs.iter().map(|h| h.updated).min()) else { return HashMap::new() };
    let Some(times) = crate::git::commit_times(&dir, oldest, &[], GIT_LIMIT) else { return HashMap::new() };
    hs.iter().map(|h| (h.id.clone(), times.iter().filter(|&&t| t > h.updated.timestamp()).count())).collect()
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// Compact digest agents read at session start.
pub fn context(v: &Vault, project: Option<&str>, limit: usize) -> Result<String> {
    context_with(v, project, ContextOpts { recent: limit, ..Default::default() })
}

pub fn context_with(v: &Vault, project: Option<&str>, opts: ContextOpts) -> Result<String> {
    let v = &crate::config::scoped(v, project);
    let mut head = String::new();
    let proj = match project {
        Some(slug) => v.load_project(slug)?,
        None => None,
    };
    match (&proj, project) {
        (Some(p), _) => {
            write!(head, "breadcrumb context: {}", p.slug)?;
            if !p.description.trim().is_empty() {
                write!(head, ", {}", one_line(&p.description))?;
            }
            head.push('\n');
        }
        (None, Some(slug)) => writeln!(head, "breadcrumb context: {slug} (not registered yet)")?,
        (None, None) => {
            writeln!(head, "breadcrumb context: global (no project for this directory)")?;
            let slugs: Vec<String> = v.load_projects().into_iter().map(|p| p.slug).collect();
            if !slugs.is_empty() {
                writeln!(head, "Known projects: {}, bind this directory with the project tool", slugs.join(", "))?;
            }
        }
    }
    if let Some(slug) = project {
        let wfs = crate::workflow::list(v, &Scope::Project { slug: slug.to_string(), with_global: false });
        if !wfs.is_empty() {
            let names: Vec<String> = wfs.iter().map(|w| w.summary()).collect();
            writeln!(head, "Workflows: {}", names.join(", "))?;
        }
    }
    let scope = match project {
        Some(slug) => Scope::Project { slug: slug.to_string(), with_global: true },
        None => Scope::Global,
    };
    let all = v.load_notes();
    let lapsed = Lapsed::of(&all);
    let mut notes: Vec<Note> = filter_notes(all.clone(), &scope, &Filter::default())?.into_iter().filter(|n| !lapsed.hides(n) && !crate::recall::scope_hides(n, project)).collect();
    sort_notes(&mut notes, SortKey::Updated);
    if notes.is_empty() {
        writeln!(head, "No notes yet.")?;
    }
    let machine = crate::util::machine_name();
    let env = notes.iter().filter(|n| n.kind == ENV && crate::ops::on_machine(n, &machine)).count();
    notes.retain(|n| n.kind != ENV);
    let days = v.config.stale_days;
    let pins = pins_section(&notes, project, days, opts.budget / 2);
    // An unpinned global note without a scope is about no project in particular, a project digest only counts it.
    let foreign = |n: &Note| project.is_some() && n.project.is_none() && !n.pinned && crate::recall::scope_of(n).is_empty();
    let (prefs, global_prefs): (Vec<&Note>, Vec<&Note>) = notes.iter().filter(|n| !n.pinned && is_pref(n)).partition(|n| !foreign(n));
    let mut prefs_text = if prefs.is_empty() && global_prefs.is_empty() { String::new() } else { format!("{}\n", prefs_line(&prefs, global_prefs.len())) };
    if env > 0 {
        writeln!(prefs_text, "Machine facts: {env} for {machine}, `breadcrumb env` or list kind env")?;
    }
    if let Some(l) = crate::lease::digest_line(&v.root) {
        writeln!(prefs_text, "{l}")?;
    }
    if let Some(l) = crate::land::digest_lines(v, project) {
        prefs_text.push_str(&l);
    }
    let mut todos = String::new();
    for n in notes.iter().filter(|n| !n.pinned && is_open_todo(n)) {
        if todos.is_empty() {
            todos.push_str("Todos:\n");
        }
        let prio = n.field("priority").map(yaml_display).map(|p| format!(" {p}")).unwrap_or_default();
        writeln!(todos, "{} {}{prio}{}", n.handle(), crate::format::fit_title(&n.title, n.slug().as_deref()), scope_suffix(n, project))?;
    }
    let mut backlog = crate::backlog::context_section(&notes, &all);
    if crate::config::agent_review(v, project) {
        let waiting = notes.iter().filter(|n| crate::backlog::is_item(n) && crate::backlog::state(n) == "review").count();
        if waiting > 0 {
            writeln!(backlog, "Review: {waiting} waiting for an agent check (review_by = agent), `breadcrumb review` lists the steps, check only what you did not build")?;
        }
    }
    for l in crate::judge::digest_lines(v, project, &all) {
        writeln!(prefs_text, "{l}")?;
    }

    let (rest, global_rest): (Vec<&Note>, Vec<&Note>) = notes
        .iter()
        .filter(|n| !n.pinned && !is_pref(n) && !is_open_todo(n) && !crate::backlog::in_digest(n) && n.kind != HANDOFF)
        .partition(|n| !foreign(n));
    let global_more = match global_rest.len() {
        0 => String::new(),
        1 => "Global: 1 more note, breadcrumb list -p global\n".to_string(),
        k => format!("Global: {k} more notes, breadcrumb list -p global\n"),
    };
    // Prefs used to sit in Recent, so they still count against it and the digest does not grow.
    let want = opts.recent.saturating_sub(prefs.len().min(opts.recent / 2));
    let recent_rows: Vec<String> = rest.iter().take(want).map(|n| format!("{}{}\n", row(n, project, false), crate::recall::stale_mark(n, days))).collect();

    // A handoff a newer one covers, or whose items all landed, has nothing left to resume, so the newest other one leads.
    let mut live = lifecycle::live_handoffs(&notes, project, &lapsed);
    let marks: HashMap<String, String> = live.iter().filter_map(|h| lifecycle::handoff_mark(h, &live, &all).map(|m| (h.id.clone(), m))).collect();
    if let Some(i) = live.iter().position(|h| !marks.contains_key(&h.id)) {
        let lead = live.remove(i);
        live.insert(0, lead);
    }
    let lead_marked = live.first().is_some_and(|h| marks.contains_key(&h.id));
    let shown = shown_others(if lead_marked { &live[..] } else { live.get(1..).unwrap_or_default() });
    let counted: Vec<&Note> = live.first().filter(|_| !lead_marked).into_iter().copied().chain(shown.iter().take(OTHER_HANDOFFS_MAX).copied()).collect();
    let cwd = opts.cwd.clone().or_else(|| std::env::current_dir().ok());
    let counts = if counted.is_empty() { HashMap::new() } else { commits_since(proj.as_ref().and_then(|p| checkout(p, cwd.as_deref())), &counted) };
    let commits = |n: &Note| counts.get(&n.id).map(|&k| lifecycle::commits_since_text(k)).filter(|t| !t.is_empty()).map(|t| format!(", {t}")).unwrap_or_default();
    let others = other_handoffs(&shown, &commits, &|n: &Note| marks.get(&n.id).cloned());
    let fixed = chars(&head) + chars(&pins) + chars(&others) + chars(&prefs_text) + chars(&todos) + chars(&backlog) + chars(&global_more);
    let mut left = opts.budget.saturating_sub(fixed);
    let handoff = match live.first().filter(|_| !lead_marked) {
        Some(h) => {
            let reserve = RECENT_RESERVE.min(recent_rows.iter().map(|r| chars(r)).sum());
            handoff_section(h, &commits(h), days, left.saturating_sub(reserve).clamp(HANDOFF_MIN, HANDOFF_MAX))
        }
        None => String::new(),
    };
    left = left.saturating_sub(chars(&handoff));
    let mut rows = String::new();
    let mut shown = 0;
    for r in &recent_rows {
        if chars(&rows) + chars(r) + 24 > left {
            break;
        }
        rows.push_str(r);
        shown += 1;
    }
    let mut recent = match shown {
        0 => String::new(),
        _ if rest.len() > shown => format!("Recent ({shown} of {}):\n{rows}", rest.len()),
        _ => format!("Recent:\n{rows}"),
    };
    recent.push_str(&global_more);
    let kinds = if opts.kinds { format!("Kinds:\n{}", crate::format::kinds_compact(&v.load_kinds())) } else { String::new() };
    let sections = [("pins", pins), ("handoff", handoff), ("handoffs", others), ("prefs", prefs_text), ("todos", todos), ("backlog", backlog), ("recent", recent), ("kinds", kinds)];
    Ok(sections.into_iter().fold(head, |out, (_, s)| out + &s))
}
