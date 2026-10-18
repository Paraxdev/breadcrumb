use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// Runs git with stdin closed and output captured, so it can never read or corrupt the MCP stdio stream.
pub fn run(dir: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        // Read-only commands like status must not grab index.lock while another process commits.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("failed to run git, is it installed and on PATH?")
}

pub fn ok(dir: &Path, args: &[&str]) -> Result<String> {
    let out = run(dir, args)?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim(),
            String::from_utf8_lossy(&out.stdout).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Cached per process, every mutating operation checks it.
pub fn available() -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| Command::new("git").arg("--version").stdin(Stdio::null()).output().map(|o| o.status.success()).unwrap_or(false))
}

/// A rebase, merge, cherry-pick or revert that git left unfinished in the repo at `dir`.
pub fn in_progress(dir: &Path) -> Option<&'static str> {
    let g = dir.join(".git");
    if g.join("rebase-merge").exists() || g.join("rebase-apply").exists() {
        Some("rebase")
    } else if g.join("MERGE_HEAD").exists() {
        Some("merge")
    } else if g.join("CHERRY_PICK_HEAD").exists() {
        Some("cherry-pick")
    } else if g.join("REVERT_HEAD").exists() {
        Some("revert")
    } else {
        None
    }
}

pub fn is_repo_root(dir: &Path) -> bool {
    dir.join(".git").exists()
}

pub fn toplevel(dir: &Path) -> Option<String> {
    let out = run(dir, &["rev-parse", "--show-toplevel"]).ok()?;
    if out.status.success() { Some(String::from_utf8_lossy(&out.stdout).trim().to_string()) } else { None }
}

pub fn remote_urls(dir: &Path) -> Vec<String> {
    let Ok(out) = run(dir, &["config", "--get-regexp", r"^remote\..*\.url$"]) else {
        return vec![];
    };
    if !out.status.success() {
        return vec![];
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once(' ').map(|(_, u)| u.trim().to_string()))
        .filter(|u| !u.is_empty())
        .collect()
}

pub fn origin_url(dir: &Path) -> Option<String> {
    let out = run(dir, &["remote", "get-url", "origin"]).ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
    } else {
        None
    }
}

pub fn has_staged(dir: &Path) -> Result<bool> {
    let out = run(dir, &["diff", "--cached", "--quiet"])?;
    match out.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => {
            // A repo without commits has no HEAD to diff against.
            let st = ok(dir, &["status", "--porcelain"])?;
            Ok(st.lines().any(|l| !l.starts_with("??") && !l.starts_with(' ')))
        }
    }
}

pub fn is_dirty(dir: &Path) -> bool {
    ok(dir, &["status", "--porcelain"]).map(|s| !s.trim().is_empty()).unwrap_or(false)
}

/// Commits what is staged. Falls back to a generic identity if the user has none configured.
pub fn commit(dir: &Path, msg: &str) -> Result<()> {
    let out = run(dir, &["commit", "-q", "-m", msg])?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if err.contains("user.email") || err.contains("Please tell me who you are") || err.contains("user.name") {
        ok(dir, &["-c", "user.name=breadcrumb", "-c", "user.email=breadcrumb@localhost", "commit", "-q", "-m", msg])?;
        return Ok(());
    }
    bail!("git commit failed: {}", err.trim())
}

pub fn current_branch(dir: &Path) -> Option<String> {
    ok(dir, &["symbolic-ref", "--short", "HEAD"]).ok().filter(|s| !s.is_empty())
}

pub fn commit_count(dir: &Path) -> usize {
    ok(dir, &["rev-list", "--count", "HEAD"]).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

/// Remote URLs read straight from the repo config file around `dir`, without spawning git.
pub fn config_remote_urls(dir: &Path) -> Vec<String> {
    let Some(git_dir) = dir.ancestors().map(|d| d.join(".git")).find(|g| g.exists()) else {
        return vec![];
    };
    let mut git_dir = git_dir;
    if git_dir.is_file() {
        let Ok(text) = std::fs::read_to_string(&git_dir) else { return vec![] };
        let Some(target) = text.lines().find_map(|l| l.strip_prefix("gitdir:")) else { return vec![] };
        let base = git_dir.parent().map(Path::to_path_buf).unwrap_or_default();
        git_dir = base.join(target.trim());
    }
    if let Ok(common) = std::fs::read_to_string(git_dir.join("commondir")) {
        git_dir = git_dir.join(common.trim());
    }
    let Ok(cfg) = std::fs::read_to_string(git_dir.join("config")) else { return vec![] };
    let mut in_remote = false;
    let mut out = vec![];
    for line in cfg.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_remote = l.starts_with("[remote ");
        } else if in_remote
            && let Some((k, v)) = l.split_once('=')
            && k.trim().eq_ignore_ascii_case("url")
        {
            let v = v.trim().trim_matches('"');
            if !v.is_empty() {
                out.push(v.to_string());
            }
        }
    }
    out
}

/// Commits on HEAD that the remote tracking branch of origin does not have yet.
pub fn ahead_of_origin(dir: &Path) -> usize {
    let Some(branch) = current_branch(dir) else { return 0 };
    let range = format!("origin/{branch}..HEAD");
    match ok(dir, &["rev-list", "--count", &range]) {
        Ok(s) => s.trim().parse().unwrap_or(0),
        Err(_) => {
            if origin_url(dir).is_some() { commit_count(dir) } else { 0 }
        }
    }
}
