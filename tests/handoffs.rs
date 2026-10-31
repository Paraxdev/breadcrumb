mod common;

use breadcrumb::lifecycle::SUPERSEDES;
use breadcrumb::vault::Vault;
use std::process::Command;

const A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const B: &str = "bbbbbbbb-0000-4000-8000-000000000002";

fn crumb(v: &Vault, session: &str, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_crumb"))
        .args(args)
        .env("BREADCRUMB_HOME", &v.root)
        .env("BREADCRUMB_CLAUDE_DIR", v.root.join("no-claude"))
        .env("CLAUDE_CODE_SESSION_ID", session)
        .env("BREADCRUMB_SESSION", "")
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn handoff(v: &Vault, session: &str, title: &str, extra: &[&str]) -> (String, String) {
    let mut args = vec!["add", "-k", "handoff", "-t", title, "-p", "app"];
    args.extend_from_slice(extra);
    let out = crumb(v, session, &args);
    (out.split_whitespace().nth(1).unwrap().to_string(), out)
}

fn superseded(v: &Vault, id: &str) -> Vec<String> {
    v.get(id).unwrap().links.iter().filter(|l| l.rel == SUPERSEDES).map(|l| l.to.clone()).collect()
}

fn digest(v: &Vault) -> String {
    breadcrumb::ops::context(v, Some("app"), 15).unwrap()
}

fn tick() {
    std::thread::sleep(std::time::Duration::from_millis(1100));
}

fn age(v: &Vault, id: &str, days: i64) {
    let mut n = v.get(id).unwrap();
    n.created = breadcrumb::util::now() - chrono::Duration::days(days);
    v.write_note(&mut n).unwrap();
}

#[test]
fn another_sessions_handoff_stays_live_and_add_says_so() {
    let (_d, v) = common::vault();
    let (first, out) = handoff(&v, A, "Engine state", &[]);
    assert!(out.contains("superseded nothing, no other live handoff"), "{out}");
    tick();
    let (second, out) = handoff(&v, B, "UI state", &[]);
    assert!(superseded(&v, &second).is_empty());
    assert!(out.contains("superseded nothing, other live handoffs") && out.contains(&first) && out.contains("aaaaaaaa") && out.contains("Engine state"), "{out}");

    let d = digest(&v);
    assert!(d.contains(&format!("Latest handoff {} (", v.get(&second).unwrap().handle())), "{d}");
    assert!(d.contains("Other live handoffs:\n") && d.contains(&format!("{} aaaaaaaa ", v.get(&first).unwrap().handle())) && d.contains(": Engine state\n"), "{d}");
}

#[test]
fn the_same_session_replaces_its_own_and_prints_it() {
    let (_d, v) = common::vault();
    let (other, _) = handoff(&v, B, "Their state", &[]);
    tick();
    let (first, _) = handoff(&v, A, "My state", &[]);
    let (second, out) = handoff(&v, A, "My newer state", &[]);
    assert_eq!(superseded(&v, &second), vec![first.clone()]);
    assert!(out.contains("superseded ") && out.contains(&first) && !out.contains("superseded nothing"), "{out}");
    let d = digest(&v);
    assert!(!d.contains("My state") && d.contains(&other), "{d}");
}

#[test]
fn a_track_replaces_its_own_across_sessions_and_leaves_the_rest() {
    let (_d, v) = common::vault();
    let (plain, _) = handoff(&v, A, "Plain state", &[]);
    let (eng, _) = handoff(&v, A, "Engine state", &["--track", "Engine"]);
    assert!(superseded(&v, &eng).is_empty(), "a track does not replace the session's plain handoff");
    assert_eq!(v.get(&eng).unwrap().field("track").and_then(|x| x.as_str()), Some("engine"));
    let (ui, _) = handoff(&v, A, "UI state", &["--track", "ui"]);
    assert!(superseded(&v, &ui).is_empty());
    let (next, out) = handoff(&v, B, "Engine state two", &["--track", "engine"]);
    assert_eq!(superseded(&v, &next), vec![eng.clone()], "{out}");
    let (again, _) = handoff(&v, A, "Plain state two", &[]);
    assert_eq!(superseded(&v, &again), vec![plain]);
    assert!(digest(&v).contains("track ui"), "{}", digest(&v));
}

#[test]
fn supersedes_asks_for_another_sessions_by_id_or_slug() {
    let (_d, v) = common::vault();
    let (a, _) = handoff(&v, A, "Engine state of things", &[]);
    v.get(&a).unwrap();
    let (b, out) = handoff(&v, B, "Taking over", &["--supersedes", &a]);
    assert_eq!(superseded(&v, &b), vec![a.clone()], "{out}");
    assert!(out.contains(&format!("superseded {}", v.get(&a).unwrap().handle())), "{out}");

    let slug = v.get(&b).unwrap().slug().unwrap();
    let (c, _) = handoff(&v, A, "Taking it back", &["--supersedes", &slug]);
    assert_eq!(superseded(&v, &c), vec![b]);
}

#[test]
fn a_handoff_without_a_stamp_is_never_replaced_by_itself() {
    let (_d, v) = common::vault();
    let old = common::add(&v, "Legacy handoff", "handoff", Some("app"), &[]);
    let (mine, out) = handoff(&v, A, "New handoff", &[]);
    assert!(superseded(&v, &mine).is_empty() && out.contains(&old.id), "{out}");
    let next = common::add(&v, "Handoff outside a session", "handoff", Some("app"), &[]);
    assert!(next.links.is_empty());
}

#[test]
fn only_what_a_handoff_replaces_gets_unpinned() {
    let (_d, v) = common::vault();
    let (theirs, _) = handoff(&v, B, "Their state", &["--pin"]);
    let (mine, _) = handoff(&v, A, "My state", &["--pin"]);
    handoff(&v, A, "My newer state", &[]);
    assert!(v.get(&theirs).unwrap().pinned);
    assert!(!v.get(&mine).unwrap().pinned);
}

#[test]
fn old_others_leave_the_digest_unless_pinned_and_at_most_three_show() {
    let (_d, v) = common::vault();
    let stale = common::add(&v, "Stale legacy", "handoff", Some("app"), &[]);
    let kept = common::add(&v, "Pinned legacy", "handoff", Some("app"), &[]);
    breadcrumb::ops::update(&v, &kept.id, breadcrumb::ops::UpdateReq { pinned: Some(true), ..Default::default() }).unwrap();
    age(&v, &stale.id, 8);
    age(&v, &kept.id, 8);
    let newest = common::add(&v, "Newest", "handoff", Some("app"), &[]);
    let d = digest(&v);
    assert!(!d.contains(&stale.id) && d.contains(&kept.id) && d.contains(": Pinned legacy\n") && d.contains(&newest.id), "{d}");

    let mut recent = vec![];
    for i in 0..4 {
        let n = common::add(&v, &format!("Recent {i}"), "handoff", Some("app"), &[]);
        age(&v, &n.id, 6 - i);
        recent.push(n.id);
    }
    let d = digest(&v);
    let others: Vec<&str> = d.lines().skip_while(|l| *l != "Other live handoffs:").skip(1).take_while(|l| !l.starts_with("Recent")).collect();
    assert_eq!(others.iter().filter(|l| l.contains(": Recent ") || l.contains(": Pinned legacy")).count(), 3, "{d}");
    assert!(others.iter().any(|l| l.starts_with("+2 more: ")), "{d}");
}

#[test]
fn the_newest_stays_in_full_however_old_it_is() {
    let (_d, v) = common::vault();
    let old = breadcrumb::ops::add(&v, breadcrumb::ops::AddReq { title: "Long ago".into(), kind: Some("handoff".into()), project: Some("app".into()), body: Some("Where we stopped".into()), ..Default::default() }).unwrap();
    age(&v, &old.id, 30);
    let d = digest(&v);
    assert!(d.contains("Latest handoff") && d.contains("Where we stopped"), "{d}");
}

#[test]
fn the_digest_fits_with_many_live_handoffs() {
    let (_d, v) = common::vault();
    for i in 0..20 {
        let n = common::add(&v, &format!("Standing rule number {i} with a title long enough to matter for the budget"), "decision", Some("app"), &[]);
        breadcrumb::ops::update(&v, &n.id, breadcrumb::ops::UpdateReq { pinned: Some(true), ..Default::default() }).unwrap();
    }
    let body: String = (0..200).map(|i| format!("- step {i} of a long handoff that goes on\n")).collect();
    for i in 0..6 {
        breadcrumb::ops::add(&v, breadcrumb::ops::AddReq { title: format!("Handoff {i} whose title is long enough to take up most of a whole digest line"), kind: Some("handoff".into()), project: Some("app".into()), body: Some(body.clone()), ..Default::default() }).unwrap();
    }
    let ctx = digest(&v);
    assert!(ctx.chars().count() <= breadcrumb::digest::DIGEST_BUDGET, "{} chars", ctx.chars().count());
}

#[test]
fn a_track_is_one_word() {
    let (_d, v) = common::vault();
    let out = Command::new(env!("CARGO_BIN_EXE_crumb")).args(["add", "-k", "handoff", "-t", "x", "--track", "two words"]).env("BREADCRUMB_HOME", &v.root).output().unwrap();
    assert!(!out.status.success());
}

#[test]
fn the_mcp_add_takes_track_and_supersedes() {
    let (d, v) = common::vault();
    let srv = breadcrumb::mcp::Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let call = |args: serde_json::Value| srv.call_tool("add", args.as_object().unwrap()).unwrap();
    breadcrumb::ops::project_link(&v, "app", d.path(), None).unwrap();
    let first = call(serde_json::json!({"title": "First", "kind": "handoff", "track": "engine"}));
    assert!(first.contains("superseded nothing"), "{first}");
    let second = call(serde_json::json!({"title": "Second", "kind": "handoff", "track": "engine"}));
    assert!(second.contains("superseded ") && second.contains("First"), "{second}");
    let n = v.load_notes().into_iter().find(|n| n.title == "Second").unwrap();
    let third = call(serde_json::json!({"title": "Third", "kind": "handoff", "supersedes": [n.id]}));
    assert!(third.contains("Second"), "{third}");
    let t = v.load_notes().into_iter().find(|n| n.title == "Third").unwrap();
    assert!(t.links.iter().any(|l| l.to == n.id && l.rel == SUPERSEDES));
}
