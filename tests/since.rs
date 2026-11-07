mod common;

use breadcrumb::mcp::Server;
use breadcrumb::util::{fmt_time, now};
use serde_json::json;
use std::path::Path;
use std::process::Command;

const ME: &str = "a03e0d0a-951c-4892-a70a-6a4940f03ffd";
const OTHER: &str = "151cdeae-be99-4730-ba9d-daae66f59a6c";

fn session_file(claude: &Path, started_ms: Option<i64>) {
    std::fs::create_dir_all(claude.join("sessions")).unwrap();
    let pid = std::process::id();
    let started = started_ms.map(|s| format!(r#","startedAt":{s}"#)).unwrap_or_default();
    std::fs::write(claude.join("sessions").join(format!("{pid}.json")), format!(r#"{{"pid":{pid},"sessionId":"{ME}","name":"me-1"{started}}}"#)).unwrap();
}

fn crumb(vault: &Path, claude: &Path, session: &str, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_crumb"))
        .args(args)
        .env("BREADCRUMB_HOME", vault)
        .env("BREADCRUMB_CLAUDE_DIR", claude)
        .env("CLAUDE_CODE_SESSION_ID", session)
        .env("BREADCRUMB_SESSION", "")
        .output()
        .unwrap();
    (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

fn ok(vault: &Path, claude: &Path, session: &str, args: &[&str]) -> String {
    let (good, out) = crumb(vault, claude, session, args);
    assert!(good, "{args:?}: {out}");
    out
}

fn age(v: &breadcrumb::vault::Vault, id: &str, days: i64) {
    let mut n = v.get(id).unwrap();
    n.updated = now() - chrono::Duration::days(days);
    v.write_note(&mut n).unwrap();
}

#[test]
fn since_keeps_appends_moves_claims_and_links_of_older_notes() {
    let (d, v) = common::vault();
    let claude = d.path().join("claude");
    let quiet = common::add(&v, "Quiet note", "note", Some("app"), &[]);
    let appended = common::add(&v, "Appended note", "note", Some("app"), &[]);
    let moved = common::add(&v, "Moved item", "task", Some("app"), &[]);
    let claimed = common::add(&v, "Claimed item", "task", Some("app"), &[]);
    let linked = common::add(&v, "Linked note", "note", Some("app"), &[]);
    for n in [&quiet, &appended, &moved, &claimed, &linked] {
        age(&v, &n.id, 3);
    }
    ok(&v.root, &claude, OTHER, &["append", &appended.id, "new facts"]);
    ok(&v.root, &claude, OTHER, &["move", &moved.id, "queued"]);
    ok(&v.root, &claude, OTHER, &["claim", &claimed.id]);
    ok(&v.root, &claude, OTHER, &["link", &linked.id, &quiet.id]);

    let out = ok(&v.root, &claude, ME, &["list", "-p", "app", "--since", "2h"]);
    assert!(out.starts_with("changed since "), "{out}");
    for n in [&appended, &moved, &claimed, &linked] {
        assert!(out.contains(&n.id), "{} missing: {out}", n.title);
    }
    assert!(!out.contains(&quiet.id), "{out}");
    let all = ok(&v.root, &claude, ME, &["list", "-p", "app", "--since", "4d"]);
    assert!(all.contains(&quiet.id), "{all}");
}

#[test]
fn each_line_names_who_wrote_last_and_marks_other_sessions() {
    let (d, v) = common::vault();
    let claude = d.path().join("claude");
    session_file(&claude, None);
    let mine = common::add(&v, "Mine alone", "note", Some("app"), &[]);
    let theirs = common::add(&v, "Theirs now", "note", Some("app"), &[]);
    let unsigned = common::add(&v, "Old binary note", "note", Some("app"), &[]);
    ok(&v.root, &claude, ME, &["append", &mine.id, "mine"]);
    ok(&v.root, &claude, OTHER, &["append", &theirs.id, "theirs"]);

    let out = ok(&v.root, &claude, ME, &["list", "-p", "app", "--since", "1h"]);
    let line = |id: &str| out.lines().find(|l| l.contains(id)).unwrap().to_string();
    assert!(line(&mine.id).ends_with(" by you"), "{out}");
    assert!(line(&theirs.id).ends_with(" by 151cdeae [other]"), "{out}");
    assert!(!line(&unsigned.id).contains(" by "), "{out}");
    let day = breadcrumb::util::fmt_date(&now());
    assert!(line(&theirs.id).contains(&format!("({day} ")), "{out}");

    let outside = ok(&v.root, &claude, "", &["list", "-p", "app", "--since", "1h"]);
    assert!(!outside.contains("[other]") && outside.contains(" by 151cdeae"), "{outside}");
}

#[test]
fn others_hides_what_this_session_wrote_last() {
    let (d, v) = common::vault();
    let claude = d.path().join("claude");
    session_file(&claude, None);
    let mine = common::add(&v, "Mine alone", "note", Some("app"), &[]);
    let shared = common::add(&v, "Shared note", "note", Some("app"), &[]);
    ok(&v.root, &claude, ME, &["append", &mine.id, "mine"]);
    ok(&v.root, &claude, ME, &["append", &shared.id, "mine first"]);
    ok(&v.root, &claude, OTHER, &["append", &shared.id, "then theirs"]);
    ok(&v.root, &claude, ME, &["append", &mine.id, "mine again"]);

    let out = ok(&v.root, &claude, ME, &["list", "-p", "app", "--since", "1h", "--others"]);
    assert!(out.contains(&shared.id) && !out.contains(&mine.id), "{out}");
    let theirs_only = ok(&v.root, &claude, OTHER, &["list", "-p", "app", "--since", "1h", "--others"]);
    assert!(theirs_only.contains(&mine.id) && !theirs_only.contains(&shared.id), "{theirs_only}");
}

#[test]
fn since_takes_a_date_an_rfc3339_time_or_a_span_and_refuses_anything_else() {
    let (d, v) = common::vault();
    let claude = d.path().join("claude");
    let old = common::add(&v, "Old note", "note", Some("app"), &[]);
    let fresh = common::add(&v, "Fresh note", "note", Some("app"), &[]);
    age(&v, &old.id, 5);
    let hit = |when: &str| ok(&v.root, &claude, ME, &["list", "-p", "app", "--since", when]);
    let today = breadcrumb::util::fmt_date(&now());
    assert!(hit(&today).contains(&fresh.id) && !hit(&today).contains(&old.id));
    assert!(!hit(&fmt_time(&(now() + chrono::Duration::hours(1)))).contains(&fresh.id));
    assert!(hit(&fmt_time(&(now() - chrono::Duration::hours(1)))).contains(&fresh.id));
    assert!(hit("30m").contains(&fresh.id) && !hit("30m").contains(&old.id));
    assert!(hit("10d").contains(&old.id));
    let (good, msg) = crumb(&v.root, &claude, ME, &["list", "--since", "soon"]);
    assert!(!good && msg.contains("must be a date"), "{msg}");
}

#[test]
fn recent_is_list_since_this_sessions_start_without_its_own_writes() {
    let (d, v) = common::vault();
    let claude = d.path().join("claude");
    let before = common::add(&v, "Before I started", "note", Some("app"), &[]);
    let theirs = common::add(&v, "Theirs after", "note", Some("app"), &[]);
    let mine = common::add(&v, "Mine after", "note", Some("app"), &[]);
    age(&v, &before.id, 2);
    ok(&v.root, &claude, OTHER, &["append", &theirs.id, "theirs"]);
    ok(&v.root, &claude, ME, &["append", &mine.id, "mine"]);

    let (good, msg) = crumb(&v.root, &claude, ME, &["recent", "-p", "app"]);
    assert!(!good && msg.contains("start time is unknown"), "{msg}");

    session_file(&claude, Some((now() - chrono::Duration::hours(1)).timestamp_millis()));
    let out = ok(&v.root, &claude, ME, &["recent", "-p", "app"]);
    assert!(out.contains(&theirs.id) && out.contains("[other]"), "{out}");
    assert!(!out.contains(&mine.id) && !out.contains(&before.id), "{out}");
}

#[test]
fn the_mcp_list_tool_takes_since_and_others() {
    let (d, v) = common::vault();
    let srv = Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let old = common::add(&v, "Old note", "note", None, &[]);
    let fresh = common::add(&v, "Fresh note", "note", None, &[]);
    age(&v, &old.id, 5);
    let list = |args: serde_json::Value| srv.call_tool("list", args.as_object().unwrap()).unwrap();
    let out = list(json!({"since": "2h"}));
    assert!(out.starts_with("changed since ") && out.contains(&fresh.id) && !out.contains(&old.id), "{out}");
    assert!(list(json!({"since": "1d", "limit": 1})).contains(&fresh.id));
    assert!(srv.call_tool("list", json!({"since": "soon"}).as_object().unwrap()).is_err());
    assert!(list(json!({})).contains(&old.id), "without since the list is unchanged");
}
