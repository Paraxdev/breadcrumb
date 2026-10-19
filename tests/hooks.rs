mod common;

use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime};

fn breadcrumb(vault: &Path, args: &[&str], stdin: &str, cwd: &Path) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_breadcrumb"));
    c.arg("--vault").arg(vault).args(args).current_dir(cwd).env_remove("BREADCRUMB_HOME");
    c.env("BREADCRUMB_CLAUDE_DIR", vault.with_extension("no-claude"));
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn context_of(o: &Output) -> String {
    assert!(o.status.success());
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    v["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_string()
}

#[test]
fn session_start_prints_capped_project_digest() {
    let (d, v) = common::vault();
    let proj = d.path().join("work").join("demo-app");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    breadcrumb::ops::project_link(&v, "demo-app", &proj, None).unwrap();
    common::add(&v, "Use tabs in the parser", "decision", Some("demo-app"), &[]);
    common::add(&v, "Fix flaky login test", "todo", Some("demo-app"), &[("priority", "p1")]);
    for i in 0..40 {
        let body = "filler text ".repeat(60);
        let n = common::add(&v, &format!("Long note {i}"), "note", Some("demo-app"), &[]);
        breadcrumb::ops::update(&v, &n.id, breadcrumb::ops::UpdateReq { body: Some(body), pinned: Some(i < 12), ..Default::default() }).unwrap();
    }

    let input = json!({"cwd": proj.join("src"), "source": "startup", "session_id": "abc"}).to_string();
    let text = context_of(&breadcrumb(&v.root, &["hook", "session-start"], &input, d.path()));
    assert!(text.starts_with("breadcrumb context: demo-app\n"), "{text}");
    assert!(text.chars().count() <= breadcrumb::hooks::CONTEXT_CAP);
    assert!(text.contains("truncated") && text.contains("for more)"), "{text}");
    assert!(!text.contains("Kinds:") && !text.contains("\n\n"));

    let text = context_of(&breadcrumb(&v.root, &["hook", "session-start"], "not json", &proj));
    assert!(text.starts_with("breadcrumb context: demo-app"));
}

#[test]
fn session_start_small_vault_is_not_truncated() {
    let (d, v) = common::vault();
    common::add(&v, "Prefers short replies", "feedback", None, &[]);
    let text = context_of(&breadcrumb(&v.root, &["hook", "session-start"], "", d.path()));
    assert!(text.starts_with("breadcrumb context: global"));
    assert!(text.contains("Prefers short replies"));
    assert!(!text.contains("truncated"));
}

#[test]
fn hooks_are_silent_without_a_vault() {
    let d = tempfile::tempdir().unwrap();
    let missing = d.path().join("nope");
    for (args, input) in [(["hook", "session-start"], "{}"), (["hook", "session-end"], ""), (["hook", "session-start"], "garbage")] {
        let o = breadcrumb(&missing, &args, input, d.path());
        assert!(o.status.success());
        assert!(o.stdout.is_empty(), "{}", String::from_utf8_lossy(&o.stdout));
    }
    let o = breadcrumb(&missing, &["--json", "hook", "session-start"], "{}", d.path());
    assert!(o.status.success() && o.stdout.is_empty());
    assert!(!missing.exists());
}

#[test]
fn session_end_on_clean_vault_does_nothing() {
    let (d, v) = common::vault();
    let o = breadcrumb(&v.root, &["hook", "session-end"], "{}", d.path());
    assert!(o.status.success() && o.stdout.is_empty());
    assert!(!v.root.join(".local").join("sync.log").exists());
}

fn settings_fixture() -> Value {
    json!({
        "model": "some-model",
        "permissions": {"allow": ["Bash(ls:*)"]},
        "hooks": {
            "SessionStart": [{"matcher": "startup", "hooks": [{"type": "command", "command": "other-tool greet"}]}],
            "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "guard.sh"}]}]
        },
        "statusLine": {"type": "command", "command": "status.sh"}
    })
}

fn read(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn breadcrumb_cmds(s: &Value, event: &str) -> Vec<String> {
    s["hooks"][event]
        .as_array()
        .map(|es| {
            es.iter()
                .flat_map(|e| e["hooks"].as_array().cloned().unwrap_or_default())
                .filter_map(|h| h["command"].as_str().map(str::to_string))
                .filter(|c| c.contains(" hook session-"))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn hooks_install_uninstall_keep_other_settings() {
    let (d, v) = common::vault();
    let settings = d.path().join("claude").join("settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let original = serde_json::to_string_pretty(&settings_fixture()).unwrap();
    std::fs::write(&settings, &original).unwrap();
    let sp = settings.to_str().unwrap();

    let o = breadcrumb(&v.root, &["hooks", "install", "--settings", sp], "", d.path());
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let s = read(&settings);
    assert_eq!(s["model"], "some-model");
    assert_eq!(s["statusLine"]["command"], "status.sh");
    assert_eq!(s["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "guard.sh");
    assert_eq!(s["hooks"]["SessionStart"][0]["hooks"][0]["command"], "other-tool greet");
    let start = breadcrumb_cmds(&s, "SessionStart");
    assert_eq!(start.len(), 1);
    assert!(start[0].starts_with('"') && start[0].ends_with("hook session-start") && !start[0].contains('\\'));
    assert_eq!(s["hooks"]["SessionStart"][1]["matcher"], "startup|resume|clear|compact");
    assert_eq!(s["hooks"]["SessionStart"][1]["hooks"][0]["timeout"], 10);
    assert_eq!(breadcrumb_cmds(&s, "SessionEnd").len(), 1);
    let bak = d.path().join("claude").join("settings.json.breadcrumb.bak");
    assert_eq!(std::fs::read_to_string(&bak).unwrap(), original);

    let text = std::fs::read_to_string(&settings).unwrap();
    let o = breadcrumb(&v.root, &["hooks", "install", "--settings", sp], "", d.path());
    assert!(String::from_utf8_lossy(&o.stdout).contains("no change"));
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), text);

    let mut stale = read(&settings);
    stale["hooks"]["SessionStart"][1]["hooks"][0]["command"] = json!("\"/old/place/breadcrumb\" hook session-start");
    std::fs::write(&settings, serde_json::to_string_pretty(&stale).unwrap()).unwrap();
    breadcrumb(&v.root, &["hooks", "install", "--settings", sp, "--no-sync"], "", d.path());
    let s = read(&settings);
    assert_eq!(breadcrumb_cmds(&s, "SessionStart"), start);
    assert!(s["hooks"].get("SessionEnd").is_none());

    let o = breadcrumb(&v.root, &["--json", "hooks", "status", "--settings", sp], "", d.path());
    let st: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(st["session_start"], json!(start[0]));
    assert!(st["session_end"].is_null());

    breadcrumb(&v.root, &["hooks", "uninstall", "--settings", sp], "", d.path());
    assert_eq!(read(&settings), settings_fixture());
    assert_eq!(std::fs::read_to_string(&bak).unwrap(), original);
}

#[test]
fn uninstall_drops_containers_it_emptied() {
    let (d, v) = common::vault();
    let settings = d.path().join("settings.json");
    let sp = settings.to_str().unwrap();
    assert!(breadcrumb(&v.root, &["hooks", "install", "--settings", sp], "", d.path()).status.success());
    assert_eq!(breadcrumb_cmds(&read(&settings), "SessionStart").len(), 1);
    breadcrumb(&v.root, &["hooks", "uninstall", "--settings", sp], "", d.path());
    assert_eq!(read(&settings), json!({}));
}

#[test]
fn stale_git_index_lock_is_removed() {
    let (_d, v) = common::vault();
    let lock = v.root.join(".git").join("index.lock");
    std::fs::write(&lock, "").unwrap();
    let f = std::fs::File::options().write(true).open(&lock).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(600)).unwrap();
    drop(f);
    let n = common::add(&v, "Written after a crash", "note", None, &[]);
    assert!(!lock.exists());
    assert!(common::git(&v.root, &["log", "-1", "--format=%s"]).contains(&n.title));

    std::fs::write(&lock, "").unwrap();
    let r = breadcrumb::ops::add(&v, breadcrumb::ops::AddReq { title: "Blocked by a live lock".into(), ..Default::default() });
    assert!(r.is_err(), "a fresh index.lock belongs to a running git and must be left alone");
    assert!(lock.exists());
}

#[test]
fn session_end_syncs_dirty_vault_in_background() {
    let (d, v) = common::vault();
    std::fs::write(v.root.join("global").join("notes").join("hand-written.md"), "# Hand written\n").unwrap();
    let o = breadcrumb(&v.root, &["hook", "session-end"], "{}", d.path());
    assert!(o.status.success() && o.stdout.is_empty());
    let start = std::time::Instant::now();
    while !common::git(&v.root, &["status", "--porcelain"]).is_empty() && start.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(common::git(&v.root, &["status", "--porcelain"]), "");
    assert!(v.root.join(".local").join("sync.log").exists());
}
