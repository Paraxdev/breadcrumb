mod common;

use breadcrumb::land::{self, Entry};
use breadcrumb::vault::Vault;
use serde_json::json;
use std::process::Command;

struct Env {
    _d: tempfile::TempDir,
    v: Vault,
}

fn setup() -> Env {
    let (d, v) = common::vault();
    Env { _d: d, v }
}

fn item(e: &Env, title: &str, state: &str) -> String {
    common::add(&e.v, title, "task", Some("fx"), &[("state", state)]).id
}

fn run(e: &Env, session: &str, args: &[&str]) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_crumb"))
        .args(args)
        .current_dir(&e.v.root)
        .env("BREADCRUMB_HOME", &e.v.root)
        .env("BREADCRUMB_SESSION", session)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .output()
        .unwrap();
    (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

fn ok(e: &Env, session: &str, args: &[&str]) -> String {
    let (fine, out) = run(e, session, args);
    assert!(fine, "{args:?}: {out}");
    out
}

fn fails(e: &Env, session: &str, args: &[&str]) -> String {
    let (fine, out) = run(e, session, args);
    assert!(!fine, "{args:?} should fail: {out}");
    out
}

fn queue_file(e: &Env) -> std::path::PathBuf {
    e.v.project_dir("fx").join(land::FILE)
}

fn entries(e: &Env) -> Vec<Entry> {
    std::fs::read_to_string(queue_file(e)).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn digest(e: &Env, session: &str) -> String {
    ok(e, session, &["context", "--project", "fx"])
}

const S1: &str = "1111111111111111111111111111111111111111";
const S2: &str = "2222222222222222222222222222222222222222";
const S3: &str = "3333333333333333333333333333333333333333";

#[test]
fn an_entry_goes_from_built_to_green_and_its_items_move_to_review() {
    let e = setup();
    let a = item(&e, "First item", "in-progress");
    let b = item(&e, "Second item", "queued");
    let out = ok(&e, "worker", &["land", "-p", "fx", "add", S1, "--items", &format!("{a},{b}")]);
    assert!(out.contains("1111111 built"), "{out}");
    assert!(out.contains("approve"), "{out}");

    let out = ok(&e, "reviewer", &["land", "-p", "fx", "approve", &S1[..8], "--note", "pending your alone harness run"]);
    assert!(out.contains("approved (pending your alone harness run)"), "{out}");
    assert!(out.contains("verify or push: worker"), "the builder is next after an approval: {out}");

    let out = digest(&e, "worker");
    assert!(out.contains("Landing queue, 1 to land"), "{out}");
    assert!(out.contains("1111111 approved (pending your alone harness run)") && out.contains("worker (you)"), "{out}");
    assert!(!digest(&e, "reviewer").contains("worker (you)"));

    let out = ok(&e, "reviewer", &["land", "-p", "fx", "verify", S1]);
    assert!(out.contains("1111111 verified") && !out.contains("harness"), "{out}");
    let out = ok(&e, "worker", &["land", "-p", "fx", "pushed", S1, "--run", "42377831121"]);
    assert!(out.contains("pushed, run 42377831121") && out.contains("read the run: reviewer"), "{out}");
    assert!(!out.contains("STALE"));

    let out = ok(&e, "reviewer", &["land", "-p", "fx", "read", S1, "green"]);
    assert!(out.contains("green"), "{out}");
    for id in [&a, &b] {
        let n = e.v.get(id).unwrap();
        assert_eq!(breadcrumb::backlog::state(&n), "review", "{id}");
        assert!(n.refs().contains(&format!("commit:{S1}")), "{:?}", n.refs());
    }
    assert!(out.contains("went from queued to review and was never claimed"), "move warnings come through: {out}");

    let es = entries(&e);
    let steps: Vec<(&str, &str)> = es[0].steps.iter().map(|s| (s.state.as_str(), s.by.as_str())).collect();
    assert_eq!(steps, [("built", "worker"), ("approved", "reviewer"), ("verified", "reviewer"), ("pushed", "worker"), ("green", "reviewer")]);
    assert!(!digest(&e, "worker").contains("Landing queue"), "a landed queue leaves the digest");
    assert!(ok(&e, "worker", &["land", "-p", "fx"]).contains("empty"));
    assert!(ok(&e, "worker", &["land", "-p", "fx", "list", "--all"]).contains("1111111 green"));
    let log = common::git(&e.v.root, &["log", "--format=%s"]);
    assert!(log.contains("breadcrumb: land add 1111111") && log.contains("breadcrumb: land read 1111111 green"), "{log}");
}

#[test]
fn items_not_in_a_state_for_review_stay_and_are_named() {
    let e = setup();
    let working = item(&e, "Working", "in-progress");
    let closed = item(&e, "Already done", "done");
    let idea = item(&e, "Only an idea", "idea");
    ok(&e, "w", &["land", "-p", "fx", "add", S1, "--items", &format!("{working},{closed},{idea}")]);
    ok(&e, "r", &["land", "-p", "fx", "approve", S1]);
    ok(&e, "w", &["land", "-p", "fx", "pushed", S1, "--run", "1"]);
    let out = ok(&e, "r", &["land", "-p", "fx", "read", S1, "green"]);
    assert_eq!(breadcrumb::backlog::state(&e.v.get(&working).unwrap()), "review");
    assert_eq!(breadcrumb::backlog::state(&e.v.get(&closed).unwrap()), "done");
    assert_eq!(breadcrumb::backlog::state(&e.v.get(&idea).unwrap()), "idea");
    assert!(out.contains("it is done") && out.contains("it is idea"), "{out}");
    assert!(e.v.get(&closed).unwrap().refs().is_empty());
}

#[test]
fn a_red_read_keeps_the_items_and_shows_in_the_digest() {
    let e = setup();
    let a = item(&e, "Item", "in-progress");
    ok(&e, "w", &["land", "-p", "fx", "add", S1, "--items", &a]);
    ok(&e, "r", &["land", "-p", "fx", "approve", S1]);
    ok(&e, "w", &["land", "-p", "fx", "pushed", S1, "--run", "7"]);
    let out = ok(&e, "r", &["land", "-p", "fx", "read", S1, "red"]);
    assert!(out.contains("red") && out.contains("stay where they are"), "{out}");
    assert_eq!(breadcrumb::backlog::state(&e.v.get(&a).unwrap()), "in-progress");
    assert!(digest(&e, "w").contains("1111111 red"));
    fails(&e, "r", &["land", "-p", "fx", "read", S1, "green"]);
    ok(&e, "r", &["land", "-p", "fx", "drop", S1]);
    assert!(entries(&e).is_empty());
}

#[test]
fn a_pushed_entry_with_no_read_shows_stale_with_its_age() {
    let e = setup();
    item(&e, "x", "queued");
    ok(&e, "w", &["land", "-p", "fx", "add", S1]);
    ok(&e, "r", &["land", "-p", "fx", "approve", S1]);
    ok(&e, "w", &["land", "-p", "fx", "pushed", S1, "--run", "9"]);
    assert!(!digest(&e, "r").contains("STALE"));

    let mut es = entries(&e);
    es[0].at = chrono::Utc::now() - chrono::Duration::minutes(land::STALE_MINUTES + 270);
    std::fs::write(queue_file(&e), es.iter().map(|x| serde_json::to_string(x).unwrap() + "\n").collect::<String>()).unwrap();
    let out = digest(&e, "r");
    assert!(out.contains("5h ago, STALE, no read yet") && out.contains("read the run: r (you)"), "{out}");
}

#[test]
fn steps_and_order_are_checked_and_force_skips_them() {
    let e = setup();
    item(&e, "x", "queued");
    for s in [S1, S2, S3] {
        ok(&e, "w", &["land", "-p", "fx", "add", s]);
    }
    assert!(fails(&e, "w", &["land", "-p", "fx", "add", &S2[..9]]).contains("in the queue already"));
    assert!(fails(&e, "w", &["land", "-p", "fx", "pushed", S1, "--run", "1"]).contains("needs an approved entry"));
    assert!(fails(&e, "w", &["land", "-p", "fx", "verify", S1]).contains("needs an approved entry"));
    assert!(fails(&e, "w", &["land", "-p", "fx", "read", S1, "green"]).contains("needs a pushed entry"));
    ok(&e, "r", &["land", "-p", "fx", "approve", S2]);
    let out = fails(&e, "w", &["land", "-p", "fx", "pushed", S2, "--run", "1"]);
    assert!(out.contains("1111111 is ahead"), "{out}");
    ok(&e, "w", &["land", "-p", "fx", "pushed", S2, "--run", "1", "--force"]);

    ok(&e, "w", &["land", "-p", "fx", "move", S3, "--after", S1]);
    let order: Vec<String> = entries(&e).iter().map(|x| x.short().to_string()).collect();
    assert_eq!(order, ["1111111", "3333333", "2222222"]);
    ok(&e, "w", &["land", "-p", "fx", "move", S2]);
    assert_eq!(entries(&e)[0].short(), "2222222");
    assert!(fails(&e, "w", &["land", "-p", "fx", "approve", "zzzz"]).contains("not a commit sha"));
    assert!(fails(&e, "w", &["land", "-p", "fx", "approve", "abcd"]).contains("no queue entry"));
    assert!(fails(&e, "w", &["land", "-p", "fx", "add", "abc"]).contains("7 to 40"));
    assert!(fails(&e, "w", &["land", "-p", "fx", "add", "abcdef1", "--items", "nosuchitem"]).contains("nosuchitem"));
    assert_eq!(entries(&e).len(), 3);
}

#[test]
fn the_queue_needs_a_registered_project_and_keeps_keys_it_does_not_know() {
    let e = setup();
    assert!(fails(&e, "w", &["land", "-p", "nowhere", "add", S1]).contains("unknown project"));
    item(&e, "x", "queued");
    ok(&e, "w", &["land", "-p", "fx", "add", S1, "--to", "somebody"]);
    let p = queue_file(&e);
    let text = std::fs::read_to_string(&p).unwrap().replace("\"state\"", "\"future\":{\"a\":1},\"state\"");
    std::fs::write(&p, text).unwrap();
    let out = ok(&e, "r", &["land", "-p", "fx", "approve", S1, "--to", "w2"]);
    assert!(out.contains("verify or push: w2"), "{out}");
    assert!(std::fs::read_to_string(&p).unwrap().contains("\"future\":{\"a\":1}"));
    std::fs::write(&p, "<<<<<<< HEAD\n").unwrap();
    assert!(fails(&e, "w", &["land", "-p", "fx"]).contains("line 1"));
}

#[test]
fn the_mcp_land_tool_does_the_same() {
    let e = setup();
    let a = item(&e, "Item", "in-progress");
    let srv = breadcrumb::mcp::Server { vault_root: e.v.root.clone(), cwd: e.v.root.clone() };
    let call = |args: serde_json::Value| srv.call_tool("land", args.as_object().unwrap());
    call(json!({"action": "add", "project": "fx", "sha": S1, "items": [a], "by": "w"})).unwrap();
    call(json!({"action": "approve", "project": "fx", "sha": S1, "note": "after harness", "by": "r"})).unwrap();
    call(json!({"action": "pushed", "project": "fx", "sha": S1, "run": "5"})).unwrap();
    assert!(call(json!({"project": "fx"})).unwrap().contains("pushed, run 5"));
    let out = call(json!({"action": "green", "project": "fx", "sha": S1})).unwrap();
    assert!(out.contains("-> review"), "{out}");
    assert!(call(json!({"action": "pushed", "project": "fx", "sha": S1})).is_err());
    assert!(call(json!({"action": "blue", "project": "fx", "sha": S1})).is_err());
}

fn approved(e: &Env, shas: &[&str]) {
    for s in shas {
        ok(e, "w", &["land", "-p", "fx", "add", s]);
        ok(e, "r", &["land", "-p", "fx", "approve", s]);
    }
}

#[test]
fn one_push_carries_several_entries_and_one_read_closes_them() {
    let e = setup();
    let a = item(&e, "First", "in-progress");
    let b = item(&e, "Second", "in-progress");
    ok(&e, "w", &["land", "-p", "fx", "add", S1, "--items", &a]);
    ok(&e, "w", &["land", "-p", "fx", "add", S2, "--items", &b]);
    ok(&e, "w", &["land", "-p", "fx", "add", S3]);
    ok(&e, "r", &["land", "-p", "fx", "approve", S1]);
    ok(&e, "r", &["land", "-p", "fx", "approve", S2]);
    ok(&e, "r", &["land", "-p", "fx", "approve", S3]);

    let out = fails(&e, "w", &["land", "-p", "fx", "pushed", &S2[..8], "--run", "10"]);
    assert!(out.contains("1111111 is ahead in the queue and still approved") && out.contains("name it in this call"), "{out}");
    let out = fails(&e, "w", &["land", "-p", "fx", "pushed", &S3[..8], &S1[..8], "--run", "10"]);
    assert!(out.contains("2222222 is ahead"), "an entry left out of the call still blocks: {out}");
    assert_eq!(entries(&e).iter().filter(|x| x.state == "pushed").count(), 0, "a refused call changes nothing");

    let out = ok(&e, "w", &["land", "-p", "fx", "pushed", &S2[..8], &S1[..8], "--run", "10"]);
    assert!(out.contains("1111111 pushed, run 10 (with 2222222)") && out.contains("2222222 pushed, run 10 (with 1111111)"), "{out}");
    assert_eq!(out.lines().count(), 2, "{out}");
    let log = common::git(&e.v.root, &["log", "--format=%s"]);
    assert_eq!(log.matches("land pushed").count(), 1, "{log}");
    assert!(log.contains("breadcrumb: land pushed 1111111 2222222"), "{log}");

    let out = fails(&e, "w", &["land", "-p", "fx", "pushed", S3, "--run", "11"]);
    assert!(out.contains("1111111 is ahead in the queue and still pushed on run 10"), "{out}");
    let list = ok(&e, "w", &["land", "-p", "fx"]);
    assert!(list.contains("1. 1111111 pushed, run 10 (with 2222222)"), "{list}");
    assert!(digest(&e, "r").contains("1111111 pushed, run 10 (with 2222222)"));

    let out = ok(&e, "w", &["land", "-p", "fx", "pushed", S3, "--run", "10"]);
    assert!(out.contains("3333333 pushed, run 10 (with 1111111, 2222222)"), "same run later in order: {out}");

    let out = ok(&e, "r", &["land", "-p", "fx", "read", S1, "green"]);
    assert!(out.contains("run 10 also holds 2222222, 3333333 unread") && out.contains("`crumb land read 2222222 3333333 green` reads them too"), "{out}");
    assert_eq!(entries(&e).iter().filter(|x| x.state == "green").count(), 1, "the reply only names the others");
    assert_eq!(breadcrumb::backlog::state(&e.v.get(&b).unwrap()), "in-progress");

    let out = ok(&e, "r", &["land", "-p", "fx", "read", &S2[..8], &S3[..8], "green"]);
    assert!(!out.contains("also holds"), "{out}");
    assert_eq!(breadcrumb::backlog::state(&e.v.get(&b).unwrap()), "review");
    assert!(entries(&e).iter().all(|x| x.state == "green"));
    let out = ok(&e, "r", &["land", "-p", "fx", "read", S1, &S2[..8], "green"]);
    assert!(out.contains("1111111 was green already") && out.contains("2222222 was green already"), "{out}");
}

#[test]
fn a_joint_read_can_be_red_and_checks_every_entry_first() {
    let e = setup();
    item(&e, "x", "queued");
    approved(&e, &[S1, S2, S3]);
    ok(&e, "w", &["land", "-p", "fx", "pushed", &format!("{S1},{S2}"), "--run", "5"]);
    let out = fails(&e, "r", &["land", "-p", "fx", "read", S1, S3, "red"]);
    assert!(out.contains("3333333 is approved, read needs a pushed entry"), "{out}");
    assert_eq!(entries(&e).iter().filter(|x| x.state == "pushed").count(), 2);
    let out = ok(&e, "r", &["land", "-p", "fx", "read", S1, S2, "red", "--to", "w"]);
    assert!(out.contains("1111111 red") && out.contains("2222222 red") && out.contains("their items stay"), "{out}");
    assert_eq!(entries(&e).iter().filter(|x| x.state == "red").count(), 2);
}

#[test]
fn the_mcp_tool_takes_several_shas_for_pushed_and_the_reads() {
    let e = setup();
    let a = item(&e, "Item", "in-progress");
    approved(&e, &[S1, S2]);
    ok(&e, "w", &["land", "-p", "fx", "add", S3, "--items", &a]);
    ok(&e, "r", &["land", "-p", "fx", "approve", S3]);
    let srv = breadcrumb::mcp::Server { vault_root: e.v.root.clone(), cwd: e.v.root.clone() };
    let call = |args: serde_json::Value| srv.call_tool("land", args.as_object().unwrap());
    let out = call(json!({"action": "pushed", "project": "fx", "sha": format!("{}, {} {}", &S1[..8], &S2[..8], &S3[..8]), "run": "77"})).unwrap();
    assert_eq!(out.lines().count(), 3, "{out}");
    let out = call(json!({"action": "green", "project": "fx", "sha": format!("{},{}", S1, S2)})).unwrap();
    assert!(out.contains("run 77 also holds 3333333 unread") && out.contains("reads it too"), "{out}");
    call(json!({"action": "green", "project": "fx", "sha": S3})).unwrap();
    assert_eq!(breadcrumb::backlog::state(&e.v.get(&a).unwrap()), "review");
    assert!(call(json!({"action": "pushed", "project": "fx", "sha": " , ", "run": "1"})).is_err());
}

#[test]
fn a_note_on_add_shows_in_the_list_and_digest_and_no_step_clears_it() {
    let e = setup();
    item(&e, "x", "queued");
    let out = ok(&e, "w", &["land", "-p", "fx", "add", S1, "--note", "settle probe first\nworktree D:/wt/a, branch lane/a"]);
    assert!(out.contains("note: settle probe first worktree D:/wt/a, branch lane/a"), "{out}");
    assert!(ok(&e, "r", &["land", "-p", "fx"]).contains("note: \"settle probe first worktree"));
    assert!(digest(&e, "r").contains("note: \"settle probe first worktree"));

    let out = ok(&e, "r", &["land", "-p", "fx", "approve", S1, "--note", "pending harness"]);
    assert!(out.contains("approved (pending harness)") && out.contains("note: settle probe first"), "{out}");
    let out = ok(&e, "r", &["land", "-p", "fx", "verify", S1]);
    assert!(!out.contains("pending harness") && out.contains("note: settle probe first"), "verify clears the condition only: {out}");
    ok(&e, "w", &["land", "-p", "fx", "pushed", S1, "--run", "3"]);
    assert_eq!(entries(&e)[0].memo.as_deref(), Some("settle probe first worktree D:/wt/a, branch lane/a"));
    assert!(entries(&e)[0].note.is_none());
    ok(&e, "r", &["land", "-p", "fx", "read", S1, "green"]);
    assert!(ok(&e, "r", &["land", "-p", "fx", "list", "--all"]).contains("note: \"settle probe first"));

    let long = "x".repeat(200);
    ok(&e, "w", &["land", "-p", "fx", "add", S2, "--note", &long]);
    assert!(digest(&e, "r").contains(&format!("note: \"{}...\"", "x".repeat(57))));
    assert_eq!(entries(&e)[1].memo.as_ref().unwrap().chars().count(), 120);

    let srv = breadcrumb::mcp::Server { vault_root: e.v.root.clone(), cwd: e.v.root.clone() };
    srv.call_tool("land", json!({"action": "add", "project": "fx", "sha": S3, "note": "from mcp"}).as_object().unwrap()).unwrap();
    assert_eq!(entries(&e)[2].memo.as_deref(), Some("from mcp"));
    assert!(std::fs::read_to_string(queue_file(&e)).unwrap().lines().all(|l| l.contains("\"memo\"") || l.contains("\"green\"")));
}
