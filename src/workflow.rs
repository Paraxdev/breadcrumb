//! Claude Code workflow scripts (`.claude/workflows/<name>.js`) kept in the vault, since that folder is often gitignored.

use crate::note::{Note, yaml_scalar_string};
use crate::ops::{self, Scope, ScopeSel};
use crate::util::{atomic_write, normalize_newlines, now, one_line, path_str, read_text, truncate_chars};
use crate::vault::Vault;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::Serialize;
use serde_yaml::Value;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const KIND: &str = "workflow";
pub const TAG: &str = "workflow";
pub const LOCAL_DIR: &str = ".claude/workflows";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Meta {
    pub name: String,
    pub description: String,
    pub when_to_use: String,
    pub phases: Vec<String>,
}

const STR: &str = r#"(?:"((?:[^"\\]|\\.)*)"|'((?:[^'\\]|\\.)*)'|`([^`]*)`)"#;

static META_START: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"export\s+const\s+meta\b[^=]*=\s*\{").unwrap());
static PHASES_START: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?:^|[^\w$])["']?phases["']?\s*:\s*\["#).unwrap());
static TITLE: LazyLock<Regex> = LazyLock::new(|| key_regex("title"));

fn key_regex(key: &str) -> Regex {
    Regex::new(&format!(r#"(?:^|[^\w$])["']?{key}["']?\s*:\s*{STR}"#)).unwrap()
}

fn captured(c: &regex::Captures) -> String {
    let raw = c.get(1).or_else(|| c.get(2)).or_else(|| c.get(3)).map(|m| m.as_str()).unwrap_or("");
    let mut out = String::new();
    let mut it = raw.chars();
    while let Some(ch) = it.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(x) => out.push(x),
            None => {}
        }
    }
    one_line(&out)
}

/// Text from `open` (an opening bracket at `start`) to its matching close, skipping strings and comments.
/// Unbalanced input yields the rest of the text.
fn balanced(src: &str, start: usize, open: u8, close: u8) -> &str {
    let b = src.as_bytes();
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut i = start;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            if c == b'\\' {
                i += 1;
            } else if c == q {
                quote = None;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 1;
        } else if c == b'"' || c == b'\'' || c == b'`' {
            quote = Some(c);
        } else if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return &src[start..=i];
            }
        }
        i += 1;
    }
    &src[start.min(src.len())..]
}

/// Reads name, description, whenToUse and phase titles from `export const meta = {...}`.
/// Anything missing stays empty, the name falls back to `stem`.
pub fn parse_meta(src: &str, stem: &str) -> Meta {
    let mut m = Meta { name: stem.to_string(), ..Default::default() };
    let Some(start) = META_START.find(src) else {
        return m;
    };
    let block = balanced(src, start.end() - 1, b'{', b'}');
    let get = |key: &str| key_regex(key).captures(block).map(|c| captured(&c)).filter(|s| !s.is_empty());
    if let Some(n) = get("name") {
        m.name = n;
    }
    m.description = get("description").unwrap_or_default();
    m.when_to_use = get("whenToUse").or_else(|| get("when_to_use")).unwrap_or_default();
    if let Some(p) = PHASES_START.find(block) {
        let list = balanced(block, p.end() - 1, b'[', b']');
        m.phases = TITLE.captures_iter(list).map(|c| captured(&c)).filter(|s| !s.is_empty()).collect();
    }
    m
}

pub fn note_title(m: &Meta) -> String {
    let t = if m.description.is_empty() { m.name.clone() } else { format!("{}: {}", m.name, m.description) };
    truncate_chars(&one_line(&t), 120)
}

pub fn note_body(m: &Meta, file: &str) -> String {
    let mut out = String::new();
    if !m.description.is_empty() {
        let _ = writeln!(out, "{}\n", m.description);
    }
    if !m.when_to_use.is_empty() {
        let _ = writeln!(out, "When to use: {}\n", m.when_to_use);
    }
    if !m.phases.is_empty() {
        let _ = writeln!(out, "Phases:\n");
        for p in &m.phases {
            let _ = writeln!(out, "- {p}");
        }
        out.push('\n');
    }
    let _ = write!(out, "Workflow script, installed to `{LOCAL_DIR}/{file}.js` by `breadcrumb workflow install`.");
    out
}

pub fn vault_dir(v: &Vault, project: Option<&str>) -> PathBuf {
    match project {
        Some(slug) => v.project_dir(slug).join("workflows"),
        None => v.root.join("global").join("workflows"),
    }
}

fn js_files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_file() && is_js(p)).collect())
        .unwrap_or_default();
    out.sort();
    out
}

fn is_js(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("js"))
}

fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

fn same_text(a: &str, b: &str) -> bool {
    normalize_newlines(a) == normalize_newlines(b)
}

#[derive(Clone, Debug, Serialize)]
pub struct Workflow {
    pub name: String,
    pub project: Option<String>,
    pub meta: Meta,
    pub script: String,
    pub note_id: Option<String>,
    #[serde(skip)]
    pub path: PathBuf,
    #[serde(skip)]
    pub saved: Option<DateTime<Utc>>,
}

impl Workflow {
    pub fn summary(&self) -> String {
        if self.meta.description.is_empty() { self.name.clone() } else { format!("{} ({})", self.name, truncate_chars(&self.meta.description, 80)) }
    }
}

fn workflow_notes(v: &Vault) -> HashMap<(Option<String>, String), Note> {
    let mut out = HashMap::new();
    for n in v.load_notes().into_iter().filter(|n| n.kind == KIND) {
        if let Some(name) = n.field("workflow_name").and_then(yaml_scalar_string) {
            out.insert((n.project.clone(), name), n);
        }
    }
    out
}

pub fn list(v: &Vault, scope: &Scope) -> Vec<Workflow> {
    let mut dirs: Vec<(Option<String>, PathBuf)> = vec![(None, vault_dir(v, None))];
    dirs.extend(v.load_projects().into_iter().map(|p| (Some(p.slug.clone()), vault_dir(v, Some(&p.slug)))));
    let notes = workflow_notes(v);
    let mut out = vec![];
    for (project, dir) in dirs {
        let inside = match scope {
            Scope::All => true,
            Scope::Global => project.is_none(),
            Scope::Project { slug, with_global } => project.as_deref() == Some(slug.as_str()) || (project.is_none() && *with_global),
        };
        if !inside {
            continue;
        }
        for path in js_files(&dir) {
            let name = stem(&path);
            let meta = parse_meta(&read_text(&path).unwrap_or_default(), &name);
            let note = notes.get(&(project.clone(), name.clone()));
            out.push(Workflow {
                script: v.rel(&path),
                note_id: note.map(|n| n.id.clone()),
                saved: note.map(|n| n.updated),
                name,
                project: project.clone(),
                meta,
                path,
            });
        }
    }
    out
}

/// Explicit project (must exist), else the one detected for `cwd`.
pub fn resolve_project(v: &Vault, project: Option<&str>, cwd: &Path) -> Result<String> {
    if let Some(p) = project {
        return match ops::resolve_scope(v, &ScopeSel { project: Some(p.to_string()), ..Default::default() }, cwd)? {
            Scope::Project { slug, .. } => Ok(slug),
            _ => unreachable!(),
        };
    }
    v.detect_project(cwd).with_context(|| format!("no project detected for {}, pass --project <slug>", path_str(cwd)))
}

#[derive(Clone, Debug, Serialize)]
pub struct Saved {
    pub name: String,
    pub project: Option<String>,
    pub script: String,
    pub note_id: String,
    /// added, updated or unchanged
    pub action: &'static str,
}

fn want_fields(script: &str, name: &str, m: &Meta) -> [(&'static str, String); 3] {
    [("script", script.to_string()), ("workflow_name", name.to_string()), ("when_to_use", m.when_to_use.clone())]
}

/// Copies scripts into the vault and creates or refreshes their notes, in one commit. Unchanged input commits nothing.
pub fn save(v: &Vault, files: &[PathBuf], project: Option<&str>, global: bool, cwd: &Path) -> Result<Vec<Saved>> {
    if files.is_empty() {
        bail!("no workflow files given");
    }
    let explicit = match (global, project) {
        (true, _) => Some(None),
        (false, Some(p)) => Some(Some(resolve_project(v, Some(p), cwd)?)),
        _ => None,
    };
    let mut inputs = vec![];
    for f in files {
        let path = if f.is_relative() { cwd.join(f) } else { f.clone() };
        if !is_js(&path) {
            bail!("{} is not a .js workflow script", path_str(&path));
        }
        let content = normalize_newlines(&read_text(&path)?);
        let name = stem(&path);
        let proj = match &explicit {
            Some(p) => p.clone(),
            None => {
                let dir = path.parent().unwrap_or(cwd);
                let found = v.detect_project(dir).or_else(|| v.detect_project(cwd));
                Some(found.with_context(|| format!("no project detected for {}, pass --project <slug> or --global", path_str(&path)))?)
            }
        };
        inputs.push((name, proj, content));
    }
    let kd = v.require_kind(KIND)?;
    let _g = v.lock()?;
    let mut notes = workflow_notes(v);
    let mut touched = vec![];
    let mut out = vec![];
    for (name, proj, content) in inputs {
        let meta = parse_meta(&content, &name);
        let dest = vault_dir(v, proj.as_deref()).join(format!("{name}.js"));
        let script = v.rel(&dest);
        let mut changed = !read_text(&dest).is_ok_and(|old| same_text(&old, &content));
        if changed {
            atomic_write(&dest, content.as_bytes())?;
            touched.push(dest.clone());
        }
        let title = note_title(&meta);
        let body = note_body(&meta, &name);
        let fields = want_fields(&script, &name, &meta);
        let key = (proj.clone(), name.clone());
        let (mut note, added) = match notes.remove(&key) {
            Some(n) => (n, false),
            None => {
                let mut n = Note::new(&v.fresh_id(), &title, KIND);
                n.project = proj.clone();
                (n, true)
            }
        };
        let stale = added
            || note.title != title
            || note.body.trim_end() != body.trim_end()
            || !note.has_tag(TAG)
            || note.is_archived()
            || fields.iter().any(|(key, want)| note.field(key).and_then(yaml_scalar_string).as_deref() != Some(want.as_str()));
        if stale {
            note.title = title;
            note.clear_kept("title");
            note.body = body;
            if !note.has_tag(TAG) {
                note.tags.push(TAG.into());
            }
            note.status = "active".into();
            for (key, want) in fields {
                note.set_field(key, Value::String(want));
            }
            kd.validate(&mut note.extra)?;
            note.updated = now();
            touched.push(v.write_note(&mut note)?);
            changed = true;
        }
        out.push(Saved {
            name: name.clone(),
            project: proj.clone(),
            script,
            note_id: note.id.clone(),
            action: if added { "added" } else if changed { "updated" } else { "unchanged" },
        });
        notes.insert(key, note);
    }
    if !touched.is_empty() {
        let names: Vec<&str> = out.iter().filter(|s| s.action != "unchanged").map(|s| s.name.as_str()).collect();
        v.commit(&touched, &format!("breadcrumb: workflow save {}", names.join(", ")))?;
    }
    Ok(out)
}

/// `--to`, else the project root containing `cwd` when it belongs to the project, else its first recorded path on this PC.
pub fn target_dir(v: &Vault, slug: &str, to: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    if let Some(t) = to {
        return Ok(if t.is_relative() { cwd.join(t) } else { t.to_path_buf() });
    }
    let p = v.load_project(slug)?.with_context(|| format!("unknown project '{slug}'"))?;
    if v.detect_project(cwd).as_deref() == Some(slug) {
        let cwd_n = crate::project::normalize_path(&path_str(cwd));
        let best = p
            .paths
            .iter()
            .filter(|x| crate::project::path_has_prefix(&cwd_n, &crate::project::normalize_path(x)))
            .max_by_key(|x| x.len());
        if let Some(b) = best {
            return Ok(PathBuf::from(b));
        }
        if let Some(t) = crate::git::toplevel(cwd) {
            return Ok(PathBuf::from(t));
        }
        return Ok(cwd.to_path_buf());
    }
    p.paths
        .iter()
        .map(PathBuf::from)
        .find(|x| x.is_dir())
        .with_context(|| format!("project {slug} has no recorded path on this PC, pass --to <dir> or run `breadcrumb project link {slug}` in its directory"))
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct InstallReport {
    pub dir: String,
    pub installed: Vec<String>,
    pub unchanged: Vec<String>,
    /// Local copies that differ from the vault and were left alone (without force).
    pub differ: Vec<String>,
}

pub fn install(v: &Vault, slug: &str, dir: &Path, force: bool) -> Result<InstallReport> {
    let local_dir = dir.join(LOCAL_DIR);
    let mut rep = InstallReport { dir: path_str(&local_dir), ..Default::default() };
    for w in list(v, &Scope::Project { slug: slug.to_string(), with_global: false }) {
        let content = read_text(&w.path)?;
        let dest = local_dir.join(format!("{}.js", w.name));
        match std::fs::read_to_string(&dest) {
            Ok(old) if same_text(&old, &content) => rep.unchanged.push(w.name),
            Ok(_) if !force => rep.differ.push(w.name),
            _ => {
                atomic_write(&dest, content.as_bytes())?;
                rep.installed.push(w.name);
            }
        }
    }
    Ok(rep)
}

pub fn install_text(r: &InstallReport) -> String {
    let mut out = format!("{}\n", r.dir);
    if r.installed.is_empty() && r.unchanged.is_empty() && r.differ.is_empty() {
        out.push_str("no workflows stored for this project, save some with `breadcrumb workflow save <file.js>`\n");
    }
    for n in &r.installed {
        let _ = writeln!(out, "  installed  {n}");
    }
    for n in &r.unchanged {
        let _ = writeln!(out, "  same       {n}");
    }
    for n in &r.differ {
        let _ = writeln!(out, "  differs    {n} (local copy kept, use --force to overwrite)");
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Same,
    LocalNewer,
    VaultNewer,
    MissingLocally,
    OnlyLocal,
}

impl State {
    pub fn label(&self) -> &'static str {
        match self {
            State::Same => "same",
            State::LocalNewer => "local newer",
            State::VaultNewer => "vault newer",
            State::MissingLocally => "missing locally",
            State::OnlyLocal => "only local",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct FileStatus {
    pub name: String,
    pub state: State,
    pub local: String,
}

fn mtime(p: &Path) -> Option<DateTime<Utc>> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok().map(DateTime::<Utc>::from)
}

/// Vault time is when the note was last saved, file mtimes in the vault only say when git checked them out.
pub fn status(v: &Vault, slug: &str, dir: &Path) -> Result<Vec<FileStatus>> {
    let local_dir = dir.join(LOCAL_DIR);
    let stored = list(v, &Scope::Project { slug: slug.to_string(), with_global: false });
    let mut out = vec![];
    for w in &stored {
        let local = local_dir.join(format!("{}.js", w.name));
        let state = match std::fs::read_to_string(&local) {
            Err(_) => State::MissingLocally,
            Ok(text) if same_text(&text, &read_text(&w.path)?) => State::Same,
            Ok(_) => {
                let vault_time = w.saved.or_else(|| mtime(&w.path));
                if mtime(&local) > vault_time { State::LocalNewer } else { State::VaultNewer }
            }
        };
        out.push(FileStatus { name: w.name.clone(), state, local: path_str(&local) });
    }
    for p in js_files(&local_dir) {
        let name = stem(&p);
        if !stored.iter().any(|w| w.name == name) {
            out.push(FileStatus { name, state: State::OnlyLocal, local: path_str(&p) });
        }
    }
    Ok(out)
}

pub fn status_text(dir: &Path, st: &[FileStatus]) -> String {
    let mut out = format!("{}\n", path_str(&dir.join(LOCAL_DIR)));
    if st.is_empty() {
        out.push_str("no workflows in the vault or locally\n");
    }
    for s in st {
        let _ = writeln!(out, "  {:<16} {}", s.state.label(), s.name);
    }
    if st.iter().any(|s| s.state == State::LocalNewer || s.state == State::OnlyLocal) {
        out.push_str("back up local changes with `breadcrumb workflow save <file.js>`\n");
    }
    if st.iter().any(|s| s.state == State::VaultNewer || s.state == State::MissingLocally) {
        out.push_str("restore from the vault with `breadcrumb workflow install` (--force to overwrite)\n");
    }
    out
}

pub fn list_text(ws: &[Workflow], scope: &str) -> String {
    if ws.is_empty() {
        return format!("no workflows in {scope}, save one with `breadcrumb workflow save <file.js>`");
    }
    let mut out = String::new();
    for w in ws {
        let _ = writeln!(out, "{:<24} {:<16} {}", w.name, w.project.as_deref().unwrap_or("global"), w.meta.description);
    }
    out
}

pub fn save_text(saved: &[Saved]) -> String {
    let mut out = String::new();
    for s in saved {
        let _ = writeln!(out, "{:<9} {}  {}  {} ({})", s.action, s.name, s.project.as_deref().unwrap_or("global"), s.script, s.note_id);
    }
    out
}
