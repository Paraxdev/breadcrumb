mod common;

use breadcrumb::backlog::{self, BoardFilter};
use breadcrumb::mcp::Server;
use breadcrumb::ops::{self, Scope};
use breadcrumb::question::{self, AskReq, Opt};
use breadcrumb::vault::Vault;
use serde_json::{Value, json};
use std::io::Cursor;

fn err<T: std::fmt::Debug>(r: anyhow::Result<T>) -> String {
    format!("{:#}", r.unwrap_err())
}

fn app() -> Scope {
    Scope::Project { slug: "app".into(), with_global: false }
}

fn opts(labels: &[&str]) -> Vec<Opt> {
    labels.iter().map(|l| Opt { label: l.to_string(), desc: format!("{l} way") }).collect()
}

fn ask(v: &Vault, id: &str, text: &str, multi: bool, free: bool) -> String {
    let req = AskReq { id: id.into(), text: text.into(), options: opts(&["red", "blue", "green"]), multi, free, by: Some("agent-1".into()) };
    let (n, q) = question::ask(v, req).unwrap();
    question::qid(&n, &q)
}

fn queued(v: &Vault, title: &str) -> String {
    common::add(v, title, "task", Some("app"), &[("state", "queued")]).id
}

#[test]
fn an_open_question_locks_the_item_for_next_and_claim() {
    let (_d, v) = common::vault();
    let a = queued(&v, "Pick the palette");
    let b = queued(&v, "Second item");
    backlog::queue(&v, &a, backlog::QueuePos::Top, None).unwrap();
    assert_eq!(backlog::next(&v, &app(), None, None).unwrap().unwrap().id, a);

    let qid = ask(&v, &a, "Which colour?", false, false);
    assert_eq!(qid, format!("{a}.1"));
    assert_eq!(backlog::next(&v, &app(), None, None).unwrap().unwrap().id, b, "next skips the locked item");
    let e = err(backlog::claim(&v, &a, "agent-2", true));
    assert!(e.contains("waiting on the user") && e.contains(&qid), "claim says why, even forced: {e}");

    let cols = backlog::board(&v, &app(), &BoardFilter::default()).unwrap();
    let waiting = cols.iter().find(|c| c.state == backlog::WAITING).unwrap();
    assert_eq!(waiting.items.len(), 1);
    assert!(waiting.items[0].line().contains("[waiting on user]"));
    assert!(!cols.iter().filter(|c| c.state != backlog::WAITING).flat_map(|c| &c.items).any(|i| i.id == a), "the item sits only in the waiting column");

    let ctx = ops::context(&v, Some("app"), 15).unwrap();
    assert!(ctx.contains(&format!("waiting on user {qid}: Which colour?")), "{ctx}");
}

#[test]
fn answering_unlocks_the_item_and_next_returns_it_first() {
    let (_d, v) = common::vault();
    let a = queued(&v, "Pick the palette");
    let b = queued(&v, "Important other item");
    let mut top = v.get(&b).unwrap();
    top.set_field("priority", "p0".into());
    v.write_note(&mut top).unwrap();
    let qid = ask(&v, &a, "Which colour?", false, true);

    assert!(err(question::answer(&v, &qid, &[], None, "user")).contains("pick an option"));
    assert!(err(question::answer(&v, &qid, &[1, 2], None, "user")).contains("single"));
    assert!(err(question::answer(&v, &qid, &[4], None, "user")).contains("3 options"));
    let (n, q) = question::answer(&v, &qid, &[2], Some("but darker"), "user").unwrap();
    assert_eq!(q.answer_text(), "blue, but darker");
    assert!(!question::locked(&n) && question::answered_unseen(&n));
    assert!(err(question::answer(&v, &qid, &[1], None, "user")).contains("already answered"));

    let (first, notice) = backlog::next_notice(&v, &app(), None, None, None).unwrap().unwrap();
    assert_eq!(first.id, a, "an answered item beats a p0 item");
    assert!(notice.contains("Which colour? -> blue, but darker") && notice.contains("not built yet, build it with the answer"), "{notice}");
    assert!(question::answered_unseen(&v.get(&a).unwrap()), "next without claim leaves the flag");

    let ctx = ops::context(&v, Some("app"), 15).unwrap();
    assert!(ctx.contains(&format!("answer arrived on {qid}: Which colour? -> blue, but darker")), "{ctx}");

    backlog::next_notice(&v, &app(), None, Some("agent-2"), Some("agent-2")).unwrap();
    let after = v.get(&a).unwrap();
    assert!(!question::answered_unseen(&after) && backlog::state(&after) == "in-progress");
    assert_eq!(backlog::next(&v, &app(), None, None).unwrap().unwrap().id, b);
    assert!(!ops::context(&v, Some("app"), 15).unwrap().contains("answer arrived"));
}

#[test]
fn a_built_item_gets_the_adjust_hint_and_an_explicit_ack_clears_the_flag() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "Palette", "task", Some("app"), &[("state", "review")]).id;
    let qid = ask(&v, &a, "Keep red?", false, false);
    question::answer(&v, &qid, &[1], None, "user").unwrap();

    let (n, notice) = backlog::next_notice(&v, &app(), None, None, None).unwrap().unwrap();
    assert_eq!(n.id, a, "answers on built items come back too");
    assert!(notice.contains("already built, adjust it to the answer"), "{notice}");
    assert!(ops::context(&v, Some("app"), 15).unwrap().contains("(built, adjust)"));

    assert!(question::ack(&v, &a, "agent-3").unwrap().is_some());
    assert!(question::ack(&v, &a, "agent-3").unwrap().is_none());
    assert!(backlog::next(&v, &app(), None, None).unwrap().is_none());
}

#[test]
fn asks_are_validated_and_withdrawing_unlocks() {
    let (_d, v) = common::vault();
    let a = queued(&v, "Palette");
    let bad = |options: Vec<Opt>, free| err(question::ask(&v, AskReq { id: a.clone(), text: "Q?".into(), options, multi: false, free, by: None }));
    assert!(bad(opts(&["one"]), false).contains("2 to 4"));
    assert!(bad(opts(&["a", "b", "c", "d", "e"]), false).contains("2 to 4"));
    assert!(bad(vec![], false).contains("options"));
    let (_, q) = question::ask(&v, AskReq { id: a.clone(), text: "Why?".into(), options: vec![], multi: false, free: true, by: None }).unwrap();
    assert!(q.free && q.options.is_empty());

    let n = question::withdraw(&v, &format!("{a}.1"), "agent").unwrap();
    assert!(!question::locked(&n));
    assert!(err(question::withdraw(&v, &a, "agent")).contains("no open question"));
    let note = common::add(&v, "A plain note", "note", Some("app"), &[]);
    assert!(err(question::ask(&v, AskReq { id: note.id, text: "Q?".into(), options: opts(&["a", "b"]), multi: false, free: false, by: None })).contains("not a backlog item"));
}

#[test]
fn the_digest_part_stays_short_with_many_questions() {
    let (_d, v) = common::vault();
    let base = ops::context(&v, Some("app"), 15).unwrap();
    for i in 0..8 {
        let id = queued(&v, &format!("Item number {i}"));
        let qid = ask(&v, &id, &format!("A rather long question number {i} that keeps going to test the cut of the digest line {}", "x".repeat(200)), false, false);
        if i % 2 == 0 {
            question::answer(&v, &qid, &[1], None, "user").unwrap();
        }
    }
    let ctx = ops::context(&v, Some("app"), 15).unwrap();
    assert_eq!(ctx.matches("answer arrived on").count(), 3, "{ctx}");
    assert_eq!(ctx.matches("waiting on user ").count(), 3, "{ctx}");
    assert_eq!(ctx.matches("more, see `breadcrumb questions`").count(), 2, "{ctx}");
    assert!(ctx.chars().count() <= breadcrumb::digest::DIGEST_BUDGET, "{} chars", ctx.chars().count());
    assert!(ctx.chars().count() - base.chars().count() < 1500, "{ctx}");
}

#[test]
fn an_old_vault_without_question_fields_loads_unchanged() {
    let (_d, v) = common::vault();
    let a = queued(&v, "Plain item");
    let before = std::fs::read_to_string(v.get(&a).unwrap().path.unwrap()).unwrap();
    let n = v.get(&a).unwrap();
    assert!(question::questions(&n).is_empty() && !question::locked(&n) && !question::answered_unseen(&n));
    assert_eq!(backlog::next(&v, &app(), None, None).unwrap().unwrap().id, a);
    let claimed = backlog::claim(&v, &a, "agent", false).unwrap();
    assert!(claimed.field(question::KEY).is_none(), "claiming adds no question fields");
    assert!(!before.contains("questions"));
    let cols = backlog::board(&v, &app(), &BoardFilter::default()).unwrap();
    assert!(cols.iter().find(|c| c.state == backlog::WAITING).unwrap().items.is_empty());
    assert_eq!(question::notice(&n), "");
}

fn call(srv: &Server, args: Value) -> String {
    let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "question", "arguments": args}});
    let mut out = Vec::new();
    srv.serve(Cursor::new(format!("{msg}\n")), &mut out).unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    v["result"]["content"][0]["text"].as_str().unwrap().to_string()
}

#[test]
fn the_question_tool_asks_lists_answers_and_acks() {
    let (d, v) = common::vault();
    let a = queued(&v, "Palette");
    let srv = Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let r = call(&srv, json!({"action": "ask", "id": a, "text": "Which?", "options": ["red: warm", {"label": "blue", "desc": "cool"}], "by": "s1"}));
    assert!(r.contains(&format!("asked {a}.1")), "{r}");
    let list = call(&srv, json!({}));
    assert!(list.contains("1. red: warm") && list.contains("2. blue: cool"), "{list}");
    let r = call(&srv, json!({"action": "answer", "id": format!("{a}.1"), "choices": [2]}));
    assert!(r.contains("answered") && r.contains("blue"), "{r}");
    assert!(call(&srv, json!({})).contains("answer arrived on"));
    assert!(call(&srv, json!({"action": "ack", "id": a})).contains("acknowledged"));
    assert_eq!(call(&srv, json!({})), "no open questions");
}
