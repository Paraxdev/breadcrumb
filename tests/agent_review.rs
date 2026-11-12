mod common;

use breadcrumb::backlog::{self, MoveReq, ReviewBy};
use breadcrumb::vault::Vault;
use std::process::Command;

fn crumb(v: &Vault, args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_crumb")).args(args).env("BREADCRUMB_HOME", &v.root).env_remove("CLAUDE_CODE_SESSION_ID").env_remove("BREADCRUMB_SESSION").output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

fn built_by(v: &Vault, id: &str, who: &str) {
    backlog::move_item(v, id, MoveReq { state: "review".into(), steps: vec!["Run it".into()], by: Some(who.into()), ..Default::default() }).unwrap();
}

fn by(who: &str, evidence: Option<&str>) -> ReviewBy {
    ReviewBy { by: Some(who.into()), evidence: evidence.map(str::to_string), by_user: false }
}

fn agents(v: &mut Vault) {
    breadcrumb::config::set(v, "review_by", "agent").unwrap();
}

#[test]
fn user_review_needs_no_evidence_and_anyone_may_pass() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "Thing", "feature", Some("app"), &[]);
    built_by(&v, &a.id, "builder");
    assert_eq!(v.get(&a.id).unwrap().field("built_by").and_then(|x| x.as_str()), Some("builder"));
    let r = backlog::review_pass(&v, &a.id, by("builder", None)).unwrap();
    assert_eq!(r.to, "done", "review_by user keeps the old behaviour");
}

#[test]
fn agent_review_refuses_the_builder_and_a_pass_without_evidence() {
    let (_d, mut v) = common::vault();
    agents(&mut v);
    let a = common::add(&v, "Thing", "feature", Some("app"), &[]);
    built_by(&v, &a.id, "builder");

    let e = backlog::review_pass(&v, &a.id, by("builder", Some("ran it"))).unwrap_err().to_string();
    assert!(e.contains("built by builder") && e.contains("--by"), "{e}");
    let e = backlog::review_fail(&v, &a.id, "broken", by("Builder", None)).unwrap_err().to_string();
    assert!(e.contains("built by"), "a fail by the builder is refused too: {e}");

    let e = backlog::review_pass(&v, &a.id, by("reviewer", None)).unwrap_err().to_string();
    assert!(e.contains("--evidence"), "{e}");
    let e = backlog::review_pass(&v, &a.id, by("reviewer", Some("  "))).unwrap_err().to_string();
    assert!(e.contains("--evidence"), "blank evidence is none: {e}");
    assert_eq!(backlog::state(&v.get(&a.id).unwrap()), "review", "refusals change nothing");

    let r = backlog::review_pass(&v, &a.id, by("reviewer", Some("ran crumb x, saw y"))).unwrap();
    assert_eq!(r.to, "done");
    assert!(r.note.body.contains("review passed: ran crumb x, saw y"), "{}", r.note.body);
    assert!(backlog::unmet_acceptance(&r.note).is_empty(), "a pass ticks the steps");
}

#[test]
fn agent_review_fail_keeps_reason_and_evidence_and_the_user_on_the_web_is_exempt() {
    let (_d, mut v) = common::vault();
    agents(&mut v);
    let a = common::add(&v, "Thing", "bug", Some("app"), &[]);
    built_by(&v, &a.id, "builder");
    let r = backlog::review_fail(&v, &a.id, "button does nothing", by("reviewer", Some("clicked it twice"))).unwrap();
    assert_eq!(r.to, "queued");
    assert!(r.note.body.contains("review failed: button does nothing\nchecked: clicked it twice"), "{}", r.note.body);

    built_by(&v, &a.id, "builder");
    let r = backlog::review_pass(&v, &a.id, ReviewBy { by: Some("builder".into()), evidence: None, by_user: true }).unwrap();
    assert_eq!(r.to, "done", "the user's own verdict is not held to agent rules");
}

#[test]
fn moving_back_to_review_records_the_new_builder() {
    let (_d, mut v) = common::vault();
    agents(&mut v);
    let a = common::add(&v, "Thing", "task", Some("app"), &[]);
    built_by(&v, &a.id, "first");
    backlog::review_fail(&v, &a.id, "no", by("second", None)).unwrap();
    built_by(&v, &a.id, "second");
    let e = backlog::review_pass(&v, &a.id, by("second", Some("ok"))).unwrap_err().to_string();
    assert!(e.contains("built by second"), "the fix's builder cannot pass it: {e}");
    assert!(backlog::review_pass(&v, &a.id, by("first", Some("ok"))).is_ok(), "the first builder may review the second's fix");
}

#[test]
fn config_lists_sets_validates_and_commits() {
    let (_d, v) = common::vault();
    let (ok, out, _) = crumb(&v, &["config"]);
    assert!(ok && out.contains("review_by = user") && out.contains("judge_nudge = off"), "{out}");

    let (ok, out, err) = crumb(&v, &["config", "set", "review_by", "agent"]);
    assert!(ok, "{err}");
    assert!(out.contains("review_by: user -> agent"), "{out}");
    let (_, out, _) = crumb(&v, &["config"]);
    assert!(out.contains("review_by = agent (default user)"), "{out}");
    assert_eq!(crumb(&v, &["config", "get", "review_by"]).1.trim(), "agent");
    assert!(Vault::open(&v.root).unwrap().config.review_by == "agent", "saved to breadcrumb.toml");

    let (ok, _, err) = crumb(&v, &["config", "set", "review_by", "robot"]);
    assert!(!ok && err.contains("user, agent"), "{err}");
    let (ok, _, err) = crumb(&v, &["config", "set", "stale_days", "soon"]);
    assert!(!ok && err.contains("whole number"), "{err}");
    let (ok, _, err) = crumb(&v, &["config", "set", "auto_commit", "maybe"]);
    assert!(!ok && err.contains("true or false"), "{err}");
    let (ok, _, err) = crumb(&v, &["config", "set", "colour", "red"]);
    assert!(!ok && err.contains("unknown setting"), "{err}");
    assert!(crumb(&v, &["config", "set", "remote", "https://example.com/v.git"]).0);
    assert!(crumb(&v, &["config", "set", "remote", ""]).0, "an empty remote unsets it");
    assert!(Vault::open(&v.root).unwrap().config.remote.is_none());
    let log = common::git(&v.root, &["log", "--format=%s"]);
    assert!(log.contains("breadcrumb: config review_by = agent"), "{log}");
}

#[test]
fn cli_review_pass_takes_evidence_and_review_lists_the_agent_rules() {
    let (_d, mut v) = common::vault();
    agents(&mut v);
    let a = common::add(&v, "Thing", "feature", Some("app"), &[]);
    built_by(&v, &a.id, "builder");
    let (_, out, _) = crumb(&v, &["review", "--all"]);
    assert!(out.contains("agents review here") && out.contains("--evidence"), "{out}");
    let (ok, _, err) = crumb(&v, &["review", "pass", &a.id, "--by", "reviewer"]);
    assert!(!ok && err.contains("--evidence"), "{err}");
    let (ok, _, err) = crumb(&v, &["review", "pass", &a.id, "--by", "reviewer", "--evidence", "ran it, it works"]);
    assert!(ok, "{err}");
    assert!(v.get(&a.id).unwrap().body.contains("review passed: ran it, it works"));
}

#[test]
fn digest_names_review_work_for_agents() {
    let (_d, mut v) = common::vault();
    let a = common::add(&v, "Thing", "feature", Some("app"), &[]);
    built_by(&v, &a.id, "builder");
    let d = breadcrumb::digest::context(&v, Some("app"), 15).unwrap();
    assert!(!d.contains("Review:"), "off with review_by user: {d}");
    agents(&mut v);
    let d = breadcrumb::digest::context(&v, Some("app"), 15).unwrap();
    assert!(d.contains("Review: 1 waiting for an agent check"), "{d}");
}

fn user_steps(v: &Vault, id: &str) {
    backlog::move_item(v, id, MoveReq { state: "review".into(), steps: vec!["Run it".into(), "USER: install it on the laptop".into(), "Check the log".into()], by: Some("builder".into()), ..Default::default() }).unwrap();
}

#[test]
fn an_agent_pass_ticks_its_steps_and_leaves_user_steps_for_the_user() {
    let (_d, mut v) = common::vault();
    agents(&mut v);
    let a = common::add(&v, "Thing", "feature", Some("app"), &[]);
    user_steps(&v, &a.id);
    assert!(backlog::is_user_step("[x] user: done") && !backlog::is_user_step("users can log in"));

    let (_, out, _) = crumb(&v, &["review", "--all"]);
    assert!(out.contains("2. [user] install it on the laptop") && out.contains("1. Run it") && out.contains("Steps marked [user] are the user's"), "{out}");

    let (ok, out, err) = crumb(&v, &["review", "pass", &a.id, "--by", "reviewer", "--evidence", "ran it, log is clean"]);
    assert!(ok, "{err}");
    assert!(out.contains("the agent steps passed, it stays in review for the user's steps: 2. USER: install it on the laptop"), "{out}");
    assert!(out.contains(&format!("`breadcrumb review pass {} --by user`", a.id)) && out.contains("Review page of `breadcrumb serve`"), "{out}");
    let n = v.get(&a.id).unwrap();
    assert_eq!(backlog::state(&n), "review");
    assert_eq!(backlog::unmet_acceptance(&n), ["USER: install it on the laptop"]);
    assert!(n.body.contains("agent steps passed: ran it, log is clean") && n.body.contains("agent steps passed, waiting for the user (by reviewer)"), "{}", n.body);

    let r = backlog::move_item(&v, &a.id, MoveReq { state: "done".into(), verdict: Some(backlog::Verdict::Pass), by: Some("other".into()), evidence: Some("again".into()), ..Default::default() }).unwrap();
    assert_eq!((r.to.as_str(), r.waiting_for_user.len()), ("review", 1), "move with verdict pass behaves the same");

    let (ok, _, err) = crumb(&v, &["review", "pass", &a.id, "--by", "User"]);
    assert!(ok, "{err}");
    let n = v.get(&a.id).unwrap();
    assert_eq!(backlog::state(&n), "done", "the user's pass needs no evidence and ticks everything");
    assert!(backlog::unmet_acceptance(&n).is_empty());
}

#[test]
fn a_session_cannot_review_as_the_user() {
    let (_d, mut v) = common::vault();
    agents(&mut v);
    let a = common::add(&v, "Thing", "feature", Some("app"), &[]);
    user_steps(&v, &a.id);
    let refused = "only the user reviews as user";
    for (key, val) in [("CLAUDE_CODE_SESSION_ID", "aaaaaaaa-0000-4000-8000-000000000001"), ("BREADCRUMB_SESSION", "night-shift")] {
        for args in [vec!["review", "pass", &a.id, "--by", "user"], vec!["review", "pass", &a.id, "--by", "USER", "--evidence", "looked fine"], vec!["review", "fail", &a.id, "--reason", "nope", "--by", "user"]] {
            let o = Command::new(env!("CARGO_BIN_EXE_crumb")).args(&args).env("BREADCRUMB_HOME", &v.root).env_remove("CLAUDE_CODE_SESSION_ID").env_remove("BREADCRUMB_SESSION").env(key, val).output().unwrap();
            assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains(refused), "{key} {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        }
    }
    let err = backlog::move_item(&v, &a.id, MoveReq { state: "done".into(), verdict: Some(backlog::Verdict::Pass), by: Some(" User ".into()), evidence: Some("fine".into()), ..Default::default() }).unwrap_err().to_string();
    assert!(err.starts_with(refused), "the MCP move carries no user either: {err}");
    assert_eq!(backlog::state(&v.get(&a.id).unwrap()), "review");

    let b = common::add(&v, "Other", "feature", Some("app"), &[]);
    built_by(&v, &b.id, "builder");
    let (ok, _, err) = crumb(&v, &["review", "pass", &b.id, "--by", "user"]);
    assert!(ok, "a terminal with no session is the user's: {err}");
    assert_eq!(backlog::state(&v.get(&b.id).unwrap()), "done");
}

#[test]
fn user_steps_are_plain_steps_for_a_user_review_and_the_web_pass_finishes_them() {
    let (_d, mut v) = common::vault();
    let a = common::add(&v, "Thing", "feature", Some("app"), &[]);
    user_steps(&v, &a.id);
    let (_, out, _) = crumb(&v, &["review", "--all"]);
    assert!(!out.contains("[user]"), "{out}");
    assert_eq!(backlog::review_pass(&v, &a.id, by("someone", None)).unwrap().to, "done");

    agents(&mut v);
    let b = common::add(&v, "Other", "feature", Some("app"), &[]);
    user_steps(&v, &b.id);
    let r = backlog::review_pass(&v, &b.id, ReviewBy { by: Some("web".into()), evidence: None, by_user: true }).unwrap();
    assert_eq!(r.to, "done");
    let c = common::add(&v, "Failing", "feature", Some("app"), &[]);
    user_steps(&v, &c.id);
    assert_eq!(backlog::review_fail(&v, &c.id, "log has errors", by("reviewer", None)).unwrap().to, "queued", "fail works as before");
}

#[test]
fn digest_counts_items_left_with_user_steps_as_waiting_for_the_user() {
    let (_d, mut v) = common::vault();
    agents(&mut v);
    let a = common::add(&v, "Thing", "feature", Some("app"), &[]);
    user_steps(&v, &a.id);
    backlog::review_pass(&v, &a.id, by("reviewer", Some("ran it"))).unwrap();
    let d = breadcrumb::digest::context(&v, Some("app"), 15).unwrap();
    assert!(d.contains("Review: 0 waiting for an agent check, 1 waiting for the user (review_by = agent), the user passes them on the Review page of `breadcrumb serve`"), "{d}");
    let b = common::add(&v, "Other", "feature", Some("app"), &[]);
    built_by(&v, &b.id, "builder");
    let d = breadcrumb::digest::context(&v, Some("app"), 15).unwrap();
    assert!(d.contains("Review: 1 waiting for an agent check, 1 waiting for the user (review_by = agent), `breadcrumb review`"), "{d}");
}
