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
    let req = AskReq { id: Some(id.to_string()), text: text.into(), options: opts(&["red", "blue", "green"]), multi, free, by: Some("agent-1".into()), soft: false, project: None };
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

    assert!(err(question::answer(&v, &qid, &[], None, "user", false)).contains("pick an option"));
    assert!(err(question::answer(&v, &qid, &[1, 2], None, "user", false)).contains("single"));
    assert!(err(question::answer(&v, &qid, &[4], None, "user", false)).contains("3 options"));
    let (n, q) = question::answer(&v, &qid, &[2], Some("but darker"), "user", false).unwrap();
    assert_eq!(q.answer_text(), "blue, but darker");
    assert!(!question::locked(&n) && question::answered_unseen(&n));
    assert!(err(question::answer(&v, &qid, &[1], None, "user", false)).contains("already answered"));

    let (first, notice) = backlog::next_notice(&v, &app(), None, None, None).unwrap().unwrap();
    assert_eq!(first.id, a, "an answered item beats a p0 item");
    assert!(notice.contains("Which colour? -> blue, but darker") && notice.contains("not built yet, build it with the answer"), "{notice}");
    assert!(question::answered_unseen(&v.get(&a).unwrap()), "next without claim leaves the flag");

    let ctx = ops::context(&v, Some("app"), 15).unwrap();
    assert!(ctx.contains(&format!("answer arrived on {qid} [queued, 0m ago]: Which colour? -> blue, but darker")), "{ctx}");

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
    question::answer(&v, &qid, &[1], None, "user", false).unwrap();

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
    let bad = |options: Vec<Opt>, free| err(question::ask(&v, AskReq { id: Some(a.clone()), text: "Q?".into(), options, multi: false, free, by: None, soft: false, project: None }));
    assert!(bad(opts(&["one"]), false).contains("2 to 6"));
    assert!(bad(opts(&["a", "b", "c", "d", "e", "f", "g"]), false).contains("2 to 6"));
    assert!(question::ask(&v, AskReq { id: Some(queued(&v, "Six")), text: "Q?".into(), options: opts(&["a", "b", "c", "d", "e", "f"]), multi: false, free: false, by: None, soft: false, project: None }).is_ok());
    assert!(bad(vec![], false).contains("options"));
    let (_, q) = question::ask(&v, AskReq { id: Some(a.clone()), text: "Why?".into(), options: vec![], multi: false, free: true, by: None, soft: false, project: None }).unwrap();
    assert!(q.free && q.options.is_empty());

    let n = question::withdraw(&v, &format!("{a}.1"), "agent").unwrap();
    assert!(!question::locked(&n));
    assert!(err(question::withdraw(&v, &a, "agent")).contains("no open question"));
    let note = common::add(&v, "A plain note", "note", Some("app"), &[]);
    assert!(err(question::ask(&v, AskReq { id: Some(note.id), text: "Q?".into(), options: opts(&["a", "b"]), multi: false, free: false, by: None, soft: false, project: None })).contains("not a backlog item"));
}

#[test]
fn the_digest_part_stays_short_with_many_questions() {
    let (_d, v) = common::vault();
    let base = ops::context(&v, Some("app"), 15).unwrap();
    for i in 0..8 {
        let id = queued(&v, &format!("Item number {i}"));
        let qid = ask(&v, &id, &format!("A rather long question number {i} that keeps going to test the cut of the digest line {}", "x".repeat(200)), false, false);
        if i % 2 == 0 {
            question::answer(&v, &qid, &[1], None, "user", false).unwrap();
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

fn req(id: Option<&str>, text: &str, soft: bool, free: bool) -> AskReq {
    AskReq { id: id.map(str::to_string), text: text.into(), options: opts(&["red", "blue", "green"]), multi: false, free, by: None, soft, project: Some("app".into()) }
}

#[test]
fn a_soft_question_leaves_the_item_open_for_work() {
    let (_d, v) = common::vault();
    let a = queued(&v, "Pick the palette");
    let (n, q) = question::ask(&v, req(Some(&a), "Which colour?", true, false)).unwrap();
    assert!(!question::locked(&n) && q.open());
    assert!(question::asked_line(&n, &q).contains("stays open for work"));
    assert!(question::render(&n, &q).contains("[app]") && question::render(&n, &q).contains("(soft"));
    assert_eq!(backlog::next(&v, &app(), None, None).unwrap().unwrap().id, a, "next still offers it");
    backlog::claim(&v, &a, "agent-2", false).unwrap();
    let cols = backlog::board(&v, &app(), &BoardFilter::default()).unwrap();
    assert!(cols.iter().find(|c| c.state == backlog::WAITING).unwrap().items.is_empty());
    question::answer(&v, &question::qid(&n, &q), &[1], None, "user", false).unwrap();
    assert!(question::answered_unseen(&v.get(&a).unwrap()));
}

#[test]
fn a_question_without_an_item_files_an_idea() {
    let (_d, v) = common::vault();
    let (n, q) = question::ask(&v, req(None, "Should the board group by project?", false, false)).unwrap();
    assert_eq!(n.kind, "idea");
    assert_eq!(n.project.as_deref(), Some("app"));
    assert!(n.title.starts_with("Question: Should the board"), "{}", n.title);
    assert_eq!(question::qid(&n, &q), format!("{}.1", n.id));
    assert!(question::locked(&n));
}

#[test]
fn answers_take_numbers_as_text_and_can_be_replaced() {
    let (_d, v) = common::vault();
    let a = queued(&v, "Palette");
    let qid = ask(&v, &a, "Which colour?", false, false);
    let (_, q) = question::answer(&v, &qid, &[], Some("2"), "user", false).unwrap();
    assert_eq!(q.answer_text(), "blue");
    let e = err(question::answer(&v, &qid, &[3], None, "user", false));
    assert!(e.contains("already answered") && e.contains("--replace"), "{e}");
    let (n, q) = question::answer(&v, &qid, &[3], None, "user", true).unwrap();
    assert_eq!(q.answer_text(), "green");
    assert!(n.body.contains("question 1 answer replaced"), "{}", n.body);
    let b = queued(&v, "Shade");
    let qid = ask(&v, &b, "Which shade?", false, false);
    assert!(err(question::answer(&v, &qid, &[], Some("dark please"), "user", false)).contains("takes no free text"));
    let qid = ask(&v, &b, "Which tone?", false, true);
    let (_, q) = question::answer(&v, &qid, &[], Some("7 parts"), "user", false).unwrap();
    assert_eq!(q.answer_text(), "7 parts");
}

#[test]
fn ack_takes_a_question_id_and_the_list_points_elsewhere() {
    let (_d, v) = common::vault();
    let a = queued(&v, "Palette");
    let qid = ask(&v, &a, "Which colour?", false, false);
    question::answer(&v, &qid, &[1], None, "user", false).unwrap();
    assert!(question::ack(&v, &qid, "agent").unwrap().is_some());
    let other = common::add(&v, "Elsewhere", "task", Some("web"), &[("state", "queued")]).id;
    ask(&v, &other, "Which font?", false, false);
    let all = v.load_notes();
    let app_notes: Vec<_> = all.iter().filter(|n| n.project.as_deref() == Some("app")).cloned().collect();
    assert_eq!(question::elsewhere_line(&all, &app_notes), "1 more open question in other projects, `breadcrumb questions -p all` lists them\n");
    assert_eq!(question::elsewhere_line(&all, &all), "");
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
