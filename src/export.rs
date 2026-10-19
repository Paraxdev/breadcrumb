//! Moving vaults between machines: bundles, JSONL dumps, restore, remotes and sync.

use crate::git;
use crate::util::{absolutize, path_str};
use crate::vault::{CONFIG_FILE, Config, Vault};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Git work on the vault must not fall through to an enclosing repository or into an unfinished rebase.
fn require_repo(v: &Vault) -> Result<()> {
    if !git::is_repo_root(&v.root) {
        bail!("vault at {} is not a git repository (no .git there), run `breadcrumb init` in it first", path_str(&v.root));
    }
    if !git::available() {
        bail!("git is not installed or not on PATH");
    }
    if let Some(op) = git::in_progress(&v.root) {
        bail!(
            "a git {op} is in progress in {}, finish it (resolve the conflicts, git add, git {op} --continue) or run git {op} --abort, then run sync again",
            path_str(&v.root)
        );
    }
    Ok(())
}

/// Export files written into the vault would be committed and pushed by the next sync.
fn refuse_inside_vault(v: &Vault, file: &Path) -> Result<()> {
    let canon = |p: &Path| {
        let c = std::fs::canonicalize(p).map(|c| path_str(&c)).unwrap_or_else(|_| path_str(p));
        crate::project::normalize_path(&c)
    };
    let root = canon(&v.root);
    let mut dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
    while !dir.as_os_str().is_empty() && !dir.exists() {
        dir = dir.parent().map(Path::to_path_buf).unwrap_or_default();
    }
    let d = canon(&dir);
    if d == root || d.starts_with(&format!("{}/", root.trim_end_matches('/'))) {
        bail!("{} is inside the vault, write exports somewhere else so sync does not commit them", path_str(file));
    }
    Ok(())
}

/// Stages and commits everything pending. Caller holds the lock.
fn commit_pending(v: &Vault, msg: &str) -> Result<bool> {
    git::ok(&v.root, &["add", "-A"])?;
    if git::has_staged(&v.root)? {
        git::commit(&v.root, msg)?;
        return Ok(true);
    }
    Ok(false)
}

pub fn bundle(v: &Vault, file: &Path) -> Result<PathBuf> {
    let file = absolutize(file);
    refuse_inside_vault(v, &file)?;
    require_repo(v)?;
    let _g = v.lock()?;
    commit_pending(v, "breadcrumb: commit pending changes before export")?;
    if git::commit_count(&v.root) == 0 {
        bail!("vault has no commits to bundle");
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    git::ok(&v.root, &["bundle", "create", &file.to_string_lossy(), "--all"])?;
    Ok(file)
}

pub fn jsonl(v: &Vault, file: &Path) -> Result<(PathBuf, usize)> {
    let file = absolutize(file);
    refuse_inside_vault(v, &file)?;
    let mut out = Vec::new();
    for k in v.load_kinds() {
        let mut obj = serde_json::to_value(&k)?;
        obj["record"] = "kind".into();
        writeln!(out, "{}", serde_json::to_string(&obj)?)?;
    }
    for p in v.load_projects() {
        let mut obj = serde_json::to_value(&p)?;
        obj["record"] = "project".into();
        writeln!(out, "{}", serde_json::to_string(&obj)?)?;
    }
    let notes = v.load_notes();
    for n in &notes {
        let mut obj = n.to_json();
        if let Some(p) = &n.path {
            obj["path"] = v.rel(p).into();
        }
        obj["record"] = "note".into();
        writeln!(out, "{}", serde_json::to_string(&obj)?)?;
    }
    crate::util::atomic_write(&file, &out)?;
    Ok((file, notes.len()))
}

/// Clones a bundle into an empty directory and points origin at the remote recorded in breadcrumb.toml.
pub fn restore(bundle: &Path, to: &Path) -> Result<(PathBuf, Option<String>)> {
    let bundle = absolutize(bundle);
    if !bundle.is_file() {
        bail!("bundle {} does not exist", bundle.display());
    }
    let to = absolutize(to);
    if to.is_file() {
        bail!("target {} is a file, pass --to with an empty or new directory", to.display());
    }
    let existed = to.exists();
    if existed && std::fs::read_dir(&to).with_context(|| format!("reading {}", to.display()))?.next().is_some() {
        bail!("target {} is not empty, pass --to with an empty or new directory", to.display());
    }
    let parent = to.parent().context("target has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    git::ok(parent, &["clone", "-q", &bundle.to_string_lossy(), &to.to_string_lossy()])?;
    let cfg_path = to.join(CONFIG_FILE);
    if !cfg_path.exists() {
        let _ = std::fs::remove_dir_all(&to);
        if existed {
            let _ = std::fs::create_dir_all(&to);
        }
        bail!("bundle {} has no {CONFIG_FILE}, it is not a breadcrumb vault (nothing was restored)", bundle.display());
    }
    let cfg: Config = toml::from_str(&crate::util::read_text(&cfg_path)?)?;
    match &cfg.remote {
        Some(r) => git::ok(&to, &["remote", "set-url", "origin", r])?,
        None => git::ok(&to, &["remote", "remove", "origin"])?,
    };
    Ok((to, cfg.remote))
}

pub fn remote_set(v: &mut Vault, url: &str) -> Result<()> {
    require_repo(v)?;
    let _g = v.lock()?;
    v.set_origin(url)?;
    let cfg = v.root.join(CONFIG_FILE);
    v.commit(&[cfg], &format!("breadcrumb: remote set {url}"))?;
    Ok(())
}

pub fn remote_github(v: &mut Vault, name: &str, public: bool) -> Result<String> {
    require_repo(v)?;
    if git::origin_url(&v.root).is_some() {
        bail!("vault already has an origin remote, use `breadcrumb remote set <url>` to change it");
    }
    let _g = v.lock()?;
    commit_pending(v, "breadcrumb: commit pending changes before publishing")?;
    let vis = if public { "--public" } else { "--private" };
    let out = crate::util::hidden(&mut Command::new("gh"))
        .args(["repo", "create", name, vis, "--source"])
        .arg(&v.root)
        .args(["--remote", "origin", "--push"])
        .stdin(Stdio::null())
        .output()
        .context("failed to run gh, install the GitHub CLI and run `gh auth login`")?;
    if !out.status.success() {
        bail!("gh repo create failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let url = git::origin_url(&v.root).context("gh did not configure an origin remote")?;
    v.config.remote = Some(url.clone());
    v.save_config()?;
    let cfg = v.root.join(CONFIG_FILE);
    v.commit(&[cfg], "breadcrumb: record github remote")?;
    let _ = git::ok(&v.root, &["push", "-q", "origin", "HEAD"]);
    Ok(url)
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SyncReport {
    pub committed: bool,
    pub pulled: bool,
    pub pushed: bool,
    pub remote: Option<String>,
    pub conflicts: Vec<String>,
    pub message: String,
}

pub fn sync(v: &Vault, msg: Option<&str>) -> Result<SyncReport> {
    require_repo(v)?;
    let _g = v.lock()?;
    let mut r = SyncReport { committed: commit_pending(v, msg.unwrap_or("breadcrumb: sync"))?, ..Default::default() };
    let Some(origin) = git::origin_url(&v.root) else {
        r.message = "no origin remote configured, committed locally only (use `breadcrumb remote set <url>` or `breadcrumb remote github <name>`)".into();
        return Ok(r);
    };
    r.remote = Some(origin);
    let branch = git::current_branch(&v.root).unwrap_or_else(|| "main".into());
    let heads = git::ok(&v.root, &["ls-remote", "--heads", "origin", &branch])?;
    if !heads.trim().is_empty() {
        let out = git::run(&v.root, &["pull", "--rebase", "--autostash", "-q", "origin", &branch])?;
        if !out.status.success() {
            let conflicts = git::ok(&v.root, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
            r.conflicts = conflicts.lines().map(str::to_string).filter(|s| !s.is_empty()).collect();
            // require_repo refused an unfinished rebase, so any rebase now is the one this pull started.
            if git::in_progress(&v.root) == Some("rebase") {
                let _ = git::run(&v.root, &["rebase", "--abort"]);
            }
            let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if r.conflicts.is_empty() {
                bail!("git pull failed, local commits are intact: {detail}");
            }
            bail!(
                "sync stopped: rebase conflict in {}. The rebase was aborted, your local commits are intact. To resolve, run `git pull --rebase origin {branch}` in {}, fix the conflicts, `git add` the files and run `git rebase --continue` (breadcrumb refuses to write until the rebase is finished or aborted), then run sync again",
                r.conflicts.join(", "),
                path_str(&v.root)
            );
        }
        r.pulled = true;
    }
    git::ok(&v.root, &["push", "-q", "-u", "origin", "HEAD"])?;
    r.pushed = true;
    r.message = format!("synced with origin/{branch}");
    Ok(r)
}

#[derive(Debug, Clone, Serialize)]
pub struct WhereInfo {
    pub vault: String,
    pub exists: bool,
    pub project: Option<String>,
    /// ok, not installed, or not a repository.
    pub git: String,
    pub remote: Option<String>,
    pub dirty: bool,
    pub notes: usize,
    pub commits: usize,
}

pub fn where_info(root: &Path, cwd: &Path) -> WhereInfo {
    match Vault::open(root) {
        Ok(v) => {
            let git_state = if !git::available() {
                "not installed or not on PATH".to_string()
            } else if !git::is_repo_root(root) {
                "vault is not a git repository, run `breadcrumb init`".to_string()
            } else if let Some(op) = git::in_progress(root) {
                format!("{op} in progress, finish or abort it")
            } else {
                "ok".to_string()
            };
            let usable = git::available() && git::is_repo_root(root);
            WhereInfo {
                vault: path_str(root),
                exists: true,
                project: v.detect_project(cwd),
                git: git_state,
                remote: if usable { git::origin_url(root) } else { None },
                dirty: usable && git::is_dirty(root),
                notes: v.note_files().len(),
                commits: if usable { git::commit_count(root) } else { 0 },
            }
        }
        Err(_) => WhereInfo { vault: path_str(root), exists: false, project: None, git: String::new(), remote: None, dirty: false, notes: 0, commits: 0 },
    }
}
