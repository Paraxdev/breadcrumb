mod common;

use breadcrumb::backlog::{self, BoardFilter, Index, MoveReq, QueuePos};
use breadcrumb::ops::{self, Scope};
use serde_yaml::Value;

fn mv(v: &breadcrumb::vault::Vault, id: &str, state: &str, by: Option<&str>) -> backlog::MoveResult {
    backlog::move_item(v, id, MoveReq { state: state.into(), by: by.map(Into::into), ..Default::default() }).unwrap()
}

fn fresh(v: &breadcrumb::vault::Vault, id: &str) -> breadcrumb::note::Note {
    v.get(id).unwrap()
}

#[test]
fn blocked_computation() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "A", "task", None, &[]);
    let b = common::add(&v, "B", "task", None, &[]);
    let c = common::add(&v, "C", "task", None, &[]);
    ops::link(&v, &a.id, &b.id, Some("depends-on"), false).unwrap();
    let blocked = |id: &str| {
        let all = v.load_notes();
        let idx = Index::new(&all);
        idx.is_blocked(all.iter().find(|n| n.id == id).unwrap())
    };
    assert!(blocked(&a.id));
    assert!(!blocked(&b.id));
    mv(&v, &b.id, "done", None);
    assert!(!blocked(&a.id), "a done dependency no longer blocks");
    ops::link(&v, &c.id, &a.id, Some("blocks"), false).unwrap();
    assert!(blocked(&a.id), "target of an open blocks link is blocked");
    backlog::move_item(&v, &c.id, MoveReq { state: "rejected".into(), resolution: Some("not needed".into()), ..Default::default() }).unwrap();
    assert!(!blocked(&a.id));
    ops::link(&v, &a.id, &c.id, Some("relates"), false).unwrap();
    assert!(!blocked(&a.id), "other rels never block");
}

#[test]
fn next_ordering_skips_blocked_and_claims() {
    let (_d, v) = common::vault();
    let low = common::add(&v, "low prio", "task", Some("app"), &[("priority", "p3")]);
    let first = common::add(&v, "p1 first", "feature", Some("app"), &[("priority", "p1")]);
    let second = common::add(&v, "p1 second", "bug", Some("app"), &[("priority", "p1")]);
    let urgent_blocked = common::add(&v, "p0 blocked", "task", Some("app"), &[("priority", "p0")]);
    let blocker = common::add(&v, "blocker", "task", Some("app"), &[("priority", "p3")]);
    ops::link(&v, &urgent_blocked.id, &blocker.id, Some("depends-on"), false).unwrap();
    for n in [&low, &second, &first, &urgent_blocked] {
        backlog::queue(&v, &n.id, QueuePos::End, None).unwrap();
    }
    backlog::queue(&v, &first.id, QueuePos::Top, None).unwrap();
    let scope = Scope::Project { slug: "app".into(), with_global: false };
    let pick = |claim: Option<&str>| backlog::next(&v, &scope, None, claim).unwrap().map(|n| n.id);
    assert_eq!(pick(None), Some(first.id.clone()), "highest priority, lowest rank, not blocked");
    assert_eq!(pick(Some("agent-1")), Some(first.id.clone()));
    let claimed = fresh(&v, &first.id);
    assert_eq!(backlog::state(&claimed), "in-progress");
    assert_eq!(backlog::assignee(&claimed).as_deref(), Some("agent-1"));
    assert!(claimed.body.contains("state queued -> in-progress (by agent-1)"));
    assert_eq!(pick(Some("agent-2")), Some(second.id.clone()));
    assert_eq!(pick(None), Some(low.id.clone()), "blocked p0 is skipped");
    mv(&v, &blocker.id, "done", None);
    assert_eq!(pick(None), Some(urgent_blocked.id.clone()));
    assert_eq!(backlog::next(&v, &Scope::Global, None, None).unwrap().map(|n| n.id), None);
}

#[test]
fn queue_positions() {
    let (_d, v) = common::vault();
    let ids: Vec<String> = (0..3).map(|i| common::add(&v, &format!("t{i}"), "task", None, &[]).id).collect();
    for id in &ids {
        backlog::queue(&v, id, QueuePos::End, None).unwrap();
    }
    let x = common::add(&v, "x", "task", None, &[]);
    backlog::queue(&v, &x.id, QueuePos::After(ids[0].clone()), None).unwrap();
    let rank = |id: &str| backlog::rank(&fresh(&v, id)).unwrap();
    assert!(rank(&ids[0]) < rank(&x.id) && rank(&x.id) < rank(&ids[1]));
    backlog::queue(&v, &ids[2], QueuePos::Top, None).unwrap();
    assert!(rank(&ids[2]) < rank(&ids[0]));
    let cols = backlog::board(&v, &Scope::Global, &BoardFilter::default()).unwrap();
    let queued: Vec<&str> = cols.iter().find(|c| c.state == "queued").unwrap().items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(queued, vec![ids[2].as_str(), ids[0].as_str(), x.id.as_str(), ids[1].as_str()]);
}

#[test]
fn move_records_history_and_acceptance() {
    let (_d, v) = common::vault();
    let f = common::add(&v, "Feature", "feature", Some("web"), &[("acceptance", "[works, '[x] documented', fast]")]);
    assert_eq!(backlog::state(&f), "backlog");
    mv(&v, &f.id, "in-progress", Some("claude"));
    ops::append(&v, &f.id, "progress note").unwrap();
    let r = backlog::move_item(&v, &f.id, MoveReq { state: "done".into(), shipped_in: Some("v1.2".into()), by: Some("claude".into()), ..Default::default() }).unwrap();
    assert_eq!(r.unmet, vec!["works", "fast"]);
    let n = fresh(&v, &f.id);
    assert_eq!(n.field("shipped_in"), Some(&Value::String("v1.2".into())));
    let body = &n.body;
    let hist = body.find("## History").unwrap();
    assert!(body.find("progress note").unwrap() < hist, "appended text stays above history: {body}");
    let lines: Vec<&str> = body[hist..].lines().filter(|l| l.starts_with("- ")).collect();
    assert_eq!(lines.len(), 2, "{body}");
    assert!(lines[0].ends_with("state backlog -> in-progress (by claude)"));
    assert!(lines[1].ends_with("state in-progress -> done (by claude)"));
    assert!(lines[0].len() > 20 && lines[0][2..].starts_with("20") && lines[0].contains("Z: state"));

    let e = backlog::move_item(&v, &f.id, MoveReq { state: "rejected".into(), ..Default::default() });
    assert!(e.unwrap_err().to_string().contains("resolution"));
    assert!(backlog::move_item(&v, &f.id, MoveReq { state: "bogus".into(), ..Default::default() }).is_err());

    ops::update(&v, &f.id, ops::UpdateReq { fields: vec![("state".into(), ops::FieldInput::Raw("review".into()))], by: Some("web".into()), ..Default::default() }).unwrap();
    assert!(fresh(&v, &f.id).body.ends_with("state done -> review (by web)"), "update also records state changes");
    let note = common::add(&v, "plain", "note", None, &[]);
    assert!(backlog::move_item(&v, &note.id, MoveReq { state: "done".into(), ..Default::default() }).is_err());
}

#[test]
fn deps_and_tree_are_cycle_safe() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "A", "task", None, &[]);
    let b = common::add(&v, "B", "task", None, &[]);
    let c = common::add(&v, "C", "task", None, &[]);
    ops::link(&v, &a.id, &b.id, Some("depends-on"), false).unwrap();
    ops::link(&v, &b.id, &c.id, Some("depends-on"), false).unwrap();
    ops::link(&v, &c.id, &a.id, Some("depends-on"), false).unwrap();
    let d = backlog::deps(&v, &a.id).unwrap();
    let by: Vec<(&str, usize)> = d.blocked_by.iter().map(|e| (e.id.as_str(), e.depth)).collect();
    assert_eq!(by, vec![(b.id.as_str(), 1), (c.id.as_str(), 2)]);
    assert_eq!(d.blocks.len(), 2);
    assert!(d.blocked);

    ops::update(&v, &a.id, ops::UpdateReq { fields: vec![("parent".into(), ops::FieldInput::Raw(b.id.clone()))], ..Default::default() }).unwrap();
    ops::update(&v, &b.id, ops::UpdateReq { fields: vec![("parent".into(), ops::FieldInput::Raw(a.id.clone()))], ..Default::default() }).unwrap();
    let t = backlog::tree(&v, &a.id).unwrap();
    assert!(t.contains("cycle"), "{t}");
    assert!(t.lines().count() < 10);
}

#[test]
fn tree_progress_promote_and_context() {
    let (_d, v) = common::vault();
    let epic = common::add(&v, "Epic", "epic", Some("app"), &[]);
    let f1 = common::add(&v, "F1", "feature", Some("app"), &[("parent", &epic.id)]);
    let f2 = common::add(&v, "F2", "feature", Some("app"), &[("parent", &epic.id)]);
    let t1 = common::add(&v, "T1", "task", Some("app"), &[("parent", &f1.id)]);
    mv(&v, &f2.id, "shipped", None);
    let t = backlog::tree(&v, &epic.id).unwrap();
    assert!(t.contains("APP-") && t.contains("    - ") && t.contains(&t1.id), "{t}");
    let cols = backlog::board(&v, &Scope::All, &BoardFilter { include_closed: true, ..Default::default() }).unwrap();
    let e = cols.iter().flat_map(|c| &c.items).find(|i| i.id == epic.id).unwrap();
    assert_eq!(e.progress, Some((1, 2)));
    assert!(e.line().contains("(1/2 done)"));
    assert!(e.key.starts_with("APP-"));

    let idea = backlog::capture_idea(&v, "Sync over LAN", None, Some("app".into()), vec![], false).unwrap();
    assert_eq!(backlog::state(&idea), "idea");
    ops::link(&v, &idea.id, &f1.id, Some("relates"), false).unwrap();
    let p = backlog::promote(&v, &idea.id, "feature", Some("me")).unwrap();
    assert_eq!(p.id, idea.id);
    assert_eq!(p.kind, "feature");
    assert_eq!(backlog::state(&p), "backlog");
    assert_eq!(p.links.len(), 1);
    assert!(p.body.contains("kind idea -> feature (by me)"));
    assert_eq!(p.path, fresh(&v, &idea.id).path);

    backlog::queue(&v, &t1.id, QueuePos::End, None).unwrap();
    mv(&v, &f1.id, "in-progress", None);
    let ctx = ops::context(&v, Some("app"), 10).unwrap();
    let b = &ctx[ctx.find("## Backlog").expect("backlog section")..];
    assert!(b.contains("Counts:") && b.contains("in-progress 1") && b.contains("queued 1"));
    assert!(b.contains("In progress:") && b.contains("F1"));
    assert!(b.contains("Top queued") && b.contains("T1"));
}
