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
        "unknown project '{slug}', known projects: {}. Create it first with the project tool, action link (or `breadcrumb project link {slug}` in its directory), or use an existing slug",
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
        if key == crate::note::PATHS_KEY {
            let raw: Vec<String> = match input {
                FieldInput::Raw(s) => s.split(',').map(str::to_string).collect(),
                FieldInput::Value(Value::Sequence(xs)) => xs.iter().filter_map(yaml_scalar_string).collect(),
                FieldInput::Value(x) => yaml_scalar_string(&x).into_iter().collect(),
            };
            set_paths(note, crate::recall::normalize_globs(&raw)?);
            continue;
        }
        if key == crate::note::REFS_KEY {
            let raw: Vec<String> = match input {
                FieldInput::Raw(s) => s.split(',').map(str::to_string).collect(),
                FieldInput::Value(Value::Sequence(xs)) => xs.iter().filter_map(yaml_scalar_string).collect(),
                FieldInput::Value(x) => yaml_scalar_string(&x).into_iter().collect(),
            };
            note.set_refs(crate::note::normalize_refs(&raw)?);
            continue;
        }
        if key == crate::note::EXPIRES_KEY && k.field(&key).is_none() {
            let raw = match input {
                FieldInput::Raw(s) => s,
                FieldInput::Value(x) => yaml_scalar_string(&x).unwrap_or_default(),
            };
            if raw.trim().is_empty() {
                note.unset_field(&key);
            } else {
                note.set_field(&key, Value::String(crate::lifecycle::normalize_expires(v, &raw)?));
            }
            continue;
        }
        if key == crate::note::VERIFIED_KEY {
            bail!("'verified' is set by breadcrumb, use verify to mark a note as still accurate");
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

/// Adds an option that is a plain field under the hood, like summary, as one line of text.
pub fn with_field(mut fields: Vec<(String, FieldInput)>, key: &str, val: Option<String>) -> Vec<(String, FieldInput)> {
    if let Some(s) = val.map(|s| crate::util::one_line(&s)).filter(|s| !s.is_empty()) {
        fields.push((key.to_string(), FieldInput::Value(Value::String(s))));
    }
    fields
}

fn set_paths(note: &mut Note, globs: Vec<String>) {
    if globs.is_empty() {
        note.unset_field(crate::note::PATHS_KEY);
    } else {
        note.set_field(crate::note::PATHS_KEY, Value::Sequence(globs.into_iter().map(Value::String).collect()));
    }
}

/// `type:value` or `+type:value` adds a ref, `-type:value` removes it.
pub fn edit_refs(note: &mut Note, edits: &[String]) -> Result<()> {
    if edits.is_empty() {
        return Ok(());
    }
    let mut refs = note.refs();
    for e in edits {
        let e = e.trim();
        if let Some(rm) = e.strip_prefix('-') {
            let rm = crate::note::normalize_refs(&[rm.to_string()])?;
            refs.retain(|r| !rm.contains(r));
        } else {
            refs.extend(crate::note::normalize_refs(&[e.strip_prefix('+').unwrap_or(e).to_string()])?);
        }
    }
    note.set_refs(crate::note::normalize_refs(&refs)?);
    Ok(())
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
    /// File globs relative to the project root.
    pub paths: Vec<String>,
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
    if !req.paths.is_empty() {
        set_paths(&mut note, crate::recall::normalize_globs(&req.paths)?);
    }
    note.mark_verified();
    k.validate(&mut note.extra)?;
    for l in req.links {
        let to = v.resolve_id(&l.to)?;
        if !note.links.iter().any(|x| x.to == to && x.rel == l.rel) {
            note.links.push(Link { to, rel: l.rel });
        }
    }
    // One living handoff per project: the new one supersedes the current one and older pinned ones stop being pinned.
    let mut unpin: Vec<Note> = vec![];
    if note.kind == HANDOFF {
        let all = v.load_notes();
        if !note.links.iter().any(|l| l.rel == SUPERSEDES)
            && let Some(p) = crate::lifecycle::latest_handoff(&all, note.project.as_deref(), &crate::lifecycle::Lapsed::of(&all))
        {
            note.links.push(Link { to: p.id.clone(), rel: SUPERSEDES.into() });
        }
        unpin = all.into_iter().filter(|n| n.kind == HANDOFF && n.project == note.project && n.pinned).collect();
    }
    let mut touched = vec![];
    if let Some(p) = &project
        && let Some(created) = ensure_project(v, p)? {
            touched.push(created);
        }
    for n in &mut unpin {
        n.pinned = false;
        n.clear_kept("pinned");
        touched.push(v.write_note(n)?);
    }
    touched.push(v.write_note(&mut note)?);
    v.commit(&touched, &commit_msg("add", &note))?;
    Ok(note)
}

pub const SIMILAR_MAX: usize = 2;

fn title_terms(title: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for t in search::tokenize(title) {
        if !crate::recall::STOPWORDS.contains(&t.as_str()) && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// Same word, or one a prefix of the other when both have 4 or more chars, like limit and limits.
fn same_term(a: &str, b: &str) -> bool {
    a == b || (a.len() >= 4 && b.len() >= 4 && (a.starts_with(b) || b.starts_with(a)))
}

/// Notes that may hold the same fact as `n`, at most SIMILAR_MAX: an equal title, or two or more shared title words
/// covering half of the longer title, or a third when they share a tag. Handoffs are new every session, so never.
pub fn similar(v: &Vault, n: &Note) -> Vec<Note> {
    if n.kind == HANDOFF {
        return vec![];
    }
    let all = v.load_notes();
    let lapsed = crate::lifecycle::Lapsed::of(&all);
    let mine = title_terms(&n.title);
    let title = crate::util::one_line(&n.title).to_lowercase();
    let mut hits: Vec<(bool, f64, Note)> = all
        .into_iter()
        .filter(|x| x.id != n.id && x.kind != HANDOFF && !x.is_archived() && !lapsed.hides(x) && (x.project == n.project || x.project.is_none()))
        .filter_map(|x| {
            let exact = crate::util::one_line(&x.title).to_lowercase() == title;
            let theirs = title_terms(&x.title);
            let shared = mine.iter().filter(|a| theirs.iter().any(|b| same_term(a, b))).count();
            let tags = n.tags.iter().filter(|t| x.has_tag(t)).count().min(3);
            let cover = shared as f64 / mine.len().max(theirs.len()).max(1) as f64;
            let alike = exact || (shared >= 2 && (cover >= 0.5 || (cover >= 0.34 && tags > 0)));
            let kind = if x.kind == n.kind { 0.05 } else { 0.0 };
            alike.then_some((exact, cover + 0.1 * tags as f64 + kind, x))
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)).then(b.2.updated.cmp(&a.2.updated)));
    hits.into_iter().take(SIMILAR_MAX).map(|h| h.2).collect()
}

pub fn similar_hint(xs: &[Note]) -> Option<String> {
    if xs.is_empty() {
        return None;
    }
    let items: Vec<String> = xs.iter().map(|x| format!("{} \"{}\"", x.id, crate::util::truncate_chars(&crate::util::one_line(&x.title), 60))).collect();
    Some(format!("possible duplicates, update one instead if it holds the same fact: {}", items.join(", ")))
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
    /// `glob` or `+glob` adds, `-glob` removes.
    pub paths: Vec<String>,
    /// Replaces all path globs when set, before `paths`.
    pub set_paths: Option<Vec<String>>,
    /// `type:value` or `+type:value` adds a ref, `-type:value` removes it.
    pub refs: Vec<String>,
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
    if req.set_paths.is_some() || !req.paths.is_empty() {
        let mut globs = match &req.set_paths {
            Some(xs) => crate::recall::normalize_globs(xs)?,
            None => note.path_globs(),
        };
        for p in &req.paths {
            if let Some(rm) = p.strip_prefix('-') {
                let rm = crate::recall::normalize_globs(&[rm.to_string()])?;
                globs.retain(|g| !rm.contains(g));
            } else {
                for g in crate::recall::normalize_globs(&[p.strip_prefix('+').unwrap_or(p).to_string()])? {
                    if !globs.contains(&g) {
                        globs.push(g);
                    }
                }
            }
        }
        set_paths(&mut note, globs);
    }
    edit_refs(&mut note, &req.refs)?;
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
    note.mark_verified();
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
    note.mark_verified();
    note.updated = now();
    let p = v.write_note(&mut note)?;
    v.commit(&[p], &commit_msg("append", &note))?;
    Ok(note)
}

/// Marks notes as still accurate without touching their content or `updated`.
pub fn verify(v: &Vault, ids: &[String]) -> Result<Vec<Note>> {
    if ids.is_empty() {
        bail!("no note ids given");
    }
    let _g = v.lock()?;
    let mut notes = vec![];
    for id in ids {
        notes.push(v.get(id)?);
    }
    let mut touched = vec![];
    for n in &mut notes {
        n.mark_verified();
        touched.push(v.write_note(n)?);
    }
    let msg = match notes.as_slice() {
        [n] => commit_msg("verify", n),
        ns => format!("breadcrumb: verify {} notes", ns.len()),
    };
    v.commit(&touched, &msg)?;
    Ok(notes)
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
    if key.trim() == "ref" {
        return Ok(n.refs().iter().any(|r| ref_matches(r, val.trim())) != negate);
    }
    let vals = note_values(n, key.trim());
    let hit = vals.iter().any(|x| x.eq_ignore_ascii_case(val.trim()));
    Ok(hit != negate)
}

/// `want` is a whole ref or just its value. A commit also matches by a prefix of at least 7 characters, either way round.
pub fn ref_matches(have: &str, want: &str) -> bool {
    let want = want.to_ascii_lowercase();
    let have = have.to_ascii_lowercase();
    let (kind, value) = have.split_once(':').unwrap_or(("", have.as_str()));
    let (want_kind, want_value) = match want.split_once(':') {
        Some((k, v)) => (Some(k), v),
        None => (None, want.as_str()),
    };
    if want_kind.is_some_and(|k| k != kind) {
        return false;
    }
    value == want_value || (kind == "commit" && want_value.len() >= 7 && value.len() >= 7 && (value.starts_with(want_value) || want_value.starts_with(value)))
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

pub const DEFAULT_SEARCH_LIMIT: usize = 5;

/// Superseded and expired notes rank after every note that still holds, and say so.
pub fn search(v: &Vault, query: &str, scope: &Scope, f: &Filter) -> Result<SearchResult> {
    let all = v.load_notes();
    let lapsed = crate::lifecycle::Lapsed::of(&all);
    let notes = filter_notes(all, scope, f)?;
    let mut r = search::search(notes, query);
    for h in &mut r.hits {
        h.lapsed = lapsed.label(&h.note);
    }
    r.hits.sort_by_key(|h| h.lapsed.is_some());
    r.hits.truncate(f.limit.unwrap_or(DEFAULT_SEARCH_LIMIT));
    Ok(r)
}

pub use crate::digest::{ContextOpts, context, context_with};
pub use crate::lifecycle::{HANDOFF, SUPERSEDES};

pub const PREFS_LINE_MAX: usize = 400;
pub const PIN_HINT: &str = "pin it if every session must see it, unpinned preferences only share one short line";
const PREF_TITLE_MAX: usize = 60;

/// Feedback and user notes hold preferences, they only reach a new session with a line of their own when pinned.
pub fn is_pref(n: &Note) -> bool {
    n.kind == "feedback" || n.kind == "user"
}

pub fn prefs_line(prefs: &[&Note]) -> String {
    let mut s = "Prefs (unpinned):".to_string();
    for (i, n) in prefs.iter().enumerate() {
        let item = format!(" {} {};", n.id, crate::util::truncate_chars(&crate::util::one_line(&n.title), PREF_TITLE_MAX));
        let more = prefs.len() - i;
        if s.chars().count() + item.chars().count() > PREFS_LINE_MAX - 12 && more > 1 {
            s.push_str(&format!(" +{more} more"));
            return s;
        }
        s.push_str(&item);
    }
    s.trim_end_matches(';').to_string()
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
        if RESERVED_KEYS.contains(&f.name.as_str()) || [crate::note::PATHS_KEY, crate::note::VERIFIED_KEY, crate::note::REFS_KEY, crate::note::EXPIRES_KEY].contains(&f.name.as_str()) {
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
