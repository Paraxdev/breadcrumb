mod common;

use breadcrumb::backlog::{self, BoardFilter, MoveReq, QueuePos};
use breadcrumb::ops::Scope;
use breadcrumb::vault::Vault;
use serde_yaml::Value;
use std::path::Path;
use std::process::Command;

const LIVE: &str = "abcd1234-1111-2222-3333-444444444444";
const DEAD: &str = "dead0000-1111-2222-3333-444444444444";
const ME: &str = "ffff0000-1111-2222-3333-444444444444";

struct Env {
    _d: tempfile::TempDir,
    v: Vault,
    claude: std::path::PathBuf,
}

fn setup() -> Env {
    let (d, v) = common::vault();
    let claude = d.path().join("claude");
    std::fs::create_dir_all(claude.join("sessions")).unwrap();
    let file = |pid: u32, id: &str, name: &str| std::fs::write(claude.join("sessions").join(format!("{pid}.json")), format!(r#"{{"pid":{pid},"sessionId":"{id}","name":"{name}"}}"#)).unwrap();
    file(std::process::id(), LIVE, "neocad-live");
    Env { _d: d, v, claude }
}

fn crumb(e: &Env, session: &str, machine: &str, args: &[&str]) -> (bool, String) {
    run(&e.v.root, &e.claude, session, machine, args)
}

fn run(vault: &Path, claude: &Path, session: &str, machine: &str, args: &[&str]) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_crumb"))
        .args(args)
        .env("BREADCRUMB_HOME", vault)
        .env("BREADCRUMB_CLAUDE_DIR", claude)
        .env("CLAUDE_CODE_SESSION_ID", session)
        .env("BREADCRUMB_MACHINE", machine)
        .env("BREADCRUMB_SESSION", "")
        .output()
        .unwrap();
    (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

fn queued(e: &Env, title: &str) -> String {
    let id = common::add(&e.v, title, "task", Some("app"), &[]).id;
    backlog::queue(&e.v, &id, QueuePos::End, None).unwrap();
    id
}

fn next_id(e: &Env) -> String {
    crumb(e, ME, "pc-a", &["next", "--project", "app"]).1
}

fn context(e: &Env) -> String {
    crumb(e, ME, "pc-a", &["context", "--project", "app"]).1
}

fn go_quiet(v: &Vault, id: &str) {
    let mut n = v.get(id).unwrap();
    let old = breadcrumb::util::now() - chrono::Duration::days(2);
    n.updated = old;
    if n.field("claimed").is_some() {
        n.set_field("claimed", Value::String(breadcrumb::util::fmt_time(&old)));
    }
    v.write_note(&mut n).unwrap();
}

#[test]
fn a_branch_or_run_ref_on_a_queued_item_is_being_built_without_changing_its_state() {
    let e = setup();
    let lane = queued(&e, "Lane item");
    let run_ref = queued(&e, "Run item");
    let free = queued(&e, "Free item");
    crumb(&e, ME, "pc-a", &["update", &lane, "--ref", "branch:wave/one"]);
    crumb(&e, ME, "pc-a", &["update", &run_ref, "--ref", "run:wf_9"]);

    assert_eq!(backlog::state(&e.v.get(&lane).unwrap()), "queued", "the stored state stays");
    assert!(next_id(&e).contains(&free), "{}", next_id(&e));

    let ctx = context(&e);
    let (built, rest) = ctx.split_once("Queued (next").unwrap();
    assert!(built.contains("Being built") && built.contains("[being built branch:wave/one 0m]"), "{ctx}");
    assert!(built.contains("[being built run:wf_9 0m]"), "{ctx}");
    assert!(rest.contains(&free) && !rest.contains(&lane), "{ctx}");
    assert!(ctx.contains(&lane), "the id stays next to any slug");

    let scope = Scope::Project { slug: "app".into(), with_global: false };
    let cols = backlog::board(&e.v, &scope, &BoardFilter::default()).unwrap();
    let col = |st: &str| cols.iter().find(|c| c.state == st).unwrap();
    assert_eq!(col("being-built").items.len(), 2);
    assert_eq!(col("queued").items.len(), 1);
    assert!(backlog::board_text(&cols, "app").contains("## being-built (2)"));

    go_quiet(&e.v, &lane);
    assert!(context(&e).contains(&format!("{lane}")));
    let n = e.v.get(&lane).unwrap();
    assert!(backlog::being_built(&n).is_none(), "a lost lane does not hide its item forever");

    crumb(&e, ME, "pc-a", &["update", &run_ref, "--ref", "-run:wf_9"]);
    assert!(backlog::being_built(&e.v.get(&run_ref).unwrap()).is_none());
    let commit = queued(&e, "Other ref");
    crumb(&e, ME, "pc-a", &["update", &commit, "--ref", "commit:abc1234"]);
    assert!(backlog::being_built(&e.v.get(&commit).unwrap()).is_none(), "only branch and run mean running");
}

#[test]
fn the_holder_of_a_being_built_item_still_gets_it_from_next() {
    let e = setup();
    let lane = queued(&e, "Lane item");
    crumb(&e, LIVE, "pc-a", &["claim", &lane]);
    crumb(&e, LIVE, "pc-a", &["update", &lane, "--ref", "branch:x"]);
    assert!(crumb(&e, LIVE, "pc-a", &["next", "--project", "app"]).1.contains(&lane));
    assert!(next_id(&e).contains("no unblocked"), "{}", next_id(&e));
}

#[test]
fn going_back_to_the_queue_ends_the_old_build() {
    let e = setup();
    let a = common::add(&e.v, "Failed in review", "task", Some("app"), &[]).id;
    crumb(&e, ME, "pc-a", &["move", &a, "review", "--ref", "branch:one", "--ref", "commit:abc1234", "--check", "Try"]);
    let (ok, _) = crumb(&e, ME, "pc-a", &["review", "fail", &a, "--reason", "no"]);
    assert!(ok);
    let n = e.v.get(&a).unwrap();
    assert_eq!(n.refs(), ["commit:abc1234"]);
    assert!(n.body.contains("build refs ended: branch:one"), "{}", n.body);
    assert!(next_id(&e).contains(&a), "a failed item goes out again at once");

    let b = common::add(&e.v, "Lane restarted", "task", Some("app"), &[]).id;
    crumb(&e, ME, "pc-a", &["move", &b, "in-progress", "--ref", "branch:one"]);
    crumb(&e, ME, "pc-a", &["move", &b, "queued", "--ref", "branch:two"]);
    assert_eq!(e.v.get(&b).unwrap().refs(), ["branch:two"], "a ref the same move adds counts");

    let c = common::add(&e.v, "From the backlog", "task", Some("app"), &[]).id;
    crumb(&e, ME, "pc-a", &["update", &c, "--ref", "run:wf_1"]);
    backlog::queue(&e.v, &c, QueuePos::End, None).unwrap();
    assert_eq!(e.v.get(&c).unwrap().refs(), ["run:wf_1"], "backlog to queued ended nothing");
}

#[test]
fn queued_straight_to_review_warns_unless_claimed() {
    let e = setup();
    let a = queued(&e, "Never claimed");
    let (ok, out) = crumb(&e, ME, "pc-a", &["move", &a, "review", "--check", "Try"]);
    assert!(ok && out.contains("never claimed"), "{out}");
    let b = queued(&e, "Claimed");
    crumb(&e, ME, "pc-a", &["claim", &b]);
    assert!(!crumb(&e, ME, "pc-a", &["move", &b, "review", "--check", "Try"]).1.contains("never claimed"));
    let c = common::add(&e.v, "Normal way", "task", Some("app"), &[]).id;
    crumb(&e, ME, "pc-a", &["move", &c, "in-progress"]);
    assert!(!crumb(&e, ME, "pc-a", &["move", &c, "review", "--check", "Try"]).1.contains("never claimed"));
}

#[test]
fn in_progress_lines_show_owner_and_age() {
    let e = setup();
    let live = common::add(&e.v, "Live one", "task", Some("app"), &[]).id;
    crumb(&e, LIVE, "pc-a", &["move", &live, "in-progress"]);
    crumb(&e, LIVE, "pc-a", &["claim", &live]);
    let nobody = common::add(&e.v, "Nobody", "task", Some("app"), &[]).id;
    crumb(&e, ME, "pc-a", &["move", &nobody, "in-progress"]);
    let free = common::add(&e.v, "Free text owner", "task", Some("app"), &[]).id;
    crumb(&e, ME, "pc-a", &["move", &free, "in-progress"]);
    crumb(&e, ME, "pc-a", &["claim", &free, "--by", "a-long-free-text-name"]);

    let ctx = context(&e);
    assert!(ctx.contains("@neocad-live 0m"), "the stored id reads as today's name: {ctx}");
    assert!(ctx.contains("(no owner) 0m"), "{ctx}");
    assert!(ctx.contains("@a-long-free-text-name 0m"), "free text is not cut to 8 characters: {ctx}");
}

#[test]
fn a_dead_sessions_claim_is_stale_on_its_own_machine_only() {
    let e = setup();
    let dead_here = queued(&e, "Dead here");
    crumb(&e, DEAD, "pc-a", &["claim", &dead_here]);
    let n = e.v.get(&dead_here).unwrap();
    assert_eq!(n.field("claim_machine").and_then(|v| v.as_str()), Some("pc-a dead0000"));
    assert!(context(&e).contains("@dead0000 (stale)"), "{}", context(&e));
    assert!(next_id(&e).contains(&dead_here), "no longer keeps others away");
    let (ok, out) = crumb(&e, ME, "pc-a", &["claim", &dead_here, "--by", "taker"]);
    assert!(ok, "no force needed: {out}");
    assert!(e.v.get(&dead_here).unwrap().body.contains("claim taken over from dead0000"));
    assert_eq!(e.v.get(&dead_here).unwrap().field("claim_machine").and_then(|v| v.as_str()), Some("pc-a taker"));

    let live = queued(&e, "Live here");
    crumb(&e, LIVE, "pc-a", &["claim", &live]);
    assert!(context(&e).contains("@neocad-live") && !context(&e).contains("@neocad-live (stale)"));
    assert!(!crumb(&e, ME, "pc-a", &["claim", &live, "--by", "taker"]).0, "a live session's claim holds");
    go_quiet(&e.v, &live);
    assert!(crumb(&e, ME, "pc-a", &["claim", &live, "--by", "taker"]).0, "the one day rule still applies");
}

#[test]
fn a_session_of_another_machine_or_free_text_only_goes_stale_after_a_day() {
    let e = setup();
    let elsewhere = queued(&e, "Elsewhere");
    crumb(&e, DEAD, "pc-b", &["claim", &elsewhere]);
    let free = queued(&e, "Free text");
    crumb(&e, ME, "pc-a", &["claim", &free, "--by", "neocad-lead"]);
    let no_key = queued(&e, "Old binary claim");
    backlog::claim(&e.v, &no_key, DEAD.split('-').next().unwrap(), false).unwrap();
    let mut n = e.v.get(&no_key).unwrap();
    n.unset_field("claim_machine");
    e.v.write_note(&mut n).unwrap();
    let carried = queued(&e, "Old binary claimed over a new claim");
    crumb(&e, LIVE, "pc-a", &["claim", &carried]);
    let mut n = e.v.get(&carried).unwrap();
    n.set_field("assignee", Value::String("dead0000".into()));
    e.v.write_note(&mut n).unwrap();

    let ctx = context(&e);
    assert!(!ctx.contains("(stale)"), "{ctx}");
    assert!(next_id(&e).contains("no unblocked"), "{}", next_id(&e));
    for id in [&elsewhere, &free, &no_key, &carried] {
        assert!(!crumb(&e, ME, "pc-a", &["claim", id, "--by", "taker"]).0, "{id}");
    }
    assert!(crumb(&e, DEAD, "pc-b", &["context", "--project", "app"]).1.contains("@dead0000"), "own session never reads dead");

    go_quiet(&e.v, &elsewhere);
    assert!(context(&e).contains("(stale)"), "the day rule still covers them");
    assert!(next_id(&e).contains(&elsewhere));
}

#[test]
fn no_session_list_says_nothing_about_who_is_dead() {
    let e = setup();
    let a = queued(&e, "Claim");
    crumb(&e, DEAD, "pc-a", &["claim", &a]);
    let nowhere = e.claude.join("missing");
    let out = run(&e.v.root, &nowhere, ME, "pc-a", &["context", "--project", "app"]).1;
    assert!(out.contains("@dead0000") && !out.contains("(stale)"), "{out}");
}

#[test]
fn release_and_failed_review_drop_the_machine_key() {
    let e = setup();
    let a = queued(&e, "Claim");
    crumb(&e, ME, "pc-a", &["claim", &a]);
    crumb(&e, ME, "pc-a", &["release", &a]);
    assert!(e.v.get(&a).unwrap().field("claim_machine").is_none());
    crumb(&e, ME, "pc-a", &["claim", &a]);
    backlog::move_item(&e.v, &a, MoveReq { state: "review".into(), ..Default::default() }).unwrap();
    backlog::review_fail(&e.v, &a, "no", None).unwrap();
    assert!(e.v.get(&a).unwrap().field("claim_machine").is_none());
}
