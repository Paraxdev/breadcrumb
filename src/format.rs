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

pub fn kinds_text(kinds: &[KindDef]) -> String {
    kinds.iter().map(kind_text).collect::<Vec<_>>().join("")
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
