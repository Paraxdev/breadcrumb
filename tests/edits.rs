mod common;

use breadcrumb::vault::Vault;
use std::io::Write;
use std::process::{Command, Stdio};

fn crumb(v: &Vault, args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_crumb")).args(args).args(["--vault", v.root.to_str().unwrap()]).output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

fn ok(v: &Vault, args: &[&str]) -> String {
    let (ok, out, err) = crumb(v, args);
    assert!(ok, "{args:?}: {err}");
    out
}

fn with_stdin(v: &Vault, args: &[&str], input: &str) -> (bool, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_crumb")).args(args).args(["--vault", v.root.to_str().unwrap()]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let o = c.wait_with_output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stderr).to_string())
}

fn commits(v: &Vault) -> usize {
    let o = Command::new("git").args(["rev-list", "--count", "HEAD"]).current_dir(&v.root).output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().parse().unwrap()
}

#[test]
fn replace_changes_one_exact_string() {
    let (_d, v) = common::vault();
    let id = common::add(&v, "A note", "note", Some("app"), &[]).id;
    ok(&v, &["update", &id, "--body", "the wrong ref\nand again a thing\nand again a thing"]);
    ok(&v, &["update", &id, "--replace", "wrong ref", "right ref"]);
    assert!(v.get(&id).unwrap().body.starts_with("the right ref\n"));
    ok(&v, &["update", &id, "--replace", "right", "-right"]);
    assert!(v.get(&id).unwrap().body.starts_with("the -right ref\n"));
}

#[test]
fn replace_refuses_zero_or_several_and_changes_nothing() {
    let (_d, v) = common::vault();
    let id = common::add(&v, "A note", "note", Some("app"), &[]).id;
    ok(&v, &["update", &id, "--body", "twice twice once"]);
    let before = std::fs::read_to_string(v.resolve_path(&id).unwrap()).unwrap();
    let count = commits(&v);
    for (old, n) in [("twice", "2 times"), ("absent", "0 times"), ("", "0 times")] {
        let (ok, _, err) = crumb(&v, &["update", &id, "--replace", old, "x", "--title", "Changed"]);
        assert!(!ok && err.contains(n), "{old}: {err}");
    }
    assert_eq!(before, std::fs::read_to_string(v.resolve_path(&id).unwrap()).unwrap());
    assert_eq!(count, commits(&v));
    assert!(!crumb(&v, &["update", &id, "--replace", "once", "x", "--body", "y"]).0);
}

#[test]
fn update_reads_the_body_from_stdin_and_refuses_an_empty_one() {
    let (_d, v) = common::vault();
    let id = common::add(&v, "A note", "note", Some("app"), &[]).id;
    assert!(with_stdin(&v, &["update", &id, "--body", "-"], "from \"stdin\"\n").0);
    assert_eq!(v.get(&id).unwrap().body, "from \"stdin\"");
    let (ok, err) = with_stdin(&v, &["update", &id, "--body", "-"], "");
    assert!(!ok && err.contains("empty"), "{err}");
    assert_eq!(v.get(&id).unwrap().body, "from \"stdin\"");
    ok_clear(&v, &id);
}

fn ok_clear(v: &Vault, id: &str) {
    ok(v, &["update", id, "--body", ""]);
    assert_eq!(v.get(id).unwrap().body, "");
}

#[test]
fn move_takes_several_ids_in_one_commit() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "First", "task", Some("app"), &[]).id;
    let b = common::add(&v, "Second", "task", Some("app"), &[]).id;
    let c = common::add(&v, "Third", "task", Some("app"), &[]).id;
    let count = commits(&v);
    let out = ok(&v, &["move", &a, &b, &c, "queued"]);
    assert_eq!(out.lines().count(), 3, "{out}");
    for (id, line) in [&a, &b, &c].iter().zip(out.lines()) {
        assert!(line.contains(id.as_str()) && line.ends_with("-> queued"), "{line}");
    }
    assert_eq!(commits(&v), count + 1);
    let status = Command::new("git").args(["status", "--short"]).current_dir(&v.root).output().unwrap();
    assert!(status.stdout.is_empty(), "{}", String::from_utf8_lossy(&status.stdout));
}

#[test]
fn one_failure_does_not_stop_the_rest_and_fails_at_the_end() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "First", "task", Some("app"), &[]).id;
    let b = common::add(&v, "Second", "task", Some("app"), &[]).id;
    let note = common::add(&v, "A plain note", "note", Some("app"), &[]).id;
    let (ok_, out, err) = crumb(&v, &["move", &a, "zzzzzzzz", &note, &b, "queued"]);
    assert!(!ok_);
    assert!(err.contains("2 of 4 moves failed"), "{err}");
    assert!(out.contains("failed zzzzzzzz: ") && out.contains(&format!("failed {note}: ")), "{out}");
    assert_eq!(breadcrumb::backlog::state(&v.get(&a).unwrap()), "queued");
    assert_eq!(breadcrumb::backlog::state(&v.get(&b).unwrap()), "queued");
}

#[test]
fn a_bad_state_or_a_position_with_several_ids_moves_nothing() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "First", "task", Some("app"), &[]).id;
    let b = common::add(&v, "Second", "task", Some("app"), &[]).id;
    let before = breadcrumb::backlog::state(&v.get(&a).unwrap());
    assert!(!crumb(&v, &["move", &a, "queued", &b]).0);
    assert!(!crumb(&v, &["move", &a, &b, "queued", "--position", "top"]).0);
    assert_eq!(breadcrumb::backlog::state(&v.get(&a).unwrap()), before);
}

#[test]
fn one_id_keeps_its_json_and_several_give_a_list() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "First", "task", Some("app"), &[]).id;
    let b = common::add(&v, "Second", "task", Some("app"), &[]).id;
    let one: serde_json::Value = serde_json::from_str(&ok(&v, &["move", &a, "queued", "--json"])).unwrap();
    assert_eq!(one["to"], "queued");
    let (_, out, _) = crumb(&v, &["move", &a, &b, "zzzzzzzz", "review", "--json"]);
    let many: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(many[1]["to"], "review");
    assert_eq!(many[2]["id"], "zzzzzzzz");
    assert!(many[2]["error"].is_string());
}

#[test]
fn output_uses_the_slug_and_bare_id_and_never_the_key() {
    let (_d, v) = common::vault();
    let e = common::add(&v, "The epic", "epic", Some("app"), &[]).id;
    let t = common::add(&v, "The task", "task", Some("app"), &[("parent", &e)]).id;
    for args in [vec!["move", &t, "queued"], vec!["board", "-p", "app"], vec!["tree", &t], vec!["next", "-p", "app"]] {
        let out = ok(&v, &args);
        assert!(!out.contains("APP-") && out.contains(&t), "{args:?}: {out}");
    }
    let board: serde_json::Value = serde_json::from_str(&ok(&v, &["board", "-p", "app", "--json"])).unwrap();
    assert!(board.to_string().contains(&format!("APP-{t}")), "--json keeps key for scripts");
}

#[test]
fn the_key_form_resolves_everywhere_and_checks_the_project() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "First", "task", Some("app"), &[]).id;
    let b = common::add(&v, "Second", "task", Some("app"), &[]).id;
    let key = format!("APP-{a}");
    assert!(ok(&v, &["get", &key]).contains(&a));
    assert!(ok(&v, &["get", &format!("app-{}", &a[..6])]).contains(&a));
    ok(&v, &["update", &key, "--title", "Renamed"]);
    ok(&v, &["append", &key, "More."]);
    ok(&v, &["link", &key, &format!("APP-{b}"), "--rel", "relates"]);
    ok(&v, &["move", &key, "queued"]);
    assert_eq!(breadcrumb::backlog::state(&v.get(&a).unwrap()), "queued");
    let (found, _, err) = crumb(&v, &["get", &format!("OTHER-{a}")]);
    assert!(!found && err.contains("no note with id or slug"), "{err}");
    let (found, _, err) = crumb(&v, &["get", "no-such-note"]);
    assert!(!found && err.contains("no note with id or slug"), "{err}");
}

#[test]
fn search_lines_show_the_state_of_items_only() {
    let (_d, v) = common::vault();
    let t = common::add(&v, "Zebra task", "task", Some("app"), &[]).id;
    let n = common::add(&v, "Zebra note", "note", Some("app"), &[]).id;
    ok(&v, &["move", &t, "shipped", "--shipped-in", "1.0"]);
    let out = ok(&v, &["search", "zebra", "-p", "app"]);
    let line = |id: &str| out.lines().find(|l| l.contains(id)).unwrap().to_string();
    assert!(line(&t).contains("[shipped] Zebra task"), "{out}");
    assert!(!line(&n).contains('['), "{out}");
    let srv = breadcrumb::mcp::Server { vault_root: v.root.clone(), cwd: v.root.clone() };
    let a = serde_json::json!({"query": "zebra", "project": "app"});
    let rows = srv.call_tool("search", a.as_object().unwrap()).unwrap();
    assert!(rows.contains("[shipped] Zebra task"), "{rows}");
}
