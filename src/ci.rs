//! A project's CI gate for the landing queue: the GitHub repo, workflow and branch its merges are pushed to and the jobs
//! that must pass before a pushed entry reads green. It lives in `projects/<slug>/land-ci.toml`, a file older binaries
//! never read. Runs are looked up with the GitHub CLI under a time limit, and nothing that works without gh needs it.

use crate::util::{atomic_write, one_line, short_age};
use crate::vault::Vault;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const FILE: &str = "land-ci.toml";
/// The program run in place of `gh`, tests point it at a fake.
pub const GH_ENV: &str = "BREADCRUMB_GH";
/// Seconds between the looks `land pushed` takes for a run GitHub has not started yet.
pub const PAUSE_ENV: &str = "BREADCRUMB_GH_PAUSE";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Ci {
    /// owner/name on GitHub.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repo: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub workflow: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub branch: String,
    /// Job names, a matrix job's name before ` (`, or a prefix ending in `*`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gate: Vec<String>,
    #[serde(flatten)]
    pub extra: toml::Table,
}

pub fn path(v: &Vault, slug: &str) -> PathBuf {
    v.project_dir(slug).join(FILE)
}

/// The project's CI gate, None when it has none.
pub fn load(v: &Vault, slug: &str) -> Result<Option<Ci>> {
    let p = path(v, slug);
    if !p.exists() {
        return Ok(None);
    }
    let ci: Ci = toml::from_str(&crate::util::read_text(&p)?).map_err(|e| anyhow::anyhow!("{} is not valid ({}), fix it or set it again with `crumb land ci`", crate::util::path_str(&p), one_line(e.message())))?;
    Ok(Some(ci))
}

pub fn save(v: &Vault, slug: &str, ci: Option<&Ci>) -> Result<()> {
    let p = path(v, slug);
    let _g = v.lock()?;
    match ci {
        Some(ci) => atomic_write(&p, toml::to_string(ci)?.as_bytes())?,
        None if p.exists() => std::fs::remove_file(&p).with_context(|| format!("removing {}", p.display()))?,
        None => return Ok(()),
    }
    v.commit(&[p], &format!("breadcrumb: land ci {slug}"))?;
    Ok(())
}

impl Ci {
    pub fn text(&self) -> String {
        let or = |s: &str, none: &str| if s.is_empty() { none.to_string() } else { s.to_string() };
        format!(
            "repo {}, workflow {}, branch {}, gate jobs {}",
            or(&self.repo, "unset"),
            or(&self.workflow, "any"),
            or(&self.branch, "any"),
            if self.gate.is_empty() { "unset".to_string() } else { self.gate.join(", ") }
        )
    }
}

#[derive(Debug)]
pub enum GhError {
    Missing,
    Auth,
    Slow(Duration),
    Failed(String),
}

impl std::fmt::Display for GhError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GhError::Missing => write!(f, "gh (the GitHub CLI) is not installed or not on PATH"),
            GhError::Auth => write!(f, "gh is not logged in, `gh auth login` fixes it"),
            GhError::Slow(d) => write!(f, "gh did not answer within {}s, offline?", d.as_secs()),
            GhError::Failed(m) => write!(f, "gh failed: {m}"),
        }
    }
}

fn program() -> String {
    std::env::var(GH_ENV).ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "gh".to_string())
}

/// Runs gh with `args`, killed after `limit`, with stdin closed so it never prompts.
fn gh(args: &[&str], limit: Duration) -> Result<Vec<u8>, GhError> {
    use std::io::Read;
    #[cfg(windows)]
    crate::git::keep_own_output();
    let mut cmd = Command::new(program());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let spawned = crate::util::hidden(&mut cmd)
        .args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(GhError::Missing),
        Err(e) => return Err(GhError::Failed(e.to_string())),
    };
    let pipe = |r: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel();
        if let Some(mut r) = r {
            std::thread::spawn(move || {
                let mut buf = vec![];
                let _ = r.read_to_end(&mut buf);
                let _ = tx.send(buf);
            });
        }
        rx
    };
    let out = pipe(child.stdout.take().map(|o| Box::new(o) as Box<dyn Read + Send>));
    let err = pipe(child.stderr.take().map(|o| Box::new(o) as Box<dyn Read + Send>));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                crate::git::kill_tree(&mut child);
                return Err(GhError::Slow(limit));
            }
        }
    };
    let left = limit.saturating_sub(start.elapsed()).max(Duration::from_millis(200));
    let stdout = out.recv_timeout(left).unwrap_or_default();
    let stderr = String::from_utf8_lossy(&err.recv_timeout(Duration::from_millis(200)).unwrap_or_default()).to_string();
    if status.success() {
        return Ok(stdout);
    }
    if status.code() == Some(4) || stderr.contains("auth login") {
        return Err(GhError::Auth);
    }
    let first = stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("no error text").to_string();
    Err(GhError::Failed(first))
}

fn parse<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, GhError> {
    serde_json::from_slice(bytes).map_err(|e| GhError::Failed(format!("its output did not read as JSON ({e})")))
}

/// A row of `gh run list`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRow {
    pub database_id: u64,
    pub head_sha: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub conclusion: String,
}

/// `gh run view --json`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    #[serde(default = "first")]
    pub attempt: u32,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub conclusion: String,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub jobs: Vec<Job>,
}

fn first() -> u32 {
    1
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub conclusion: String,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
}

/// The newest runs of the project's workflow on its branch.
pub fn runs(ci: &Ci, limit: Duration) -> Result<Vec<RunRow>, GhError> {
    let mut args = vec!["run", "list", "-R", ci.repo.as_str(), "-L", "30", "--json", "databaseId,headSha,status,conclusion"];
    if !ci.workflow.is_empty() {
        args.extend(["-w", ci.workflow.as_str()]);
    }
    if !ci.branch.is_empty() {
        args.extend(["-b", ci.branch.as_str()]);
    }
    parse(&gh(&args, limit)?)
}

/// The newest run whose commit is one of `shas`, given by any leading part.
pub fn run_for<'a>(rows: &'a [RunRow], shas: &[&str]) -> Option<&'a RunRow> {
    rows.iter().find(|r| shas.iter().any(|s| !s.is_empty() && r.head_sha.starts_with(&s.to_ascii_lowercase())))
}

/// One run with its jobs, its latest attempt or the one given.
pub fn view(ci: &Ci, id: &str, attempt: Option<u32>, limit: Duration) -> Result<Run, GhError> {
    let n = attempt.map(|a| a.to_string()).unwrap_or_default();
    let mut args = vec!["run", "view", id];
    if attempt.is_some() {
        args.extend(["--attempt", n.as_str()]);
    }
    args.extend(["-R", ci.repo.as_str(), "--json", "attempt,status,conclusion,startedAt,jobs"]);
    parse(&gh(&args, limit)?)
}

/// Seconds between looks for a run GitHub has not started yet.
pub fn pause() -> Duration {
    Duration::from_secs(std::env::var(PAUSE_ENV).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(5))
}

/// A gate entry names a job, a matrix job's legs (`bundle` is `bundle (macos-15)` too), or every job a prefix ending in `*` starts.
pub fn matches(pat: &str, name: &str) -> bool {
    match pat.strip_suffix('*') {
        Some(pre) => name.starts_with(pre),
        None => name == pat || name.strip_prefix(pat).is_some_and(|r| r.starts_with(" (")),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    Going,
    Green,
    /// The gate jobs that failed, with how.
    Red(Vec<String>),
    Cancelled,
}

impl Verdict {
    pub fn word(&self) -> &'static str {
        match self {
            Verdict::Going => "going",
            Verdict::Green => "green",
            Verdict::Red(_) => "red",
            Verdict::Cancelled => "cancelled",
        }
    }
}

pub struct Gate {
    /// One line per gate job, `name: state`.
    pub lines: Vec<String>,
    pub verdict: Verdict,
}

fn job_state(j: &Job) -> String {
    if j.status == "completed" {
        return if j.conclusion.is_empty() { "completed".to_string() } else { j.conclusion.clone() };
    }
    let age = j.started_at.filter(|t| t.timestamp() > 0 && j.status == "in_progress").map(|t| format!(" {}", short_age(&t))).unwrap_or_default();
    format!("{}{age}", j.status.replace('_', " "))
}

/// Reads the gate off a run. A failed gate job decides red at once, a gate job that never ran in a finished run is red
/// too, and green needs every gate job finished with success.
pub fn gate(pats: &[String], run: &Run) -> Gate {
    let done = run.status == "completed";
    let (mut lines, mut red, mut never) = (vec![], vec![], vec![]);
    let (mut going, mut cancelled) = (false, false);
    for p in pats {
        let jobs: Vec<&Job> = run.jobs.iter().filter(|j| matches(p, &j.name)).collect();
        if jobs.is_empty() {
            lines.push(format!("{p}: {}", if done { "never ran" } else { "not started" }));
            if done {
                never.push(format!("{p} never ran"));
            } else {
                going = true;
            }
        }
        for j in jobs {
            lines.push(format!("{}: {}", j.name, job_state(j)));
            match (j.status == "completed", j.conclusion.as_str()) {
                (false, _) => going = true,
                (true, "success" | "neutral") => {}
                (true, "cancelled") => cancelled = true,
                (true, how) => red.push(format!("{} {}", j.name, if how.is_empty() { "failed" } else { how })),
            }
        }
    }
    let verdict = if !red.is_empty() {
        Verdict::Red(red)
    } else if cancelled || (done && run.conclusion == "cancelled" && !never.is_empty()) {
        Verdict::Cancelled
    } else if !never.is_empty() {
        Verdict::Red(never)
    } else if going {
        Verdict::Going
    } else {
        Verdict::Green
    };
    Gate { lines, verdict }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(status: &str, jobs: &[(&str, &str, &str)]) -> Run {
        let jobs: Vec<serde_json::Value> = jobs.iter().map(|(n, s, c)| serde_json::json!({"name": n, "status": s, "conclusion": c, "startedAt": "2026-11-17T13:44:24Z", "completedAt": "0001-01-01T00:00:00Z", "databaseId": 1, "url": "u"})).collect();
        serde_json::from_value(serde_json::json!({"attempt": 1, "status": status, "conclusion": "", "startedAt": "2026-11-17T13:44:20Z", "jobs": jobs})).unwrap()
    }

    fn pats() -> Vec<String> {
        ["test", "rust-geom", "bundle"].map(String::from).to_vec()
    }

    #[test]
    fn names_and_matrix_legs_and_prefixes_match() {
        assert!(matches("bundle", "bundle (macos-15)"));
        assert!(matches("bundle", "bundle"));
        assert!(!matches("bundle", "bundles"));
        assert!(!matches("test", "tracer-macos"));
        assert!(!matches("e2e", "e2e-heavy"));
        assert!(matches("e2e*", "e2e-heavy"));
    }

    #[test]
    fn the_gate_waits_for_jobs_not_started_and_skips_other_jobs() {
        let r = run("in_progress", &[("test", "completed", "success"), ("rust-geom", "in_progress", ""), ("e2e", "completed", "failure")]);
        let g = gate(&pats(), &r);
        assert_eq!(g.verdict, Verdict::Going);
        assert!(g.lines.iter().any(|l| l.starts_with("rust-geom: in progress")), "{:?}", g.lines);
        assert!(g.lines.contains(&"bundle: not started".to_string()));
        assert!(!g.lines.iter().any(|l| l.starts_with("e2e")));
    }

    #[test]
    fn a_failed_gate_job_decides_red_early_and_green_needs_every_leg() {
        let r = run("in_progress", &[("test", "completed", "failure"), ("rust-geom", "in_progress", "")]);
        assert_eq!(gate(&pats(), &r).verdict, Verdict::Red(vec!["test failure".to_string()]));
        let legs = [("test", "completed", "success"), ("rust-geom", "completed", "success"), ("bundle (macos-15)", "completed", "success"), ("bundle (windows-latest)", "in_progress", "")];
        assert_eq!(gate(&pats(), &run("in_progress", &legs)).verdict, Verdict::Going);
        let mut all = legs.to_vec();
        all[3] = ("bundle (windows-latest)", "completed", "success");
        all.push(("deb-install (debian:12, deb)", "completed", "failure"));
        assert_eq!(gate(&pats(), &run("completed", &all)).verdict, Verdict::Green);
    }

    #[test]
    fn a_finished_run_without_a_gate_job_is_red_and_a_cancelled_one_is_cancelled() {
        let r = run("completed", &[("test", "completed", "success"), ("rust-geom", "completed", "success")]);
        assert_eq!(gate(&pats(), &r).verdict, Verdict::Red(vec!["bundle never ran".to_string()]));
        let c = run("completed", &[("test", "completed", "success"), ("rust-geom", "completed", "cancelled")]);
        assert_eq!(gate(&pats(), &c).verdict, Verdict::Cancelled);
    }

    #[test]
    fn run_list_rows_match_by_a_leading_sha() {
        let rows: Vec<RunRow> = serde_json::from_str(r#"[{"attempt":1,"conclusion":"","databaseId":44095728300,"headSha":"874b0465297dba6fd8d27715ed5103f6d1035f05","status":"in_progress"},{"attempt":1,"conclusion":"success","databaseId":44090955822,"headSha":"ce6ccbc2e86d61700744c83b6faf01aa4fe0e831","status":"completed"}]"#).unwrap();
        assert_eq!(run_for(&rows, &["ce6ccbc2e"]).map(|r| r.database_id), Some(44090955822));
        assert!(run_for(&rows, &["abcdef1"]).is_none());
    }
}
