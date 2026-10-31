mod common;

use breadcrumb::ops::{self, AddReq, UpdateReq};
use breadcrumb::vault::Vault;
use chrono::TimeZone;
use std::process::Command;

fn crumb(v: &Vault, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_crumb")).args(args).env("BREADCRUMB_HOME", &v.root).env("BREADCRUMB_CLAUDE_DIR", v.root.join("no-claude")).output().unwrap()
}

fn ok(v: &Vault, args: &[&str]) -> String {
    let o = crumb(v, args);
    assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn field(v: &Vault, id: &str, k: &str) -> Option<String> {
    v.get(id).unwrap().field(k).and_then(|x| x.as_str().map(String::from))
}

fn pin(v: &Vault, title: &str, kind: &str, y: i32, m: u32, d: u32, said: bool) -> String {
    let fields = if said { ops::with_said(vec![], Some("Exact words".into()), None) } else { vec![] };
    let n = ops::add(v, AddReq { title: title.into(), kind: Some(kind.into()), project: Some("app".into()), pinned: true, fields, ..Default::default() }).unwrap();
    let mut n = v.get(&n.id).unwrap();
    n.created = chrono::Utc.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap();
    v.write_note(&mut n).unwrap();
    n.id
}

#[test]
fn said_defaults_its_date_to_today_and_get_shows_both() {
    let (_d, v) = common::vault();
    let today = breadcrumb::util::fmt_date(&breadcrumb::util::now());
    let id = ok(&v, &["add", "-k", "feedback", "-t", "Plain words", "--said", "no walls of text: \"ever\""]).split_whitespace().nth(1).unwrap().to_string();
    assert_eq!(field(&v, &id, "said").as_deref(), Some("no walls of text: \"ever\""));
    assert_eq!(field(&v, &id, "said_on").as_deref(), Some(today.as_str()));
    let got = ok(&v, &["get", &id]);
    assert!(got.contains("said:") && got.contains("said_on:"), "{got}");

    ok(&v, &["update", &id, "--said", "shorter", "--said-on", "2026-10-30"]);
    assert_eq!(field(&v, &id, "said").as_deref(), Some("shorter"));
    assert_eq!(field(&v, &id, "said_on").as_deref(), Some("2026-10-30"));
    ok(&v, &["update", &id, "--said", "again"]);
    assert_eq!(field(&v, &id, "said_on").as_deref(), Some(today.as_str()), "a new quote is a new date");
    assert!(!crumb(&v, &["update", &id, "--said-on", "yesterday"]).status.success());

    ok(&v, &["update", &id, "--unset", "said"]);
    assert!(v.get(&id).unwrap().field("said").is_none() && v.get(&id).unwrap().field("said_on").is_none());
}

#[test]
fn only_new_prefs_without_a_quote_read_inferred() {
    let (_d, v) = common::vault();
    let old = pin(&v, "Old feedback", "feedback", 2026, 10, 20, false);
    let new = pin(&v, "New feedback", "feedback", 2030, 1, 2, false);
    let user = pin(&v, "New user fact", "user", 2030, 1, 2, false);
    let quoted = pin(&v, "Quoted feedback", "feedback", 2030, 1, 2, true);
    let decision = pin(&v, "New decision", "decision", 2030, 1, 2, false);
    let d = ops::context(&v, Some("app"), 15).unwrap();
    assert!(d.contains(&format!("{old} feedback Old feedback\n")), "{d}");
    assert!(d.contains(&format!("{new} feedback (inferred) New feedback\n")), "{d}");
    assert!(d.contains(&format!("{user} user (inferred) New user fact\n")), "{d}");
    assert!(d.contains(&format!("{quoted} feedback Quoted feedback\n")), "{d}");
    assert!(d.contains(&format!("{decision} decision New decision\n")), "{d}");

    ops::update(&v, &new, UpdateReq { fields: ops::with_said(vec![], Some("Now I said it".into()), None), ..Default::default() }).unwrap();
    assert!(ops::context(&v, Some("app"), 15).unwrap().contains(&format!("{new} feedback New feedback\n")));
}

#[test]
fn add_tells_the_writer_a_quote_is_missing() {
    let (_d, v) = common::vault();
    assert!(ok(&v, &["add", "-k", "feedback", "-t", "Some rule"]).contains("no quote"));
    assert!(!ok(&v, &["add", "-k", "feedback", "-t", "Other rule", "--said", "x"]).contains("no quote"));
    assert!(!ok(&v, &["add", "-k", "decision", "-t", "A decision"]).contains("no quote"));
}

#[test]
fn the_mcp_takes_said_as_an_argument_and_a_field() {
    let (d, v) = common::vault();
    let srv = breadcrumb::mcp::Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let call = |name: &str, args: serde_json::Value| srv.call_tool(name, args.as_object().unwrap()).unwrap();
    call("add", serde_json::json!({"title": "One", "kind": "feedback", "project": "global", "said": "first words"}));
    call("add", serde_json::json!({"title": "Two", "kind": "feedback", "project": "global", "fields": {"said": "second words", "said_on": "2026-10-01"}}));
    let find = |t: &str| v.load_notes().into_iter().find(|n| n.title == t).unwrap();
    assert_eq!(field(&v, &find("One").id, "said").as_deref(), Some("first words"));
    assert!(field(&v, &find("One").id, "said_on").is_some());
    assert_eq!(field(&v, &find("Two").id, "said_on").as_deref(), Some("2026-10-01"));
    call("update", serde_json::json!({"id": find("Two").id, "said": "third words"}));
    assert_eq!(field(&v, &find("Two").id, "said").as_deref(), Some("third words"));
}

#[test]
fn searching_finds_the_quote_but_not_its_date() {
    let (_d, v) = common::vault();
    let n = ops::add(&v, AddReq { title: "Rule".into(), kind: Some("feedback".into()), project: Some("app".into()), fields: ops::with_said(vec![], Some("xylophones are fine".into()), Some("1999-09-09".into())), ..Default::default() }).unwrap();
    let s = breadcrumb::ops::Scope::Project { slug: "app".into(), with_global: false };
    assert_eq!(ops::search(&v, "xylophones", &s, &Default::default()).unwrap().hits[0].note.id, n.id);
    assert!(ops::search(&v, "1999", &s, &Default::default()).unwrap().hits.is_empty());
}
