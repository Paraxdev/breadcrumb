mod common;

use breadcrumb::backlog;
use breadcrumb::cli::Cli;
use breadcrumb::forgive;
use breadcrumb::mcp::Server;
use breadcrumb::vault::Vault;
use clap::CommandFactory;
use serde_json::{Value, json};
use std::io::Cursor;
use std::process::Command;

fn crumb(v: &Vault, args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_crumb")).args(args).env("BREADCRUMB_HOME", &v.root).env_remove("CLAUDE_CODE_SESSION_ID").env_remove("BREADCRUMB_SESSION").output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

fn ok(v: &Vault, args: &[&str]) -> (String, String) {
    let (ok, out, err) = crumb(v, args);
    assert!(ok, "{args:?}: {err}");
    (out, err)
}

fn refused(v: &Vault, args: &[&str]) -> String {
    let (ok, out, err) = crumb(v, args);
    assert!(!ok, "{args:?} should fail: {out}");
    err
}

fn mcp(v: &Vault, d: &std::path::Path, calls: &[(&str, Value)]) -> Vec<(bool, String)> {
    let srv = Server { vault_root: v.root.clone(), cwd: d.to_path_buf() };
    let mut input = String::new();
    for (i, (name, args)) in calls.iter().enumerate() {
        input.push_str(&json!({"jsonrpc": "2.0", "id": i, "method": "tools/call", "params": {"name": name, "arguments": args}}).to_string());
        input.push('\n');
    }
    let mut out = Vec::new();
    srv.serve(Cursor::new(input), &mut out).unwrap();
    String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| {
            let r: Value = serde_json::from_str(l).unwrap();
            (r["result"]["isError"] == false, r["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string())
        })
        .collect()
}

fn words(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|x| x.to_string()).collect()
}

#[test]
fn priority_words_map_to_p0_to_p3_with_a_note() {
    let (_d, v) = common::vault();
    let (out, _) = ok(&v, &["add", "Urgent one", "-k", "bug", "-p", "app", "--field", "priority=urgent"]);
    assert!(out.contains("priority urgent taken as p0"), "{out}");
    let (out, _) = ok(&v, &["add", "High one", "-k", "task", "-p", "app", "--priority", "High"]);
    assert!(out.contains("taken as p1"), "{out}");
    ok(&v, &["add", "Plain one", "-k", "task", "-p", "app", "--priority", "p3"]);
    let pri = |t: &str| backlog::priority(&v.load_notes().into_iter().find(|n| n.title == t).unwrap());
    assert_eq!((pri("Urgent one"), pri("High one"), pri("Plain one")), ("p0".into(), "p1".into(), "p3".into()));
    let err = refused(&v, &["add", "Odd one", "-k", "task", "-p", "app", "--field", "priority=sometime"]);
    assert!(err.contains("p0") && err.contains("p3"), "{err}");
}

#[test]
fn list_filters_backlog_items_by_state_and_track() {
    let (_d, v) = common::vault();
    common::add(&v, "Queued item", "task", Some("app"), &[("state", "queued")]);
    common::add(&v, "Review item", "task", Some("app"), &[("state", "review")]);
    common::add(&v, "Plain note", "note", Some("app"), &[]);
    let (out, _) = ok(&v, &["list", "-p", "app", "--state", "queued"]);
    assert!(out.contains("Queued item") && !out.contains("Review item") && !out.contains("Plain note"), "{out}");
    let (out, _) = ok(&v, &["list", "-p", "app", "--state", "queued,review"]);
    assert!(out.contains("Queued item") && out.contains("Review item"), "{out}");
    let (out, err) = ok(&v, &["list", "-p", "app", "--status", "review"]);
    assert!(out.contains("Review item") && !out.contains("Queued item") && err.contains("read as --state review"), "{out} {err}");
    let err = refused(&v, &["list", "-p", "app", "--state", "being-built"]);
    assert!(err.contains("board --state being-built"), "{err}");
    ok(&v, &["add", "Handoff A", "-k", "handoff", "-p", "app", "--track", "lane-a"]);
    let (out, _) = ok(&v, &["list", "-p", "app", "--track", "lane-a"]);
    assert!(out.contains("Handoff A") && !out.contains("Queued item"), "{out}");
}

#[test]
fn a_word_in_the_wrong_field_names_the_call_that_works() {
    let (_d, v) = common::vault();
    let id = common::add(&v, "An item", "task", Some("app"), &[]).id;
    let err = refused(&v, &["update", &id, "--status", "review"]);
    assert!(err.contains(&format!("breadcrumb move {id} review")), "{err}");
    let err = refused(&v, &["move", &id, "land"]);
    assert!(err.contains("breadcrumb land add <sha> --items <id>"), "{err}");
    let err = refused(&v, &["move", &id, "finished"]);
    assert!(err.contains("did you mean done?"), "{err}");
    let err = refused(&v, &["add", "Another", "-k", "task", "-p", "app", "--field", "status=queued"]);
    assert!(err.contains("--field state=queued"), "{err}");
}

#[test]
fn the_flags_a_sibling_command_takes_work_too() {
    let (_d, v) = common::vault();
    let id = common::add(&v, "An item", "task", Some("app"), &[]).id;
    ok(&v, &["move", &id, "review", "--note", "Built on fix/x, try the login page"]);
    let n = v.get(&id).unwrap();
    assert_eq!(backlog::state(&n), "review");
    assert!(n.body.contains("Built on fix/x, try the login page"), "{}", n.body);
    ok(&v, &["append", &id, "--body", "A second paragraph"]);
    assert!(v.get(&id).unwrap().body.contains("A second paragraph"));
    ok(&v, &["update", &id, "--priority", "low"]);
    assert_eq!(backlog::priority(&v.get(&id).unwrap()), "p3");
    let (out, _) = ok(&v, &["add", "Done by someone", "-k", "task", "-p", "app", "--by", "lead", "--state", "queued"]);
    assert!(out.contains("--by was ignored"), "{out}");
    let note = common::add(&v, "Pin me", "note", Some("app"), &[]).id;
    ok(&v, &["pin", &note]);
    assert!(v.get(&note).unwrap().pinned);
    ok(&v, &["unpin", &note]);
    assert!(!v.get(&note).unwrap().pinned);
    let (out, _) = ok(&v, &["ls", "-p", "app"]);
    assert!(out.contains("Pin me"), "{out}");
}

#[test]
fn mcp_words_on_the_cli_are_read_with_a_note() {
    let (_d, v) = common::vault();
    let id = common::add(&v, "An item", "task", Some("app"), &[]).id;
    let (out, err) = ok(&v, &["get", "--id", &id]);
    assert!(out.contains("An item") && err.contains(&format!("read as `breadcrumb get {id}`")), "{err}");
    let (_, err) = ok(&v, &["question", "--action", "ask", "--id", &id, "--text", "Which one?", "--free"]);
    assert!(err.contains("read as `breadcrumb ask"), "{err}");
    let (out, _) = ok(&v, &["question", "list", "-p", "app"]);
    assert!(out.contains("Which one?"), "{out}");
    let (out, _) = ok(&v, &["backlog", "tree", &id]);
    assert!(out.contains("An item"), "{out}");
}

#[test]
fn rewrites_follow_the_mcp_shape() {
    let root = Cli::command();
    let r = |xs: &[&str]| forgive::rewrite(&root, &words(xs));
    assert_eq!(r(&["question", "--action", "ask", "--id", "abc", "--text", "Q"]), Some(words(&["ask", "abc", "Q"])));
    assert_eq!(r(&["move", "abc", "review", "--verdict", "pass", "--evidence", "ran it"]), Some(words(&["review", "pass", "abc", "--evidence", "ran it"])));
    assert_eq!(r(&["move", "abc", "--verdict", "fail", "--evidence", "broke"]), Some(words(&["review", "fail", "abc", "--reason", "broke"])));
    assert_eq!(r(&["--json", "backlog", "deps", "abc"]), Some(words(&["--json", "deps", "abc"])));
    assert_eq!(r(&["project", "config", "set", "k", "v"]), Some(words(&["config", "set", "k", "v"])));
    assert_eq!(r(&["move", "abc", "done", "--shipped_in", "1.2"]), Some(words(&["move", "abc", "done", "--shipped-in", "1.2"])));
    assert_eq!(r(&["move", "abc", "--state", "review"]), Some(words(&["move", "abc", "review"])));
    assert_eq!(r(&["land", "read", "abc1234", "--result", "green"]), Some(words(&["land", "read", "abc1234", "green"])));
    assert_eq!(r(&["list", "-p", "app"]), None);
}

#[test]
fn a_refused_command_line_gets_the_nearest_one() {
    let (_d, v) = common::vault();
    let err = refused(&v, &["serach", "login", "bug"]);
    assert!(err.contains("did you mean: breadcrumb search login bug"), "{err}");
    let err = refused(&v, &["add", "A title", "--prioirty", "p1"]);
    assert!(err.contains("did you mean: breadcrumb add \"A title\" --priority p1"), "{err}");
    let err = refused(&v, &["list", "--sort", "tilte"]);
    assert!(err.contains("did you mean title?"), "{err}");
    let err = refused(&v, &["doctor", "--severity", "eror"]);
    assert!(err.contains("did you mean: breadcrumb doctor --severity error"), "{err}");
    let err = refused(&v, &["green", "abc1234"]);
    assert!(err.contains("did you mean: breadcrumb land green abc1234"), "{err}");
    let err = refused(&v, &["add", "A title", "--type", "gotcha"]);
    assert!(err.contains("did you mean: breadcrumb add \"A title\" --kind gotcha"), "{err}");
    let err = refused(&v, &["update", "abc", "--link", "def"]);
    assert!(err.contains("breadcrumb link <id> <other-id>"), "{err}");
}

#[test]
fn mcp_takes_kind_fields_at_the_top_and_filters_by_state() {
    let (d, v) = common::vault();
    mcp(&v, d.path(), &[("project", json!({"action": "link", "slug": "app", "path": d.path().join("appdir").to_string_lossy()}))]);
    let r = mcp(
        &v,
        d.path(),
        &[
            ("add", json!({"title": "Top level priority", "kind": "task", "project": "app", "priority": "p3"})),
            ("add", json!({"title": "Top level status", "kind": "task", "project": "app", "status": "queued", "titel": "x"})),
            ("list", json!({"project": "app", "state": "queued"})),
            ("list", json!({"project": "app", "status": "queued"})),
        ],
    );
    assert!(r[0].0 && r[0].1.contains("note: priority read as fields"), "{:?}", r[0]);
    let n = v.load_notes().into_iter().find(|n| n.title == "Top level priority").unwrap();
    assert_eq!(backlog::priority(&n), "p3");
    let n = v.load_notes().into_iter().find(|n| n.title == "Top level status").unwrap();
    assert_eq!(backlog::state(&n), "queued");
    assert!(r[1].1.contains("titel is no argument of add, did you mean title?"), "{:?}", r[1]);
    assert!(r[2].1.contains("Top level status") && !r[2].1.contains("Top level priority"), "{:?}", r[2]);
    assert!(r[3].1.starts_with("note: status is active, archived or all") && r[3].1.contains("Top level status"), "{:?}", r[3]);
}

#[test]
fn a_refused_mcp_call_lists_the_parameters_and_a_working_call() {
    let (d, v) = common::vault();
    let id = common::add(&v, "An item", "task", Some("app"), &[]).id;
    let r = mcp(
        &v,
        d.path(),
        &[
            ("land", json!({"action": "read", "sha": "abc1234"})),
            ("question", json!({"action": "lst"})),
            ("add", json!({"titel": "No title"})),
            ("move", json!({"id": id, "state": "review", "note": "Built on fix/x"})),
            ("map", json!({"action": "get", "id": "db"})),
        ],
    );
    assert!(!r[0].0 && r[0].1.contains("land takes ") && r[0].1.contains(r#"example: land {"action": "read", "sha": "abc1234", "result": "green"}"#), "{}", r[0].1);
    assert!(r[1].1.contains("action lst: did you mean list?") && r[1].1.contains("question takes "), "{}", r[1].1);
    assert!(r[2].1.contains("titel is no argument of add, did you mean title?") && r[2].1.contains("title (required)") && r[2].1.contains("example: add "), "{}", r[2].1);
    assert!(r[3].0, "{}", r[3].1);
    assert!(v.get(&id).unwrap().body.contains("Built on fix/x"));
    assert!(r[4].1.contains("id is no argument of map") && r[4].1.contains(r#"example: map {"action": "get", "area": "db"}"#), "{}", r[4].1);
}
