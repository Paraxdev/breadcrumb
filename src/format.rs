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
    format!("{}  {:<12} {:<14} {}{}  ({})", n.id, n.kind, n.scope_label(), n.title, flags, fmt_date(&n.updated))
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

pub fn search_text(r: &SearchResult, scope_label: &str) -> String {
    let mut s = String::new();
    if r.hits.is_empty() {
        let _ = writeln!(s, "no matches in {scope_label}");
        return s;
    }
    if r.fell_back_to_or {
        let _ = writeln!(s, "(no note matched all terms, showing notes matching any term)");
    }
    for h in &r.hits {
        let _ = writeln!(s, "{}  {:<12} {:<14} {}", h.note.id, h.note.kind, h.note.scope_label(), h.note.title);
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

/// `id kind title`, plus the project when it is not `current`, plus flags and optionally the date.
pub fn row(n: &Note, current: Option<&str>, date: bool) -> String {
    let mut s = format!("{} {} {}", n.id, n.kind, short_title(&n.title));
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

pub fn search_rows(r: &SearchResult, current: Option<&str>) -> String {
    if r.hits.is_empty() {
        return "no matches\n".into();
    }
    let mut s = String::new();
    if r.fell_back_to_or {
        s.push_str("no note matched every term, showing any-term matches\n");
    }
    for h in &r.hits {
        let _ = writeln!(s, "{}", row(&h.note, current, false));
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
    let updated = fmt_date(&n.updated);
    drop(&mut m, crate::note::VERIFIED_KEY, &|v: &Y| v.as_str() == Some(updated.as_str()));
    let yaml = serde_yaml::to_string(&Y::Mapping(m)).unwrap_or_default();
    let body = n.body.trim_end();
    if body.is_empty() { format!("---\n{yaml}---\n") } else { format!("---\n{yaml}---\n{body}\n") }
}

/// One line write acknowledgement: `verb id "title"`.
pub fn ack(verb: &str, n: &Note) -> String {
    format!("{verb} {} \"{}\"", n.id, short_title(&n.title))
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
