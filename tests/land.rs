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
    let before = common::git(&e.v.root, &["rev-list", "--count", "HEAD"]);
    let out = ok(&e, "r", &["land", "-p", "fx", "read", S1, &S2[..8], "green"]);
    assert!(out.contains("1111111 was green already") && out.contains("2222222 was green already"), "{out}");
    assert_eq!(common::git(&e.v.root, &["rev-list", "--count", "HEAD"]), before, "a read of green entries writes no commit");
    assert_eq!(common::git(&e.v.root, &["status", "--porcelain"]), "");
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
    assert_eq!(entries(&e)[1].memo.as_ref().unwrap().chars().count(), 200, "the note is kept whole up to 2000 chars, the list and digest cut it");

    let srv = breadcrumb::mcp::Server { vault_root: e.v.root.clone(), cwd: e.v.root.clone() };
    srv.call_tool("land", json!({"action": "add", "project": "fx", "sha": S3, "note": "from mcp"}).as_object().unwrap()).unwrap();
    assert_eq!(entries(&e)[2].memo.as_deref(), Some("from mcp"));
    assert!(std::fs::read_to_string(queue_file(&e)).unwrap().lines().all(|l| l.contains("\"memo\"") || l.contains("\"green\"")));
}

fn built_by(e: &Env, id: &str) -> Option<String> {
    e.v.get(id).unwrap().field(breadcrumb::backlog::BUILT_BY_KEY).and_then(|v| v.as_str().map(str::to_string))
}

#[test]
fn a_green_read_waits_for_every_entry_carrying_an_item_and_keeps_the_builder() {
    let e = setup();
    let shared = item(&e, "Shared by two pieces", "in-progress");
    let claimed = item(&e, "Claimed only", "backlog");
    let bare = item(&e, "Never claimed", "backlog");
    ok(&e, "builder", &["claim", &claimed]);
    let out = ok(&e, "w", &["land", "-p", "fx", "add", S1, "--items", &format!("{shared},{claimed},{bare}")]);
    assert!(out.contains(&format!("is backlog and unclaimed, a green read leaves it there, `crumb claim {bare}`")) && !out.contains(&format!("claim {claimed}`")), "{out}");
    ok(&e, "w", &["land", "-p", "fx", "add", S2, "--items", &shared]);
    approved_only(&e, &[S1, S2]);
    ok(&e, "w", &["land", "-p", "fx", "pushed", S1, "--run", "1"]);
    let out = ok(&e, "r", &["land", "-p", "fx", "read", S1, "green"]);
    assert!(out.contains("2222222 approved still carries it, the last of them read green moves it"), "{out}");
    assert_eq!(breadcrumb::backlog::state(&e.v.get(&shared).unwrap()), "in-progress");
    assert_eq!(breadcrumb::backlog::state(&e.v.get(&claimed).unwrap()), "review", "a claim counts as work started: {out}");
    assert_eq!(built_by(&e, &claimed).as_deref(), Some("builder"), "the claim holder built it, not the reader");
    assert_eq!(breadcrumb::backlog::state(&e.v.get(&bare).unwrap()), "backlog");

    ok(&e, "w", &["land", "-p", "fx", "pushed", S2, "--run", "2"]);
    let out = ok(&e, "r", &["land", "-p", "fx", "read", S2, "green"]);
    let n = e.v.get(&shared).unwrap();
    assert_eq!(breadcrumb::backlog::state(&n), "review", "{out}");
    assert!(n.refs().contains(&format!("commit:{S1}")) && n.refs().contains(&format!("commit:{S2}")), "every green piece is a ref: {:?}", n.refs());
    assert_eq!(built_by(&e, &shared).as_deref(), Some("w"), "unclaimed, the session that queued the merge built it");
}

#[test]
fn one_read_of_two_entries_with_the_same_item_moves_it_once() {
    let e = setup();
    let shared = item(&e, "Shared", "in-progress");
    ok(&e, "w", &["land", "-p", "fx", "add", S1, "--items", &shared]);
    ok(&e, "w", &["land", "-p", "fx", "add", S2, "--items", &shared]);
    approved_only(&e, &[S1, S2]);
    ok(&e, "w", &["land", "-p", "fx", "pushed", S1, S2, "--run", "1"]);
    let out = ok(&e, "r", &["land", "-p", "fx", "read", S1, S2, "green"]);
    assert_eq!(out.matches("-> review").count(), 1, "{out}");
    let n = e.v.get(&shared).unwrap();
    assert!(n.refs().contains(&format!("commit:{S1}")) && n.refs().contains(&format!("commit:{S2}")), "{:?}", n.refs());
}

fn approved_only(e: &Env, shas: &[&str]) {
    for s in shas {
        ok(e, "r", &["land", "-p", "fx", "approve", s]);
    }
}

const S4: &str = "4444444444444444444444444444444444444444";
const S5: &str = "5555555555555555555555555555555555555555";

#[test]
fn a_replace_takes_the_old_entry_place_items_note_and_approval() {
    let e = setup();
    let a = item(&e, "Item", "in-progress");
    ok(&e, "w", &["land", "-p", "fx", "add", S1, "--items", &a, "--note", "lane x in wt/x"]);
    ok(&e, "w", &["land", "-p", "fx", "add", S2]);
    ok(&e, "r", &["land", "-p", "fx", "approve", S1, "--note", "pending the harness"]);
    ok(&e, "w", &["land", "-p", "fx", "verify", S1]);
    let out = ok(&e, "w", &["land", "-p", "fx", "replace", &S1[..8], S4]);
    assert!(out.contains("4444444 verified") && out.contains("replaces 1111111, its approval by r carries over as verified, `git diff 1111111 4444444` shows what changed"), "{out}");
    let es = entries(&e);
    assert_eq!(es.iter().map(|x| x.short()).collect::<Vec<_>>(), ["4444444", "2222222"], "it takes the old place");
    assert_eq!(es[0].items, [a.clone()]);
    assert_eq!(es[0].memo.as_deref(), Some("lane x in wt/x"));
    assert_eq!(es[0].replaces.as_deref(), Some(S1));
    assert_eq!(es[0].earlier_notes[0].note, "pending the harness", "the verified condition stays readable");
    let out = ok(&e, "w", &["land", "-p", "fx", "pushed", S4, "--run", "1"]);
    assert!(out.contains("read the run: r"), "the approver reads the run: {out}");
    assert!(fails(&e, "w", &["land", "-p", "fx", "replace", S4, S5]).contains("4444444 is pushed, it is on the branch already"));

    ok(&e, "r", &["land", "-p", "fx", "approve", S2]);
    let out = ok(&e, "w", &["land", "-p", "fx", "add", S5, "--replaces", S2, "--clear-approval", "--note", "new base"]);
    assert!(out.contains("5555555 built") && out.contains("replaces 2222222, starts built since its approval was cleared") && out.contains("approve: r"), "{out}");
    assert!(fails(&e, "w", &["land", "-p", "fx", "add", S3, "--replaces", "abcdef1"]).contains("it is the entry this one replaces"));
    let log = common::git(&e.v.root, &["log", "--format=%s"]);
    assert!(log.contains("breadcrumb: land replace 1111111 4444444") && log.contains("breadcrumb: land replace 2222222 5555555"), "{log}");
}

/// A checkout of fx with `n` commits, their full shas oldest first.
fn commits(e: &Env, n: usize) -> Vec<String> {
    let dir = e._d.path().join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    common::git(&dir, &["init", "-q"]);
    let shas = (0..n)
        .map(|i| {
            common::git(&dir, &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "--allow-empty", "-m", &format!("c{i}")]);
            common::git(&dir, &["rev-parse", "HEAD"])
        })
        .collect();
    let mut p = e.v.load_project("fx").unwrap().unwrap();
    p.add_path(&dir.to_string_lossy());
    e.v.write_project(&p).unwrap();
    shas
}

fn parked_file(e: &Env) -> std::path::PathBuf {
    e.v.project_dir("fx").join(land::PARKED_FILE)
}

/// What a binary from before parked approvals writes for `land add`.
fn old_binary_adds(e: &Env, sha: &str, state: &str) {
    let line = format!(r#"{{"sha":"{sha}","items":[],"state":"{state}","at":"2026-11-17T10:00:00Z","steps":[{{"state":"built","at":"2026-11-17T10:00:00Z","by":"w"}}]}}"#);
    let mut text = std::fs::read_to_string(queue_file(e)).unwrap_or_default();
    text.push_str(&line);
    text.push('\n');
    std::fs::write(queue_file(e), text).unwrap();
}

#[test]
fn an_approval_before_the_entry_is_queued_parks_and_attaches_on_add() {
    let e = setup();
    item(&e, "x", "queued");
    let out = fails(&e, "r", &["land", "-p", "fx", "approve", "1234567"]);
    assert!(out.contains("fx has no checkout on this machine") && out.contains("not parked"), "{out}");
    let c = commits(&e, 2);
    let out = ok(&e, "r", &["land", "-p", "fx", "approve", &c[0][..10], "--note", "merge check only"]);
    assert!(out.contains(&format!("parked: {} approved by r", &c[0][..7])) && out.contains(&format!("attaches when `crumb land add {}` queues it", c[0])), "{out}");
    assert!(std::fs::read_to_string(parked_file(&e)).unwrap().contains(&c[0]), "the full sha is parked");
    assert!(!queue_file(&e).exists(), "the queue file old binaries read stays as it was");
    let list = ok(&e, "w", &["land", "-p", "fx"]);
    assert!(list.contains("the landing queue is empty\nparked approvals") && list.contains(&format!("{} approved by r", &c[0][..7])) && list.contains("(merge check only)"), "{list}");
    let out = ok(&e, "w", &["land", "-p", "fx", "add", &c[0][..9]]);
    assert!(out.contains(&format!("{} approved (merge check only)", &c[0][..7])) && out.contains("approved by r") && out.contains("before it was queued") && out.contains("verify or push: w"), "{out}");
    assert!(!ok(&e, "w", &["land", "-p", "fx"]).contains("parked"), "it attached");
    ok(&e, "w", &["land", "-p", "fx", "pushed", &c[0][..9], "--run", "3"]);

    assert!(fails(&e, "r", &["land", "-p", "fx", "approve", "abcd"]).contains("no queue entry"), "a short sha does not park");
    let out = fails(&e, "r", &["land", "-p", "fx", "approve", "1234567"]);
    assert!(out.contains("no checkout of fx here has that commit") && out.contains("not parked"), "a typo does not park: {out}");
    ok(&e, "r", &["land", "-p", "fx", "approve", &c[1]]);
    let out = ok(&e, "r", &["land", "-p", "fx", "drop", &c[1][..7]]);
    assert!(out.contains(&format!("dropped the parked approval {}", &c[1][..7])), "{out}");
}

#[test]
fn a_parked_approval_attaches_to_an_entry_an_older_binary_queued() {
    let e = setup();
    item(&e, "x", "queued");
    let c = commits(&e, 4);
    for s in &c {
        ok(&e, "r", &["land", "-p", "fx", "approve", s, "--note", "looked at it"]);
    }
    old_binary_adds(&e, &c[0][..7], "built");
    old_binary_adds(&e, &c[1], "approved");
    old_binary_adds(&e, &c[2], "built");
    let list = ok(&e, "w", &["land", "-p", "fx"]);
    assert!(list.contains(&format!("{} approved (looked at it)", &c[0][..7])) && list.contains(&format!("{} approved by r", &c[0][..7])) && list.contains("before it was queued"), "{list}");
    assert!(list.contains(&format!("dropped the approval r parked for {}", &c[1][..7])) && list.contains("is approved already"), "{list}");
    assert!(list.contains(&format!("{} approved by r", &c[3][..7])), "the one not queued stays parked: {list}");
    let es = entries(&e);
    assert_eq!(es[0].state, "approved");
    assert_eq!(es[0].next.as_deref(), Some("w"), "the builder pushes next");
    assert_eq!(es[2].state, "approved");
    assert!(!std::fs::read_to_string(parked_file(&e)).unwrap().contains(&c[0]));

    old_binary_adds(&e, &c[3], "built");
    let out = ok(&e, "w", &["land", "-p", "fx", "drop", &c[3]]);
    assert!(out.contains(&format!("dropped {} (approved) from the queue", &c[3][..7])), "{out}");
    assert!(!ok(&e, "w", &["land", "-p", "fx"]).contains("parked approvals"), "a drop clears it");
}

#[test]
fn approving_a_replaced_sha_names_the_new_one() {
    let e = setup();
    item(&e, "x", "queued");
    ok(&e, "w", &["land", "-p", "fx", "add", S1]);
    ok(&e, "w", &["land", "-p", "fx", "replace", S1, S4]);
    let out = fails(&e, "r", &["land", "-p", "fx", "approve", &S1[..9]]);
    assert!(out.contains("1111111 was replaced by 4444444, `crumb land approve 4444444` approves the new merge"), "{out}");
    assert!(!parked_file(&e).exists());
}

#[test]
fn a_replace_that_carries_or_clears_an_approval_drops_the_parked_one() {
    let e = setup();
    item(&e, "x", "queued");
    let c = commits(&e, 2);
    ok(&e, "w", &["land", "-p", "fx", "add", S1]);
    ok(&e, "w", &["land", "-p", "fx", "add", S2]);
    ok(&e, "r", &["land", "-p", "fx", "approve", S1, "--note", "pending the harness"]);
    ok(&e, "r", &["land", "-p", "fx", "verify", S1]);
    ok(&e, "r", &["land", "-p", "fx", "approve", S2]);
    ok(&e, "r", &["land", "-p", "fx", "approve", &c[0], "--note", "parked look"]);
    ok(&e, "r", &["land", "-p", "fx", "approve", &c[1], "--note", "parked look"]);
    let out = ok(&e, "w", &["land", "-p", "fx", "replace", S1, &c[0]]);
    assert!(out.contains(&format!("{} verified", &c[0][..7])) && out.contains(&format!("dropped the approval r parked for {}", &c[0][..7])) && out.contains("the approval of 1111111 carries over"), "{out}");
    let out = ok(&e, "w", &["land", "-p", "fx", "replace", S2, &c[1], "--clear-approval"]);
    assert!(out.contains(&format!("{} built", &c[1][..7])) && out.contains("`--clear-approval` asks for a new one"), "{out}");
    let es = entries(&e);
    assert_eq!((es[0].state.as_str(), es[1].state.as_str()), ("verified", "built"));
    assert!(es[1].note.is_none() && !ok(&e, "w", &["land", "-p", "fx"]).contains("parked"));
}

#[test]
fn dropping_or_replacing_the_last_open_carrier_moves_what_waited_for_it() {
    let e = setup();
    let x = item(&e, "Waits on a drop", "in-progress");
    let y = item(&e, "Waits on a replace", "queued");
    let z = item(&e, "Other", "queued");
    ok(&e, "w", &["land", "-p", "fx", "add", S1, "--items", &format!("{x},{y}")]);
    ok(&e, "w2", &["land", "-p", "fx", "add", S2, "--items", &x]);
    ok(&e, "w2", &["land", "-p", "fx", "add", S3, "--items", &y]);
    approved_only(&e, &[S1]);
    ok(&e, "w", &["land", "-p", "fx", "pushed", S1, "--run", "1"]);
    ok(&e, "r", &["land", "-p", "fx", "green", S1]);
    for id in [&x, &y] {
        let n = e.v.get(id).unwrap();
        assert_ne!(breadcrumb::backlog::state(&n), "review");
        assert!(n.refs().contains(&format!("commit:{S1}")), "a waiting item has the green piece as a ref at once: {:?}", n.refs());
    }
    let out = ok(&e, "r", &["land", "-p", "fx", "drop", S2]);
    assert!(out.contains("waited for 2222222, 1111111 read green carries it, so it moves now") && out.contains("-> review"), "{out}");
    let n = e.v.get(&x).unwrap();
    assert_eq!(breadcrumb::backlog::state(&n), "review");
    assert_eq!(built_by(&e, &x).as_deref(), Some("w"), "the green carrier's builder, not the one who dropped");
    let out = ok(&e, "w2", &["land", "-p", "fx", "replace", S3, S4, "--items", &z]);
    assert!(out.contains("waited for 3333333") && out.contains("-> review"), "{out}");
    assert_eq!(breadcrumb::backlog::state(&e.v.get(&y).unwrap()), "review");
    assert_eq!(breadcrumb::backlog::state(&e.v.get(&z).unwrap()), "queued");
}

#[test]
fn a_bad_parked_line_is_skipped_kept_and_named_by_doctor() {
    let e = setup();
    item(&e, "x", "queued");
    let c = commits(&e, 1);
    ok(&e, "r", &["land", "-p", "fx", "approve", &c[0]]);
    let good = std::fs::read_to_string(parked_file(&e)).unwrap();
    let text = format!("<<<<<<< HEAD\n{good}");
    std::fs::write(parked_file(&e), &text).unwrap();
    let list = ok(&e, "w", &["land", "-p", "fx"]);
    assert!(list.contains(&format!("{} approved by r", &c[0][..7])), "{list}");
    ok(&e, "w", &["land", "-p", "fx", "add", S1]);
    assert_eq!(std::fs::read_to_string(parked_file(&e)).unwrap(), text, "a step that leaves the parked approvals alone does not rewrite the file");
    let (_, out) = run(&e, "w", &["doctor"]);
    assert!(out.contains(land::PARKED_FILE) && out.contains("line 1 is not a parked approval"), "{out}");
    ok(&e, "w", &["land", "-p", "fx", "add", &c[0]]);
    assert_eq!(std::fs::read_to_string(parked_file(&e)).unwrap(), "<<<<<<< HEAD\n", "the parked one attached, the bad line stays");
}

#[test]
fn an_approval_note_is_kept_to_120_characters() {
    let e = setup();
    item(&e, "x", "queued");
    let long = "word ".repeat(100);
    ok(&e, "w", &["land", "-p", "fx", "add", S1, "--note", &long]);
    ok(&e, "r", &["land", "-p", "fx", "approve", S1, "--note", &long]);
    let es = entries(&e);
    assert!(es[0].note.as_ref().unwrap().chars().count() <= 120, "{:?}", es[0].note);
    assert!(es[0].memo.as_ref().unwrap().chars().count() > 400, "the add note keeps more");
}

#[test]
fn read_green_takes_comma_separated_shas_after_the_word() {
    let e = setup();
    item(&e, "x", "queued");
    approved(&e, &[S1, S2]);
    ok(&e, "w", &["land", "-p", "fx", "pushed", S1, S2, "--run", "1"]);
    let out = ok(&e, "r", &["land", "-p", "fx", "read", "green", &format!("{},{}", &S1[..7], &S2[..7])]);
    assert!(out.contains("1111111 green") && out.contains("2222222 green"), "{out}");
}

#[test]
fn a_re_approve_keeps_the_earlier_note_and_show_prints_one_entry_whole() {
    let e = setup();
    let a = item(&e, "Item", "in-progress");
    let long = format!("lane x {}", "word ".repeat(40)).trim().to_string();
    ok(&e, "w", &["land", "-p", "fx", "add", S1, "--items", &a, "--note", &long]);
    ok(&e, "r", &["land", "-p", "fx", "approve", S1, "--note", "tests pass on linux"]);
    let out = ok(&e, "r", &["land", "-p", "fx", "approve", S1, "--note", "tests pass on linux and windows"]);
    assert!(out.contains("the earlier note \"tests pass on linux\" by r stays with the entry, `crumb land show 1111111` lists it"), "{out}");
    let out = ok(&e, "r", &["land", "-p", "fx", "approve", S1]);
    assert!(out.contains("approved (tests pass on linux and windows)") && out.contains("keeps the note"), "{out}");

    let list = ok(&e, "w", &["land", "-p", "fx", "list"]);
    assert!(list.contains("...") && !list.contains(&long), "the list cuts the note: {list}");
    let show = ok(&e, "w", &["land", "-p", "fx", "show", &S1[..6]]);
    assert!(show.starts_with(&format!("{S1} approved")) && show.contains(&format!("  note: {long}\n")), "{show}");
    assert!(show.contains("  approval note: tests pass on linux and windows (by r)") && show.contains("  earlier approval notes:\n") && show.contains(" r: tests pass on linux\n"), "{show}");
    assert!(show.contains(&format!("    {a}")) || show.contains("[in-progress] Item"), "{show}");
    assert!(show.contains("  steps:\n    built ") && show.contains(" by w\n    approved "), "{show}");
    ok(&e, "w", &["land", "-p", "fx", "verify", S1]);
    let show = ok(&e, "w", &["land", "-p", "fx", "show", S1]);
    assert!(!show.contains("approval note:") && show.contains(" r: tests pass on linux and windows"), "verify keeps the condition readable: {show}");
}

#[test]
fn a_push_after_an_earlier_push_whose_run_is_unread_needs_no_force() {
    let e = setup();
    item(&e, "x", "queued");
    approved(&e, &[S1, S2, S3]);
    ok(&e, "w", &["land", "-p", "fx", "pushed", S1, "--run", "1"]);
    let out = ok(&e, "w", &["land", "-p", "fx", "pushed", S2, "--run", "2"]);
    assert!(out.contains("2222222 pushed, run 2"), "{out}");
    ok(&e, "w", &["land", "-p", "fx", "add", S4]);
    ok(&e, "w", &["land", "-p", "fx", "add", S5]);
    ok(&e, "r", &["land", "-p", "fx", "approve", S5]);
    let out = fails(&e, "w", &["land", "-p", "fx", "pushed", S5, "--run", "3"]);
    assert!(out.contains("3333333 is ahead in the queue and still approved"), "an entry not pushed yet still blocks: {out}");
}

#[test]
fn the_cli_and_the_mcp_tool_share_the_read_verbs() {
    let e = setup();
    let a = item(&e, "Item", "in-progress");
    ok(&e, "w", &["land", "-p", "fx", "add", S1, "--items", &a]);
    approved_only(&e, &[S1]);
    approved(&e, &[S2, S3]);
    ok(&e, "w", &["land", "-p", "fx", "pushed", S1, S2, S3, "--run", "9"]);
    let out = ok(&e, "r", &["land", "-p", "fx", "green", S1]);
    assert!(out.contains("1111111 green") && out.contains("-> review"), "{out}");
    let out = ok(&e, "r", &["land", "-p", "fx", "read", "red", S2]);
    assert!(out.contains("2222222 red"), "read <result> <sha> reads too: {out}");
    let srv = breadcrumb::mcp::Server { vault_root: e.v.root.clone(), cwd: e.v.root.clone() };
    let call = |args: serde_json::Value| srv.call_tool("land", args.as_object().unwrap());
    let out = call(json!({"action": "read", "result": "green", "project": "fx", "sha": S3})).unwrap();
    assert!(out.contains("3333333 green"), "{out}");
    assert!(call(json!({"action": "read", "project": "fx", "sha": S3})).unwrap_err().to_string().contains("result is required"));
    let out = call(json!({"action": "show", "project": "fx", "sha": S1})).unwrap();
    assert!(out.starts_with(S1), "{out}");
    let out = fails(&e, "r", &["land", "-p", "fx", "gren", S1]);
    assert!(out.contains("green"), "clap names the close one: {out}");
}

#[test]
fn the_next_line_names_a_renamed_session_by_its_current_name() {
    let e = setup();
    item(&e, "x", "queued");
    let claude = e._d.path().join("claude");
    const ID: &str = "aaaaaaaa-1111-2222-3333-444444444444";
    let file = |name: &str| {
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        let pid = std::process::id();
        std::fs::write(claude.join("sessions").join(format!("{pid}.json")), format!(r#"{{"pid":{pid},"sessionId":"{ID}","name":"{name}"}}"#)).unwrap();
    };
    let as_session = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_crumb")).args(args).env("BREADCRUMB_HOME", &e.v.root).env("BREADCRUMB_CLAUDE_DIR", &claude).env("CLAUDE_CODE_SESSION_ID", ID).env("BREADCRUMB_SESSION", "").output().unwrap();
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    file("neocad-e4");
    ok(&e, "w", &["land", "-p", "fx", "add", S1]);
    as_session(&["land", "-p", "fx", "approve", S1]);
    ok(&e, "w", &["land", "-p", "fx", "pushed", S1, "--run", "4"]);
    assert!(as_session(&["land", "-p", "fx"]).contains("read the run: neocad-e4 aaaaaaaa (you)"));
    file("neocad-bd");
    let list = as_session(&["land", "-p", "fx"]);
    assert!(list.contains("read the run: neocad-bd aaaaaaaa (you)") && !list.contains("neocad-e4"), "{list}");
}
