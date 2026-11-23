//! A project's shared folder: `.breadcrumb/` at the root of its repo, committed with the code so everyone who works on
//! the repo reads the same backlog, decisions and gotchas. The vault stays the store, the folder is a copy that both
//! sides merge by note id, and the copy with the newer `updated` wins. Nothing here commits to the repo, the files go in
//! with the change they belong to.

use crate::note::{Note, yaml_scalar_string};
use crate::util::{atomic_write, path_str, read_text};
use crate::vault::Vault;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const DIR: &str = ".breadcrumb";
pub const MANIFEST: &str = "project.toml";
pub const NOTES: &str = "notes";
pub const KINDS: &str = "kinds";
/// `share: true` sends a note of any kind, `share: false` keeps one of a shared kind in the vault.
pub const SHARE_KEY: &str = "share";
/// Kinds that go to the repo unless the manifest names its own list.
pub const DEFAULT_KINDS: [&str; 12] = ["idea", "epic", "feature", "task", "bug", "todo", "decision", "gotcha", "reference", "plan", "note", "map"];
pub const AUTO: [&str; 3] = ["sync", "pull", "off"];
pub const INSTALL_PS1: &str = "irm https://raw.githubusercontent.com/Paraxdev/breadcrumb/main/install.ps1 | iex";
pub const INSTALL_SH: &str = "curl -fsSL https://raw.githubusercontent.com/Paraxdev/breadcrumb/main/install.sh | sh";

fn default_auto() -> String {
    "sync".into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub slug: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Kinds that are shared, empty means DEFAULT_KINDS.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
    /// sync: pull at session start and push at session end, pull: only pull, off: only by hand.
    #[serde(default = "default_auto")]
    pub auto: String,
    #[serde(flatten)]
    pub extra: toml::Table,
}

impl Manifest {
    pub fn kinds(&self) -> Vec<String> {
        if self.kinds.is_empty() { DEFAULT_KINDS.iter().map(|s| s.to_string()).collect() } else { self.kinds.clone() }
    }
}

pub fn manifest_path(repo: &Path) -> PathBuf {
    repo.join(DIR).join(MANIFEST)
}

pub fn load_manifest(repo: &Path) -> Result<Manifest> {
    let p = manifest_path(repo);
    let m: Manifest = toml::from_str(&read_text(&p)?).with_context(|| format!("parsing {}", path_str(&p)))?;
    if !crate::util::is_valid_slug(&m.slug) {
        bail!("{} has no valid slug", path_str(&p));
    }
    if !AUTO.contains(&m.auto.as_str()) {
        bail!("{}: auto must be {}, got '{}'", path_str(&p), AUTO.join(", "), m.auto);
    }
    Ok(m)
}

/// The nearest directory at or above `cwd` holding `.breadcrumb/project.toml`, looking no higher than the repo root,
/// without running git so the hooks stay fast.
pub fn find_repo(cwd: &Path) -> Option<PathBuf> {
    for d in cwd.ancestors() {
        if manifest_path(d).is_file() {
            return Some(d.to_path_buf());
        }
        if d.join(".git").exists() {
            return None;
        }
    }
    None
}

fn repo_or_cwd(repo: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    if let Some(r) = repo {
        return Ok(crate::util::absolutize(r));
    }
    find_repo(cwd).with_context(|| format!("no {DIR}/{MANIFEST} at or above {}, `breadcrumb share init` creates one in the repo root", path_str(cwd)))
}

fn truthy(n: &Note) -> Option<bool> {
    match n.field(SHARE_KEY).and_then(yaml_scalar_string)?.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" => Some(true),
        "false" | "no" | "0" => Some(false),
        _ => None,
    }
}

pub fn shareable(n: &Note, kinds: &[String]) -> bool {
    truthy(n).unwrap_or_else(|| kinds.iter().any(|k| *k == n.kind))
}

/// Shapes of credentials that must never reach a shared repo, the vault rule is no secrets but a repo is public reach.
pub fn secret_in(text: &str) -> Option<&'static str> {
    use std::sync::OnceLock;
    static PATTERNS: OnceLock<Vec<(&'static str, regex::Regex)>> = OnceLock::new();
    let pats = PATTERNS.get_or_init(|| {
        [
            ("a GitHub token", r"\b(gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,})"),
            ("an API key", r"\bsk-(ant-|proj-)?[A-Za-z0-9_\-]{20,}"),
            ("an AWS key", r"\bAKIA[0-9A-Z]{16}\b"),
            ("a Google API key", r"\bAIza[0-9A-Za-z_\-]{35}"),
            ("a Slack token", r"\bxox[abprs]-[A-Za-z0-9\-]{10,}"),
            ("a private key", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
            ("a password", r"\b([Pp]ass(word|wd)|PASS(WORD|WD)|[Ss]ecret|SECRET|[Tt]oken|TOKEN|[Aa]pi_?[Kk]ey|API_?KEY)\s*[:=]\s*\S{8,}"),
            ("a connection string with credentials", r"[a-z][a-z0-9+.\-]*://[^/\s:@]+:[^/\s@]+@"),
        ]
        .into_iter()
        // Note: the regex crate is built without unicode-case, so (?i) does not compile and cases are spelled out.
        .map(|(n, p)| (n, regex::Regex::new(p).expect("secret pattern compiles")))
        .collect()
    });
    pats.iter().find(|(_, r)| r.is_match(text)).map(|(n, _)| *n)
}

/// The note as it is written on one side, so a copy that only differs in its project slug reads as the same.
fn canon(n: &Note, slug: &str) -> String {
    let mut c = n.clone();
    c.project = Some(slug.to_string());
    c.path = None;
    c.to_markdown()
}

fn repo_notes(repo: &Path) -> (HashMap<String, (PathBuf, Note)>, Vec<(String, String)>) {
    let mut out = HashMap::new();
    let mut bad = vec![];
    let dir = repo.join(DIR).join(NOTES);
    let Ok(rd) = std::fs::read_dir(&dir) else { return (out, bad) };
    let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "md") && p.is_file()).collect();
    files.sort();
    for p in files {
        let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        match read_text(&p).and_then(|t| Note::parse(&t, Some(&name))) {
            Ok(n) if crate::id::is_valid_id(&n.id) => {
                out.insert(n.id.clone(), (p, n));
            }
            Ok(_) => bad.push((name, "no valid note id".into())),
            Err(e) => bad.push((name, format!("{e:#}"))),
        }
    }
    (out, bad)
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Entry {
    pub id: String,
    pub title: String,
}

fn entry(n: &Note) -> Entry {
    Entry { id: n.id.clone(), title: n.title.clone() }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Report {
    /// pull or push
    pub direction: &'static str,
    pub project: String,
    pub repo: String,
    pub dry_run: bool,
    /// The repo was bound to the project in the vault by this pull.
    pub bound: bool,
    pub added: Vec<Entry>,
    pub updated: Vec<Entry>,
    pub removed: Vec<Entry>,
    pub unchanged: usize,
    /// Notes the other side holds a newer copy of, left as they are.
    pub newer_there: Vec<Entry>,
    /// id or file name, and why it was left out.
    pub skipped: Vec<(String, String)>,
    pub kinds: Vec<String>,
}

impl Report {
    pub fn changed(&self) -> usize {
        self.added.len() + self.updated.len() + self.removed.len() + self.kinds.len()
    }

    pub fn text(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let verb = match (self.direction, self.dry_run) {
            ("pull", false) => "pulled into the vault",
            ("pull", true) => "pull would bring into the vault",
            (_, false) => "pushed to the repo",
            (_, true) => "push would write to the repo",
        };
        let _ = writeln!(
            s,
            "{}: {verb} for {}: {} new, {} updated{}, {} unchanged ({}/{DIR})",
            self.direction,
            self.project,
            self.added.len(),
            self.updated.len(),
            if self.removed.is_empty() { String::new() } else { format!(", {} removed", self.removed.len()) },
            self.unchanged,
            self.repo
        );
        if self.bound {
            let _ = writeln!(s, "  bound {} to project {} in the vault", self.repo, self.project);
        }
        for (label, list) in [("new", &self.added), ("updated", &self.updated), ("removed", &self.removed)] {
            for e in list.iter().take(20) {
                let _ = writeln!(s, "  {label} {} {}", e.id, e.title);
            }
            if list.len() > 20 {
                let _ = writeln!(s, "  +{} more {label}", list.len() - 20);
            }
        }
        for k in &self.kinds {
            let _ = writeln!(s, "  kind {k}");
        }
        if !self.newer_there.is_empty() {
            let there = if self.direction == "pull" { "the vault" } else { "the repo" };
            let fix = if self.direction == "pull" { "push" } else { "pull" };
            let ids: Vec<&str> = self.newer_there.iter().map(|e| e.id.as_str()).collect();
            let _ = writeln!(s, "  {} newer in {there}, kept: {} (`breadcrumb share {fix}` takes them)", ids.len(), ids.join(", "));
        }
        for (what, why) in &self.skipped {
            let _ = writeln!(s, "  skipped {what}: {why}");
        }
        s
    }
}

/// The vault project of the repo, bound from the manifest when the vault does not know the repo yet.
fn local_project(v: &Vault, repo: &Path, m: &Manifest, dry_run: bool) -> Result<(String, bool)> {
    if let Some(s) = v.detect_project(repo) {
        return Ok((s, false));
    }
    if dry_run {
        return Ok((m.slug.clone(), true));
    }
    let desc = (!m.description.trim().is_empty()).then_some(m.description.as_str());
    let r = crate::ops::project_link(v, &m.slug, repo, desc)?;
    if r.created && !m.name.trim().is_empty() && m.name != m.slug {
        let _g = v.lock()?;
        let mut p = r.project.clone();
        p.name = m.name.clone();
        let f = v.write_project(&p)?;
        v.commit(&[f], &format!("breadcrumb: project {} named from {DIR}", p.slug))?;
    }
    Ok((r.project.slug, true))
}

/// Loads the repo's notes into the vault: a note the vault lacks is added, one it holds an older copy of is replaced.
pub fn pull(v: &Vault, repo: &Path, dry_run: bool) -> Result<Report> {
    let m = load_manifest(repo)?;
    let (slug, bound) = local_project(v, repo, &m, dry_run)?;
    let (theirs, bad) = repo_notes(repo);
    let mut r = Report { direction: "pull", project: slug.clone(), repo: path_str(repo), dry_run, bound, ..Default::default() };
    r.skipped.extend(bad);
    let _g = if dry_run { None } else { Some(v.lock()?) };
    let mine: HashMap<String, PathBuf> = v.id_index().into_iter().map(|(id, _, p)| (id, p)).collect();
    let mut written = vec![];
    let mut ids: Vec<&String> = theirs.keys().collect();
    ids.sort();
    for id in ids {
        let (file, n) = &theirs[id];
        let text = canon(n, &slug);
        match mine.get(id) {
            None => {
                let name = file.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| n.file_name());
                let to = v.notes_dir(Some(&slug)).join(name);
                if !dry_run {
                    atomic_write(&to, text.as_bytes())?;
                    written.push(to);
                }
                r.added.push(entry(n));
            }
            Some(p) => {
                let own = v.parse_note_file(p)?;
                if own.project.as_deref() != Some(slug.as_str()) {
                    r.skipped.push((id.clone(), format!("the vault holds this id in {}, not {slug}", own.scope_label())));
                } else if canon(&own, &slug) == text {
                    r.unchanged += 1;
                } else if n.updated > own.updated {
                    if !dry_run {
                        atomic_write(p, text.as_bytes())?;
                        written.push(p.clone());
                    }
                    r.updated.push(entry(n));
                } else {
                    r.newer_there.push(entry(&own));
                }
            }
        }
    }
    for (name, text) in repo_kinds(repo) {
        if v.load_kind(&name).ok().flatten().is_some() || crate::kind::seed_kinds().iter().any(|k| k.name == name) {
            continue;
        }
        if !dry_run {
            let p = v.kinds_dir().join(format!("{name}.toml"));
            atomic_write(&p, text.as_bytes())?;
            written.push(p);
        }
        r.kinds.push(name);
    }
    if !written.is_empty() {
        v.commit(&written, &format!("breadcrumb: share pull {slug}: {} new, {} updated", r.added.len(), r.updated.len()))?;
    }
    Ok(r)
}

fn repo_kinds(repo: &Path) -> Vec<(String, String)> {
    let Ok(rd) = std::fs::read_dir(repo.join(DIR).join(KINDS)) else { return vec![] };
    let mut out: Vec<(String, String)> = rd
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_stem()?.to_string_lossy().to_string();
            (p.extension().is_some_and(|x| x == "toml") && crate::kind::is_valid_kind_name(&name)).then_some(())?;
            Some((name, read_text(&p).ok()?))
        })
        .collect();
    out.sort();
    out
}

#[derive(Debug, Default, Clone)]
pub struct PushOpts {
    /// Only these notes, any kind, unless one says share: false.
    pub ids: Vec<String>,
    pub dry_run: bool,
    /// Remove repo copies of vault notes that are no longer shared.
    pub prune: bool,
}

/// Writes the project's shared notes into the repo folder. A repo copy newer than the vault's is kept.
pub fn push(v: &Vault, repo: &Path, opts: &PushOpts) -> Result<Report> {
    let m = load_manifest(repo)?;
    let slug = match v.detect_project(repo) {
        Some(s) => s,
        None if opts.dry_run => m.slug.clone(),
        None => bail!("{} is bound to no project in the vault, `breadcrumb share pull` binds it to {}", path_str(repo), m.slug),
    };
    let kinds = m.kinds();
    let wanted: Vec<String> = opts.ids.iter().map(|i| v.resolve_id(i)).collect::<Result<_>>()?;
    let (theirs, bad) = repo_notes(repo);
    let mut r = Report { direction: "push", project: slug.clone(), repo: path_str(repo), dry_run: opts.dry_run, ..Default::default() };
    r.skipped.extend(bad);
    let notes_dir = repo.join(DIR).join(NOTES);
    let mut custom_kinds = std::collections::BTreeSet::new();
    let mut notes: Vec<Note> = v.load_notes().into_iter().filter(|n| n.project.as_deref() == Some(slug.as_str())).collect();
    notes.sort_by(|a, b| a.id.cmp(&b.id));
    for n in &notes {
        let picked = if wanted.is_empty() { shareable(n, &kinds) } else { wanted.contains(&n.id) && truthy(n) != Some(false) };
        if !picked {
            if opts.prune && wanted.is_empty()
                && let Some((p, _)) = theirs.get(&n.id)
            {
                if !opts.dry_run {
                    std::fs::remove_file(p).with_context(|| format!("removing {}", path_str(p)))?;
                }
                r.removed.push(entry(n));
            }
            continue;
        }
        let text = canon(n, &m.slug);
        if let Some(what) = secret_in(&text) {
            r.skipped.push((n.id.clone(), format!("looks like it holds {what}, take it out of the note or set share: false")));
            continue;
        }
        if !crate::kind::seed_kinds().iter().any(|k| k.name == n.kind) {
            custom_kinds.insert(n.kind.clone());
        }
        match theirs.get(&n.id) {
            None => {
                if !opts.dry_run {
                    atomic_write(&notes_dir.join(n.file_name()), text.as_bytes())?;
                }
                r.added.push(entry(n));
            }
            Some((p, t)) => {
                if canon(t, &m.slug) == text {
                    r.unchanged += 1;
                } else if t.updated > n.updated {
                    r.newer_there.push(entry(t));
                } else {
                    if !opts.dry_run {
                        atomic_write(p, text.as_bytes())?;
                    }
                    r.updated.push(entry(n));
                }
            }
        }
    }
    for k in custom_kinds {
        let Some(def) = v.load_kind(&k).ok().flatten() else { continue };
        let p = repo.join(DIR).join(KINDS).join(format!("{k}.toml"));
        let text = def.to_toml()?;
        if read_text(&p).ok().as_deref() == Some(text.as_str()) {
            continue;
        }
        if !opts.dry_run {
            atomic_write(&p, text.as_bytes())?;
        }
        r.kinds.push(k);
    }
    Ok(r)
}

pub const README: &str = r#"# .breadcrumb

Shared notes and backlog of this project, kept by [breadcrumb](https://github.com/Paraxdev/breadcrumb). Every file in
`notes/` is one note, markdown with YAML frontmatter, readable without the tool. Commit changes here with the code
change they belong to.

With breadcrumb installed, an agent session started in this repo loads these notes into your own vault (the repo gets
bound to the project in `project.toml` on first use), and with `auto = "sync"` it writes back what changed when the
session ends. Personal kinds (feedback, user, env, handoff, judgment, workflow) never come here, a note with
`share: true` does whatever its kind, one with `share: false` stays in the vault.

Install or update breadcrumb:

    Windows:      INSTALL_PS1
    macOS, Linux: INSTALL_SH

By hand: `breadcrumb share pull`, `push`, `sync` (both) and `status`.
"#;

#[derive(Debug, Clone, Serialize)]
pub struct InitReport {
    pub repo: String,
    pub created: bool,
    pub manifest: Manifest,
    pub push: Report,
}

/// Creates `.breadcrumb` in the repo root for the project, or keeps the one there, then pushes.
pub fn init(v: &Vault, project: Option<&str>, repo: Option<&Path>, auto: &str, cwd: &Path) -> Result<InitReport> {
    if !AUTO.contains(&auto) {
        bail!("auto must be {}, got '{auto}'", AUTO.join(", "));
    }
    let repo = match repo {
        Some(r) => crate::util::absolutize(r),
        None => find_repo(cwd).or_else(|| crate::git::toplevel(cwd).map(PathBuf::from)).unwrap_or_else(|| cwd.to_path_buf()),
    };
    let slug = match project {
        Some(p) => crate::workflow::resolve_project(v, Some(p), cwd)?,
        None => v.detect_project(&repo).with_context(|| format!("{} is bound to no project, pass --project <slug> or run `breadcrumb project link <slug>` there first", path_str(&repo)))?,
    };
    if v.detect_project(&repo).as_deref() != Some(slug.as_str()) {
        crate::ops::project_link(v, &slug, &repo, None)?;
    }
    let created = !manifest_path(&repo).exists();
    if created {
        let p = v.load_project(&slug)?;
        let m = Manifest {
            slug: slug.clone(),
            name: p.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| slug.clone()),
            description: p.map(|p| p.description).unwrap_or_default(),
            kinds: vec![],
            auto: auto.to_string(),
            extra: toml::Table::new(),
        };
        let body = format!(
            "# breadcrumb shared folder, see README.md. kinds = [...] replaces the shared kinds ({}), auto is sync, pull or off.\n{}",
            DEFAULT_KINDS.join(", "),
            toml::to_string_pretty(&m)?
        );
        atomic_write(&manifest_path(&repo), body.as_bytes())?;
        atomic_write(&repo.join(DIR).join("README.md"), README.replace("INSTALL_PS1", INSTALL_PS1).replace("INSTALL_SH", INSTALL_SH).as_bytes())?;
        atomic_write(&repo.join(DIR).join(".gitattributes"), b"* text=auto eol=lf\n")?;
        std::fs::create_dir_all(repo.join(DIR).join(NOTES))?;
    }
    let manifest = load_manifest(&repo)?;
    let push = push(v, &repo, &PushOpts::default())?;
    Ok(InitReport { repo: path_str(&repo), created, manifest, push })
}

pub fn pull_at(v: &Vault, repo: Option<&Path>, cwd: &Path, dry_run: bool) -> Result<Report> {
    pull(v, &repo_or_cwd(repo, cwd)?, dry_run)
}

pub fn push_at(v: &Vault, repo: Option<&Path>, cwd: &Path, opts: &PushOpts) -> Result<Report> {
    push(v, &repo_or_cwd(repo, cwd)?, opts)
}

pub fn sync_at(v: &Vault, repo: Option<&Path>, cwd: &Path, dry_run: bool) -> Result<(Report, Report)> {
    let repo = repo_or_cwd(repo, cwd)?;
    let a = pull(v, &repo, dry_run)?;
    let b = push(v, &repo, &PushOpts { dry_run, ..Default::default() })?;
    Ok((a, b))
}

/// SessionStart: pulls when the repo asks for it, the line for the digest when something came in or waits to go out.
pub fn auto_pull(root: &Path, cwd: &Path) -> Option<String> {
    let repo = find_repo(cwd)?;
    let m = load_manifest(&repo).ok()?;
    if m.auto == "off" {
        return None;
    }
    let v = Vault::open(root).ok()?;
    let mut parts = vec![];
    match pull(&v, &repo, false) {
        Ok(r) if r.changed() > 0 || r.bound => {
            let mut s = format!("pulled {} new and {} updated notes from {DIR}", r.added.len(), r.updated.len());
            if r.bound {
                s.push_str(&format!(", bound this repo to project {}", r.project));
            }
            parts.push(s);
        }
        Ok(_) => {}
        Err(e) => parts.push(format!("pull failed: {}", crate::util::one_line(&format!("{e:#}")))),
    }
    if m.auto == "pull"
        && let Ok(r) = push(&v, &repo, &PushOpts { dry_run: true, ..Default::default() })
        && r.changed() > 0
    {
        parts.push(format!("{} notes changed in the vault are not in {DIR} yet, `breadcrumb share push` writes them", r.changed()));
    }
    (!parts.is_empty()).then(|| format!("Shared ({}/{DIR}): {}\n", path_str(&repo), parts.join(", ")))
}

/// SessionEnd: with auto = sync, writes what changed this session into the repo folder.
pub fn auto_push(root: &Path, cwd: &Path) -> Option<Report> {
    let repo = find_repo(cwd)?;
    if load_manifest(&repo).ok()?.auto != "sync" {
        return None;
    }
    let v = Vault::open(root).ok()?;
    push(&v, &repo, &PushOpts::default()).ok()
}

pub fn status_text(pull: &Report, push: &Report) -> String {
    format!("{}{}", pull.text(), push.text())
}
