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

fn crumb_in_session(v: &Vault, args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_crumb")).args(args).env("BREADCRUMB_HOME", &v.root).env_remove("CLAUDE_CODE_SESSION_ID").env("BREADCRUMB_SESSION", "night-shift").output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

#[test]
fn a_session_check_leaves_user_steps_open_where_agents_review() {
    let (_d, mut v) = common::vault();
    agents(&mut v);
    let a = common::add(&v, "Thing", "feature", Some("app"), &[]);
    user_steps(&v, &a.id);

    let (ok, _, err) = crumb_in_session(&v, &["check", &a.id, "2"]);
    assert!(!ok && err.contains(&format!("step 2 of {} is the user's (USER: install it on the laptop), only the user ticks it, on the Review page of `breadcrumb serve`", a.id)), "{err}");
    let (ok, _, err) = crumb_in_session(&v, &["move", &a.id, "review", "--check", "1,2"]);
    assert!(!ok && err.contains("is the user's"), "move --check is held the same: {err}");
    assert_eq!(backlog::unmet_acceptance(&v.get(&a.id).unwrap()).len(), 3, "refusals tick nothing");

    let (ok, out, err) = crumb_in_session(&v, &["check", &a.id, "all"]);
    assert!(ok, "{err}");
    assert!(out.contains(&format!("{}: ticked steps 1, 3, still open: 2. USER: install it on the laptop\n", a.id)) && out.contains(&format!("left open for the user: 2. USER: install it on the laptop, only the user ticks these, with a pass on the Review page of `breadcrumb serve` or `breadcrumb check {} 2` in a terminal of their own", a.id)), "{out}");
    let r = backlog::review_pass(&v, &a.id, by("reviewer", Some("ran it"))).unwrap();
    assert_eq!(r.to, "review", "check all then an agent pass no longer reaches done past the user's step");

    let (ok, out, err) = crumb(&v, &["check", &a.id, "2"]);
    assert!(ok && !out.contains("left open"), "the user's terminal ticks it: {err}");
    assert!(backlog::unmet_acceptance(&v.get(&a.id).unwrap()).is_empty());

    let b = common::add(&v, "Other", "feature", Some("app"), &[]);
    user_steps(&v, &b.id);
    let r = backlog::move_item(&v, &b.id, MoveReq { state: "review".into(), check: Some(backlog::Check::All), ..Default::default() }).unwrap();
    assert_eq!(r.kept_for_user.len(), 1, "the MCP move check is a session's");
    assert!(backlog::move_text(&r).contains("left open for the user: 2."), "{}", backlog::move_text(&r));

    let mut v2 = v.clone();
    breadcrumb::config::set(&mut v2, "review_by", "user").unwrap();
    let c = common::add(&v2, "Third", "feature", Some("app"), &[]);
    user_steps(&v2, &c.id);
    let (ok, out, _) = crumb_in_session(&v2, &["check", &c.id, "all"]);
    assert!(ok && out.contains("ticked steps 1, 2, 3, all acceptance criteria met") && !out.contains("left open"), "with review_by user a user: step is a plain step: {out}");
}

#[test]
fn a_session_cannot_close_an_item_waiting_for_review_without_a_verdict() {
    let (_d, mut v) = common::vault();
    agents(&mut v);
    let a = common::add(&v, "Thing", "feature", Some("app"), &[]);
    let b = common::add(&v, "Other", "feature", Some("app"), &[]);
    built_by(&v, &a.id, "builder");
    built_by(&v, &b.id, "builder");
    let waits = format!("{} is in review and waits for a check (review_by = agent), `breadcrumb review pass {} --by <name> --evidence \"...\"` (MCP move with verdict pass and evidence)", a.id, a.id);

    for state in ["done", "shipped"] {
        let (ok, _, err) = crumb_in_session(&v, &["move", &a.id, state]);
        assert!(!ok && err.contains(&waits), "{state}: {err}");
    }
    let (ok, out, _) = crumb_in_session(&v, &["move", &a.id, &b.id, "done"]);
    assert!(!ok && out.contains(&format!("failed {}: {waits}", a.id)), "a batch move holds each: {out}");
    let (ok, _, err) = crumb_in_session(&v, &["update", &a.id, "--field", "state=done"]);
    assert!(!ok && err.contains(&waits), "{err}");
    let e = backlog::move_item(&v, &a.id, MoveReq { state: "done".into(), ..Default::default() }).unwrap_err().to_string();
    assert_eq!(e, waits, "the MCP move is a session's");
    let e = breadcrumb::ops::update(&v, &a.id, breadcrumb::ops::UpdateReq { fields: vec![("state".into(), breadcrumb::ops::FieldInput::Raw("shipped".into()))], ..Default::default() }).unwrap_err().to_string();
    assert_eq!(e, waits, "and so is the MCP update");
    assert_eq!(backlog::state(&v.get(&a.id).unwrap()), "review");

    let commits = [breadcrumb::ship::Commit { sha: "abcdef1234567".into(), merge: true, subject: "merge".into() }];
    breadcrumb::ops::update(&v, &a.id, breadcrumb::ops::UpdateReq { refs: vec!["commit:abcdef1234567".into()], ..Default::default() }).unwrap();
    let r = breadcrumb::ship::report(&v, "app", &commits, false).unwrap();
    assert!(r.open.is_empty() && r.not_landed[0].1.starts_with("waits for a check"), "ship leaves it in review for a session: {:?}", r.not_landed);
    assert!(backlog::ship_items(&v, std::slice::from_ref(&a.id), None, None, false).unwrap().is_empty());
    assert_eq!(breadcrumb::ship::report(&v, "app", &commits, true).unwrap().open.len(), 1, "the user ships it");

    let (ok, _, err) = crumb_in_session(&v, &["move", &a.id, "queued"]);
    assert!(ok, "other moves are free: {err}");
    let (ok, _, err) = crumb(&v, &["move", &b.id, "done"]);
    assert!(ok, "the user's terminal may close it: {err}");

    let c = common::add(&v, "Mine", "feature", Some("app"), &[]);
    user_steps(&v, &c.id);
    backlog::review_pass(&v, &c.id, by("reviewer", Some("ran it"))).unwrap();
    let (ok, _, err) = crumb_in_session(&v, &["move", &c.id, "done"]);
    assert!(!ok && err.contains("waits for the user's steps") && err.contains(&format!("`breadcrumb review pass {} --by user`", c.id)), "{err}");
}

#[test]
fn a_session_update_cannot_tick_drop_or_reword_the_users_steps_where_agents_review() {
    use breadcrumb::ops::{FieldInput, UpdateReq, update};
    let (_d, mut v) = common::vault();
    agents(&mut v);
    let a = common::add(&v, "Thing", "feature", Some("app"), &[]);
    user_steps(&v, &a.id);
    let set = |steps: &[&str], by_user: bool| {
        let list = serde_yaml::Value::Sequence(steps.iter().map(|s| serde_yaml::Value::String(s.to_string())).collect());
        update(&v, &a.id, UpdateReq { fields: vec![("acceptance".into(), FieldInput::Value(list))], by_user, ..Default::default() })
    };
    let e = set(&["[ ] Run it", "[x] USER: install it on the laptop", "[ ] Check the log"], false).unwrap_err().to_string();
    assert!(e.contains("step 'USER: install it on the laptop' of") && e.contains("only the user ticks it (review_by = agent)"), "{e}");
    for steps in [&["[ ] Run it", "[ ] Check the log"][..], &["[ ] Run it", "[ ] user: skip the laptop", "[ ] Check the log"][..]] {
        let e = set(steps, false).unwrap_err().to_string();
        assert!(e.contains("is in review and its step 'USER: install it on the laptop' waits for the user") && e.contains(&format!("`breadcrumb move {} queued` takes the item out of review", a.id)), "{e}");
    }
    assert_eq!(backlog::unmet_acceptance(&v.get(&a.id).unwrap()).len(), 3, "refusals change nothing");

    set(&["[x] Run it", "[ ] USER: install it on the laptop", "[ ] Check the log", "[ ] Read the diff"], false).unwrap();
    assert_eq!(backlog::unmet_acceptance(&v.get(&a.id).unwrap()).len(), 3, "the other steps are free to edit");
    set(&["[x] Run it", "[x] USER: install it on the laptop", "[ ] Check the log"], true).unwrap();
    assert_eq!(backlog::unmet_acceptance(&v.get(&a.id).unwrap()), vec!["[ ] Check the log"], "the user's own edit holds");

    let b = common::add(&v, "Not in review", "feature", Some("app"), &[]);
    let list = |steps: &[&str]| serde_yaml::Value::Sequence(steps.iter().map(|s| serde_yaml::Value::String(s.to_string())).collect());
    update(&v, &b.id, UpdateReq { fields: vec![("acceptance".into(), FieldInput::Value(list(&["[ ] user: try it"])))], ..Default::default() }).unwrap();
    update(&v, &b.id, UpdateReq { fields: vec![("acceptance".into(), FieldInput::Value(list(&["[ ] user: try it twice"])))], ..Default::default() }).unwrap();
    let e = update(&v, &b.id, UpdateReq { fields: vec![("acceptance".into(), FieldInput::Value(list(&["[x] user: try it twice"])))], ..Default::default() }).unwrap_err().to_string();
    assert!(e.contains("only the user ticks it"), "before review a session rewords but never ticks: {e}");
}

#[test]
fn a_session_keeps_the_users_open_steps_in_a_re_review_and_cannot_close_past_them_from_any_state() {
    use breadcrumb::ops::{FieldInput, UpdateReq, update};
    let (_d, mut v) = common::vault();
    agents(&mut v);
    let a = common::add(&v, "Thing", "feature", Some("app"), &[]);
    user_steps(&v, &a.id);
    let (ok, out, err) = crumb_in_session(&v, &["move", &a.id, "review", "--by", "builder", "--step", "Run it again", "--step", "Check the log"]);
    assert!(ok && out.contains("replaced 1 open step with the ones given"), "{out}{err}");
    let n = v.get(&a.id).unwrap();
    let mut open = backlog::review_steps(&n);
    open.sort();
    assert_eq!(open, vec!["Check the log", "Run it again", "USER: install it on the laptop"], "the user's step stays");
    backlog::move_item(&v, &a.id, MoveReq { state: "review".into(), steps: vec!["Only this".into()], by: Some("builder".into()), ..Default::default() }).unwrap();
    assert_eq!(backlog::open_user_steps(&v.get(&a.id).unwrap()).len(), 1, "the MCP move keeps it too");

    let (ok, _, err) = crumb_in_session(&v, &["move", &a.id, "queued"]);
    assert!(ok, "{err}");
    let holds = format!("{} has steps only the user ticks (", a.id);
    let to_review = format!("`breadcrumb move {} review --by <name>` sends it to review, where the user passes them", a.id);
    for args in [&["move", &a.id, "done"][..], &["move", &a.id, "shipped"][..], &["update", &a.id, "--field", "state=done"][..], &["update", &a.id, "--field", "state=shipped"][..]] {
        let (ok, _, err) = crumb_in_session(&v, args);
        assert!(!ok && err.contains(&holds) && err.contains("USER: install it on the laptop") && err.contains(&to_review), "{args:?}: {err}");
    }
    let (ok, _, err) = crumb_in_session(&v, &["update", &a.id, "--field", "state=in-progress"]);
    assert!(ok, "{err}");
    let e = backlog::move_item(&v, &a.id, MoveReq { state: "done".into(), ..Default::default() }).unwrap_err().to_string();
    assert!(e.starts_with(&holds), "from in-progress as well: {e}");
    assert_eq!(backlog::state(&v.get(&a.id).unwrap()), "in-progress");

    let list = |steps: &[&str]| FieldInput::Value(serde_yaml::Value::Sequence(steps.iter().map(|s| serde_yaml::Value::String(s.to_string())).collect()));
    let (ok, _, err) = crumb_in_session(&v, &["update", &a.id, "--kind", "note", "--unset", "state"]);
    assert!(ok, "{err}");
    let e = update(&v, &a.id, UpdateReq { fields: vec![("acceptance".into(), list(&["[x] USER: install it on the laptop"]))], ..Default::default() }).unwrap_err().to_string();
    assert!(e.contains("only the user ticks it"), "a kind change is no way to tick it: {e}");

    let b = common::add(&v, "Theirs", "feature", Some("app"), &[]);
    user_steps(&v, &b.id);
    backlog::move_item(&v, &b.id, MoveReq { state: "review".into(), steps: vec!["Only this".into()], by: Some("builder".into()), by_user: true, ..Default::default() }).unwrap();
    assert!(backlog::open_user_steps(&v.get(&b.id).unwrap()).is_empty(), "the user's own re-review replaces every open step");
    let c = common::add(&v, "Plain", "feature", Some("app"), &[("state", "queued")]);
    update(&v, &c.id, UpdateReq { fields: vec![("acceptance".into(), list(&["[ ] user: try it"]))], ..Default::default() }).unwrap();
    let (ok, _, err) = crumb(&v, &["move", &c.id, "done"]);
    assert!(ok, "the user's terminal closes it: {err}");
}
