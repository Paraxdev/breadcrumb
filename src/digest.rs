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
use std::fmt::Write;

/// Chars the whole digest may take. The SessionStart hook injects it as is.
pub const DIGEST_BUDGET: usize = 6000;
pub const SUMMARY_KEY: &str = "summary";
pub const SUMMARY_MAX: usize = 120;
/// The live handoff gets what the other sections leave, within these bounds.
const HANDOFF_MIN: usize = 800;
const HANDOFF_MAX: usize = 3500;
const RECENT_RESERVE: usize = 800;
const PINS_HEAD: &str = "Pinned (get an id for its body):\n";

#[derive(Clone, Copy, Debug)]
pub struct ContextOpts {
    pub recent: usize,
    pub kinds: bool,
    pub budget: usize,
}

impl Default for ContextOpts {
    fn default() -> Self {
        ContextOpts { recent: 15, kinds: false, budget: DIGEST_BUDGET }
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
        let line = format!("{} {} {}{}{}\n", n.id, n.kind, summary(n), scope_suffix(n, project), crate::recall::stale_mark(n, stale_days));
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

/// Body lines without blank lines, headings as `Heading:`, cut to `max` chars.
fn compact_body(n: &Note, max: usize) -> String {
    let more = format!("... (get {} for more)", n.id);
    let mut out = String::new();
    for l in n.body.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()) {
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
            out.push_str(l.chars().take(room).collect::<String>().trim_end());
        }
        out = out.trim_end().to_string();
        out.push_str(&more);
        out.push('\n');
        break;
    }
    out
}

fn handoff_section(h: &Note, stale_days: u64, max: usize) -> String {
    let prev = h.links.iter().find(|l| l.rel == SUPERSEDES).map(|l| format!(", previous {}", l.to)).unwrap_or_default();
    let by = crate::session::created_by(h).map(|id| format!(", {}", crate::session::label(&id))).unwrap_or_default();
    let head = format!(
        "Latest handoff {} ({}{by}{prev}): {}{}\n",
        h.id,
        crate::util::fmt_date(&h.created),
        short_title(&h.title),
        crate::recall::stale_mark(h, stale_days)
    );
    let body = compact_body(h, max.saturating_sub(head.chars().count()));
    format!("{head}{body}")
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// Compact digest agents read at session start.
pub fn context(v: &Vault, project: Option<&str>, limit: usize) -> Result<String> {
    context_with(v, project, ContextOpts { recent: limit, ..Default::default() })
}

pub fn context_with(v: &Vault, project: Option<&str>, opts: ContextOpts) -> Result<String> {
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
    let mut notes: Vec<Note> = filter_notes(all.clone(), &scope, &Filter::default())?.into_iter().filter(|n| !lapsed.hides(n)).collect();
    sort_notes(&mut notes, SortKey::Updated);
    if notes.is_empty() {
        writeln!(head, "No notes yet.")?;
    }
    let machine = crate::util::machine_name();
    let env = notes.iter().filter(|n| n.kind == ENV && crate::ops::on_machine(n, &machine)).count();
    notes.retain(|n| n.kind != ENV);
    let days = v.config.stale_days;
    let pins = pins_section(&notes, project, days, opts.budget / 2);
    let prefs: Vec<&Note> = notes.iter().filter(|n| !n.pinned && is_pref(n)).collect();
    let mut prefs_text = if prefs.is_empty() { String::new() } else { format!("{}\n", prefs_line(&prefs)) };
    if env > 0 {
        writeln!(prefs_text, "Machine facts: {env} for {machine}, `breadcrumb env` or list kind env")?;
    }
    let mut todos = String::new();
    for n in notes.iter().filter(|n| !n.pinned && is_open_todo(n)) {
        if todos.is_empty() {
            todos.push_str("Todos:\n");
        }
        let prio = n.field("priority").map(yaml_display).map(|p| format!(" {p}")).unwrap_or_default();
        writeln!(todos, "{} {}{prio}{}", n.id, short_title(&n.title), scope_suffix(n, project))?;
    }
    let backlog = crate::backlog::context_section(&notes, &all);

    let rest: Vec<&Note> = notes
        .iter()
        .filter(|n| !n.pinned && !is_pref(n) && !is_open_todo(n) && !crate::backlog::in_digest(n) && n.kind != HANDOFF)
        .collect();
    // Prefs used to sit in Recent, so they still count against it and the digest does not grow.
    let want = opts.recent.saturating_sub(prefs.len().min(opts.recent / 2));
    let recent_rows: Vec<String> = rest.iter().take(want).map(|n| format!("{}{}\n", row(n, project, false), crate::recall::stale_mark(n, days))).collect();

    let fixed = chars(&head) + chars(&pins) + chars(&prefs_text) + chars(&todos) + chars(&backlog);
    let mut left = opts.budget.saturating_sub(fixed);
    let handoff = match lifecycle::latest_handoff(&notes, project, &lapsed) {
        Some(h) => {
            let reserve = RECENT_RESERVE.min(recent_rows.iter().map(|r| chars(r)).sum());
            handoff_section(h, days, left.saturating_sub(reserve).clamp(HANDOFF_MIN, HANDOFF_MAX))
        }
        None => String::new(),
    };
    left = left.saturating_sub(chars(&handoff));
    let mut recent = String::new();
    let mut shown = 0;
    for r in &recent_rows {
        if chars(&recent) + chars(r) + 24 > left {
            break;
        }
        recent.push_str(r);
        shown += 1;
    }
    let mut out = format!("{head}{pins}{handoff}{prefs_text}{todos}{backlog}");
    if shown > 0 {
        if rest.len() > shown {
            writeln!(out, "Recent ({shown} of {}):", rest.len())?;
        } else {
            writeln!(out, "Recent:")?;
        }
        out.push_str(&recent);
    }
    if opts.kinds {
        writeln!(out, "Kinds:")?;
        out.push_str(&crate::format::kinds_compact(&v.load_kinds()));
    }
    Ok(out)
}
