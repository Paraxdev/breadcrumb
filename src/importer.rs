//! Imports Claude Code auto-memory, plans and arbitrary markdown or text files, idempotently.

use crate::note::{Link, Note, yaml_scalar_string};
use crate::project::{self, Project};
use crate::util::{now, path_str, read_text, sha256_hex, slugify};
use crate::vault::Vault;
use anyhow::{Result, bail};
use serde_yaml::{Mapping, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoded {
    Global,
    Project { slug: String, name: String, path: Option<PathBuf> },
}

fn enc(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect()
}

/// Encodes a path the way Claude Code names its per-project directories.
pub fn encode_claude_path(p: &str) -> String {
    p.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

fn find_path(dir: &Path, rest: &str, budget: &mut usize) -> Option<PathBuf> {
    if rest.is_empty() {
        return Some(dir.to_path_buf());
    }
    if *budget == 0 {
        return None;
    }
    *budget -= 1;
    let mut entries: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| (enc(&e.file_name().to_string_lossy()), e.path()))
        .filter(|(e, _)| !e.is_empty())
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.0.len()));
    for (e, p) in entries {
        if rest == e {
            return Some(p);
        }
        if rest.len() > e.len() && rest.starts_with(&e) && rest.as_bytes()[e.len()] == b'-'
            && let Some(found) = find_path(&p, &rest[e.len() + 1..], budget) {
                return Some(found);
            }
    }
    None
}

/// Reconstructs an existing directory from an encoded name, trying multi-dash segments greedily.
pub fn reconstruct_path(name: &str) -> Option<PathBuf> {
    let lower = enc(name);
    let b = name.as_bytes();
    let (root, rest): (PathBuf, &str) = if b.len() >= 3 && b[0].is_ascii_alphabetic() && &lower[1..3] == "--" {
        if !cfg!(windows) {
            return None;
        }
        (PathBuf::from(format!("{}:\\", name[..1].to_ascii_uppercase())), &lower[3..])
    } else if lower.starts_with("--") {
        // UNC or network path: probing an unreachable host can hang for a long time, so skip it.
        return None;
    } else {
        let r = lower.strip_prefix('-')?;
        if cfg!(windows) {
            return None;
        }
        (PathBuf::from("/"), r)
    };
    let mut budget = 400;
    find_path(&root, rest, &mut budget)
}

/// Maps a Claude project directory name to a breadcrumb project, or global for the home dir and .claude itself.
pub fn decode_claude_dir_name(name: &str, home: Option<&Path>, claude_dir: Option<&Path>) -> Decoded {
    let n = enc(name);
    if let Some(h) = home
        && n == enc(&h.to_string_lossy()) {
            return Decoded::Global;
        }
    if let Some(c) = claude_dir
        && n.starts_with(&enc(&c.to_string_lossy())) {
            return Decoded::Global;
        }
    if n.contains("--claude-") || n.ends_with("--claude") {
        return Decoded::Global;
    }
    if let Some(p) = reconstruct_path(name) {
        let same = |a: &Path, b: &Path| enc(&a.to_string_lossy()) == enc(&b.to_string_lossy());
        if home.is_some_and(|h| same(&p, h)) {
            return Decoded::Global;
        }
        let base = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let slug = slugify(&base, 60);
        if !slug.is_empty() {
            return Decoded::Project { slug, name: base, path: Some(p) };
        }
    }
    let last = name.split('-').rfind(|s| !s.is_empty()).unwrap_or("");
    let slug = slugify(last, 60);
    if slug.is_empty() {
        Decoded::Global
    } else {
        Decoded::Project { slug, name: last.to_string(), path: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Created,
    Updated,
    Skipped,
}

#[derive(Debug, Clone)]
pub struct ImportItem {
    pub action: Action,
    pub project: String,
    pub id: String,
    pub title: String,
    pub source: String,
    pub warning: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct Counts {
    pub created: usize,
    pub updated: usize,
    pub skipped: usize,
}

#[derive(Debug, Default, Clone)]
pub struct ImportReport {
    pub dry_run: bool,
    pub items: Vec<ImportItem>,
    pub projects_created: Vec<String>,
}

impl ImportReport {
    pub fn counts(&self) -> BTreeMap<String, Counts> {
        let mut m: BTreeMap<String, Counts> = BTreeMap::new();
        for it in &self.items {
            let c = m.entry(it.project.clone()).or_default();
            match it.action {
                Action::Created => c.created += 1,
                Action::Updated => c.updated += 1,
                Action::Skipped => c.skipped += 1,
            }
        }
        m
    }

    pub fn table(&self) -> String {
        use std::fmt::Write;
        let mut s = String::new();
        if self.dry_run {
            let _ = writeln!(s, "dry run, nothing was written");
        }
        let _ = writeln!(s, "{:<28} {:>8} {:>8} {:>8}", "project", "created", "updated", "skipped");
        let mut tot = Counts::default();
        for (p, c) in self.counts() {
            let _ = writeln!(s, "{:<28} {:>8} {:>8} {:>8}", p, c.created, c.updated, c.skipped);
            tot.created += c.created;
            tot.updated += c.updated;
            tot.skipped += c.skipped;
        }
        let _ = writeln!(s, "{:<28} {:>8} {:>8} {:>8}", "total", tot.created, tot.updated, tot.skipped);
        for it in &self.items {
            if let Some(w) = &it.warning {
                let _ = writeln!(s, "warning: {} ({}): {w}", it.title, it.source);
            }
        }
        if !self.projects_created.is_empty() {
            let _ = writeln!(s, "new projects: {}", self.projects_created.join(", "));
        }
        s
    }

    pub fn to_json(&self) -> serde_json::Value {
        let counts: serde_json::Map<String, serde_json::Value> = self
            .counts()
            .into_iter()
            .map(|(k, c)| (k, serde_json::json!({"created": c.created, "updated": c.updated, "skipped": c.skipped})))
            .collect();
        let items: Vec<serde_json::Value> = self
            .items
            .iter()
            .map(|i| {
                serde_json::json!({
                    "action": format!("{:?}", i.action).to_lowercase(),
                    "project": i.project, "id": i.id, "title": i.title, "source": i.source, "warning": i.warning,
                })
            })
            .collect();
        serde_json::json!({"dry_run": self.dry_run, "counts": counts, "items": items, "projects_created": self.projects_created})
    }
}

#[derive(Default)]
struct Candidate {
    project: Option<String>,
    kind: String,
    title: String,
    body: String,
    tags: Vec<String>,
    extra: Mapping,
    source: String,
    hash: String,
    created: Option<chrono::DateTime<chrono::Utc>>,
    /// Kept for a new note when it is a valid id that is still free, so links from another vault keep working.
    id: Option<String>,
    links: Vec<Link>,
    pinned: bool,
    status: Option<String>,
}

struct HashEntry {
    id: String,
    project: Option<String>,
    source: Option<String>,
}

struct Importer<'a> {
    v: &'a Vault,
    dry_run: bool,
    by_hash: HashMap<String, Vec<HashEntry>>,
    by_source: HashMap<String, Note>,
    ids: HashSet<String>,
    known_projects: HashMap<String, Project>,
    touched: Vec<PathBuf>,
    report: ImportReport,
}

impl<'a> Importer<'a> {
    fn new(v: &'a Vault, dry_run: bool) -> Importer<'a> {
        let mut by_hash: HashMap<String, Vec<HashEntry>> = HashMap::new();
        let mut by_source = HashMap::new();
        let mut ids: HashSet<String> = v.id_index().into_iter().map(|(i, _, _)| i).collect();
        for n in v.load_notes() {
            ids.insert(n.id.clone());
            if let Some(h) = n.field("source_hash").and_then(yaml_scalar_string) {
                by_hash.entry(h).or_default().push(HashEntry { id: n.id.clone(), project: n.project.clone(), source: n.source.clone() });
            }
            if let Some(s) = &n.source {
                by_source.insert(s.clone(), n);
            }
        }
        let known_projects = v.load_projects().into_iter().map(|p| (p.slug.clone(), p)).collect();
        Importer { v, dry_run, by_hash, by_source, ids, known_projects, touched: vec![], report: ImportReport { dry_run, ..Default::default() } }
    }

    fn new_id(&self, wanted: Option<&str>) -> String {
        if let Some(w) = wanted
            && crate::id::is_valid_id(w)
            && !self.ids.contains(w)
        {
            return w.to_string();
        }
        loop {
            let id = crate::id::new_id();
            if !self.ids.contains(&id) {
                return id;
            }
        }
    }

    /// Picks the project for a reconstructed Claude project directory. An existing project that claims the
    /// path (by git remote or path prefix, like cwd detection) wins. Otherwise a subdirectory of a git repo
    /// maps to the repo's project, and a slug already used for an unrelated directory gets its parent's name.
    fn place_claude_dir(&self, slug: &str, name: &str, path: Option<&Path>, home: Option<&Path>) -> (String, String, Option<PathBuf>) {
        let Some(p) = path else { return (slug.to_string(), name.to_string(), None) };
        let known: Vec<Project> = self.known_projects.values().cloned().collect();
        let top = crate::git::toplevel(p).map(PathBuf::from);
        let remotes = if top.is_some() { crate::git::remote_urls(p) } else { vec![] };
        if let Some(s) = project::detect(&known, &remotes, p) {
            return (s, name.to_string(), None);
        }
        let is_home = |r: &Path| home.is_some_and(|h| project::normalize_path(&path_str(h)) == project::normalize_path(&path_str(r)));
        let root = top.filter(|r| !is_home(r)).unwrap_or_else(|| p.to_path_buf());
        let base = root.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| name.to_string());
        let mut new_slug = slugify(&base, 60);
        if new_slug.is_empty() {
            new_slug = slug.to_string();
        }
        let root_n = project::normalize_path(&path_str(&root));
        let related = |pr: &Project| {
            pr.paths.is_empty()
                || pr.paths.iter().any(|x| {
                    let x = project::normalize_path(x);
                    x == root_n || root_n.starts_with(&format!("{x}/")) || x.starts_with(&format!("{root_n}/"))
                })
        };
        let mut anc = root.parent();
        while let Some(existing) = self.known_projects.get(&new_slug) {
            if related(existing) {
                break;
            }
            let Some(a) = anc else { break };
            let an = a.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            new_slug = slugify(&format!("{an}-{new_slug}"), 60);
            anc = a.parent();
        }
        (new_slug, base, Some(root))
    }

    fn ensure_project(&mut self, slug: &str, name: &str, path: Option<&Path>) -> Result<()> {
        let mut changed = false;
        let p = self.known_projects.entry(slug.to_string()).or_insert_with(|| {
            changed = true;
            self.report.projects_created.push(slug.to_string());
            Project::new(slug, Some(name))
        });
        if let Some(path) = path {
            changed |= p.add_path(&path_str(path));
        }
        if changed && !self.dry_run {
            let f = self.v.write_project(p)?;
            self.touched.push(f);
        }
        Ok(())
    }

    fn upsert(&mut self, c: Candidate) -> Result<()> {
        let project_label = c.project.clone().unwrap_or_else(|| "global".into());
        let mut item = ImportItem {
            action: Action::Skipped,
            project: project_label,
            id: String::new(),
            title: c.title.clone(),
            source: c.source.clone(),
            warning: None,
        };
        // The same content in another project is a separate memory there, only skip it in the same place.
        if let Some(e) = self.by_hash.get(&c.hash).and_then(|es| es.iter().find(|e| e.project == c.project || e.source.as_deref() == Some(c.source.as_str()))) {
            item.id = e.id.clone();
            self.report.items.push(item);
            return Ok(());
        }
        let kind = self.v.require_kind(&c.kind)?;
        let mut note = match self.by_source.get(&c.source) {
            Some(existing) => {
                item.action = Action::Updated;
                let mut n = existing.clone();
                n.title = c.title.clone();
                n.kind = c.kind.clone();
                n.body = c.body.clone();
                for t in &c.tags {
                    if !n.has_tag(t) {
                        n.tags.push(t.clone());
                    }
                }
                // Fields come from the source again, so ones it no longer has go away. Fields the target
                // kind declares (set by a user or agent after import) stay.
                let mut extra = c.extra.clone();
                for (k, val) in &existing.extra {
                    let declared = k.as_str().is_some_and(|k| kind.field(k).is_some());
                    if declared && !extra.contains_key(k) {
                        extra.insert(k.clone(), val.clone());
                    }
                }
                n.extra = extra;
                n.updated = now();
                n
            }
            None => {
                item.action = Action::Created;
                let mut n = Note::new(&self.new_id(c.id.as_deref()), &c.title, &c.kind);
                n.project = c.project.clone();
                n.tags = c.tags.clone();
                n.body = c.body.clone();
                n.extra = c.extra.clone();
                n.links = c.links.clone();
                n.pinned = c.pinned;
                if let Some(s) = &c.status {
                    n.status = s.clone();
                }
                if let Some(t) = c.created {
                    n.created = t;
                }
                n
            }
        };
        if let Err(e) = kind.validate(&mut note.extra) {
            item.warning = Some(format!("{e:#}"));
        }
        note.source = Some(c.source.clone());
        note.set_field("source_hash", Value::String(c.hash.clone()));
        item.id = note.id.clone();
        if !self.dry_run {
            let p = self.v.write_note(&mut note)?;
            self.touched.push(p);
        }
        self.ids.insert(note.id.clone());
        self.by_hash.entry(c.hash).or_default().push(HashEntry { id: note.id.clone(), project: note.project.clone(), source: note.source.clone() });
        self.by_source.insert(c.source, note);
        self.report.items.push(item);
        Ok(())
    }

    /// A file that cannot be parsed is reported and skipped so one bad file never aborts an import.
    fn parse_or_skip(&mut self, text: &str, fname: &str, path: &Path, project: Option<&str>) -> Option<Note> {
        match Note::parse(text, Some(fname)) {
            Ok(n) => Some(n),
            Err(e) => {
                self.report.items.push(ImportItem {
                    action: Action::Skipped,
                    project: project.unwrap_or("global").to_string(),
                    id: String::new(),
                    title: fname.to_string(),
                    source: path_str(path),
                    warning: Some(format!("not imported, {e:#}")),
                });
                None
            }
        }
    }

    fn finish(mut self, what: &str) -> Result<ImportReport> {
        if !self.dry_run && !self.touched.is_empty() {
            let counts = self.report.counts();
            let created: usize = counts.values().map(|c| c.created).sum();
            let updated: usize = counts.values().map(|c| c.updated).sum();
            self.v.commit(&self.touched, &format!("breadcrumb: import {what} ({created} created, {updated} updated)"))?;
        }
        self.report.items.sort_by(|a, b| a.project.cmp(&b.project).then(a.title.cmp(&b.title)));
        Ok(self.report)
    }
}

fn take_str(m: &mut Mapping, k: &str) -> Option<String> {
    m.shift_remove(k).and_then(|v| yaml_scalar_string(&v)).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn memory_kind(t: Option<&str>) -> &'static str {
    match t.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("user") => "user",
        Some("feedback") => "feedback",
        Some("reference") => "reference",
        _ => "note",
    }
}

pub struct ClaudeOpts {
    pub claude_dir: PathBuf,
    pub dry_run: bool,
    pub project_map: Vec<(String, String)>,
}

pub fn parse_project_map(args: &[String]) -> Result<Vec<(String, String)>> {
    args.iter()
        .map(|a| match a.split_once('=') {
            Some((d, s)) => Ok((d.trim().to_string(), s.trim().to_string())),
            None => bail!("invalid --project-map '{a}', expected dirname=slug"),
        })
        .collect()
}

pub fn import_claude(v: &Vault, opts: &ClaudeOpts) -> Result<ImportReport> {
    let cdir = &opts.claude_dir;
    if !cdir.is_dir() {
        bail!("claude directory {} does not exist", cdir.display());
    }
    let home = dirs::home_dir();
    let _g = v.lock()?;
    let mut imp = Importer::new(v, opts.dry_run);

    let mut proj_dirs: Vec<PathBuf> = std::fs::read_dir(cdir.join("projects"))
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.join("memory").is_dir()).collect())
        .unwrap_or_default();
    proj_dirs.sort();
    for pd in proj_dirs {
        let mut files: Vec<PathBuf> = std::fs::read_dir(pd.join("memory"))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
            .filter(|p| !p.file_name().unwrap().to_string_lossy().eq_ignore_ascii_case("MEMORY.md"))
            .collect();
        files.sort();
        if files.is_empty() {
            continue;
        }
        let dname = pd.file_name().unwrap().to_string_lossy().to_string();
        let mapped = opts.project_map.iter().find(|(d, _)| d.eq_ignore_ascii_case(&dname)).map(|(_, s)| s.clone());
        let decoded = decode_claude_dir_name(&dname, home.as_deref(), Some(cdir));
        let project: Option<String> = match (&mapped, &decoded) {
            (Some(s), _) if s == "global" => None,
            (Some(s), d) => {
                let slug = slugify(s, 60);
                if slug.is_empty() {
                    bail!("invalid project slug '{s}' in --project-map");
                }
                let path = match d {
                    Decoded::Project { path, .. } => path.clone(),
                    Decoded::Global => None,
                };
                imp.ensure_project(&slug, s, path.as_deref())?;
                Some(slug)
            }
            (None, Decoded::Global) => None,
            (None, Decoded::Project { slug, name, path }) => {
                let (slug, name, path) = imp.place_claude_dir(slug, name, path.as_deref(), home.as_deref());
                imp.ensure_project(&slug, &name, path.as_deref())?;
                Some(slug)
            }
        };
        for f in files {
            let raw = std::fs::read(&f)?;
            let text = String::from_utf8_lossy(&raw);
            let fname = f.file_name().unwrap().to_string_lossy().to_string();
            let Some(mut n) = imp.parse_or_skip(&text, &fname, &f, project.as_deref()) else { continue };
            let mut extra = std::mem::take(&mut n.extra);
            let name = take_str(&mut extra, "name");
            let description = take_str(&mut extra, "description");
            let mut mtype = take_str(&mut extra, "type");
            if let Some(Value::Mapping(mut meta)) = extra.shift_remove("metadata") {
                if let Some(t) = take_str(&mut meta, "type") {
                    mtype = Some(t);
                }
                if !meta.is_empty() {
                    extra.insert("metadata".into(), Value::Mapping(meta));
                }
            }
            let title = name.clone().or_else(|| description.clone()).unwrap_or_else(|| n.title.clone());
            if let Some(d) = &description {
                extra.insert("summary".into(), Value::String(d.clone()));
            }
            let mut tags = n.tags.clone();
            for t in ["imported", "claude-memory"] {
                if !tags.iter().any(|x| x == t) {
                    tags.push(t.to_string());
                }
            }
            imp.upsert(Candidate {
                project: project.clone(),
                kind: memory_kind(mtype.as_deref()).to_string(),
                title,
                body: n.body,
                tags,
                extra,
                source: format!("claude:projects/{dname}/memory/{fname}"),
                hash: sha256_hex(&raw),
                ..Default::default()
            })?;
        }
    }

    let mut plans: Vec<PathBuf> = std::fs::read_dir(cdir.join("plans"))
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md")).collect())
        .unwrap_or_default();
    plans.sort();
    for f in plans {
        let raw = std::fs::read(&f)?;
        let fname = f.file_name().unwrap().to_string_lossy().to_string();
        let Some(n) = imp.parse_or_skip(&String::from_utf8_lossy(&raw), &fname, &f, None) else { continue };
        let mut extra = n.extra;
        let title = take_str(&mut extra, "name").unwrap_or(n.title);
        imp.upsert(Candidate {
            project: None,
            kind: "plan".into(),
            title,
            body: n.body,
            tags: vec!["imported".into(), "claude-plan".into()],
            extra,
            source: format!("claude:plans/{fname}"),
            hash: sha256_hex(&raw),
            ..Default::default()
        })?;
    }
    imp.finish("claude")
}

pub struct FilesOpts {
    pub path: PathBuf,
    pub project: Option<String>,
    pub kind: Option<String>,
    pub tags: Vec<String>,
    pub dry_run: bool,
}

pub fn import_files(v: &Vault, opts: &FilesOpts) -> Result<ImportReport> {
    let root = crate::util::absolutize(&opts.path);
    if !root.exists() {
        bail!("{} does not exist", root.display());
    }
    let mut files: Vec<PathBuf> = walkdir::WalkDir::new(&root)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'))
        .flatten()
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| matches!(p.extension().and_then(|x| x.to_str()).map(str::to_ascii_lowercase).as_deref(), Some("md" | "txt" | "markdown")))
        .collect();
    files.sort();
    let default_kind = opts.kind.clone().unwrap_or_else(|| v.config.default_kind.clone());
    v.require_kind(&default_kind)?;
    let _g = v.lock()?;
    let mut imp = Importer::new(v, opts.dry_run);
    if let Some(p) = &opts.project {
        imp.ensure_project(p, p, None)?;
    }
    for f in files {
        let raw = std::fs::read(&f)?;
        let fname = f.file_name().unwrap().to_string_lossy().to_string();
        let text = read_text(&f)?;
        let Some(n) = imp.parse_or_skip(&text, &fname, &f, opts.project.as_deref()) else { continue };
        let mut extra = n.extra.clone();
        let kind = if opts.kind.is_none() && n.had_frontmatter && v.load_kind(&n.kind)?.is_some() {
            n.kind.clone()
        } else {
            if n.had_frontmatter && n.kind != default_kind && n.kind != "note" {
                extra.insert("original_kind".into(), Value::String(n.kind.clone()));
            }
            default_kind.clone()
        };
        let mut tags = n.tags.clone();
        for t in opts.tags.iter().map(String::as_str).chain(["imported"]) {
            if !tags.iter().any(|x| x.eq_ignore_ascii_case(t)) {
                tags.push(t.to_string());
            }
        }
        // source holds the file path for idempotent re-imports, the file's own provenance is kept beside it.
        if let Some(src) = n.source.as_ref().filter(|s| **s != path_str(&f)) {
            extra.insert("original_source".into(), Value::String(src.clone()));
        }
        let status = (n.had_frontmatter && (n.status == "active" || n.status == "archived")).then(|| n.status.clone());
        let project = opts.project.clone();
        imp.upsert(Candidate {
            project,
            kind,
            title: n.title.clone(),
            body: n.body.clone(),
            tags,
            extra,
            source: path_str(&f),
            hash: sha256_hex(&raw),
            created: if n.had_frontmatter && !n.warnings.iter().any(|w| w.contains("timestamps")) { Some(n.created) } else { None },
            id: n.had_frontmatter.then(|| n.id.clone()),
            links: n.links.clone(),
            pinned: n.pinned,
            status,
        })?;
    }
    imp.finish("files")
}
