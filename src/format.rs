use crate::kind::KindDef;
use crate::note::Note;
use crate::project::Project;
use crate::search::SearchResult;
use crate::util::fmt_date;
use std::fmt::Write;

pub fn note_line(n: &Note) -> String {
    let mut flags = String::new();
    if n.pinned {
        flags.push_str(" [pinned]");
    }
    if n.is_archived() {
        flags.push_str(" [archived]");
    }
    format!("{}  {:<12} {:<14} {}{}  ({})", n.handle(), n.kind, n.scope_label(), n.title, flags, fmt_date(&n.updated))
}

pub fn notes_text(notes: &[Note]) -> String {
    if notes.is_empty() {
        return "no notes found\n".into();
    }
    let mut s = String::new();
    for n in notes {
        let _ = writeln!(s, "{}", note_line(n));
    }
    s
}

/// The note exactly as stored on disk: frontmatter plus body.
pub fn note_full(n: &Note) -> String {
    n.to_markdown()
}

/// What an import leaves behind: nothing a reader acts on, `--full` and `--json` keep it. `source` only counts
/// next to a hash, `add --source` sets it by hand too.
fn hide_import_keys(m: &mut serde_yaml::Mapping) {
    use serde_yaml::Value as Y;
    let imported = m.contains_key(Y::String("source_hash".into()));
    for k in ["source_hash", "memory_name", "metadata"].into_iter().chain(imported.then_some("source")) {
        m.remove(Y::String(k.into()));
    }
}

/// The note as `get` shows it by default: as stored, without the import keys.
pub fn note_reading(n: &Note) -> String {
    let mut m = n.frontmatter();
    hide_import_keys(&mut m);
    let yaml = serde_yaml::to_string(&serde_yaml::Value::Mapping(m)).unwrap_or_default();
    let body = n.body.trim_end();
    if body.is_empty() { format!("---\n{yaml}---\n") } else { format!("---\n{yaml}---\n\n{body}\n") }
}

/// The last `lines` of `body`, and the line saying how many were cut when any were.
fn last_lines(body: &str, lines: usize) -> (String, Option<String>) {
    let all: Vec<&str> = body.trim_end().lines().collect();
    let cut = all.len().saturating_sub(lines);
    (all[cut..].join("\n"), (cut > 0).then(|| format!("({cut} earlier body lines cut, read without tail for them)")))
}

/// A copy to print with only the end of its log. Appends land above History, so History stays whole and is not
/// counted, or a tail of a long running item would show state changes and none of the log.
pub fn tailed(n: &Note, lines: usize) -> Note {
    let (log, history) = crate::backlog::split_history(&n.body);
    let (kept, cut) = last_lines(&log, lines);
    let mut out = n.clone();
    out.body = [cut.unwrap_or_default(), kept, history.unwrap_or_default()].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("\n\n");
    out
}

/// The body alone for piping, no History, and the cut line apart so a pipe stays clean.
pub fn body_only(n: &Note, tail: Option<usize>) -> (String, Option<String>) {
    let (log, _) = crate::backlog::split_history(&n.body);
    match tail {
        Some(lines) => last_lines(&log, lines),
        None => (log.trim().to_string(), None),
    }
}

const INBOUND_MAX: usize = 20;

/// The notes that link to this one, `(rel, note)`, superseding ones first. An archived note supersedes nothing,
/// as in `Lapsed`, so it reads as a plain link.
pub fn inbound<'a>(n: &Note, all: &'a [Note]) -> Vec<(String, &'a Note)> {
    let mut out: Vec<(String, &Note)> = vec![];
    for o in all.iter().filter(|o| o.id != n.id) {
        out.extend(o.links.iter().filter(|l| l.to == n.id).map(|l| (l.rel.clone(), o)));
    }
    let live_super = |(rel, o): &(String, &Note)| rel == crate::lifecycle::SUPERSEDES && !o.is_archived();
    out.sort_by_key(|x| (!live_super(x), x.0.clone(), x.1.id.clone()));
    out
}

/// `superseded by ...` and `linked from:` lines, empty when nothing links here.
pub fn inbound_text(n: &Note, all: &[Note]) -> String {
    let links = inbound(n, all);
    let mut s = String::new();
    let (by, rest): (Vec<_>, Vec<_>) = links.iter().partition(|(rel, o)| rel == crate::lifecycle::SUPERSEDES && !o.is_archived());
    for (_, o) in &by {
        let _ = writeln!(s, "\nsuperseded by {} \"{}\"", o.handle(), short_title(&o.title));
    }
    if !rest.is_empty() {
        s.push_str("\nlinked from:\n");
    }
    for (rel, o) in rest.iter().take(INBOUND_MAX) {
        let _ = writeln!(s, "  {rel} {} \"{}\"", o.handle(), short_title(&o.title));
    }
    if rest.len() > INBOUND_MAX {
        let _ = writeln!(s, "  and {} more, --json lists all", rest.len() - INBOUND_MAX);
    }
    s
}

pub fn inbound_json(n: &Note, all: &[Note]) -> serde_json::Value {
    inbound(n, all).iter().map(|(rel, o)| serde_json::json!({"rel": rel, "id": o.id, "slug": o.slug(), "title": o.title})).collect()
}

pub fn search_text(r: &SearchResult, scope_label: &str) -> String {
    let mut s = String::new();
    if r.hits.is_empty() {
        let _ = writeln!(s, "no matches in {scope_label}");
        return s;
    }
    for h in &r.hits {
        let _ = writeln!(s, "{}  {:<12} {:<14} {}{}{}", h.note.handle(), h.note.kind, h.note.scope_label(), state_mark(&h.note), h.note.title, hit_flags(r, h));
        if !h.snippet.is_empty() {
            let _ = writeln!(s, "    {}", h.snippet);
        }
    }
    if r.total > r.hits.len() {
        let _ = writeln!(s, "(showing {} of {} matches, raise the limit or refine the query to see the rest)", r.hits.len(), r.total);
    }
    s
}

pub const TITLE_MAX: usize = 80;

pub fn short_title(t: &str) -> String {
    crate::util::truncate_chars(&crate::util::one_line(t), TITLE_MAX)
}

/// Compact lines keep their width with a slug in front, its words already say what the end of the title would.
pub fn fit_title(t: &str, slug: Option<&str>) -> String {
    let room = TITLE_MAX.saturating_sub(slug.map_or(0, |s| s.len() + 1)).max(40);
    crate::util::truncate_chars(&crate::util::one_line(t), room)
}

/// `[queued] ` for a backlog item, nothing for a note.
fn state_mark(n: &Note) -> String {
    if crate::backlog::is_item(n) { format!("[{}] ", crate::backlog::state(n)) } else { String::new() }
}

/// `id kind title`, plus the project when it is not `current`, plus flags and optionally the date.
pub fn row(n: &Note, current: Option<&str>, date: bool) -> String {
    row_marked(n, "", current, date)
}

fn row_marked(n: &Note, mark: &str, current: Option<&str>, date: bool) -> String {
    let mut s = format!("{} {} {mark}{}", n.handle(), n.kind, fit_title(&n.title, n.slug().as_deref()));
    if let Some(p) = env_path(n) {
        let _ = write!(s, ": {p}");
    }
    if n.project.as_deref() != current {
        let _ = write!(s, " ({})", n.scope_label());
    }
    if n.pinned {
        s.push_str(" [pinned]");
    }
    if n.is_archived() {
        s.push_str(" [archived]");
    }
    if date {
        let _ = write!(s, " {}", fmt_date(&n.updated));
    }
    s
}

fn env_path(n: &Note) -> Option<String> {
    (n.kind == crate::kind::ENV).then(|| n.field("location").and_then(crate::note::yaml_scalar_string)).flatten().filter(|p| !p.trim().is_empty())
}

/// Machine facts as a table: id, path, title, a read-only flag, and the machine when the list spans several.
pub fn env_table(notes: &[Note], machine: Option<&str>) -> String {
    if notes.is_empty() {
        return match machine {
            Some(m) => format!("no machine facts for {m} yet, add one: breadcrumb add -k env -t \"Screen recordings\" --field location=C:/Users/me/Videos --field access=read-only\n"),
            None => "no machine facts yet\n".into(),
        };
    }
    let width = notes.iter().map(|n| env_path(n).unwrap_or_default().chars().count()).max().unwrap_or(0).min(60);
    let mut s = String::new();
    for n in notes {
        let path = env_path(n).unwrap_or_else(|| "-".into());
        let ro = if n.field("access").and_then(crate::note::yaml_scalar_string).as_deref() == Some("read-only") { "  [read-only]" } else { "" };
        let on = match (machine, n.field("machine").and_then(crate::note::yaml_scalar_string)) {
            (None, Some(m)) => format!("  ({m})"),
            _ => String::new(),
        };
        let _ = writeln!(s, "{}  {path:<width$}  {}{ro}{on}", n.id, short_title(&n.title));
    }
    s
}

pub fn rows(notes: &[Note], total: usize, current: Option<&str>) -> String {
    if notes.is_empty() {
        return "no notes\n".into();
    }
    let mut s = String::new();
    for n in notes {
        let _ = writeln!(s, "{}", row(n, current, true));
    }
    if total > notes.len() {
        let _ = writeln!(s, "{} more, narrow with filters", total - notes.len());
    }
    s
}

pub const SNIPPET_MAX: usize = 100;
const TERMS_SHOWN: usize = 5;

/// `[expired]` or `[superseded]`, `(exact)` when a query of several terms stands literally in this note and not in all
/// of them, and `(2/3 terms: a, b)` for a partial match.
fn hit_flags(r: &SearchResult, h: &crate::search::Hit) -> String {
    let mut s = h.lapsed.map(|l| format!(" [{l}]")).unwrap_or_default();
    if h.exact_whole && r.hits.iter().any(|x| !x.exact_whole) {
        s.push_str(" (exact)");
    }
    if h.matched < r.terms && h.matched > 0 {
        let shown = h.matched_terms.iter().take(TERMS_SHOWN).map(String::as_str).collect::<Vec<_>>().join(", ");
        let more = if h.matched > TERMS_SHOWN { ", ..." } else { "" };
        let _ = write!(s, " ({}/{} terms: {shown}{more})", h.matched, r.terms);
    }
    s
}

pub fn search_rows(r: &SearchResult, current: Option<&str>) -> String {
    if r.hits.is_empty() {
        return "no matches\n".into();
    }
    let mut s = String::new();
    for h in &r.hits {
        let _ = writeln!(s, "{}{}", row_marked(&h.note, &state_mark(&h.note), current, false), hit_flags(r, h));
        let snip = crate::util::one_line(&h.snippet);
        if !snip.is_empty() {
            let _ = writeln!(s, "  {}", crate::util::truncate_chars(&snip, SNIPPET_MAX));
        }
    }
    if r.total > r.hits.len() {
        let _ = writeln!(s, "{} more, refine the query or narrow with filters", r.total - r.hits.len());
    }
    s
}

/// Frontmatter without keys at their defaults (and without `created` unless `full`), then the body.
pub fn note_compact(n: &Note, full: bool) -> String {
    if full {
        return n.to_markdown();
    }
    use serde_yaml::Value as Y;
    let mut m = n.frontmatter();
    let drop = |m: &mut serde_yaml::Mapping, k: &str, default: &dyn Fn(&Y) -> bool| {
        let key = Y::String(k.into());
        if m.get(&key).is_some_and(default) {
            m.remove(&key);
        }
    };
    let empty = |v: &Y| matches!(v, Y::Sequence(x) if x.is_empty());
    drop(&mut m, "tags", &empty);
    drop(&mut m, "links", &empty);
    drop(&mut m, "status", &|v: &Y| v.as_str() == Some("active"));
    drop(&mut m, "pinned", &|v: &Y| v.as_bool() == Some(false));
    drop(&mut m, "created", &|_: &Y| true);
    hide_import_keys(&mut m);
    drop(&mut m, crate::session::SESSION_KEY, &|_: &Y| true);
    drop(&mut m, crate::session::LAST_SESSION_KEY, &|_: &Y| true);
    if let Some(w) = crate::session::written_line(n) {
        m.insert(Y::String(crate::session::SESSION_KEY.into()), Y::String(w));
    }
    let updated = fmt_date(&n.updated);
    drop(&mut m, crate::note::VERIFIED_KEY, &|v: &Y| v.as_str() == Some(updated.as_str()));
    let refs = Y::String(crate::note::REFS_KEY.into());
    if let Some(Y::Sequence(_)) = m.get(&refs) {
        m.insert(refs, Y::String(n.refs().join(" ")));
    }
    let yaml = serde_yaml::to_string(&Y::Mapping(m)).unwrap_or_default();
    let body = n.body.trim_end();
    if body.is_empty() { format!("---\n{yaml}---\n") } else { format!("---\n{yaml}---\n{body}\n") }
}

/// One line write acknowledgement: `verb slug id "title"`.
pub fn ack(verb: &str, n: &Note) -> String {
    format!("{verb} {} \"{}\"", n.handle(), short_title(&n.title))
}

pub fn kinds_compact(kinds: &[KindDef]) -> String {
    let mut s = String::new();
    for k in kinds {
        let _ = write!(s, "{}: {}", k.name, k.description);
        if !k.fields.is_empty() {
            let fs: Vec<String> = k.fields.iter().map(|f| f.summary()).collect();
            let _ = write!(s, " [{}]", fs.join("; "));
        }
        s.push('\n');
    }
    s
}

pub fn kind_text(k: &KindDef) -> String {
    let mut s = format!("{}: {}\n", k.name, k.description);
    for f in &k.fields {
        let _ = writeln!(s, "  - {}", f.summary());
    }
    if let Some(t) = &k.template {
        let _ = writeln!(s, "  template:\n{}", t.lines().map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n"));
    }
    s
}

pub fn project_text(p: &Project) -> String {
    let mut s = format!("{} ({})\n", p.name, p.slug);
    if !p.description.is_empty() {
        let _ = writeln!(s, "  description: {}", p.description);
    }
    if !p.paths.is_empty() {
        let _ = writeln!(s, "  paths: {}", p.paths.join(", "));
    }
    if !p.remotes.is_empty() {
        let _ = writeln!(s, "  remotes: {}", p.remotes.join(", "));
    }
    s
}

pub fn kind_json(k: &KindDef) -> serde_json::Value {
    serde_json::to_value(k).unwrap_or_default()
}

pub fn project_json(p: &Project) -> serde_json::Value {
    serde_json::to_value(p).unwrap_or_default()
}
