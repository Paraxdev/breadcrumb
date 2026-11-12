//! High level operations shared by the CLI and the MCP server.

use crate::kind::{self, KindDef};
use crate::note::{DEFAULT_REL, JSON_KEYS, Link, Note, RESERVED_KEYS, yaml_display, yaml_scalar_string};
use crate::project::Project;
use crate::search::{self, SearchResult};
use crate::util::{is_valid_slug, now, path_str, slugify};
use crate::vault::Vault;
use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
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

    /// The one project this scope is about, None for global or all.
    pub fn project(&self) -> Option<&str> {
        match self {
            Scope::Project { slug, .. } => Some(slug),
            _ => None,
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

pub(crate) fn check_slug(slug: &str) -> Result<String> {
    let s = slug.trim().to_string();
    if is_valid_slug(&s) {
        Ok(s)
    } else {
        let suggestion = slugify(&s, 60);
        if RESERVED_SLUGS.contains(&suggestion.as_str()) {
            bail!("invalid project slug '{slug}', use lowercase [a-z0-9-], and '{suggestion}' names a scope, so pick another word");
        }
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
    if global || project.is_some_and(|p| p.trim().eq_ignore_ascii_case("global")) {
        return Ok(None);
    }
    if let Some(p) = project {
        return Ok(Some(check_slug(p)?));
    }
    Ok(v.detect_project(cwd))
}

/// The project a digest is for, which is never all of them.
pub fn digest_project(v: &Vault, project: Option<&str>, global: bool, cwd: &Path) -> Result<Option<String>> {
    if !global && project.is_some_and(|p| p.trim().eq_ignore_ascii_case("all")) {
        bail!("the digest is per project or global, there is no digest of all projects, name one project or global, list and board take all");
    }
    target_project(v, project, global, cwd)
}

/// The project a `claim` or `release` of `track:<name>` looks in, `project` only matters for a track.
pub fn track_project(v: &Vault, target: &str, project: Option<&str>, global: bool, cwd: &Path) -> Result<Option<String>> {
    let p = project.map(str::trim).map(str::to_ascii_lowercase);
    Ok(match p.as_deref() {
        _ if global => None,
        Some("global") => None,
        Some("all") | None => v.detect_project(cwd),
        Some(slug) if target.trim().starts_with("track:") && v.load_project(slug)?.is_none() => {
            let known: Vec<String> = v.load_projects().into_iter().map(|p| p.slug).collect();
            bail!("unknown project '{slug}', known projects: {}", if known.is_empty() { "(none)".into() } else { known.join(", ") });
        }
        Some(slug) => Some(slug.to_string()),
    })
}

pub(crate) fn unknown_project_error(v: &Vault, slug: &str) -> anyhow::Error {
    let known: Vec<String> = v.load_projects().into_iter().map(|p| p.slug).collect();
    anyhow::anyhow!(
        "unknown project '{slug}', known projects: {}. Create it first with the project tool, action link (or `breadcrumb project link {slug}` in its directory), or use an existing slug",
        if known.is_empty() { "(none)".into() } else { known.join(", ") }
    )
}

/// Slugs a project cannot take, `-p global` and `-p all` name scopes and would never reach it.
pub const RESERVED_SLUGS: [&str; 2] = ["global", "all"];

/// `check_slug` for a project being created or bound.
pub(crate) fn check_new_slug(slug: &str) -> Result<String> {
    let s = check_slug(slug)?;
    if RESERVED_SLUGS.contains(&s.as_str()) {
        bail!("'{s}' cannot be a project slug, it names a scope (global notes, all projects), pick another slug");
    }
    Ok(s)
}

pub(crate) fn ensure_project(v: &Vault, slug: &str) -> Result<Option<PathBuf>> {
    if v.load_project(slug)?.is_some() {
        return Ok(None);
    }
    check_new_slug(slug)?;
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
    let said = fields.iter().any(|(key, _)| key.trim() == crate::note::SAID_KEY);
    let said_on = fields.iter().any(|(key, _)| key.trim() == crate::note::SAID_ON_KEY);
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
        if key == crate::note::VERIFIED_KEY || key == crate::note::VERIFIED_AT_KEY || key == crate::note::PIN_VERIFIED_KEY {
            bail!("'{key}' is set by breadcrumb, use verify to mark a note as still accurate");
        }
        if key == crate::session::SESSION_KEY || key == crate::session::LAST_SESSION_KEY {
            bail!("'{key}' is set by breadcrumb from the session that writes");
        }
        if crate::merge::KEYS.contains(&key.as_str()) {
            bail!("'{key}' is set by merge, use the merge tool or `breadcrumb merge`");
        }
        if key == "status" {
            bail!("'status' is the active or archived flag of a note, backlog progress is the field state, like state=queued");
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
    if said && !said_on {
        note.set_field(crate::note::SAID_ON_KEY, Value::String(crate::util::fmt_date(&crate::util::now())));
    }
    if let Some(d) = note.field(crate::note::SAID_ON_KEY).and_then(yaml_scalar_string)
        && chrono::NaiveDate::parse_from_str(&d, "%Y-%m-%d").is_err()
    {
        bail!("said_on '{d}' must be a date like 2026-10-30");
    }
    Ok(())
}

/// `slug` is accepted as a field too, since the MCP `fields` argument is where an agent may put it.
fn split_slug(fields: Vec<(String, FieldInput)>) -> (Vec<(String, FieldInput)>, Option<String>) {
    let (mine, rest): (Vec<_>, Vec<_>) = fields.into_iter().partition(|(k, _)| k.trim() == crate::slug::SLUG_KEY);
    let given = mine.into_iter().next_back().and_then(|(_, f)| match f {
        FieldInput::Raw(s) => Some(s),
        FieldInput::Value(x) => yaml_scalar_string(&x),
    });
    (rest, given)
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

/// The quote stays as given, line breaks too, it is what the user said.
pub fn with_said(mut fields: Vec<(String, FieldInput)>, said: Option<String>, on: Option<String>) -> Vec<(String, FieldInput)> {
    if let Some(q) = said.map(|q| q.trim().to_string()).filter(|q| !q.is_empty()) {
        fields.push((crate::note::SAID_KEY.to_string(), FieldInput::Value(Value::String(q))));
    }
    with_field(fields, crate::note::SAID_ON_KEY, on)
}

pub(crate) fn set_paths(note: &mut Note, globs: Vec<String>) {
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

pub(crate) fn push_tag(tags: &mut Vec<String>, t: &str) {
    let t = t.trim();
    if !t.is_empty() && !tags.iter().any(|x| x.eq_ignore_ascii_case(t)) {
        tags.push(t.to_string());
    }
}

pub(crate) fn commit_msg(op: &str, n: &Note) -> String {
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
    /// A better name than the one generated from the title.
    pub slug: Option<String>,
    /// A handoff's track, see lifecycle::same_track.
    pub track: Option<String>,
    /// Refuse a project that does not exist yet instead of creating it, so a typo cannot start an orphan project.
    pub require_existing_project: bool,
}

pub fn add(v: &Vault, req: AddReq) -> Result<Note> {
    add_noted(v, req).map(|r| r.0)
}

/// The note and what `add` changed on the way that the caller should say, a repaired slug.
pub fn add_noted(v: &Vault, req: AddReq) -> Result<(Note, Option<String>)> {
    let _g = v.lock()?;
    add_held(v, req)
}

fn default_kind_error(v: &Vault, project: Option<&str>, name: &str) -> anyhow::Error {
    let from = project.filter(|p| is_valid_slug(p) && crate::config::overrides(v, p).is_ok_and(|o| o.contains_key("default_kind")));
    let (whose, flag) = match from {
        Some(p) => (format!("project {p}'s default_kind setting"), format!(" --project {p}")),
        None => ("the default_kind setting".to_string(), String::new()),
    };
    let names: Vec<String> = v.load_kinds().into_iter().map(|k| k.name).collect();
    anyhow::anyhow!(
        "the note has no kind and {whose} names '{name}', which is no kind, give a kind or fix the setting with `breadcrumb config set default_kind <kind>{flag}`. Kinds: {}",
        names.join(", ")
    )
}

/// `add_noted` for a caller that already holds the vault lock.
pub(crate) fn add_held(v: &Vault, req: AddReq) -> Result<(Note, Option<String>)> {
    let title = crate::util::one_line(&req.title);
    if title.is_empty() {
        bail!("title is required");
    }
    let given = req.kind.clone().filter(|k| !k.trim().is_empty());
    let kind_name = given.clone().unwrap_or_else(|| crate::config::effective(v, req.project.as_deref()).default_kind);
    let k = match v.require_kind(kind_name.trim()) {
        Err(_) if given.is_none() => return Err(default_kind_error(v, req.project.as_deref(), &kind_name)),
        r => r?,
    };
    let mut req = req;
    if k.name == crate::judge::JUDGMENT && crate::judge::home(v).is_some() && req.project != crate::judge::home(v) {
        if let Some(p) = req.project.take().filter(|_| !req.fields.iter().any(|(f, _)| f == "seen_in")) {
            req.fields.push(("seen_in".into(), FieldInput::Raw(p)));
        }
        req.project = crate::judge::home(v);
    }
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
    if let Some(p) = &project
        && v.load_project(p)?.is_none()
    {
        check_new_slug(p)?;
    }
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
    let (fields, field_slug) = split_slug(req.fields);
    let slug_note = crate::slug::assign(v, &mut note, req.slug.or(field_slug).as_deref());
    apply_fields(v, &mut note, &k, fields)?;
    if let Some(t) = req.track.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        if t.split_whitespace().count() > 1 {
            bail!("track '{t}' must be one word");
        }
        note.set_field(crate::lifecycle::TRACK_KEY, Value::String(t.to_lowercase()));
    }
    if !req.paths.is_empty() {
        set_paths(&mut note, crate::recall::normalize_globs(&req.paths)?);
    }
    if note.kind == crate::kind::ENV && note.field("machine").is_none() {
        note.set_field("machine", Value::String(crate::util::machine_name()));
    }
    note.mark_verified();
    crate::session::stamp_new(&mut note);
    if note.kind == HANDOFF {
        crate::lifecycle::stamp_writer_name(&mut note);
    }
    k.validate(&mut note.extra)?;
    if note.kind == crate::map::MAP {
        crate::map::check_new(v, &note)?;
    }
    for l in req.links {
        let to = v.resolve_id(&l.to)?;
        if !note.links.iter().any(|x| x.to == to && x.rel == l.rel) {
            note.links.push(Link { to, rel: l.rel });
        }
    }
    // A new handoff replaces only its own track's, parallel sessions keep theirs. What it replaces stops being pinned.
    let mut all = vec![];
    if note.kind == HANDOFF {
        all = v.load_notes();
        let lapsed = crate::lifecycle::Lapsed::of(&all);
        for p in crate::lifecycle::live_handoffs(&all, note.project.as_deref(), &lapsed) {
            if crate::lifecycle::same_track(&note, p) && !note.links.iter().any(|l| l.to == p.id && l.rel == SUPERSEDES) {
                note.links.push(Link { to: p.id.clone(), rel: SUPERSEDES.into() });
            }
        }
    }
    let mut touched: Vec<PathBuf> = match &project {
        Some(p) => ensure_project(v, p)?.into_iter().collect(),
        None => vec![],
    };
    touched.extend(v.ensure_kind_file(&note.kind)?);
    if note.kind == crate::judge::JUDGMENT {
        touched.extend(crate::judge::upgrade_kind_file(v)?);
    }
    touched.extend(crate::lifecycle::unpin_superseded(v, &note, all)?);
    touched.push(v.write_note(&mut note)?);
    v.commit(&touched, &commit_msg("add", &note))?;
    Ok((note, slug_note))
}

/// What landed for a backlog item, so the agent sees the stored values.
pub fn landed(n: &Note) -> Option<String> {
    crate::backlog::is_item(n).then(|| format!("state={} priority={}", crate::backlog::state(n), crate::backlog::priority(n)))
}

pub const SIMILAR_MAX: usize = 2;

pub(crate) fn title_terms(title: &str) -> Vec<String> {
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
        .filter_map(|x| look_alike(n, &mine, &title, &x, &title_terms(&x.title)).map(|(exact, score)| (exact, score, x)))
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)).then(b.2.updated.cmp(&a.2.updated)));
    hits.into_iter().take(SIMILAR_MAX).map(|h| h.2).collect()
}

/// `(equal title, score)` when `x` reads like the same fact as `n`. `mine` and `theirs` are title_terms, `title` is n's lowercase title.
pub(crate) fn look_alike(n: &Note, mine: &[String], title: &str, x: &Note, theirs: &[String]) -> Option<(bool, f64)> {
    let exact = crate::util::one_line(&x.title).to_lowercase() == title;
    let shared = mine.iter().filter(|a| theirs.iter().any(|b| same_term(a, b))).count();
    let tags = n.tags.iter().filter(|t| x.has_tag(t)).count().min(3);
    let cover = shared as f64 / mine.len().max(theirs.len()).max(1) as f64;
    let alike = exact || (shared >= 2 && (cover >= 0.5 || (cover >= 0.34 && tags > 0)));
    let kind = if x.kind == n.kind { 0.05 } else { 0.0 };
    alike.then_some((exact, cover + 0.1 * tags as f64 + kind))
}

pub fn similar_hint(xs: &[Note]) -> Option<String> {
    if xs.is_empty() {
        return None;
    }
    let items: Vec<String> = xs.iter().map(|x| format!("{} \"{}\"", x.id, crate::util::truncate_chars(&crate::util::one_line(&x.title), 60))).collect();
    Some(format!("possible duplicates, update one instead if it holds the same fact: {}", items.join(", ")))
}

/// A project gotcha or decision with no path glob, which `brief` and the file hook can only find by glob.
pub fn paths_hint(n: &Note) -> Option<String> {
    let unreachable = matches!(n.kind.as_str(), "gotcha" | "decision") && n.project.is_some() && n.path_globs().is_empty();
    unreachable.then(|| format!("no path glob, so `brief` and the file hook cannot find this {} when its files are edited, `breadcrumb update {} --path-glob 'src/x/**'` adds one", n.kind, n.id))
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
    /// One exact string of the body and what takes its place.
    pub replace: Option<(String, String)>,
    pub pinned: Option<bool>,
    pub status: Option<String>,
    /// Replaces all links when set.
    pub links: Option<Vec<Link>>,
    /// Ids or slugs this note replaces, added as `supersedes` links after `links`. A handoff unpins pinned handoffs among them.
    pub supersedes: Vec<String>,
    /// `glob` or `+glob` adds, `-glob` removes.
    pub paths: Vec<String>,
    /// Replaces all path globs when set, before `paths`.
    pub set_paths: Option<Vec<String>>,
    /// `type:value` or `+type:value` adds a ref, `-type:value` removes it.
    pub refs: Vec<String>,
    /// Renames the note. A retitle never does.
    pub slug: Option<String>,
    /// Actor recorded in the backlog history when the state changes.
    pub by: Option<String>,
    /// `version` of the note the caller edited. A mismatch means someone else changed it meanwhile.
    pub expected_version: Option<String>,
    /// The user edits (the web page, a terminal with no session), see `MoveReq::by_user`.
    pub by_user: bool,
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
    let before = note.clone();
    crate::merge::refuse_merged(&note)?;
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
        } else if RESERVED_KEYS.contains(&key.as_str()) || key == crate::session::SESSION_KEY || key == crate::session::LAST_SESSION_KEY || key == crate::slug::SLUG_KEY || key == crate::note::PIN_VERIFIED_KEY {
            bail!("cannot unset note metadata '{key}'");
        } else {
            note.unset_field(key);
            if key == crate::note::SAID_KEY {
                note.unset_field(crate::note::SAID_ON_KEY);
            }
        }
    }
    let (fields, field_slug) = split_slug(req.fields);
    if let Some(s) = req.slug.or(field_slug) {
        crate::slug::rename(v, &mut note, &s)?;
    }
    apply_fields(v, &mut note, &k, fields)?;
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
    if let Some((old, new)) = &req.replace {
        let found = if old.is_empty() { 0 } else { note.body.matches(old.as_str()).count() };
        if found != 1 {
            bail!("'{old}' occurs {found} times in the body of {}, nothing changed, it has to occur once", note.id);
        }
        note.body = note.body.replacen(old.as_str(), new, 1).trim_end().to_string();
    }
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
    for s in &req.supersedes {
        let to = v.resolve_id(s)?;
        if to == note.id {
            bail!("a note cannot supersede itself");
        }
        if v.get(&to).is_ok_and(|t| t.links.iter().any(|l| l.to == note.id && l.rel == SUPERSEDES)) {
            bail!("{to} already supersedes {}, a note cannot supersede the one that replaced it", note.id);
        }
        if !note.links.iter().any(|l| l.to == to && l.rel == SUPERSEDES) {
            note.links.push(Link { to, rel: SUPERSEDES.into() });
        }
    }
    k.validate(&mut note.extra)?;
    if crate::backlog::is_item(&note) {
        let new_state = crate::backlog::state(&note);
        let from = old_state.unwrap_or_else(|| new_state.clone());
        let has_resolution = note.field("resolution").and_then(yaml_scalar_string).is_some_and(|r| !r.trim().is_empty());
        if new_state == "rejected" && from != "rejected" && !has_resolution {
            bail!("moving to rejected requires a resolution explaining why, set the resolution field too");
        }
        if from == "review"
            && (new_state == "done" || new_state == "shipped")
            && let Some(why) = crate::backlog::review_hold(v, &before, req.by_user)
        {
            bail!("{} is in review and {why}", note.id);
        }
        if let Some(why) = crate::backlog::user_steps_hold(v, &before, &note, req.by_user) {
            bail!("{why}");
        }
        crate::backlog::record_state_change(&mut note, &from, &new_state, req.by.as_deref());
    }
    let mut was = before;
    was.links.clone_from(&note.links);
    was.pinned = note.pinned;
    let only_links_or_pin = was.to_markdown() == note.to_markdown();
    note.mark_verified();
    crate::session::stamp_write(&mut note);
    if !only_links_or_pin {
        note.updated = now();
    }
    let mut touched: Vec<PathBuf> = v.ensure_kind_file(&note.kind)?.into_iter().collect();
    if note.kind == crate::judge::JUDGMENT {
        touched.extend(crate::judge::upgrade_kind_file(v)?);
    }
    if !req.supersedes.is_empty() {
        touched.extend(crate::lifecycle::unpin_superseded(v, &note, v.load_notes())?);
    }
    touched.push(v.write_note(&mut note)?);
    v.commit(&touched, &commit_msg("update", &note))?;
    Ok(note)
}

pub fn append(v: &Vault, id: &str, text: &str) -> Result<Note> {
    let text = crate::session::strip_own_stamp(&crate::util::normalize_newlines(text));
    if text.is_empty() {
        bail!("nothing to append");
    }
    let text = format!("{}\n{text}", crate::session::append_head());
    let _g = v.lock()?;
    let mut note = v.get(id)?;
    crate::merge::refuse_merged(&note)?;
    note.body = if crate::backlog::is_item(&note) {
        crate::backlog::insert_before_history(&note.body, &text)
    } else if note.body.trim().is_empty() {
        text
    } else {
        format!("{}\n\n{text}", note.body.trim_end())
    };
    note.mark_verified();
    crate::session::stamp_write(&mut note);
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
        n.set_field(crate::note::PIN_VERIFIED_KEY, Value::String(crate::util::fmt_date(&crate::util::now())));
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
    crate::merge::refuse_merged(&note)?;
    let to_id = match v.resolve_id(to) {
        Ok(id) => id,
        // Links to deleted notes can still be removed, matched with the same rules as note ids.
        Err(e) if remove => match_link_target(&note, to)?.ok_or(e)?,
        Err(e) => return Err(e),
    };
    if to_id == note.id && !remove {
        bail!("a note cannot link to itself");
    }
    if !remove {
        crate::merge::refuse_merged(&v.get(&to_id)?)?;
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
    crate::session::stamp_write(&mut note);
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
    crate::session::stamp_write(&mut note);
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
    /// Only notes written at or after this time, appends, moves, claims and links count since they set `updated`.
    pub since: Option<DateTime<Utc>>,
    /// Hide notes whose latest write was by this session.
    pub others: bool,
}

/// A date (from its start, UTC), an RFC3339 time, or `2h`, `3d`, `30m` ago.
pub fn parse_since(raw: &str) -> Result<DateTime<Utc>> {
    let r = raw.trim();
    if let Some(d) = crate::lifecycle::relative(r) {
        return Ok(now() - d);
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(r, "%Y-%m-%d")
        && let Some(t) = d.and_hms_opt(0, 0, 0)
    {
        return Ok(t.and_utc());
    }
    match DateTime::parse_from_rfc3339(r) {
        Ok(t) => Ok(t.with_timezone(&Utc)),
        Err(_) => bail!("since '{r}' must be a date, an RFC3339 time, or 2h, 3d or 30m ago"),
    }
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
        "verdict" if n.kind == crate::judge::JUDGMENT => vec![crate::judge::verdict(n)],
        "about" if n.kind == crate::judge::JUDGMENT => vec![crate::judge::about(n)],
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
    let me = crate::session::current();
    for n in notes {
        if !scope.contains(&n) {
            continue;
        }
        match f.status {
            StatusFilter::Active if n.is_archived() || crate::merge::merged_into(&n).is_some() => continue,
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
        if f.since.is_some_and(|t| n.updated < t) {
            continue;
        }
        if f.others && me.is_some() && crate::session::last_writer(&n) == me {
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

/// Notes of kind env in scope that hold on `machine` (or name no machine), or on every machine, sorted by location.
pub fn env_facts(v: &Vault, scope: &Scope, machine: Option<&str>) -> Result<Vec<Note>> {
    let f = Filter { kind: Some(crate::kind::ENV.into()), ..Default::default() };
    let mut notes = filter_notes(v.load_notes(), scope, &f)?;
    if let Some(m) = machine {
        notes.retain(|n| on_machine(n, m));
    }
    let key = |n: &Note| (n.field("location").and_then(yaml_scalar_string).unwrap_or_default().to_lowercase(), n.title.to_lowercase());
    notes.sort_by_key(key);
    Ok(notes)
}

pub fn on_machine(n: &Note, machine: &str) -> bool {
    n.field("machine").and_then(yaml_scalar_string).is_none_or(|x| x.trim().eq_ignore_ascii_case(machine))
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
pub const SAID_HINT: &str = "no quote, so a pin of it reads (inferred) in the digest, `said` keeps the user's exact words";
pub const PIN_HINT: &str = "pin it if every session must see it, unpinned preferences only share one short line";
const PREF_TITLE_MAX: usize = 60;

/// Feedback and user notes hold preferences, they only reach a new session with a line of their own when pinned.
pub fn lacks_quote(n: &Note) -> bool {
    is_pref(n) && n.field(crate::note::SAID_KEY).is_none()
}

pub fn is_pref(n: &Note) -> bool {
    n.kind == "feedback" || n.kind == "user"
}

/// `global` counts unpinned global prefs a project digest leaves out, named only as `+N global` at the end.
pub fn prefs_line(prefs: &[&Note], global: usize) -> String {
    let mut s = "Prefs (unpinned):".to_string();
    let tail = if global > 0 { format!(", +{global} global") } else { String::new() };
    let room = PREFS_LINE_MAX - 12 - tail.chars().count();
    for (i, n) in prefs.iter().enumerate() {
        let item = format!(" {} {};", n.id, crate::util::truncate_chars(&crate::util::one_line(&n.title), PREF_TITLE_MAX));
        let more = prefs.len() - i;
        if s.chars().count() + item.chars().count() > room && more > 1 {
            s.push_str(&format!(" +{more} more"));
            break;
        }
        s.push_str(&item);
    }
    let s = s.trim_end_matches(';');
    if prefs.is_empty() { format!("{s}{}", tail.trim_start_matches(',')) } else { format!("{s}{tail}") }
}

pub fn project_add(v: &Vault, slug: &str, name: Option<&str>, description: Option<&str>, paths: &[String]) -> Result<(Project, bool)> {
    let slug = check_new_slug(slug)?;
    let dirs: Vec<Dir> = paths.iter().map(|p| probe(Path::new(p))).collect();
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
    for d in dirs {
        bind(v, &mut p, d, false)?;
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
    /// Projects the directory was taken from by a move.
    pub moved_from: Vec<String>,
    pub moved: Vec<Moved>,
}

/// What a move took from one project: the path, remotes, or both.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Moved {
    pub from: String,
    pub path: bool,
    pub remotes: Vec<String>,
}

struct Dir {
    ps: String,
    canonical: Option<String>,
    remotes: Vec<String>,
}

struct Bound {
    added_path: Option<String>,
    added_remotes: Vec<String>,
    moved: Vec<Moved>,
    files: Vec<PathBuf>,
}

fn probe(path: &Path) -> Dir {
    // Detection compares against the cwd as the shell reports it, so store that form. The canonical form
    // (junction or symlink target, UNC share behind a mapped drive) is recorded too when it differs.
    let abs = crate::util::absolutize(path);
    let canonical = std::fs::canonicalize(&abs).ok().map(|p| path_str(&p));
    let remotes = if crate::git::toplevel(&abs).is_some() { crate::git::remote_urls(&abs) } else { vec![] };
    Dir { ps: path_str(&abs), canonical, remotes }
}

/// Adds the directory and its remotes to `p`, the caller holds the lock and writes `p`. Without `take` a path or
/// remote another project holds is refused, with it the other projects are rewritten here.
fn bind(v: &Vault, p: &mut Project, d: Dir, take: bool) -> Result<Bound> {
    let Dir { ps, canonical, remotes } = d;
    let slug = p.slug.clone();
    let dirs: Vec<String> = std::iter::once(crate::project::normalize_path(&ps)).chain(canonical.iter().map(|c| crate::project::normalize_path(c))).collect();
    let holds = |q: &Project| q.paths.iter().any(|x| dirs.contains(&crate::project::normalize_path(x)));
    let repos: Vec<String> = remotes.iter().map(|r| crate::project::normalize_remote(r).to_ascii_lowercase()).filter(|r| !r.is_empty()).collect();
    let has_repo = |x: &String| repos.contains(&crate::project::normalize_remote(x).to_ascii_lowercase());
    let mut others: Vec<Project> = v.load_projects().into_iter().filter(|q| q.slug != slug && (holds(q) || q.remotes.iter().any(has_repo))).collect();
    others.sort_by(|a, b| a.slug.cmp(&b.slug));
    let mine = holds(p);
    let quoted = if ps.contains(' ') { format!("\"{ps}\"") } else { ps.clone() };
    let path_others: Vec<&str> = others.iter().filter(|q| holds(q)).map(|q| q.slug.as_str()).collect();
    if !take && !mine && !path_others.is_empty() {
        bail!("{ps} is already a path of project {}, `breadcrumb project link {slug} --path {quoted} --move` takes it over", path_others.join(", "));
    }
    if !take && let Some(q) = others.iter().find(|q| q.remotes.iter().any(has_repo)) {
        let r = q.remotes.iter().find(|x| has_repo(x)).cloned().unwrap_or_default();
        bail!("{ps} is a clone of {r}, which project {} already holds as its remote, `breadcrumb project link {slug} --path {quoted} --move` takes the repo over", q.slug);
    }
    let mut files: Vec<PathBuf> = vec![];
    let mut moved = vec![];
    if take {
        for o in &mut others {
            let lost = Moved { from: o.slug.clone(), path: holds(o), remotes: o.remotes.iter().filter(|x| has_repo(x)).map(|x| crate::project::normalize_remote(x)).collect() };
            o.paths.retain(|x| !dirs.contains(&crate::project::normalize_path(x)));
            o.remotes.retain(|x| !has_repo(x));
            files.push(v.write_project(o)?);
            moved.push(lost);
        }
    }
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
    Ok(Bound { added_path, added_remotes, moved, files })
}

/// Binds a directory (and its git remotes) to a project, creating the project if needed.
/// A directory another project holds is refused.
pub fn project_link(v: &Vault, slug: &str, path: &Path, description: Option<&str>) -> Result<LinkReport> {
    project_link_with(v, slug, path, description, false)
}

/// With `take`, the directory leaves the projects that hold it and goes to this one in the same commit.
/// Only the exact directory counts, a project bound to a parent or child keeps it, detection picks the longest path.
pub fn project_link_with(v: &Vault, slug: &str, path: &Path, description: Option<&str>, take: bool) -> Result<LinkReport> {
    let slug = check_new_slug(slug)?;
    let dir = probe(path);
    let _g = v.lock()?;
    let (mut p, created) = match v.load_project(&slug)? {
        Some(p) => (p, false),
        None => (Project::new(&slug, None), true),
    };
    if let Some(d) = description {
        p.description = d.to_string();
    }
    let Bound { added_path, added_remotes, moved, mut files } = bind(v, &mut p, dir, take)?;
    files.push(v.write_project(&p)?);
    v.commit(&files, &format!("breadcrumb: project link {slug}"))?;
    let moved_from = moved.iter().map(|m| m.from.clone()).collect();
    Ok(LinkReport { project: p, created, added_path, added_remotes, moved_from, moved })
}

/// Creates or updates a kind. With `replace` false, fields merge by name into the existing definition.
pub fn define_kind(v: &Vault, def: KindDef, replace: bool, remove_fields: &[String]) -> Result<(KindDef, bool)> {
    if !kind::is_valid_kind_name(&def.name) {
        bail!("invalid kind name '{}', use lowercase letters, digits, '-' or '_'", def.name);
    }
    for f in &def.fields {
        if RESERVED_KEYS.contains(&f.name.as_str()) || [crate::note::PATHS_KEY, crate::note::VERIFIED_KEY, crate::note::VERIFIED_AT_KEY, crate::note::PIN_VERIFIED_KEY, crate::note::REFS_KEY, crate::note::EXPIRES_KEY].contains(&f.name.as_str()) {
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
    if k.name == crate::judge::JUDGMENT {
        let given = k.field("verdict").map(|f| serde_json::to_value(f).ok());
        k = crate::judge::current_kind(k);
        if given.is_some_and(|g| g != k.field("verdict").map(|f| serde_json::to_value(f).ok()).unwrap_or_default()) {
            bail!("the verdict field of judgment is built in ({}), `breadcrumb judge` and the digest count by it, the kind's other fields are free to change", crate::judge::VERDICTS.join(", "));
        }
    }
    let p = v.write_kind(&k)?;
    v.commit(&[p], &format!("breadcrumb: {} kind {}", if created { "define" } else { "update" }, k.name))?;
    Ok((k, created))
}
