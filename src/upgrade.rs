//! Self upgrade that works while other breadcrumb processes keep the installed exe open.

use crate::util::path_str;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const OLD_PREFIX: &str = "breadcrumb.old-";

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

#[derive(Debug, serde::Serialize)]
pub struct UpgradeReport {
    pub exe: String,
    pub source: String,
    pub old: String,
}

/// Renames the running exe aside (Windows allows that, not overwriting it), then lets cargo install the new one.
pub fn upgrade(from: Option<&Path>) -> Result<UpgradeReport> {
    let src = match from {
        Some(d) => crate::util::absolutize(d),
        None => default_source_dir().context("the source directory this binary was built from no longer exists, pass --from DIR")?,
    };
    if !src.join("Cargo.toml").exists() {
        bail!("{} has no Cargo.toml", path_str(&src));
    }
    let exe = current_exe()?;
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let old = exe.with_file_name(format!("{OLD_PREFIX}{secs}{}", std::env::consts::EXE_SUFFIX));
    std::fs::rename(&exe, &old).with_context(|| format!("renaming {} aside", path_str(&exe)))?;
    let status = crate::util::hidden(&mut Command::new("cargo"))
        .args(["install", "--path"])
        .arg(&src)
        .arg("--force")
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .status();
    let ok = matches!(&status, Ok(s) if s.success());
    if !ok || !exe.exists() {
        if !ok {
            let _ = std::fs::remove_file(&exe);
        }
        std::fs::rename(&old, &exe).with_context(|| format!("restoring {} from {}", path_str(&exe), path_str(&old)))?;
        match status {
            Err(e) => bail!("could not run cargo: {e}"),
            Ok(s) if !s.success() => bail!("cargo install failed ({s}), the previous breadcrumb is restored"),
            Ok(_) => bail!("cargo installed breadcrumb somewhere other than {}, that copy was left as is and the previous one restored", path_str(&exe)),
        }
    }
    Ok(UpgradeReport { exe: path_str(&exe), source: path_str(&src), old: path_str(&old) })
}
