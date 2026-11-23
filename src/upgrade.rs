//! Self upgrade that works while other breadcrumb processes keep the installed exe open: from the source checkout
//! (pulled first), from the latest GitHub release, or with cargo install --git.

use crate::util::path_str;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const OLD_PREFIX: &str = "breadcrumb.old-";
const BINS: [&str; 2] = ["breadcrumb", "crumb"];
pub const REPO: &str = "Paraxdev/breadcrumb";
/// owner/name of the GitHub repo releases come from, for forks.
pub const REPO_ENV: &str = "BREADCRUMB_REPO";

fn repo() -> String {
    std::env::var(REPO_ENV).ok().map(|s| s.trim().to_string()).filter(|s| s.contains('/')).unwrap_or_else(|| REPO.to_string())
}

fn current_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("cannot determine the breadcrumb executable path")?;
    Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
}

/// Best effort, copies still running stay locked and are retried on a later start.
pub fn cleanup_old_exes() {
    let Ok(exe) = current_exe() else { return };
    let Some(dir) = exe.parent() else { return };
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with(OLD_PREFIX) && name.ends_with(std::env::consts::EXE_SUFFIX) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

pub fn default_source_dir() -> Option<PathBuf> {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    d.join("Cargo.toml").exists().then_some(d)
}

/// The release asset for this platform, None where no release is built.
pub fn asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("breadcrumb-windows-x64.zip"),
        ("linux", "x86_64") => Some("breadcrumb-linux-x64.tar.gz"),
        ("macos", "aarch64") => Some("breadcrumb-macos-arm64.tar.gz"),
        ("macos", "x86_64") => Some("breadcrumb-macos-x64.tar.gz"),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub enum How {
    /// The source checkout when there is one, pulled first unless `pull` is false, else the release, else git.
    Auto { from: Option<PathBuf>, pull: bool },
    Release,
    Git,
}

#[derive(Debug, serde::Serialize)]
pub struct UpgradeReport {
    pub exe: String,
    pub source: String,
    pub old: String,
    pub version: String,
    pub steps: Vec<String>,
}

fn restore(aside: &[(PathBuf, PathBuf)]) -> Result<()> {
    for (exe, old) in aside {
        let _ = std::fs::remove_file(exe);
        std::fs::rename(old, exe).with_context(|| format!("restoring {} from {}", path_str(exe), path_str(old)))?;
    }
    Ok(())
}

/// Renames the running exe and its sibling bins aside (Windows allows that, not overwriting them), then lets `install`
/// put the new ones in place. Both bins move because an install writes both, and either one can be held open by an MCP
/// server. The old ones come back when `install` fails or leaves no exe behind.
fn swap(install: impl FnOnce(&Path) -> Result<()>) -> Result<(PathBuf, String)> {
    let exe = current_exe()?;
    let dir = exe.parent().context("the breadcrumb executable has no parent directory")?.to_path_buf();
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut targets = vec![exe.clone()];
    for b in BINS {
        let p = dir.join(format!("{b}{}", std::env::consts::EXE_SUFFIX));
        if p.exists() && !targets.contains(&p) {
            targets.push(p);
        }
    }
    let mut aside = Vec::new();
    for t in &targets {
        let stem = t.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let old = t.with_file_name(format!("{OLD_PREFIX}{stem}-{secs}{}", std::env::consts::EXE_SUFFIX));
        if let Err(e) = std::fs::rename(t, &old) {
            restore(&aside)?;
            return Err(e).with_context(|| format!("renaming {} aside", path_str(t)));
        }
        aside.push((t.clone(), old));
    }
    match install(&dir) {
        Ok(()) if exe.exists() => {}
        Ok(()) => {
            restore(&aside)?;
            bail!("the install put breadcrumb somewhere other than {}, that copy was left as is and the previous one restored", path_str(&exe));
        }
        Err(e) => {
            restore(&aside)?;
            return Err(e.context("the previous breadcrumb is restored"));
        }
    }
    Ok((exe, aside.first().map(|(_, o)| path_str(o)).unwrap_or_default()))
}

fn cargo_install(dir: &Path, args: &[&str]) -> Result<()> {
    let mut c = Command::new("cargo");
    crate::util::hidden(&mut c).arg("install").args(args).arg("--force");
    // cargo puts bins in <root>/bin, so an exe living in some bin directory gets its new copy there.
    if dir.file_name().is_some_and(|n| n == "bin")
        && let Some(root) = dir.parent()
    {
        c.arg("--root").arg(root);
    }
    let status = c.stdin(Stdio::null()).stdout(Stdio::from(std::io::stderr())).status();
    match status {
        Err(e) => bail!("could not run cargo: {e}"),
        Ok(s) if !s.success() => bail!("cargo install failed ({s})"),
        Ok(_) => Ok(()),
    }
}

fn installed_version(exe: &Path) -> String {
    crate::util::hidden(&mut Command::new(exe))
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().trim_start_matches("breadcrumb").trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "?".into())
}

fn run_tool(prog: &str, args: &[&str], what: &str) -> Result<Vec<u8>> {
    let out = crate::util::hidden(&mut Command::new(prog))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("could not run {prog} to {what}, is it installed?"))?;
    if !out.status.success() {
        bail!("{prog} could not {what}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

/// The tag of the latest release, read from the GitHub API with curl.
pub fn latest_tag() -> Result<String> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", repo());
    let body = run_tool("curl", &["-fsSL", "-H", "Accept: application/vnd.github+json", "-A", "breadcrumb-upgrade", &url], "ask GitHub for the latest release")?;
    let v: serde_json::Value = serde_json::from_slice(&body).context("GitHub answered something that is not JSON")?;
    v["tag_name"].as_str().map(str::to_string).with_context(|| format!("{} has no release yet", repo()))
}

fn from_release(steps: &mut Vec<String>) -> Result<(PathBuf, String, String)> {
    let asset = asset_name().with_context(|| format!("no release is built for {} {}, use --git", std::env::consts::OS, std::env::consts::ARCH))?;
    let tag = latest_tag()?;
    steps.push(format!("latest release {tag}, this binary is v{}", env!("CARGO_PKG_VERSION")));
    let tmp = std::env::temp_dir().join(format!("breadcrumb-upgrade-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let archive = tmp.join(asset);
    let url = format!("https://github.com/{}/releases/download/{tag}/{asset}", repo());
    let result = (|| {
        run_tool("curl", &["-fsSL", "-o", &path_str(&archive), &url], &format!("download {url}"))?;
        // Note: Windows 10 and later ship bsdtar as tar.exe, which unpacks zip too.
        run_tool("tar", &["-xf", &path_str(&archive), "-C", &path_str(&tmp)], "unpack the release")?;
        let (exe, old) = swap(|dir| {
            for b in BINS {
                let name = format!("{b}{}", std::env::consts::EXE_SUFFIX);
                let src = tmp.join(&name);
                if !src.exists() {
                    bail!("the release archive {asset} has no {name}");
                }
                std::fs::copy(&src, dir.join(&name)).with_context(|| format!("copying {name} into {}", path_str(dir)))?;
            }
            Ok(())
        })?;
        Ok((exe, old, url.clone()))
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

fn from_git(git_url: &str) -> Result<(PathBuf, String, String)> {
    let (e, o) = swap(|dir| cargo_install(dir, &["--git", git_url, "breadcrumb"]))?;
    Ok((e, o, git_url.to_string()))
}

pub fn run(how: How) -> Result<UpgradeReport> {
    let mut steps = vec![];
    let git_url = format!("https://github.com/{}", repo());
    let (exe, old, source) = match how {
        How::Release => from_release(&mut steps)?,
        How::Git => from_git(&git_url)?,
        How::Auto { from, pull } => match from.map(|d| crate::util::absolutize(&d)).or_else(default_source_dir) {
            Some(src) => {
                if !src.join("Cargo.toml").exists() {
                    bail!("{} has no Cargo.toml", path_str(&src));
                }
                if pull && crate::git::is_repo_root(&src) && crate::git::origin_url(&src).is_some() {
                    match crate::git::ok(&src, &["pull", "--ff-only"]) {
                        Ok(o) => steps.push(format!("git pull in {}: {}", path_str(&src), crate::util::one_line(o.trim()))),
                        Err(e) => steps.push(format!("git pull in {} did not fast forward, building the checkout as it is: {}", path_str(&src), crate::util::one_line(&format!("{e:#}")))),
                    }
                }
                let s = path_str(&src);
                let (e, o) = swap(|dir| cargo_install(dir, &["--path", &s]))?;
                (e, o, s)
            }
            None => match from_release(&mut steps) {
                Ok(r) => r,
                Err(e) => {
                    steps.push(format!("release install failed ({}), building main with cargo", crate::util::one_line(&format!("{e:#}"))));
                    from_git(&git_url)?
                }
            },
        },
    };
    let version = installed_version(&exe);
    Ok(UpgradeReport { exe: path_str(&exe), source, old, version, steps })
}
