mod common;

use breadcrumb::judge;
use breadcrumb::ops::{self, AddReq, FieldInput};
use breadcrumb::vault::Vault;
use std::process::Command;

fn crumb_in(v: &Vault, cwd: &std::path::Path, args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_crumb"))
        .args(args)
        .current_dir(cwd)
        .env("BREADCRUMB_HOME", &v.root)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("BREADCRUMB_SESSION")
        .output()
        .unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

#[test]
fn a_judgment_lands_in_the_judge_project_with_where_it_was_seen() {
    let (_d, v) = common::vault();
    let n = judge::judge(&v, "search ranked the gotcha below an old note", "hindered", Some("search"), Some("neocad")).unwrap();
    assert_eq!(n.kind, "judgment");
    assert_eq!(n.project.as_deref(), Some("breadcrumb"));
    assert_eq!(judge::verdict(&n), "hindered");
    assert_eq!(judge::about(&n), "search");
    assert_eq!(n.field("seen_in").and_then(|x| x.as_str()), Some("neocad"));

    let n = judge::judge(&v, "merge was easy", "helped", None, Some("breadcrumb")).unwrap();
    assert!(n.field("seen_in").is_none(), "no seen_in when it was seen at home");
    assert_eq!(judge::about(&n), "general");

    assert!(judge::judge(&v, "x", "meh", None, None).unwrap_err().to_string().contains("hindered, confusing, missing, helped"));
    assert!(judge::judge(&v, "  ", "helped", None, None).is_err());
}

#[test]
fn add_with_kind_judgment_is_routed_too_so_mcp_agents_need_nothing_new() {
    let (_d, v) = common::vault();
    let n = ops::add(
        &v,
        AddReq { title: "the digest cut my handoff".into(), kind: Some("judgment".into()), project: Some("app".into()), fields: vec![("verdict".into(), FieldInput::Raw("confusing".into()))], ..Default::default() },
    )
    .unwrap();
    assert_eq!(n.project.as_deref(), Some("breadcrumb"));
    assert_eq!(n.field("seen_in").and_then(|x| x.as_str()), Some("app"));
}

#[test]
fn list_groups_by_what_it_is_about_and_drops_triaged_ones() {
    let (_d, v) = common::vault();
    let a = judge::judge(&v, "search missed an exact id", "hindered", Some("search"), None).unwrap();
    judge::judge(&v, "search found it at once", "helped", Some("search"), None).unwrap();
    judge::judge(&v, "no way to see who holds a lease", "missing", Some("lease"), None).unwrap();
    let rows = judge::list(&v, None, false);
    assert_eq!(rows.len(), 3);
    let text = judge::list_text(&rows);
    assert!(text.contains("  search: 1 hindered, 1 helped") && text.contains("  lease: 1 missing"), "{text}");
    assert!(text.find("search:").unwrap() < text.find("lease:").unwrap(), "largest group first: {text}");

    let item = common::add(&v, "Search ranks exact ids first", "bug", Some("breadcrumb"), &[]);
    ops::link(&v, &a.id, &item.id, Some(judge::FILED_AS), false).unwrap();
    assert_eq!(judge::list(&v, None, false).len(), 2, "a filed judgment leaves the list");
    let all = judge::list(&v, None, true);
    assert!(judge::list_text(&all).contains("[triaged]"));
}

#[test]
fn the_digest_asks_by_the_setting_and_counts_open_judgments_at_home() {
    let (_d, mut v) = common::vault();
    let d = breadcrumb::digest::context(&v, Some("app"), 15).unwrap();
    assert!(!d.contains("Judge breadcrumb"), "off by default: {d}");

    breadcrumb::config::set(&mut v, "judge_nudge", "session").unwrap();
    for _ in 0..2 {
        let d = breadcrumb::digest::context(&v, Some("app"), 15).unwrap();
        assert!(d.contains("Judge breadcrumb: before you finish"), "every digest: {d}");
    }

    breadcrumb::config::set(&mut v, "judge_nudge", "daily").unwrap();
    assert!(breadcrumb::digest::context(&v, Some("app"), 15).unwrap().contains("Judge breadcrumb"));
    assert!(!breadcrumb::digest::context(&v, Some("app"), 15).unwrap().contains("Judge breadcrumb"), "once a day");

    judge::judge(&v, "x went wrong", "hindered", None, Some("app")).unwrap();
    let d = breadcrumb::digest::context(&v, Some("breadcrumb"), 15).unwrap();
    assert!(d.contains("Judgments: 1 from agents to triage"), "{d}");
    assert!(!breadcrumb::digest::context(&v, Some("app"), 15).unwrap().contains("Judgments:"), "only at home");

    let e = breadcrumb::config::set(&mut v, "judge_nudge", "hourly").unwrap_err().to_string();
    assert!(e.contains("off, session, daily"), "{e}");
}

#[test]
fn cli_judge_files_from_the_cwd_project_and_lists() {
    let (d, v) = common::vault();
    let proj = d.path().join("app");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(crumb_in(&v, &proj, &["project", "link", "app"]).0);
    let (ok, out, err) = crumb_in(&v, &proj, &["judge", "brief was what I needed", "--verdict", "helped", "--about", "brief"]);
    assert!(ok, "{err}");
    assert!(out.contains("judged"), "{out}");
    let n = v.load_notes().into_iter().find(|n| n.kind == "judgment").unwrap();
    assert_eq!(n.project.as_deref(), Some("breadcrumb"));
    assert_eq!(n.field("seen_in").and_then(|x| x.as_str()), Some("app"));
    let (_, out, _) = crumb_in(&v, &proj, &["judge", "--list"]);
    assert!(out.contains("brief: 1 helped"), "{out}");
    let (_, out, _) = crumb_in(&v, &proj, &["judge"]);
    assert!(out.contains("brief: 1 helped"), "a bare judge lists: {out}");
}
