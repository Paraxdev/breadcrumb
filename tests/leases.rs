mod common;

use breadcrumb::vault::Vault;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const A: &str = "aaaa1111-1111-2222-3333-444444444444";
const B: &str = "bbbb2222-1111-2222-3333-444444444444";
const DIR: &str = "D:/fxt-c4";

struct Env {
    _d: tempfile::TempDir,
    v: Vault,
    claude: PathBuf,
}

fn setup() -> Env {
    let (d, v) = common::vault();
    let claude = d.path().join("claude");
    std::fs::create_dir_all(claude.join("sessions")).unwrap();
    let e = Env { _d: d, v, claude };
    session_file(&e, 1, A, "lead-a");
    session_file(&e, 2, B, "lead-b");
    e
}

// The pid is this test's own so the session counts as running, the file name is what a test removes to end it.
fn session_file(e: &Env, n: u32, id: &str, name: &str) {
    std::fs::write(e.claude.join("sessions").join(format!("{n}.json")), format!(r#"{{"pid":{},"sessionId":"{id}","name":"{name}"}}"#, std::process::id())).unwrap();
}

fn end_session(e: &Env, n: u32) {
    std::fs::remove_file(e.claude.join("sessions").join(format!("{n}.json"))).unwrap();
}

fn base(e: &Env, session: Option<&str>, machine: &str, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_crumb"));
    c.args(args)
        .current_dir(&e.v.root)
        .env("BREADCRUMB_HOME", &e.v.root)
        .env("BREADCRUMB_CLAUDE_DIR", &e.claude)
        .env("BREADCRUMB_MACHINE", machine)
        .env("BREADCRUMB_SESSION", "")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CLAUDE_CODE_SESSION_ID");
    if let Some(s) = session {
        c.env("CLAUDE_CODE_SESSION_ID", s);
    }
    c
}

fn crumb(e: &Env, session: Option<&str>, args: &[&str]) -> (bool, String) {
    let o = base(e, session, "here", args).output().unwrap();
    (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

fn ok(e: &Env, session: Option<&str>, args: &[&str]) -> String {
    let (fine, out) = crumb(e, session, args);
    assert!(fine, "{args:?}: {out}");
    out
}

fn feed(mut c: Command, input: &str) -> String {
    let mut child = c.args(["hook", "bash-lease"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(0), "a hook must never fail");
    assert!(o.stderr.is_empty());
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn hook_input(session: &str, command: &str) -> String {
    json!({"session_id": session, "transcript_path": "x", "cwd": ".", "hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": command, "description": "d"}}).to_string()
}

/// What the hook prints when session `who` runs `command`, the deny reason or None.
fn bash(e: &Env, who: &str, command: &str) -> Option<String> {
    let out = feed(base(e, None, "here", &[]), &hook_input(who, command));
    if out.is_empty() {
        return None;
    }
    let j: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(j["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert_eq!(j["hookSpecificOutput"]["permissionDecision"], "deny");
    Some(j["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().to_string())
}

fn lease_a(e: &Env) {
    ok(e, Some(A), &["lease", "c4", "--path", DIR, "--note", "landing"]);
}

fn file(v: &Vault) -> PathBuf {
    v.root.join(".local").join("leases.json")
}

fn age(v: &Vault, expired: bool) {
    let mut all: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(file(v)).unwrap()).unwrap();
    for l in &mut all {
        l["expires"] = json!(if expired { "2020-01-01T00:00:00Z" } else { "2999-01-01T00:00:00Z" });
    }
    std::fs::write(file(v), serde_json::to_string(&all).unwrap()).unwrap();
}

#[test]
fn another_live_holder_refuses_and_force_takes_over() {
    let e = setup();
    assert!(ok(&e, Some(A), &["lease", "runner", "--note", "rerun"]).contains("leased runner held by lead-a aaaa1111"));
    let (fine, out) = crumb(&e, Some(B), &["lease", "runner"]);
    assert!(!fine && out.contains("leased by lead-a aaaa1111") && out.contains("--force"), "{out}");
    assert!(ok(&e, Some(A), &["lease", "runner", "--ttl", "1h"]).contains("renewed"));
    let out = ok(&e, Some(A), &["leases"]);
    assert!(out.contains("runner held by lead-a aaaa1111") && out.contains("rerun"), "a renewal keeps the note: {out}");
    assert!(ok(&e, Some(B), &["lease", "runner", "--force"]).contains("taken over from lead-a aaaa1111"));
    assert!(ok(&e, Some(A), &["leases"]).contains("held by lead-b bbbb2222"));
}

#[test]
fn release_is_the_holders_unless_forced() {
    let e = setup();
    lease_a(&e);
    let (fine, out) = crumb(&e, Some(B), &["lease", "--release", "c4"]);
    assert!(!fine && out.contains("leased by lead-a"), "{out}");
    assert!(ok(&e, Some(A), &["lease", "--release", "c4"]).contains("released c4"));
    assert!(ok(&e, Some(A), &["leases"]).contains("no leases"));
    assert!(ok(&e, Some(A), &["lease", "--release", "c4"]).contains("no lease holds c4"));
    lease_a(&e);
    assert!(ok(&e, Some(B), &["lease", "--release", "c4", "--force"]).contains("released c4"));
}

#[test]
fn expiry_frees_it() {
    let e = setup();
    lease_a(&e);
    age(&e.v, true);
    assert!(ok(&e, Some(A), &["leases"]).contains("no leases"));
    assert_eq!(bash(&e, B, &format!("CARGO_TARGET_DIR={DIR} cargo build")), None);
    assert!(ok(&e, Some(B), &["lease", "c4"]).starts_with("leased c4 held by lead-b"));
    assert!(!ok(&e, Some(B), &["leases"]).contains("landing"), "an expired lease leaves nothing behind");
}

#[test]
fn a_dead_session_frees_it_at_once() {
    let e = setup();
    lease_a(&e);
    assert!(bash(&e, B, &format!("cargo build --target-dir {DIR}")).is_some());
    end_session(&e, 1);
    assert_eq!(bash(&e, B, &format!("cargo build --target-dir {DIR}")), None);
    assert!(ok(&e, Some(B), &["leases"]).contains("no leases"));
    assert!(ok(&e, Some(B), &["lease", "c4"]).starts_with("leased"));
}

#[test]
fn a_session_with_no_file_when_taking_it_only_expires() {
    let e = setup();
    let ghost = "cccc3333-1111-2222-3333-444444444444";
    ok(&e, Some(ghost), &["lease", "c4", "--path", DIR]);
    assert!(bash(&e, B, &format!("ls {DIR}")).is_some());
    assert_eq!(bash(&e, ghost, &format!("ls {DIR}")), None);
}

#[test]
fn free_text_only_expires_and_binds_every_other_session() {
    let e = setup();
    ok(&e, Some(A), &["lease", "c4", "--by", "nightly", "--path", DIR]);
    assert!(ok(&e, None, &["leases"]).contains("c4 held by nightly"));
    assert_eq!(bash(&e, A, &format!("ls {DIR}")), None, "it ran the command, so it is its own");
    assert!(bash(&e, B, &format!("ls {DIR}")).unwrap().contains("leased by nightly"));
    end_session(&e, 1);
    assert!(bash(&e, B, &format!("ls {DIR}")).is_some(), "a dead session frees nothing here");
    assert!(ok(&e, None, &["lease", "c4", "--by", "nightly"]).contains("renewed"));
    age(&e.v, true);
    assert_eq!(bash(&e, B, &format!("ls {DIR}")), None);
}

#[test]
fn by_a_live_sessions_name_hands_it_to_that_session() {
    let e = setup();
    ok(&e, Some(A), &["lease", "c4", "--by", "lead-b", "--path", DIR]);
    assert!(ok(&e, None, &["leases"]).contains("lead-b bbbb2222"));
    assert_eq!(bash(&e, B, &format!("ls {DIR}")), None);
    assert!(bash(&e, A, &format!("ls {DIR}")).unwrap().contains("lead-b bbbb2222"));
    end_session(&e, 2);
    assert_eq!(bash(&e, A, &format!("ls {DIR}")), None);
}

#[test]
fn another_machine_does_not_bind_here() {
    let e = setup();
    lease_a(&e);
    let other = |args: &[&str]| base(&e, Some(B), "elsewhere", args);
    assert!(String::from_utf8_lossy(&other(&["leases"]).output().unwrap().stdout).contains("no leases"));
    assert_eq!(feed(other(&[]), &hook_input(B, &format!("ls {DIR}"))), "");
    assert!(other(&["lease", "c4"]).output().unwrap().status.success());
}

#[test]
fn the_hook_reads_every_spelling_of_the_path() {
    let e = setup();
    lease_a(&e);
    let denied = [
        "CARGO_TARGET_DIR=D:/fxt-c4 cargo build",
        "export CARGO_TARGET_DIR='D:/fxt-c4/sub'; cargo build",
        r"CARGO_TARGET_DIR=D:\fxt-c4 cargo build",
        r"cargo build --target-dir D:\\fxt-c4\\x",
        "cargo build --target-dir=\"D:/fxt-c4/\"",
        "cd /d/fxt-c4 && cargo build",
        "bash -c \"cargo build --target-dir D:/fxt-c4\"",
        "git status; ls D:/fxt-c4",
    ];
    for c in denied {
        let why = bash(&e, B, c).unwrap_or_else(|| panic!("{c}"));
        assert!(why.contains("lead-a aaaa1111") && why.contains("--force") && why.contains("BREADCRUMB_LEASE_OK=1") && why.contains("landing") && !why.contains('\n'), "{why}");
        assert_eq!(bash(&e, A, c), None, "{c}");
    }
    #[cfg(windows)]
    assert!(bash(&e, B, "ls d:/FXT-C4").is_some());
    let allowed = [
        "cargo build",
        "ls D:/fxt-c4-old",
        "ls D:/fxt-c4.bak",
        "ls E:/fxt-c4",
        "ls D:/x/fxt-c4",
        "ls D:/fxt",
        "CARGO_TARGET_DIR=D:/fxt-b1/target cargo test",
        "crumb lease c4 --force",
        "D:/bin/crumb.exe append x \"built into D:/fxt-c4; oops\"",
        "git commit -m \"Stop building into D:/fxt-c4\"",
        "BREADCRUMB_LEASE_OK=1 ls D:/fxt-c4",
    ];
    for c in allowed {
        assert_eq!(bash(&e, B, c), None, "{c}");
    }
}

#[test]
fn only_the_prose_part_of_a_command_is_exempt() {
    let e = setup();
    lease_a(&e);
    assert!(bash(&e, B, "crumb leases && cargo build --target-dir D:/fxt-c4").is_some());
    assert!(bash(&e, B, "git -C D:/fxt-c4 commit -m x").is_some());
    let heredoc = "crumb append x - <<'EOF'\nit's built into D:/fxt-c4\nEOF\n";
    assert_eq!(bash(&e, B, heredoc), None);
    assert!(bash(&e, B, "crumb append x - <<< hi\ncargo build --target-dir D:/fxt-c4").is_some());
    assert!(bash(&e, B, &format!("{heredoc}cargo build --target-dir D:/fxt-c4")).is_some());
}

#[test]
fn the_environment_counts_too() {
    let e = setup();
    lease_a(&e);
    let run = |dir: &str, cmd: &str| {
        let mut c = base(&e, None, "here", &[]);
        c.env("CARGO_TARGET_DIR", dir);
        feed(c, &hook_input(B, cmd))
    };
    assert!(run("D:/fxt-c4/sub", "cargo build").contains("deny"));
    assert_eq!(run("D:/other", "cargo build"), "");
    assert_eq!(run("D:/fxt-c4", "crumb leases"), "", "the way out stays open");
}

#[test]
fn any_failure_passes() {
    let e = setup();
    lease_a(&e);
    for bad in ["", "not json", "[]", r#"{"tool_input":{"command":"ls D:/fxt-c4"}}"#, r#"{"session_id":"x","tool_input":{}}"#] {
        assert_eq!(feed(base(&e, None, "here", &[]), bad), "", "{bad}");
    }
    std::fs::write(file(&e.v), "{ broken").unwrap();
    assert_eq!(bash(&e, B, &format!("ls {DIR}")), None);
    assert!(!crumb(&e, Some(B), &["leases"]).0, "a person gets told");
    assert!(!ok(&e, None, &["context", "-p", "app"]).contains("Leases"));
    std::fs::remove_file(file(&e.v)).unwrap();
    let nowhere = e.v.root.join("nowhere");
    let mut c = base(&e, None, "here", &[]);
    c.env("BREADCRUMB_HOME", &nowhere);
    assert_eq!(feed(c, &hook_input(B, "ls")), "");
}

#[test]
fn a_lease_without_a_path_stops_nothing_and_a_path_name_guards_itself() {
    let e = setup();
    ok(&e, Some(A), &["lease", "fxt-c4"]);
    assert_eq!(bash(&e, B, "ls D:/fxt-c4"), None);
    ok(&e, Some(A), &["lease", "D:/fxt-c5/"]);
    assert!(bash(&e, B, "ls D:/fxt-c5/x").is_some());
    let (fine, out) = crumb(&e, Some(A), &["lease", "D:/"]);
    assert!(!fine && out.contains("whole drive"), "{out}");
}

#[test]
fn names_and_ttls_are_checked() {
    let e = setup();
    for bad in ["0h", "h", "5", "-1h", "4x", "4\u{e9}", "99999999999999d"] {
        let (fine, out) = crumb(&e, Some(A), &["lease", "x", &format!("--ttl={bad}")]);
        assert!(!fine && out.contains("ttl must look like"), "{bad}: {out}");
    }
    assert!(!crumb(&e, Some(A), &["lease", " "]).0);
    assert!(!crumb(&e, Some(A), &["lease", "x", "--release", "--ttl", "1h"]).0);
    ok(&e, Some(A), &["lease", "Runner"]);
    #[cfg(windows)]
    assert!(!crumb(&e, Some(B), &["lease", "runner"]).0, "one name in any case");
}

#[test]
fn the_digest_names_them_only_when_there_are_any() {
    let e = setup();
    let ctx = |e: &Env| ok(e, None, &["context", "-p", "app"]);
    assert!(!ctx(&e).contains("Leases"));
    for n in ["a", "b", "c", "d", "e"] {
        ok(&e, Some(A), &["lease", n]);
    }
    let text = ctx(&e);
    let line = text.lines().find(|l| l.starts_with("Leases on this machine: ")).unwrap();
    assert_eq!(line, "Leases on this machine: a (lead-a aaaa1111), b (lead-a aaaa1111), c (lead-a aaaa1111), d (lead-a aaaa1111), +1 more, `crumb leases`");
    end_session(&e, 1);
    assert!(!ctx(&e).contains("Leases"));
}

fn tree_files(root: &Path) -> Vec<String> {
    let out = common::git(root, &["status", "--porcelain", "--untracked-files=all"]);
    out.lines().map(str::to_string).collect()
}

#[test]
fn a_lease_is_no_note_and_no_commit() {
    let e = setup();
    let notes = e.v.load_notes().len();
    let commits = common::git(&e.v.root, &["rev-list", "--count", "HEAD"]);
    lease_a(&e);
    assert_eq!(e.v.load_notes().len(), notes);
    assert_eq!(common::git(&e.v.root, &["rev-list", "--count", "HEAD"]), commits);
    assert!(tree_files(&e.v.root).is_empty(), "{:?}", tree_files(&e.v.root));
    assert!(!ok(&e, None, &["search", "landing"]).contains("c4"));
}

#[test]
fn concurrent_sessions_get_one_holder() {
    let e = setup();
    let handles: Vec<_> = (0..6)
        .map(|i| {
            let mut c = base(&e, Some(if i % 2 == 0 { A } else { B }), "here", &["lease", "shared", "--by", &format!("worker-{i}")]);
            std::thread::spawn(move || c.output().unwrap().status.success())
        })
        .collect();
    let won = handles.into_iter().map(|h| h.join().unwrap()).filter(|w| *w).count();
    assert!((1..=3).contains(&won), "only the winner's session can renew, {won}");
    let all: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(file(&e.v)).unwrap()).unwrap();
    assert_eq!(all.len(), 1);
}

#[test]
fn concurrent_names_all_survive() {
    let e = setup();
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let mut c = base(&e, Some(A), "here", &["lease", &format!("n{i}")]);
            std::thread::spawn(move || assert!(c.output().unwrap().status.success()))
        })
        .collect();
    handles.into_iter().for_each(|h| h.join().unwrap());
    let all: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(file(&e.v)).unwrap()).unwrap();
    assert_eq!(all.len(), 8);
}

fn settings(e: &Env) -> PathBuf {
    e.v.root.join(".local").join("scratch-settings.json")
}

fn install(e: &Env, extra: &[&str]) -> Value {
    let s = settings(e);
    let mut args = vec!["--json", "hooks", "install", "--settings", s.to_str().unwrap()];
    args.extend_from_slice(extra);
    serde_json::from_str(&ok(e, None, &args)).unwrap()
}

fn bash_entries(e: &Env) -> usize {
    let j: Value = serde_json::from_str(&std::fs::read_to_string(settings(e)).unwrap()).unwrap();
    j["hooks"]["PreToolUse"].as_array().unwrap().iter().filter(|x| x["matcher"] == "Bash").count()
}

#[test]
fn the_hook_is_opt_in_and_stays_once_installed() {
    let e = setup();
    let plain = install(&e, &[]);
    assert!(plain["bash_lease"].is_null());
    assert_eq!(bash_entries(&e), 0);
    let on = install(&e, &["--bash-leases"]);
    assert!(on["bash_lease"].as_str().unwrap().ends_with("hook bash-lease"));
    assert_eq!(bash_entries(&e), 1);
    assert_eq!(install(&e, &["--bash-leases"])["changed"], false);
    let again = install(&e, &[]);
    assert_eq!(again["changed"], false);
    assert!(again["bash_lease"].is_string());
    assert_eq!(bash_entries(&e), 1);
    let s = settings(&e);
    let status: Value = serde_json::from_str(&ok(&e, None, &["--json", "hooks", "status", "--settings", s.to_str().unwrap()])).unwrap();
    assert!(status["bash_lease"].is_string());
    ok(&e, None, &["hooks", "uninstall", "--settings", s.to_str().unwrap()]);
    assert!(!std::fs::read_to_string(&s).unwrap().contains("bash-lease"));
}
