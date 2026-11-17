mod common;

use breadcrumb::backlog::{self, Check, MoveReq, ReviewBy};
use breadcrumb::vault::Vault;
use serde_json::{Map, Value, json};
use std::process::Command;

fn crumb(v: &Vault, args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_crumb")).args(args).env("BREADCRUMB_HOME", &v.root).env_remove("CLAUDE_CODE_SESSION_ID").env_remove("BREADCRUMB_SESSION").output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

fn mcp(v: &Vault, tool: &str, args: Value) -> anyhow::Result<String> {
    let srv = breadcrumb::mcp::Server { vault_root: v.root.clone(), cwd: v.root.clone() };
    let a: Map<String, Value> = args.as_object().unwrap().clone();
    srv.call_tool(tool, &a)
}

fn to_review(v: &Vault, id: &str, who: &str, steps: &[&str]) -> backlog::MoveResult {
    backlog::move_item(v, id, MoveReq { state: "review".into(), steps: steps.iter().map(|s| s.to_string()).collect(), by: Some(who.into()), ..Default::default() }).unwrap()
}

fn pass(who: &str, evidence: &str) -> ReviewBy {
    ReviewBy { by: Some(who.into()), evidence: Some(evidence.into()), by_user: false }
}

fn agents(v: &mut Vault) {
    breadcrumb::config::set(v, "review_by", "agent").unwrap();
}

#[test]
fn claiming_a_backlog_or_queued_item_starts_it() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "Backlog one", "task", Some("app"), &[("state", "backlog")]);
    let b = common::add(&v, "Queued one", "task", Some("app"), &[("state", "queued")]);
    let c = common::add(&v, "Idea", "idea", Some("app"), &[]);
    for (id, want) in [(&a.id, "in-progress"), (&b.id, "in-progress")] {
        let n = backlog::claim(&v, id, "s1", false).unwrap();
        assert_eq!(backlog::state(&n), want);
        assert!(n.body.contains("-> in-progress"), "the move is in the history: {}", n.body);
    }
    let before = backlog::state(&v.get(&c.id).unwrap());
    assert_eq!(backlog::state(&backlog::claim(&v, &c.id, "s1", false).unwrap()), before, "an idea keeps its state");
    let d = common::add(&v, "In review", "task", Some("app"), &[]);
    to_review(&v, &d.id, "s1", &["try it"]);
    assert_eq!(backlog::state(&backlog::claim(&v, &d.id, "s2", true).unwrap()), "review", "only backlog and queued move");
}

#[test]
fn check_all_on_an_item_without_criteria_passes_with_a_note() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "No criteria", "task", Some("app"), &[]);
    let r = backlog::move_item(&v, &a.id, MoveReq { state: "done".into(), check: Some(Check::All), ..Default::default() }).unwrap();
    assert_eq!(r.to, "done");
    assert!(r.warnings.iter().any(|w| w.contains("no acceptance criteria")), "{:?}", r.warnings);
    let b = common::add(&v, "No criteria either", "task", Some("app"), &[]);
    let e = backlog::move_item(&v, &b.id, MoveReq { state: "done".into(), check: Some(Check::Some(vec![2])), ..Default::default() }).unwrap_err().to_string();
    assert!(e.contains("no acceptance criteria"), "{e}");
    let (ok, out, _) = crumb(&v, &["check", &b.id]);
    assert!(ok && out.contains("has no acceptance criteria"), "{out}");
}

#[test]
fn move_to_review_hints_once_at_what_is_still_missing() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "Hinted", "bug", Some("app"), &[]);
    let r = backlog::move_item(&v, &a.id, MoveReq { state: "review".into(), ..Default::default() }).unwrap();
    let w = r.warnings.join(" ");
    assert!(w.contains("steps for the reviewer") && w.contains("release note") && w.contains("none when a maker"), "{w}");

    let again = backlog::move_item(&v, &a.id, MoveReq { state: "review".into(), ..Default::default() }).unwrap();
    assert!(again.warnings.is_empty(), "a repeat that adds nothing hints nothing: {:?}", again.warnings);

    let r = backlog::move_item(&v, &a.id, MoveReq { state: "review".into(), release_note: Some("Saves faster".into()), steps: vec!["save".into()], ..Default::default() }).unwrap();
    let w = r.warnings.join(" ");
    assert!(w.contains("commit: ref") && !w.contains("release note") && !w.contains("steps for"), "only what is still missing: {w}");

    let b = common::add(&v, "All in one call", "bug", Some("app"), &[]);
    let (ok, out, err) = crumb(&v, &["move", &b.id, "review", "--step", "run it", "--ref", "commit:abcdef1234", "--release-note", "none"]);
    assert!(ok && !out.contains("warning") && !err.contains("warning"), "{out}{err}");
    let n = v.get(&b.id).unwrap();
    assert!(breadcrumb::ship::release_note_waived(&n) && breadcrumb::ship::release_note(&n).is_none());
}

#[test]
fn ship_leaves_out_an_item_whose_release_note_is_none() {
    let (_d, v) = common::vault();
    let repo = tempfile::tempdir().unwrap();
    common::git(repo.path(), &["init", "-q", "-b", "main"]);
    common::git(repo.path(), &["-c", "user.email=a@b", "-c", "user.name=a", "commit", "-q", "--allow-empty", "-m", "base"]);
    common::git(repo.path(), &["-c", "user.email=a@b", "-c", "user.name=a", "commit", "-q", "--allow-empty", "-m", "fix"]);
    let sha = common::git(repo.path(), &["rev-parse", "HEAD"]).trim().to_string();
    let a = common::add(&v, "Test only fix", "bug", Some("app"), &[]);
    let b = common::add(&v, "Visible fix", "bug", Some("app"), &[]);
    for (id, line) in [(&a.id, "none"), (&b.id, "Saving no longer hangs")] {
        backlog::move_item(&v, id, MoveReq { state: "review".into(), steps: vec!["x".into()], refs: vec![format!("commit:{sha}")], release_note: Some(line.into()), ..Default::default() }).unwrap();
    }
    let (ok, out, err) = crumb(&v, &["ship", "HEAD~1..HEAD", "--repo", &repo.path().to_string_lossy(), "-p", "app"]);
    assert!(ok, "{err}");
    assert!(out.contains("Saving no longer hangs") && !out.contains("Test only fix") && !out.contains("- none"), "{out}");
    assert!(out.contains("2 items landed"), "{out}");
}

#[test]
fn bracketed_user_steps_are_stored_as_user_steps_with_a_note() {
    let (_d, mut v) = common::vault();
    agents(&mut v);
    let a = common::add(&v, "Needs the user", "feature", Some("app"), &[]);
    let r = to_review(&v, &a.id, "a7", &["run the tests", "[user] install the build", "(User) open the app"]);
    let notes = r.warnings.join(" ");
    assert!(notes.contains("stored as 'user: install the build'") && notes.contains("'user: open the app'"), "{notes}");
    let r = backlog::review_pass(&v, &a.id, pass("b3", "tests pass")).unwrap();
    assert_eq!(r.to, "review", "the user's steps stay open");
    assert_eq!(backlog::open_user_steps(&r.note).len(), 2);
    assert!(backlog::is_user_step("[user] old text written by hand") && backlog::is_user_step("(USER) too"));
}

#[test]
fn a_reviewer_subagent_names_its_role_and_the_builder_stays_refused() {
    let (_d, mut v) = common::vault();
    agents(&mut v);
    let a = common::add(&v, "Built by a7", "feature", Some("app"), &[]);
    to_review(&v, &a.id, "a7", &["run it"]);
    let e = backlog::review_pass(&v, &a.id, pass("a7", "ran it")).unwrap_err().to_string();
    assert!(e.contains("--by a7/reviewer") && e.contains("--evidence"), "{e}");
    let e = backlog::review_pass(&v, &a.id, ReviewBy { by: Some("b3".into()), evidence: None, by_user: false }).unwrap_err().to_string();
    assert!(e.contains("--by") && e.contains("--evidence"), "{e}");
    let r = backlog::review_pass(&v, &a.id, pass("a7/reviewer", "ran it, it works")).unwrap();
    assert_eq!(r.to, "done");
    let rows = format!("{:?}", r.note.field("checked_by"));
    assert!(rows.contains("a7/reviewer") && rows.contains("run it"), "{rows}");
}

#[test]
fn a_check_split_between_two_reviewers_is_recorded_per_step() {
    let (_d, mut v) = common::vault();
    agents(&mut v);
    let a = common::add(&v, "Two halves", "feature", Some("app"), &[]);
    to_review(&v, &a.id, "a7", &["engine half", "ui half", "user: install it"]);
    let r = backlog::move_item(&v, &a.id, backlog::verdict_req(backlog::Verdict::Pass, pass("b3", "engine tests pass"), Some(Check::Some(vec![1])))).unwrap();
    assert_eq!(r.to, "review", "the ui half is still open");
    assert!(r.note.body.contains("review passed steps 1: engine tests pass"), "{}", r.note.body);

    let (ok, out, _) = crumb(&v, &["review", "-p", "app"]);
    assert!(ok && out.contains("checked: 1. engine half (by b3)") && out.contains("2. ui half"), "{out}");

    let (ok, out, err) = crumb(&v, &["review", "pass", &a.id, "--steps", "2", "--by", "c4", "--evidence", "clicked through the ui"]);
    assert!(ok, "{out}{err}");
    let n = v.get(&a.id).unwrap();
    assert_eq!(backlog::state(&n), "review", "every agent step is ticked, the user's step waits");
    assert_eq!(backlog::open_user_steps(&n).len(), 1);
    let who: Vec<(usize, String)> = backlog::checked_steps(&n).into_iter().map(|c| (c.position, c.by)).collect();
    assert_eq!(who, vec![(1, "b3".to_string()), (2, "c4".to_string())]);
    let e = backlog::move_item(&v, &a.id, backlog::verdict_req(backlog::Verdict::Pass, pass("c4", "x"), Some(Check::Some(vec![3])))).unwrap_err().to_string();
    assert!(e.contains("the user's"), "{e}");
}

#[test]
fn verdicts_go_in_bulk_on_the_cli_and_over_mcp() {
    let (_d, mut v) = common::vault();
    agents(&mut v);
    let ids: Vec<String> = (0..4).map(|i| common::add(&v, &format!("Item {i}"), "task", Some("app"), &[]).id).collect();
    for id in &ids {
        to_review(&v, id, "a7", &["try it"]);
    }
    let (ok, out, err) = crumb(&v, &["review", "pass", &ids[0], &ids[1], "--by", "b3", "--evidence", "ran both"]);
    assert!(ok, "{out}{err}");
    assert!(ids[..2].iter().all(|id| backlog::state(&v.get(id).unwrap()) == "done"));
    let (ok, _, err) = crumb(&v, &["review", "fail", &ids[2], "nosuch1", "--reason", "broken", "--by", "b3"]);
    assert!(!ok && err.contains("1 of 2 moves failed"), "{err}");
    assert_eq!(backlog::state(&v.get(&ids[2]).unwrap()), "queued");

    let c = common::add(&v, "Item mcp", "task", Some("app"), &[]);
    to_review(&v, &c.id, "a7", &["try it"]);
    to_review(&v, &ids[2], "a7", &["try it again"]);
    let out = mcp(
        &v,
        "move",
        json!({"id": [{"id": ids[3], "verdict": "pass", "evidence": "ran it"}, {"id": c.id, "verdict": "fail", "evidence": "still broken"}, ids[2]], "state": "done", "verdict": "pass", "evidence": "shared", "by": "b3"}),
    )
    .unwrap();
    assert!(out.contains("-> done") && out.contains("-> queued"), "{out}");
    assert_eq!(backlog::state(&v.get(&ids[3]).unwrap()), "done");
    assert_eq!(backlog::state(&v.get(&c.id).unwrap()), "queued");
    assert!(v.get(&ids[2]).unwrap().body.contains("review passed: shared"));
}

#[test]
fn append_keeps_long_text_whole() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "Long notes", "task", Some("app"), &[]);
    let para = "fix-round: approved at 0123456789abcdef on lane/x, integrate green. ".repeat(40);
    let text = format!("{para}\n\nsecond paragraph: the end of the text stays here");
    mcp(&v, "append", json!({"id": a.id, "text": text})).unwrap();
    let (ok, _, err) = crumb(&v, &["append", &a.id, &format!("cli {para} last words")]);
    assert!(ok, "{err}");
    let body = v.get(&a.id).unwrap().body;
    assert!(body.contains(para.trim_end()) && body.contains("the end of the text stays here") && body.contains("last words"), "{body}");
}
