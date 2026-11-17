//! The landing queue's CI gate, with a fake gh that prints canned `gh run list` and `gh run view --json` output.

mod common;

use breadcrumb::land::{self, Entry};
use breadcrumb::vault::Vault;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Env {
    _d: tempfile::TempDir,
    v: Vault,
    gh: PathBuf,
    fake: PathBuf,
}

const S1: &str = "1111111111111111111111111111111111111111";
const S2: &str = "2222222222222222222222222222222222222222";
const RUN: &str = "44095728300";

fn setup() -> Env {
    let (d, v) = common::vault();
    common::add(&v, "Registers fx", "task", Some("fx"), &[("state", "backlog")]);
    let fake = d.path().join("fakegh");
    std::fs::create_dir_all(&fake).unwrap();
    let gh = if cfg!(windows) {
        let p = fake.join("gh.cmd");
        let script = "@echo off\r\nif exist \"%~dp0fail.txt\" (\r\n  type \"%~dp0fail.txt\" 1>&2\r\n  exit /b 4\r\n)\r\nif \"%2\"==\"list\" (\r\n  type \"%~dp0list.json\"\r\n  exit /b 0\r\n)\r\nif \"%4\"==\"--attempt\" (\r\n  type \"%~dp0view-%3-%5.json\"\r\n  exit /b 0\r\n)\r\ntype \"%~dp0view-%3.json\"\r\n";
        std::fs::write(&p, script).unwrap();
        p
    } else {
        let p = fake.join("gh");
        let script = "#!/bin/sh\nd=$(dirname \"$0\")\nif [ -f \"$d/fail.txt\" ]; then cat \"$d/fail.txt\" >&2; exit 4; fi\nif [ \"$2\" = list ]; then cat \"$d/list.json\"; exit 0; fi\nif [ \"$4\" = --attempt ]; then cat \"$d/view-$3-$5.json\"; exit 0; fi\ncat \"$d/view-$3.json\"\n";
        std::fs::write(&p, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    };
    Env { _d: d, v, gh, fake }
}

fn run_with(e: &Env, gh: &Path, args: &[&str]) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_crumb"))
        .args(args)
        .current_dir(&e.v.root)
        .env("BREADCRUMB_HOME", &e.v.root)
        .env("BREADCRUMB_SESSION", "worker")
        .env("BREADCRUMB_GH", gh)
        .env("BREADCRUMB_GH_PAUSE", "0")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .output()
        .unwrap();
    (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

fn ok(e: &Env, args: &[&str]) -> String {
    let (fine, out) = run_with(e, &e.gh, args);
    assert!(fine, "{args:?}: {out}");
    out
}

fn fails(e: &Env, args: &[&str]) -> String {
    let (fine, out) = run_with(e, &e.gh, args);
    assert!(!fine, "{args:?} should fail: {out}");
    out
}

fn land(e: &Env, args: &[&str]) -> String {
    let mut all = vec!["land", "-p", "fx"];
    all.extend_from_slice(args);
    ok(e, &all)
}

fn entries(e: &Env) -> Vec<Entry> {
    std::fs::read_to_string(e.v.project_dir("fx").join(land::FILE)).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn set_ci(e: &Env) {
    land(e, &["ci", "--repo", "Paraxdev/FundaCAD", "--workflow", "build", "--branch", "dev", "--gate", "test,rust-geom,bundle"]);
}

fn list_json(e: &Env, rows: &[(&str, &str, &str, &str)]) {
    let rows: Vec<serde_json::Value> = rows.iter().map(|(id, sha, status, conclusion)| serde_json::json!({"attempt": 1, "conclusion": conclusion, "createdAt": "2026-11-17T13:44:20Z", "databaseId": id.parse::<u64>().unwrap(), "event": "push", "headBranch": "dev", "headSha": sha, "status": status, "workflowName": "build"})).collect();
    std::fs::write(e.fake.join("list.json"), serde_json::to_string(&rows).unwrap()).unwrap();
}

/// `gh run view <run> --json attempt,status,conclusion,startedAt,jobs`, as `view-<run>.json` or `view-<run>-<attempt>.json`.
fn view_json(e: &Env, file: &str, attempt: u32, status: &str, jobs: &[(&str, &str, &str)]) {
    let jobs: Vec<serde_json::Value> = jobs
        .iter()
        .map(|(name, status, conclusion)| serde_json::json!({"completedAt": "0001-01-01T00:00:00Z", "conclusion": conclusion, "databaseId": 134082221545u64, "name": name, "startedAt": "2026-11-17T13:44:24Z", "status": status, "url": "https://github.com/x"}))
        .collect();
    let conclusion = if status == "completed" { "failure" } else { "" };
    let v = serde_json::json!({"attempt": attempt, "conclusion": conclusion, "startedAt": "2099-01-01T00:00:00Z", "status": status, "jobs": jobs});
    std::fs::write(e.fake.join(format!("view-{file}.json")), serde_json::to_string(&v).unwrap()).unwrap();
}

const GREEN: [(&str, &str, &str); 6] = [
    ("test", "completed", "success"),
    ("rust-geom", "completed", "success"),
    ("bundle (macos-15)", "completed", "success"),
    ("bundle (ubuntu-22.04)", "completed", "success"),
    ("bundle (windows-latest)", "completed", "success"),
    ("e2e", "completed", "failure"),
];

fn queued(e: &Env, sha: &str) -> String {
    let id = common::add(&e.v, &format!("Item for {}", &sha[..4]), "task", Some("fx"), &[("state", "in-progress")]).id;
    land(e, &["add", sha, "--items", &id]);
    land(e, &["approve", sha]);
    id
}

#[test]
fn the_ci_gate_is_set_shown_and_cleared() {
    let e = setup();
    assert!(land(&e, &["ci"]).contains("fx has no CI gate"));
    let out = land(&e, &["ci", "--repo", "https://github.com/Paraxdev/FundaCAD.git", "--gate", "test, rust-geom,bundle"]);
    assert!(out.contains("repo Paraxdev/FundaCAD, workflow any, branch any, gate jobs test, rust-geom, bundle"), "{out}");
    let out = land(&e, &["ci", "--workflow", "build", "--branch", "dev"]);
    assert!(out.contains("workflow build, branch dev, gate jobs test, rust-geom, bundle"), "a set keeps what it does not name: {out}");
    let file = std::fs::read_to_string(e.v.project_dir("fx").join("land-ci.toml")).unwrap();
    assert!(file.contains("repo = \"Paraxdev/FundaCAD\"") && file.contains("gate = [\"test\", \"rust-geom\", \"bundle\"]"), "{file}");
    assert!(fails(&e, &["land", "-p", "fx", "ci", "--repo", "FundaCAD"]).contains("owner/name"));
    assert!(land(&e, &["ci", "--clear"]).contains("no CI gate now"));
    assert!(!e.v.project_dir("fx").join("land-ci.toml").exists());
    let log = common::git(&e.v.root, &["log", "--format=%s"]);
    assert!(log.contains("breadcrumb: land ci fx"), "{log}");
}

#[test]
fn pushed_finds_the_run_and_check_waits_then_reads_green() {
    let e = setup();
    let item = queued(&e, S1);
    assert!(fails(&e, &["land", "-p", "fx", "pushed", S1]).contains("needs the CI run id"), "no gate set, no run: as before");
    set_ci(&e);
    list_json(&e, &[(RUN, S1, "in_progress", ""), ("44090955822", S2, "completed", "success")]);
    let out = land(&e, &["pushed", &S1[..7]]);
    assert!(out.contains(&format!("pushed, run {RUN}")), "{out}");
    assert_eq!(entries(&e)[0].run.as_deref(), Some(RUN));

    view_json(&e, RUN, 1, "in_progress", &[("test", "completed", "success"), ("rust-geom", "in_progress", ""), ("e2e", "in_progress", "")]);
    let out = land(&e, &["check", S1]);
    assert!(out.contains("test: success") && out.contains("rust-geom: in progress") && out.contains("bundle: not started"), "{out}");
    assert!(out.contains("still going, nothing read") && !out.contains("e2e"), "{out}");
    assert_eq!(entries(&e)[0].state, "pushed");

    view_json(&e, RUN, 1, "completed", &GREEN);
    let out = ok(&e, &["land", "-p", "fx", "wait", S1, "--every", "1", "--timeout", "1"]);
    assert!(out.contains("gate green") && out.contains("bundle (windows-latest): success"), "{out}");
    assert!(out.contains("1111111 green"), "{out}");
    let es = entries(&e);
    assert_eq!(es[0].state, "green");
    assert_eq!(es[0].reads.last().and_then(|r| r.attempt), Some(1));
    assert_eq!(breadcrumb::backlog::state(&e.v.get(&item).unwrap()), "review");
}

#[test]
fn a_red_gate_is_read_red_and_a_rerun_reads_it_again_with_both_attempts() {
    let e = setup();
    queued(&e, S1);
    set_ci(&e);
    land(&e, &["pushed", S1, "--run", RUN]);
    view_json(&e, RUN, 1, "in_progress", &[("test", "completed", "failure"), ("rust-geom", "in_progress", "")]);
    let out = land(&e, &["check", S1]);
    assert!(out.contains("gate red") && out.contains("1111111 red"), "a failed gate job decides red early: {out}");
    assert_eq!(entries(&e)[0].reads.last().unwrap().failed, ["test failure"]);
    view_json(&e, RUN, 1, "completed", &[("test", "completed", "failure"), ("rust-geom", "completed", "success")]);
    assert!(land(&e, &["check", S1]).contains("read red for this attempt already"));

    view_json(&e, RUN, 2, "in_progress", &[("test", "in_progress", ""), ("rust-geom", "completed", "success")]);
    let out = land(&e, &["check", S1]);
    assert!(out.contains("1111111 pushed again, run 44095728300 was rerun"), "{out}");
    assert_eq!(entries(&e)[0].state, "pushed");
    view_json(&e, RUN, 2, "completed", &GREEN);
    let out = land(&e, &["check", S1]);
    assert!(out.contains("1111111 green (attempt 2, after attempt 1 red)"), "{out}");
    let shown = land(&e, &["show", S1]);
    assert!(shown.contains("red on attempt 1") && shown.contains("test failure") && shown.contains("green on attempt 2"), "{shown}");
}

#[test]
fn a_first_read_of_a_rerun_records_the_earlier_attempt() {
    let e = setup();
    queued(&e, S1);
    set_ci(&e);
    land(&e, &["pushed", S1, "--run", RUN]);
    view_json(&e, RUN, 2, "completed", &GREEN);
    view_json(&e, &format!("{RUN}-1"), 1, "completed", &[("test", "completed", "failure"), ("rust-geom", "completed", "success")]);
    let out = land(&e, &["check", S1]);
    assert!(out.contains("green (attempt 2, after attempt 1 red)"), "{out}");
    let reads = &entries(&e)[0].reads;
    assert_eq!(reads.iter().map(|r| (r.result.as_str(), r.attempt)).collect::<Vec<_>>(), [("red", Some(1)), ("green", Some(2))]);
}

#[test]
fn an_infra_red_keeps_the_entry_pushed_until_a_rerun_is_read() {
    let e = setup();
    let item = queued(&e, S1);
    set_ci(&e);
    land(&e, &["pushed", S1, "--run", RUN]);
    view_json(&e, RUN, 1, "completed", &[("test", "completed", "success"), ("rust-geom", "completed", "failure")]);
    let out = land(&e, &["read", S1, "infra", "--note", "runner disk full"]);
    assert!(out.contains("1111111 pushed (infra red on attempt 1: runner disk full)"), "the attempt comes from gh: {out}");
    assert!(out.contains("rerun the failed jobs, then read the run"), "{out}");
    let es = entries(&e);
    assert_eq!(es[0].state, "pushed");
    assert_eq!(es[0].steps.last().unwrap().state, "infra");
    assert!(land(&e, &["list"]).contains("infra red on attempt 1: runner disk full"));

    let out = land(&e, &["check", S1]);
    assert!(out.contains("waiting for a rerun") && entries(&e)[0].state == "pushed", "the infra red is not read as red: {out}");
    view_json(&e, RUN, 2, "completed", &GREEN);
    let out = land(&e, &["check", S1]);
    assert!(out.contains("1111111 green (attempt 2, after attempt 1 infra red)"), "{out}");
    assert_eq!(breadcrumb::backlog::state(&e.v.get(&item).unwrap()), "review");

    queued(&e, S2);
    land(&e, &["pushed", S2, "--run", "5"]);
    land(&e, &["red", S2]);
    let out = land(&e, &["infra", S2, "--attempt", "1"]);
    assert!(out.contains("2222222 pushed (infra red on attempt 1)"), "a red entry turns back to pushed: {out}");
}

#[test]
fn a_hand_read_takes_an_attempt_and_a_note_and_rereads_a_red_entry_for_a_rerun() {
    let e = setup();
    queued(&e, S1);
    land(&e, &["pushed", S1, "--run", RUN]);
    land(&e, &["red", S1]);
    let out = fails(&e, &["land", "-p", "fx", "green", S1]);
    assert!(out.contains("read needs a pushed entry"), "{out}");
    let out = land(&e, &["read", "green", S1, "--attempt", "2", "--note", "test flaked on the snap timer"]);
    assert!(out.contains("1111111 green (attempt 2, test flaked on the snap timer)"), "{out}");
}

#[test]
fn gh_missing_or_logged_out_degrades_and_nothing_old_needs_it() {
    let e = setup();
    queued(&e, S1);
    set_ci(&e);
    let nowhere = e.fake.join("no-such-gh.exe");
    let (fine, out) = run_with(&e, &nowhere, &["land", "-p", "fx", "pushed", S1]);
    assert!(fine && out.contains("not installed") && out.contains("--run <id>` adds it"), "{out}");
    assert_eq!(entries(&e)[0].state, "pushed");
    assert_eq!(entries(&e)[0].run, None);
    let (fine, out) = run_with(&e, &nowhere, &["land", "-p", "fx", "check", S1]);
    assert!(!fine && out.contains("not installed") && out.contains("by hand"), "{out}");
    let out = land(&e, &["pushed", S1, "--run", "77"]);
    assert!(out.contains("run 77"), "a pushed entry with no run takes one without --force: {out}");

    std::fs::write(e.fake.join("fail.txt"), "To get started with GitHub CLI, please run:  gh auth login\n").unwrap();
    let out = fails(&e, &["land", "-p", "fx", "check", S1]);
    assert!(out.contains("not logged in"), "{out}");
    assert!(land(&e, &["green", S1]).contains("1111111 green"));
}

#[test]
fn a_late_entry_whose_run_is_going_is_not_stale() {
    let e = setup();
    queued(&e, S1);
    queued(&e, S2);
    land(&e, &["pushed", S1, "--run", RUN]);
    land(&e, &["pushed", S2, "--run", "44090955822"]);
    let file = e.v.project_dir("fx").join(land::FILE);
    let text = std::fs::read_to_string(&file).unwrap();
    let old: Vec<String> = text
        .lines()
        .map(|l| {
            let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
            v["at"] = serde_json::json!("2026-01-01T00:00:00Z");
            v.to_string()
        })
        .collect();
    std::fs::write(&file, old.join("\n") + "\n").unwrap();
    assert!(land(&e, &["list"]).contains("STALE, no read yet"), "no gate, no gh: stale by the clock as before");
    set_ci(&e);
    list_json(&e, &[(RUN, S1, "in_progress", ""), ("44090955822", S2, "completed", "success")]);
    let out = land(&e, &["list"]);
    let first = out.lines().find(|l| l.contains("1111111")).unwrap();
    assert!(first.contains("run still going") && !first.contains("STALE"), "{out}");
    let second = out.lines().find(|l| l.contains("2222222")).unwrap();
    assert!(second.contains("STALE, run finished success, no read yet, `crumb land check 2222222`"), "{out}");
    let digest = ok(&e, &["context", "--project", "fx"]);
    assert!(digest.contains("1111111 pushed, run 44095728300") && digest.contains("run still going"), "{digest}");
}
