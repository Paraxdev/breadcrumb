use crate::git;
use crate::id;
use crate::kind::{self, KindDef};
use crate::note::Note;
use crate::project::{self, Project};
use crate::util::{atomic_write, path_str, read_text};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const CONFIG_FILE: &str = "breadcrumb.toml";
pub const LOCK_FILE: &str = ".breadcrumb.lock";
pub const STALE_GIT_LOCK: Duration = Duration::from_secs(60);

fn default_true() -> bool {
    true
}
fn default_kind() -> String {
    "note".into()
}
fn default_version() -> u32 {
    1
}
fn default_lock_timeout() -> u64 {
    10
}
fn default_stale_days() -> u64 {
    60
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default = "default_true")]
    pub auto_commit: bool,
    #[serde(default = "default_kind")]
    pub default_kind: String,
    #[serde(default = "default_lock_timeout")]
    pub lock_timeout_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    /// Notes not verified or updated for this many days are flagged as possibly stale.
    #[serde(default = "default_stale_days")]
    pub stale_days: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config { version: 1, auto_commit: true, default_kind: default_kind(), lock_timeout_secs: 10, remote: None, stale_days: default_stale_days() }
    }
}

#[derive(Clone, Debug)]
pub struct Vault {
    pub root: PathBuf,
    pub config: Config,
}

/// Holds the vault-wide write lock until dropped.
pub struct LockGuard {
    _file: File,
}

/// `--vault` flag, then BREADCRUMB_HOME, then ~/breadcrumb.
pub fn resolve_vault_path(flag: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = flag {
        return Ok(crate::util::absolutize(p));
    }
    if let Ok(p) = std::env::var("BREADCRUMB_HOME")
        && !p.trim().is_empty() {
            return Ok(crate::util::absolutize(Path::new(p.trim())));
        }
    let home = dirs::home_dir().context("cannot determine home directory, pass --vault or set BREADCRUMB_HOME")?;
    Ok(home.join("breadcrumb"))
}

#[derive(Debug, Default)]
pub struct InitReport {
    pub created_files: Vec<String>,
    pub git_initialized: bool,
    pub committed: bool,
}

impl Vault {
    pub fn open(root: &Path) -> Result<Vault> {
        let cfg = root.join(CONFIG_FILE);
        if !cfg.exists() {
            bail!("no breadcrumb vault at {} (run `breadcrumb init` first, or pass --vault / set BREADCRUMB_HOME)", root.display());
        }
        let config: Config = toml::from_str(&read_text(&cfg)?).with_context(|| format!("parsing {}", cfg.display()))?;
        Ok(Vault { root: root.to_path_buf(), config })
    }

    /// Creates a vault or fills in missing files of an existing one. Never overwrites notes or kinds.
    pub fn init(root: &Path, remote: Option<&str>) -> Result<(Vault, InitReport)> {
        fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
        let mut report = InitReport::default();
        if !git::is_repo_root(root) {
            git::ok(root, &["init", "-q"])?;
            // Older git ignores init.defaultBranch, so name the branch explicitly.
            let _ = git::ok(root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
            report.git_initialized = true;
        }
        let mut write_if_missing = |rel: &str, content: &str| -> Result<()> {
            let p = root.join(rel);
            if !p.exists() {
                atomic_write(&p, content.as_bytes())?;
                report.created_files.push(rel.to_string());
            }
            Ok(())
        };
        let cfg = Config::default();
        write_if_missing(CONFIG_FILE, &toml::to_string_pretty(&cfg)?)?;
        write_if_missing("AGENTS.md", crate::guide::AGENTS_MD)?;
        write_if_missing(".gitignore", ".local/\n*.lock\n*.tmp\n")?;
        write_if_missing(".gitattributes", "* text=auto eol=lf\n")?;
        for k in kind::seed_kinds() {
            write_if_missing(&format!("kinds/{}.toml", k.name), &k.to_toml()?)?;
        }
        write_if_missing("global/notes/.gitkeep", "")?;
        write_if_missing("projects/.gitkeep", "")?;

        let mut vault = Vault::open(root)?;
        let _g = vault.lock()?;
        if let Some(r) = remote {
            vault.set_origin(r)?;
        }
        git::ok(root, &["add", "-A"])?;
        if git::has_staged(root)? {
            git::commit(root, "breadcrumb: init vault")?;
            report.committed = true;
        }
        Ok((vault, report))
    }

    pub fn set_origin(&mut self, url: &str) -> Result<()> {
        let url = local_remote_url(url);
        let url = url.as_str();
        if git::origin_url(&self.root).is_some() {
            git::ok(&self.root, &["remote", "set-url", "origin", url])?;
        } else {
            git::ok(&self.root, &["remote", "add", "origin", url])?;
        }
        self.config.remote = Some(url.to_string());
        self.save_config()
    }

    pub fn save_config(&self) -> Result<()> {
        atomic_write(&self.root.join(CONFIG_FILE), toml::to_string_pretty(&self.config)?.as_bytes())
    }

    /// Takes the write lock and checks that git can commit, so a failing commit never follows a written file.
    pub fn lock(&self) -> Result<LockGuard> {
        let g = self.acquire_lock()?;
        self.clear_stale_index_lock();
        self.check_git_ready()?;
        Ok(g)
    }

    /// Only breadcrumb writes to the vault repo under this lock, so an old index.lock is left by a killed process.
    fn clear_stale_index_lock(&self) {
        let p = self.root.join(".git").join("index.lock");
        let Ok(age) = fs::metadata(&p).and_then(|m| m.modified()).map(|t| t.elapsed().unwrap_or_default()) else {
            return;
        };
        if age >= STALE_GIT_LOCK && fs::remove_file(&p).is_ok() {
            eprintln!("breadcrumb: removed stale {} ({}s old, left by an interrupted git process)", path_str(&p), age.as_secs());
        }
    }

    fn check_git_ready(&self) -> Result<()> {
        if !self.config.auto_commit || !self.is_git() {
            return Ok(());
        }
        if !git::available() {
            bail!("git is not installed or not on PATH, breadcrumb commits every change (install git, or set auto_commit = false in {CONFIG_FILE})");
        }
        if let Some(op) = git::in_progress(&self.root) {
            bail!(
                "a git {op} is in progress in {}, finish it (resolve the conflicts, git add, git {op} --continue) or run git {op} --abort before using breadcrumb",
                path_str(&self.root)
            );
        }
        Ok(())
    }

    fn acquire_lock(&self) -> Result<LockGuard> {
        let path = self.root.join(LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        let timeout = Duration::from_secs(self.config.lock_timeout_secs.max(1));
        let start = Instant::now();
        let mut wait = 10u64;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(LockGuard { _file: file }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if start.elapsed() >= timeout {
                        bail!("vault is busy: could not acquire {} within {}s", path.display(), timeout.as_secs());
                    }
                    let jitter = (crate::util::random_bytes::<1>()[0] % 20) as u64;
                    std::thread::sleep(Duration::from_millis(wait + jitter));
                    wait = (wait * 2).min(100);
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(e).context("locking vault"),
            }
        }
    }

    pub fn is_git(&self) -> bool {
        git::is_repo_root(&self.root)
    }

    /// Stages `paths` and commits when auto_commit is on and something changed. Call while holding the lock.
    pub fn commit(&self, paths: &[PathBuf], msg: &str) -> Result<bool> {
        if !self.config.auto_commit || !self.is_git() {
            return Ok(false);
        }
        let rels: Vec<String> = paths.iter().map(|p| self.rel(p)).collect();
        let mut args: Vec<&str> = vec!["add", "-A", "--"];
        args.extend(rels.iter().map(String::as_str));
        let run = || -> Result<bool> {
            if !rels.is_empty() {
                git::ok(&self.root, &args)?;
            }
            if !git::has_staged(&self.root)? {
                return Ok(false);
            }
            git::commit(&self.root, &crate::util::one_line(msg))?;
            Ok(true)
        };
        run().context("the change was written to disk but not committed, fix git and run `breadcrumb sync` to commit it")
    }

    pub fn rel(&self, p: &Path) -> String {
        path_str(p.strip_prefix(&self.root).unwrap_or(p))
    }

    pub fn kinds_dir(&self) -> PathBuf {
        self.root.join("kinds")
    }

    pub fn load_kinds_report(&self) -> (Vec<KindDef>, Vec<(PathBuf, String)>) {
        let mut kinds = vec![];
        let mut errors = vec![];
        if let Ok(rd) = fs::read_dir(self.kinds_dir()) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("toml") {
                    continue;
                }
                match read_text(&p).and_then(|s| KindDef::from_toml(&s)) {
                    Ok(k) if kind_file_matches(&p, &k) => kinds.push(k),
                    Ok(k) => errors.push((p.clone(), kind_name_mismatch(&p, &k))),
                    Err(err) => errors.push((p, format!("{err:#}"))),
                }
            }
        }
        // Built-in kinds newer than the vault have no file yet, require_kind accepts them all the same.
        for k in kind::seed_kinds() {
            if !self.kinds_dir().join(format!("{}.toml", k.name)).exists() {
                kinds.push(k);
            }
        }
        kinds.sort_by(|a, b| a.name.cmp(&b.name));
        (kinds, errors)
    }

    pub fn load_kinds(&self) -> Vec<KindDef> {
        self.load_kinds_report().0
    }

    pub fn load_kind(&self, name: &str) -> Result<Option<KindDef>> {
        if !kind::is_valid_kind_name(name) {
            return Ok(None);
        }
        let p = self.kinds_dir().join(format!("{name}.toml"));
        if !p.exists() {
            return Ok(None);
        }
        let k = KindDef::from_toml(&read_text(&p)?).with_context(|| format!("parsing {}", p.display()))?;
        if !kind_file_matches(&p, &k) {
            bail!("{}", kind_name_mismatch(&p, &k));
        }
        Ok(Some(k))
    }

    /// Returns the kind or an error that tells an agent how to define it. A built-in kind newer than the vault
    /// comes from the seed, `ensure_kind_file` writes it once a note uses it.
    pub fn require_kind(&self, name: &str) -> Result<KindDef> {
        match self.load_kind(name)? {
            Some(k) => Ok(k),
            None => {
                if let Some(k) = kind::seed_kinds().into_iter().find(|k| k.name == name) {
                    return Ok(k);
                }
                let names: Vec<String> = self.load_kinds().into_iter().map(|k| k.name).collect();
                let suggested = kind::suggest_kind_name(name);
                if suggested != name && names.contains(&suggested) {
                    bail!("unknown kind '{name}', did you mean '{suggested}'? Kind names are lowercase");
                }
                bail!(
                    "unknown kind '{name}'. Define it first with the kinds tool (define) or `breadcrumb kind define --name {suggested} --description ... --field name:type`, or use an existing kind: {}",
                    names.join(", ")
                )
            }
        }
    }

    /// Writes the file of a built-in kind the vault does not have yet. Call while holding the lock.
    pub fn ensure_kind_file(&self, name: &str) -> Result<Option<PathBuf>> {
        if self.load_kind(name)?.is_some() {
            return Ok(None);
        }
        match kind::seed_kinds().into_iter().find(|k| k.name == name) {
            Some(k) => Ok(Some(self.write_kind(&k)?)),
            None => Ok(None),
        }
    }

    pub fn write_kind(&self, k: &KindDef) -> Result<PathBuf> {
        let p = self.kinds_dir().join(format!("{}.toml", k.name));
        atomic_write(&p, k.to_toml()?.as_bytes())?;
        Ok(p)
    }

    pub fn project_dir(&self, slug: &str) -> PathBuf {
        self.root.join("projects").join(slug)
    }

    pub fn load_projects_report(&self) -> (Vec<Project>, Vec<(PathBuf, String)>) {
        let mut out = vec![];
        let mut errors = vec![];
        if let Ok(rd) = fs::read_dir(self.root.join("projects")) {
            for e in rd.flatten() {
                let p = e.path().join("project.toml");
                if !p.exists() {
                    continue;
                }
                match read_text(&p).and_then(|s| toml::from_str::<Project>(&s).map_err(Into::into)) {
                    Ok(pr) => out.push(pr),
                    Err(err) => errors.push((p, format!("{err:#}"))),
                }
            }
        }
        out.sort_by(|a, b| a.slug.cmp(&b.slug));
        (out, errors)
    }

    pub fn load_projects(&self) -> Vec<Project> {
        self.load_projects_report().0
    }

    pub fn load_project(&self, slug: &str) -> Result<Option<Project>> {
        let p = self.project_dir(slug).join("project.toml");
        if !p.exists() {
            return Ok(None);
        }
        Ok(Some(toml::from_str(&read_text(&p)?).with_context(|| format!("parsing {}", p.display()))?))
    }

    pub fn write_project(&self, p: &Project) -> Result<PathBuf> {
        let path = self.project_dir(&p.slug).join("project.toml");
        atomic_write(&path, toml::to_string_pretty(p)?.as_bytes())?;
        Ok(path)
    }

    pub fn detect_project(&self, cwd: &Path) -> Option<String> {
        let projects = self.load_projects();
        if projects.is_empty() {
            return None;
        }
        let remotes = if git::toplevel(cwd).is_some() { git::remote_urls(cwd) } else { vec![] };
        project::detect(&projects, &remotes, cwd)
    }

    /// Like detect_project but reads remotes from the git config file instead of running git.
    pub fn detect_project_fast(&self, cwd: &Path) -> Option<String> {
        let projects = self.load_projects();
        if projects.is_empty() {
            return None;
        }
        project::detect(&projects, &git::config_remote_urls(cwd), cwd)
    }

    pub fn notes_dir(&self, project: Option<&str>) -> PathBuf {
        match project {
            Some(slug) => self.project_dir(slug).join("notes"),
            None => self.root.join("global").join("notes"),
        }
    }

    pub fn notes_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.notes_dir(None)];
        if let Ok(rd) = fs::read_dir(self.root.join("projects")) {
            let mut ps: Vec<PathBuf> = rd.flatten().map(|e| e.path().join("notes")).filter(|p| p.is_dir()).collect();
            ps.sort();
            dirs.extend(ps);
        }
        dirs
    }

    pub fn note_files(&self) -> Vec<PathBuf> {
        let mut out = vec![];
        for d in self.notes_dirs() {
            if let Ok(rd) = fs::read_dir(&d) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|x| x.to_str()) == Some("md") && p.is_file() {
                        out.push(p);
                    }
                }
            }
        }
        out.sort();
        out
    }

    pub fn parse_note_file(&self, path: &Path) -> Result<Note> {
        let text = read_text(path)?;
        let stem = path.file_name().map(|s| s.to_string_lossy().to_string());
        let mut n = Note::parse(&text, stem.as_deref()).with_context(|| format!("parsing {}", self.rel(path)))?;
        n.path = Some(path.to_path_buf());
        if n.project.is_none()
            && let Some(slug) = self.project_of_path(path) {
                n.project = Some(slug);
                n.warnings.push("frontmatter lacks project, taken from directory".into());
            }
        if n.id.is_empty() {
            n.id = derived_id(&self.rel(path));
            n.warnings.push("no id in frontmatter or file name, using a derived one".into());
        }
        Ok(n)
    }

    pub fn project_of_path(&self, path: &Path) -> Option<String> {
        let rel = self.rel(path);
        let mut it = rel.split('/');
        if it.next()? == "projects" { it.next().map(|s| s.to_string()) } else { None }
    }

    pub fn load_notes_report(&self) -> (Vec<Note>, Vec<(PathBuf, String)>) {
        let mut notes = vec![];
        let mut errors = vec![];
        for p in self.note_files() {
            match self.parse_note_file(&p) {
                Ok(n) => notes.push(n),
                Err(e) => errors.push((p, format!("{e:#}"))),
            }
        }
        (notes, errors)
    }

    pub fn load_notes(&self) -> Vec<Note> {
        self.load_notes_report().0
    }

    /// Ids from file names, without parsing note contents.
    pub fn id_index(&self) -> Vec<(String, String, PathBuf)> {
        self.note_files()
            .into_iter()
            .filter_map(|p| {
                let name = p.file_name()?.to_string_lossy().to_string();
                let id = id::id_from_file_name(&name)?;
                Some((id, self.rel(&p), p))
            })
            .collect()
    }

    /// Unique match among (id, path) pairs: exact id first, then prefix. Ok(None) when nothing matches.
    fn pick_path(&self, prefix: &str, entries: &[(String, PathBuf)]) -> Result<Option<PathBuf>> {
        let p = prefix.trim().to_ascii_lowercase();
        if p.len() < id::MIN_PREFIX {
            bail!("id prefix '{prefix}' is too short, use at least {} characters", id::MIN_PREFIX);
        }
        let exact: Vec<&(String, PathBuf)> = entries.iter().filter(|(i, _)| *i == p).collect();
        let hits: Vec<&(String, PathBuf)> = if exact.is_empty() { entries.iter().filter(|(i, _)| i.starts_with(&p)).collect() } else { exact };
        let mut paths: Vec<&PathBuf> = hits.iter().map(|(_, path)| path).collect();
        paths.sort();
        paths.dedup();
        match paths.len() {
            0 => Ok(None),
            1 => Ok(Some(paths[0].clone())),
            _ => {
                let mut list: Vec<String> = hits.iter().map(|(i, path)| format!("  {i}  {}", self.rel(path))).collect();
                list.sort();
                list.dedup();
                if hits.iter().all(|(i, _)| *i == p) {
                    bail!("id '{p}' is used by several files, rename or delete the copies:\n{}", list.join("\n"));
                }
                bail!("id prefix '{prefix}' is ambiguous, candidates:\n{}", list.join("\n"))
            }
        }
    }

    /// File name ids first. When nothing matches, parses every note so ids that list and search print
    /// (frontmatter ids that differ from the file name, derived ids of files without one) resolve too.
    pub fn resolve_path(&self, prefix: &str) -> Result<PathBuf> {
        let by_name: Vec<(String, PathBuf)> = self.id_index().into_iter().map(|(i, _, p)| (i, p)).collect();
        if let Some(p) = self.pick_path(prefix, &by_name)? {
            return Ok(p);
        }
        let mut all: Vec<(String, PathBuf)> = self.load_notes().into_iter().filter_map(|n| Some((n.id, n.path?))).collect();
        all.extend(by_name);
        match self.pick_path(prefix, &all)? {
            Some(p) => Ok(p),
            None => bail!("no note with id '{prefix}'"),
        }
    }

    pub fn resolve_id(&self, prefix: &str) -> Result<String> {
        Ok(self.get(prefix)?.id)
    }

    pub fn get(&self, prefix: &str) -> Result<Note> {
        let p = self.resolve_path(prefix)?;
        self.parse_note_file(&p)
    }

    pub fn fresh_id(&self) -> String {
        let existing: std::collections::HashSet<String> = self.id_index().into_iter().map(|(id, _, _)| id).collect();
        loop {
            let id = id::new_id();
            if !existing.contains(&id) {
                return id;
            }
        }
    }

    /// Writes a note atomically. Existing notes keep their file name even if the title changed.
    pub fn write_note(&self, note: &mut Note) -> Result<PathBuf> {
        let path = match &note.path {
            Some(p) => p.clone(),
            None => self.notes_dir(note.project.as_deref()).join(note.file_name()),
        };
        atomic_write(&path, note.to_markdown().as_bytes())?;
        note.path = Some(path.clone());
        Ok(path)
    }
}

fn kind_file_matches(p: &Path, k: &KindDef) -> bool {
    p.file_stem().is_some_and(|s| s.to_string_lossy() == k.name)
}

fn kind_name_mismatch(p: &Path, k: &KindDef) -> String {
    format!(
        "kind file {} declares name '{}', the name must match the file name (rename the file to {}.toml or fix the name)",
        p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
        k.name,
        k.name
    )
}

/// A relative local path is given relative to the cwd, but git would resolve it against the vault.
fn local_remote_url(url: &str) -> String {
    let u = url.trim();
    if u.contains("://") || crate::project::is_scp_url(u) {
        return u.to_string();
    }
    let p = Path::new(u);
    if p.is_relative() && std::env::current_dir().is_ok_and(|c| c.join(p).exists()) {
        return path_str(&crate::util::absolutize(p));
    }
    u.to_string()
}

/// Stable fallback id for hand-written files without one, so lookups keep working across runs.
fn derived_id(rel: &str) -> String {
    let h = crate::util::sha256_hex(rel.as_bytes());
    let bytes = h.as_bytes();
    (0..id::ID_LEN).map(|i| id::ALPHABET[(bytes[i] as usize * 7 + bytes[i + 8] as usize) % 32] as char).collect()
}
