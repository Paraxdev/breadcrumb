//! High level operations shared by the CLI and the MCP server.

use crate::kind::{self, KindDef};
use crate::note::{DEFAULT_REL, JSON_KEYS, Link, Note, RESERVED_KEYS, yaml_display, yaml_scalar_string};
use crate::project::Project;
use crate::search::{self, SearchResult};
use crate::util::{is_valid_slug, now, path_str, slugify};
use crate::vault::Vault;
use anyhow::{Result, bail};
use serde_yaml::Value;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    All,
    Global,
    Project { slug: String, with_global: bool },
}

impl Scope {
    pub fn contains(&self, n: &Note) -> bool {
        match self {
            Scope::All => true,
            Scope::Global => n.project.is_none(),
            Scope::Project { slug, with_global } => match &n.project {
                Some(p) => p == slug,
                None => *with_global,
            },
        }
    }

    pub fn label(&self) -> String {
        match self {
            Scope::All => "all projects".into(),
            Scope::Global => "global".into(),
            Scope::Project { slug, with_global: true } => format!("{slug} + global"),
            Scope::Project { slug, .. } => slug.clone(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ScopeSel {
    pub project: Option<String>,
    pub global: bool,
    pub all: bool,
}

fn check_slug(slug: &str) -> Result<String> {
    let s = slug.trim().to_string();
    if is_valid_slug(&s) {
        Ok(s)
    } else {
        let suggestion = slugify(&s, 60);
        bail!("invalid project slug '{slug}', use lowercase [a-z0-9-] (for example '{suggestion}')")
    }
}

/// Explicit project, explicit global, or detection from cwd (none detected means global plus nothing else).
pub fn resolve_scope(v: &Vault, sel: &ScopeSel, cwd: &Path) -> Result<Scope> {
    if sel.all {
        return Ok(Scope::All);
    }
    if sel.global {
        return Ok(Scope::Global);
    }
    if let Some(p) = &sel.project {
        let slug = check_slug(p)?;
        if v.load_project(&slug)?.is_none() {
            let known: Vec<String> = v.load_projects().into_iter().map(|p| p.slug).collect();
            bail!("unknown project '{slug}', known projects: {}", if known.is_empty() { "(none)".into() } else { known.join(", ") });
        }
        return Ok(Scope::Project { slug, with_global: false });
    }
    Ok(match v.detect_project(cwd) {
        Some(slug) => Scope::Project { slug, with_global: true },
        None => Scope::Global,
    })
}

/// Project that a new note goes to.
pub fn target_project(v: &Vault, project: Option<&str>, global: bool, cwd: &Path) -> Result<Option<String>> {
    if global {
        return Ok(None);
    }
    if let Some(p) = project {
        return Ok(Some(check_slug(p)?));
    }
    Ok(v.detect_project(cwd))
}

fn unknown_project_error(v: &Vault, slug: &str) -> anyhow::Error {
    let known: Vec<String> = v.load_projects().into_iter().map(|p| p.slug).collect();
    anyhow::anyhow!(
        "unknown project '{slug}', known projects: {}. Create it first with project_link (or `breadcrumb project link {slug}` in its directory), or use an existing slug",
        if known.is_empty() { "(none)".into() } else { known.join(", ") }
    )
}

fn ensure_project(v: &Vault, slug: &str) -> Result<Option<PathBuf>> {
    if v.load_project(slug)?.is_some() {
        return Ok(None);
    }
    Ok(Some(v.write_project(&Project::new(slug, None))?))
}

#[derive(Clone, Debug)]
pub enum FieldInput {
    /// Text from the command line, parsed per the declared field type.
    Raw(String),
    /// Structured value from JSON, strings are coerced to the declared type.
    Value(Value),
}

fn apply_fields(v: &Vault, note: &mut Note, k: &KindDef, fields: Vec<(String, FieldInput)>) -> Result<()> {
    for (key, input) in fields {
        let key = key.trim().to_string();
        if key.is_empty() {
            bail!("empty field name");
        }
        if key == "source" {
            note.clear_kept("source");
            note.source = match input {
                FieldInput::Raw(s) => Some(s),
                FieldInput::Value(x) => yaml_scalar_string(&x),
            }
            .filter(|s| !s.is_empty());
            continue;
        }
        if RESERVED_KEYS.contains(&key.as_str()) {
            bail!("'{key}' is note metadata, not a field, use the dedicated option (title, kind, tags, links, status, pin)");
        }
        if JSON_KEYS.contains(&key.as_str()) {
            bail!("'{key}' cannot be a field name, it is used by the JSON output, pick another name");
        }
        let val = match k.field(&key) {
            Some(fd) => {
                let mut val = match input {
                    FieldInput::Raw(s) => fd.parse_value(&s)?,
                    FieldInput::Value(x) => fd.coerce(x)?,
                };
                if fd.ty == kind::FieldType::Ref
                    && let Value::String(s) = &val {
                        val = Value::String(v.resolve_id(s).map_err(|e| anyhow::anyhow!("field '{key}': {e}"))?);
                    }
                val
            }
            None => match input {
                FieldInput::Raw(s) => kind::parse_untyped(&s),
                FieldInput::Value(x) => x,
            },
        };
        note.set_field(&key, val);
    }
    Ok(())
}

pub fn parse_field_args(args: &[String]) -> Result<Vec<(String, FieldInput)>> {
    args.iter()
        .map(|a| match a.split_once('=') {
            Some((k, val)) => Ok((k.trim().to_string(), FieldInput::Raw(val.to_string()))),
            None => bail!("invalid --field '{a}', expected key=value"),
        })
        .collect()
}

fn push_tag(tags: &mut Vec<String>, t: &str) {
    let t = t.trim();
    if !t.is_empty() && !tags.iter().any(|x| x.eq_ignore_ascii_case(t)) {
        tags.push(t.to_string());
    }
}

fn commit_msg(op: &str, n: &Note) -> String {
    format!("breadcrumb: {op} {} {}", n.id, crate::util::one_line(&n.title))
}

#[derive(Clone, Debug, Default)]
pub struct AddReq {
    pub title: String,
    pub kind: Option<String>,
    pub project: Option<String>,
    pub tags: Vec<String>,
    pub fields: Vec<(String, FieldInput)>,
    pub pinned: bool,
    pub body: Option<String>,
    pub source: Option<String>,
    pub links: Vec<Link>,
    /// Refuse a project that does not exist yet instead of creating it, so a typo cannot start an orphan project.
    pub require_existing_project: bool,
}

pub fn add(v: &Vault, req: AddReq) -> Result<Note> {
    let title = crate::util::one_line(&req.title);
    if title.is_empty() {
        bail!("title is required");
    }
    let kind_name = req.kind.clone().filter(|k| !k.trim().is_empty()).unwrap_or_else(|| v.config.default_kind.clone());
    let k = v.require_kind(kind_name.trim())?;
    let project = match &req.project {
        Some(p) => Some(check_slug(p)?),
        None => None,
    };
    if req.require_existing_project
        && let Some(p) = &project
        && v.load_project(p)?.is_none()
    {
        return Err(unknown_project_error(v, p));
    }
    let _g = v.lock()?;
    let mut note = Note::new(&v.fresh_id(), &title, &k.name);
    note.project = project.clone();
    for t in &req.tags {
        push_tag(&mut note.tags, t);
    }
    note.pinned = req.pinned;
    note.source = req.source.clone().filter(|s| !s.is_empty());
    note.body = req.body.clone().unwrap_or_default().trim_end().to_string();
    if note.body.trim().is_empty() {
        note.body = k.template.clone().unwrap_or_default().trim_end().to_string();
    }
    apply_fields(v, &mut note, &k, req.fields)?;
    k.validate(&mut note.extra)?;
    for l in req.links {
        let to = v.resolve_id(&l.to)?;
        if !note.links.iter().any(|x| x.to == to && x.rel == l.rel) {
            note.links.push(Link { to, rel: l.rel });
        }
    }
    let mut touched = vec![];
    if let Some(p) = &project
        && let Some(created) = ensure_project(v, p)? {
            touched.push(created);
        }
    touched.push(v.write_note(&mut note)?);
    v.commit(&touched, &commit_msg("add", &note))?;
    Ok(note)
}

#[derive(Clone, Debug, Default)]
pub struct UpdateReq {
    pub title: Option<String>,
    pub kind: Option<String>,
    /// `+tag` or `tag` adds, `-tag` removes.
    pub tags: Vec<String>,
    /// Replaces all tags when set, applied under the lock before `tags`.
    pub set_tags: Option<Vec<String>>,
    pub fields: Vec<(String, FieldInput)>,
    pub unset: Vec<String>,
    pub body: Option<String>,
    pub pinned: Option<bool>,
    pub status: Option<String>,
    /// Replaces all links when set.
    pub links: Option<Vec<Link>>,
    /// Actor recorded in the backlog history when the state changes.
    pub by: Option<String>,
    /// `version` of the note the caller edited. A mismatch means someone else changed it meanwhile.
    pub expected_version: Option<String>,
}

/// Content hash of a note as breadcrumb would write it, used to detect concurrent edits.
pub fn version(n: &Note) -> String {
    crate::util::sha256_hex(n.to_markdown().as_bytes())[..16].to_string()
}

pub const CONFLICT: &str = "note changed since it was loaded";

fn check_status(s: &str) -> Result<String> {
    let s = s.trim().to_ascii_lowercase();
    if s == "active" || s == "archived" { Ok(s) } else { bail!("status must be 'active' or 'archived', got '{s}'") }
}

pub fn update(v: &Vault, id: &str, req: UpdateReq) -> Result<Note> {
    let _g = v.lock()?;
    let mut note = v.get(id)?;
    if let Some(want) = &req.expected_version
        && *want != version(&note)
    {
        bail!("{CONFLICT} (by another agent or window), reload it and apply your edit again");
    }
    if let Some(t) = &req.title {
        let t = crate::util::one_line(t);
        if t.is_empty() {
            bail!("title cannot be empty");
        }
        note.title = t;
        note.clear_kept("title");
    }
    if let Some(k) = &req.kind {
        note.kind = v.require_kind(k.trim())?.name;
        note.clear_kept("kind");
    }
    let k = v.require_kind(&note.kind)?;
    let old_state = crate::backlog::is_item(&note).then(|| crate::backlog::state(&note));
    if let Some(tags) = &req.set_tags {
        note.tags.clear();
        for t in tags {
            push_tag(&mut note.tags, t);
        }
    }
    for t in &req.tags {
        if let Some(rm) = t.strip_prefix('-') {
            note.tags.retain(|x| !x.eq_ignore_ascii_case(rm.trim()));
        } else {
            push_tag(&mut note.tags, t.strip_prefix('+').unwrap_or(t));
        }
    }
    for key in &req.unset {
        if key == "source" {
            note.source = None;
            note.clear_kept("source");
        } else if RESERVED_KEYS.contains(&key.as_str()) {
            bail!("cannot unset note metadata '{key}'");
        } else {
            note.unset_field(key);
        }
    }
    apply_fields(v, &mut note, &k, req.fields)?;
    if let Some(b) = req.body {
        note.body = b.trim_end().to_string();
    }
    if let Some(p) = req.pinned {
        note.pinned = p;
    }
    if let Some(s) = &req.status {
        note.status = check_status(s)?;
        note.clear_kept("status");
    }
    if let Some(links) = req.links {
        let mut out: Vec<Link> = vec![];
        for l in links {
            let raw = l.to.trim().to_ascii_lowercase();
            // An existing link to a deleted note stays as is, so the note can still be edited.
            let to = if note.links.iter().any(|x| x.to == raw) { raw } else { v.resolve_id(&l.to)? };
            if to == note.id {
                bail!("a note cannot link to itself");
            }
            let rel = if l.rel.trim().is_empty() { DEFAULT_REL.to_string() } else { l.rel.trim().to_string() };
            if !out.iter().any(|x| x.to == to && x.rel == rel) {
                out.push(Link { to, rel });
            }
        }
        note.links = out;
    }
    k.validate(&mut note.extra)?;
    if crate::backlog::is_item(&note) {
        let new_state = crate::backlog::state(&note);
        let from = old_state.unwrap_or_else(|| new_state.clone());
        let has_resolution = note.field("resolution").and_then(yaml_scalar_string).is_some_and(|r| !r.trim().is_empty());
        if new_state == "rejected" && from != "rejected" && !has_resolution {
            bail!("moving to rejected requires a resolution explaining why, set the resolution field too");
        }
        crate::backlog::record_state_change(&mut note, &from, &new_state, req.by.as_deref());
    }
    note.updated = now();
    let p = v.write_note(&mut note)?;
    v.commit(&[p], &commit_msg("update", &note))?;
    Ok(note)
}

pub fn append(v: &Vault, id: &str, text: &str) -> Result<Note> {
    let text = crate::util::normalize_newlines(text);
    let text = text.trim();
    if text.is_empty() {
        bail!("nothing to append");
    }
    let _g = v.lock()?;
    let mut note = v.get(id)?;
    note.body = if crate::backlog::is_item(&note) {
        crate::backlog::insert_before_history(&note.body, text)
    } else if note.body.trim().is_empty() {
        text.to_string()
    } else {
        format!("{}\n\n{text}", note.body.trim_end())
    };
    note.updated = now();
    let p = v.write_note(&mut note)?;
    v.commit(&[p], &commit_msg("append", &note))?;
    Ok(note)
}

pub fn link(v: &Vault, from: &str, to: &str, rel: Option<&str>, remove: bool) -> Result<Note> {
    let _g = v.lock()?;
    let mut note = v.get(from)?;
    let to_id = match v.resolve_id(to) {
        Ok(id) => id,
        // Links to deleted notes can still be removed, matched with the same rules as note ids.
        Err(e) if remove => match_link_target(&note, to)?.ok_or(e)?,
        Err(e) => return Err(e),
    };
    if to_id == note.id && !remove {
        bail!("a note cannot link to itself");
    }
    if remove {
        let before = note.links.len();
        note.links.retain(|l| !(l.to == to_id && rel.is_none_or(|r| r == l.rel)));
        if before == note.links.len() {
            bail!("note {} has no such link to {to_id}", note.id);
        }
    } else {
        let rel = rel.map(str::trim).filter(|r| !r.is_empty()).unwrap_or(DEFAULT_REL).to_string();
        if note.links.iter().any(|l| l.to == to_id && l.rel == rel) {
            return Ok(note);
        }
        note.links.push(Link { to: to_id, rel });
    }
    note.updated = now();
    let p = v.write_note(&mut note)?;
    v.commit(&[p], &commit_msg(if remove { "unlink" } else { "link" }, &note))?;
    Ok(note)
}

fn match_link_target(note: &Note, to: &str) -> Result<Option<String>> {
    let p = to.trim().to_ascii_lowercase();
    if p.len() < crate::id::MIN_PREFIX {
        bail!("id prefix '{to}' is too short, use at least {} characters", crate::id::MIN_PREFIX);
    }
    if note.links.iter().any(|l| l.to == p) {
        return Ok(Some(p));
    }
    let mut targets: Vec<&str> = note.links.iter().map(|l| l.to.as_str()).filter(|t| t.starts_with(&p)).collect();
    targets.sort();
    targets.dedup();
    match targets.len() {
        0 => Ok(None),
        1 => Ok(Some(targets[0].to_string())),
        _ => bail!("id prefix '{to}' matches several links of {}: {}", note.id, targets.join(", ")),
    }
}

pub fn set_status(v: &Vault, id: &str, status: &str) -> Result<Note> {
    let status = check_status(status)?;
    let _g = v.lock()?;
    let mut note = v.get(id)?;
    if note.status == status {
        return Ok(note);
    }
    note.status = status;
    note.clear_kept("status");
    note.updated = now();
    let p = v.write_note(&mut note)?;
    let op = if note.is_archived() { "archive" } else { "unarchive" };
    v.commit(&[p], &commit_msg(op, &note))?;
    Ok(note)
}

pub fn delete(v: &Vault, id: &str) -> Result<Note> {
    let _g = v.lock()?;
    let note = v.get(id)?;
    let p = note.path.clone().unwrap();
    std::fs::remove_file(&p)?;
    v.commit(&[p], &commit_msg("delete", &note))?;
    Ok(note)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatusFilter {
    #[default]
    Active,
    Archived,
    All,
}

impl StatusFilter {
    pub fn parse(s: &str) -> Result<StatusFilter> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "active" => StatusFilter::Active,
            "archived" => StatusFilter::Archived,
            "all" | "any" => StatusFilter::All,
            other => bail!("status filter must be active, archived or all, got '{other}'"),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortKey {
    #[default]
    Updated,
    Created,
    Title,
}

impl SortKey {
    pub fn parse(s: &str) -> Result<SortKey> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "updated" => SortKey::Updated,
            "created" => SortKey::Created,
            "title" => SortKey::Title,
            other => bail!("sort must be updated, created or title, got '{other}'"),
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct Filter {
    pub kind: Option<String>,
    pub tags: Vec<String>,
    pub status: StatusFilter,
    pub wheres: Vec<String>,
    pub pinned: bool,
    pub limit: Option<usize>,
    pub sort: SortKey,
}

fn note_values(n: &Note, key: &str) -> Vec<String> {
    match key {
        "id" => vec![n.id.clone()],
        "title" => vec![n.title.clone()],
        "kind" => vec![n.kind.clone()],
        "project" => vec![n.project.clone().unwrap_or_default()],
        "status" => vec![n.status.clone()],
        "pinned" => vec![n.pinned.to_string()],
        "source" => vec![n.source.clone().unwrap_or_default()],
        "tags" => n.tags.clone(),
        "links" => n.links.iter().map(|l| l.to.clone()).collect(),
        _ => match n.field(key) {
            None | Some(Value::Null) => vec![String::new()],
            Some(Value::Sequence(s)) => s.iter().map(yaml_display).collect(),
            Some(other) => vec![yaml_display(other)],
        },
    }
}

/// `key=value` or `key!=value`, case-insensitive; list fields match if any element matches.
pub fn where_matches(n: &Note, expr: &str) -> Result<bool> {
    let (key, val, negate) = if let Some((k, v)) = expr.split_once("!=") {
        (k, v, true)
    } else if let Some((k, v)) = expr.split_once('=') {
        (k, v, false)
    } else {
        bail!("invalid --where '{expr}', expected key=value or key!=value");
    };
    let vals = note_values(n, key.trim());
    let hit = vals.iter().any(|x| x.eq_ignore_ascii_case(val.trim()));
    Ok(hit != negate)
}

pub fn filter_notes(notes: Vec<Note>, scope: &Scope, f: &Filter) -> Result<Vec<Note>> {
    let mut out = vec![];
    for n in notes {
        if !scope.contains(&n) {
            continue;
        }
        match f.status {
            StatusFilter::Active if n.is_archived() => continue,
            StatusFilter::Archived if !n.is_archived() => continue,
            _ => {}
        }
        if let Some(k) = &f.kind
            && !n.kind.eq_ignore_ascii_case(k) {
                continue;
            }
        if !f.tags.iter().all(|t| n.has_tag(t)) {
            continue;
        }
        if f.pinned && !n.pinned {
            continue;
        }
        let mut ok = true;
        for w in &f.wheres {
            if !where_matches(&n, w)? {
                ok = false;
                break;
            }
        }
        if ok {
            out.push(n);
        }
    }
    Ok(out)
}

pub fn sort_notes(notes: &mut [Note], key: SortKey) {
    match key {
        SortKey::Updated => notes.sort_by(|a, b| b.updated.cmp(&a.updated).then(a.id.cmp(&b.id))),
        SortKey::Created => notes.sort_by(|a, b| b.created.cmp(&a.created).then(a.id.cmp(&b.id))),
        SortKey::Title => notes.sort_by_key(|a| a.title.to_lowercase()),
    }
}

pub fn list(v: &Vault, scope: &Scope, f: &Filter) -> Result<Vec<Note>> {
    Ok(list_counted(v, scope, f)?.0)
}

/// Matching notes after the limit, plus how many matched before it.
pub fn list_counted(v: &Vault, scope: &Scope, f: &Filter) -> Result<(Vec<Note>, usize)> {
    let mut notes = filter_notes(v.load_notes(), scope, f)?;
    sort_notes(&mut notes, f.sort);
    let total = notes.len();
    if let Some(l) = f.limit {
        notes.truncate(l);
    }
    Ok((notes, total))
}

pub const DEFAULT_SEARCH_LIMIT: usize = 20;

pub fn search(v: &Vault, query: &str, scope: &Scope, f: &Filter) -> Result<SearchResult> {
    let notes = filter_notes(v.load_notes(), scope, f)?;
    let mut r = search::search(notes, query);
    r.hits.truncate(f.limit.unwrap_or(DEFAULT_SEARCH_LIMIT));
    Ok(r)
}

fn is_open_todo(n: &Note) -> bool {
    n.kind == "todo" && !matches!(n.field("done"), Some(Value::Bool(true)))
}

/// Markdown digest agents read at session start.
pub fn context(v: &Vault, project: Option<&str>, limit: usize) -> Result<String> {
    use std::fmt::Write;
    let mut out = String::new();
    let proj = match project {
        Some(slug) => v.load_project(slug)?,
        None => None,
    };
    match (&proj, project) {
        (Some(p), _) => {
            writeln!(out, "# breadcrumb context: {} ({})", p.name, p.slug)?;
            if !p.description.trim().is_empty() {
                writeln!(out, "\n{}", p.description.trim())?;
            }
            if !p.paths.is_empty() {
                writeln!(out, "\nPaths: {}", p.paths.join(", "))?;
            }
            if !p.remotes.is_empty() {
                writeln!(out, "Remotes: {}", p.remotes.join(", "))?;
            }
        }
        (None, Some(slug)) => writeln!(out, "# breadcrumb context: {slug} (project not registered yet, notes added here will create it)")?,
        (None, None) => {
            writeln!(out, "# breadcrumb context: global (no project detected for this directory)")?;
            let slugs: Vec<String> = v.load_projects().into_iter().map(|p| p.slug).collect();
            if !slugs.is_empty() {
                writeln!(out, "\nKnown projects: {}. Run `breadcrumb project link <slug>` (or the project_link tool) to bind this directory to one.", slugs.join(", "))?;
            }
        }
    }
    if let Some(slug) = project {
        let wfs = crate::workflow::list(v, &Scope::Project { slug: slug.to_string(), with_global: false });
        if !wfs.is_empty() {
            let names: Vec<String> = wfs.iter().map(|w| w.summary()).collect();
            writeln!(out, "\nWorkflows: {} (restore into .claude/workflows with `breadcrumb workflow install`)", names.join(", "))?;
        }
    }
    let scope = match project {
        Some(slug) => Scope::Project { slug: slug.to_string(), with_global: true },
        None => Scope::Global,
    };
    let mut notes = filter_notes(v.load_notes(), &scope, &Filter::default())?;
    sort_notes(&mut notes, SortKey::Updated);

    let pinned: Vec<&Note> = notes.iter().filter(|n| n.pinned).collect();
    if !pinned.is_empty() {
        writeln!(out, "\n## Pinned")?;
        for n in &pinned {
            writeln!(out, "\n### {} (`{}`, {}, {})", n.title, n.id, n.kind, n.scope_label())?;
            if !n.body.trim().is_empty() {
                writeln!(out, "\n{}", n.body.trim())?;
            }
        }
    }
    let todos: Vec<&Note> = notes.iter().filter(|n| !n.pinned && is_open_todo(n)).collect();
    if !todos.is_empty() {
        writeln!(out, "\n## Open todos")?;
        for n in &todos {
            let prio = n.field("priority").map(yaml_display).map(|p| format!(" [{p}]")).unwrap_or_default();
            writeln!(out, "- `{}` {}{} ({})", n.id, n.title, prio, n.scope_label())?;
        }
    }
    let recent: Vec<&Note> = notes.iter().filter(|n| !n.pinned && !is_open_todo(n)).take(limit).collect();
    if !recent.is_empty() {
        let total = notes.iter().filter(|n| !n.pinned && !is_open_todo(n)).count();
        writeln!(out, "\n## Recent notes ({} of {})", recent.len(), total)?;
        for n in &recent {
            let scope = if n.project.is_none() && project.is_some() { " (global)" } else { "" };
            writeln!(out, "- `{}` {}: {}{} ({})", n.id, n.kind, n.title, scope, crate::util::fmt_date(&n.updated))?;
        }
    }
    if notes.is_empty() {
        writeln!(out, "\nNo notes yet in this scope.")?;
    }
    let all = v.load_notes();
    out.push_str(&crate::backlog::context_section(&notes, &all));
    writeln!(out, "\n## Kinds")?;
    for k in v.load_kinds() {
        let fields = if k.fields.is_empty() {
            String::new()
        } else {
            format!(" (fields: {})", k.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>().join(", "))
        };
        writeln!(out, "- {}: {}{}", k.name, k.description, fields)?;
    }
    writeln!(out, "\n## Usage")?;
    writeln!(out, "- `get <id>` for full notes, `search <query>` before adding to avoid duplicates.")?;
    writeln!(out, "- Prefer `update`/`append` over near-duplicate notes, one fact per note, never store secrets.")?;
    writeln!(out, "- New kinds or fields: `define_kind`. End of session: add a `handoff` note.")?;
    Ok(out)
}

pub fn project_add(v: &Vault, slug: &str, name: Option<&str>, description: Option<&str>, paths: &[String]) -> Result<(Project, bool)> {
    let slug = check_slug(slug)?;
    let _g = v.lock()?;
    let (mut p, created) = match v.load_project(&slug)? {
        Some(p) => (p, false),
        None => (Project::new(&slug, name), true),
    };
    if let Some(n) = name {
        p.name = n.to_string();
    }
    if let Some(d) = description {
        p.description = d.to_string();
    }
    for path in paths {
        p.add_path(&path_str(&crate::util::absolutize(Path::new(path))));
    }
    let f = v.write_project(&p)?;
    v.commit(&[f], &format!("breadcrumb: project {} {}", if created { "add" } else { "update" }, slug))?;
    Ok((p, created))
}

#[derive(Debug, Clone)]
pub struct LinkReport {
    pub project: Project,
    pub created: bool,
    pub added_path: Option<String>,
    pub added_remotes: Vec<String>,
}

/// Binds a directory (and its git remotes) to a project, creating the project if needed.
pub fn project_link(v: &Vault, slug: &str, path: &Path, description: Option<&str>) -> Result<LinkReport> {
    let slug = check_slug(slug)?;
    // Detection compares against the cwd as the shell reports it, so store that form. The canonical form
    // (junction or symlink target, UNC share behind a mapped drive) is recorded too when it differs.
    let abs = crate::util::absolutize(path);
    let canonical = std::fs::canonicalize(&abs).ok().map(|p| path_str(&p));
    let remotes = if crate::git::toplevel(&abs).is_some() { crate::git::remote_urls(&abs) } else { vec![] };
    let _g = v.lock()?;
    let (mut p, created) = match v.load_project(&slug)? {
        Some(p) => (p, false),
        None => (Project::new(&slug, None), true),
    };
    if let Some(d) = description {
        p.description = d.to_string();
    }
    let ps = path_str(&abs);
    let added_path = if p.add_path(&ps) { Some(ps) } else { None };
    if let Some(c) = canonical {
        p.add_path(&c);
    }
    let mut added_remotes = vec![];
    for r in remotes {
        if p.add_remote(&r) {
            added_remotes.push(crate::project::normalize_remote(&r));
        }
    }
    let f = v.write_project(&p)?;
    v.commit(&[f], &format!("breadcrumb: project link {slug}"))?;
    Ok(LinkReport { project: p, created, added_path, added_remotes })
}

/// Creates or updates a kind. With `replace` false, fields merge by name into the existing definition.
pub fn define_kind(v: &Vault, def: KindDef, replace: bool, remove_fields: &[String]) -> Result<(KindDef, bool)> {
    if !kind::is_valid_kind_name(&def.name) {
        bail!("invalid kind name '{}', use lowercase letters, digits, '-' or '_'", def.name);
    }
    for f in &def.fields {
        if RESERVED_KEYS.contains(&f.name.as_str()) {
            bail!("field name '{}' is reserved for note metadata", f.name);
        }
        if f.ty == kind::FieldType::Enum && f.values.is_empty() {
            bail!("enum field '{}' needs values", f.name);
        }
        if let Some(d) = f.default_yaml() {
            f.check(&d).map_err(|e| anyhow::anyhow!("default for {e}"))?;
        }
    }
    let _g = v.lock()?;
    let existing = v.load_kind(&def.name)?;
    let created = existing.is_none();
    let mut k = match existing {
        Some(mut old) if !replace => {
            old.merge(def);
            old
        }
        _ => def,
    };
    k.fields.retain(|f| !remove_fields.contains(&f.name));
    let p = v.write_kind(&k)?;
    v.commit(&[p], &format!("breadcrumb: {} kind {}", if created { "define" } else { "update" }, k.name))?;
    Ok((k, created))
}
