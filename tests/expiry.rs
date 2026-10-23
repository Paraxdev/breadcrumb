mod common;

use breadcrumb::backlog::{self, MoveReq};
use breadcrumb::lifecycle;
use breadcrumb::ops::{self, AddReq, FieldInput, Scope, UpdateReq};
use breadcrumb::util::{fmt_date, now};
use serde_yaml::Value;
use std::collections::HashSet;

fn add(v: &breadcrumb::vault::Vault, title: &str, kind: &str, expires: &str, pinned: bool) -> breadcrumb::note::Note {
    ops::add(
        v,
        AddReq {
            title: title.into(),
            kind: Some(kind.into()),
            project: Some("app".into()),
            pinned,
            fields: vec![("expires".into(), FieldInput::Raw(expires.into()))],
            ..Default::default()
        },
    )
    .unwrap()
}

fn expires(n: &breadcrumb::note::Note) -> String {
    n.field("expires").and_then(|x| x.as_str()).unwrap_or_default().to_string()
}

#[test]
fn expires_accepts_dates_times_offsets_and_until() {
    let (_d, v) = common::vault();
    let task = common::add(&v, "Ship the parser", "task", Some("app"), &[]);
    assert_eq!(expires(&add(&v, "a", "note", "2026-10-24", false)), "2026-10-24");
    assert_eq!(expires(&add(&v, "b", "note", "2026-10-24T05:00:00Z", false)), "2026-10-24T05:00:00Z");
    let in3 = expires(&add(&v, "c", "note", "3d", false));
    let t = breadcrumb::util::parse_time(&in3).unwrap();
    assert!((t - now()).num_hours() >= 71 && (t - now()).num_hours() <= 72, "{in3}");
    assert_eq!(expires(&add(&v, "d", "note", &format!("until:{}", &task.id[..5]), false)), format!("until:{}", task.id));
    for bad in ["tomorrow", "until:zzzzzzzz", "-2d"] {
        let r = ops::add(&v, AddReq { title: "bad".into(), fields: vec![("expires".into(), FieldInput::Raw(bad.into()))], ..Default::default() });
        assert!(r.is_err(), "{bad}");
    }
    let n = add(&v, "e", "note", "1d", false);
    let n = ops::update(&v, &n.id, UpdateReq { unset: vec!["expires".into()], ..Default::default() }).unwrap();
    assert!(n.field("expires").is_none());
    assert!(ops::define_kind(&v, breadcrumb::kind::KindDef { fields: vec![breadcrumb::kind::FieldDef::new("expires", breadcrumb::kind::FieldType::Date)], ..breadcrumb::kind::KindDef::new("permit", "x") }, false, &[]).is_err());
}

#[test]
fn a_date_holds_through_that_day_and_expired_notes_leave_digest_and_search_ranking() {
    let (_d, v) = common::vault();
    let today = fmt_date(&now());
    let yesterday = fmt_date(&(now() - chrono::Duration::days(1)));
    let live = add(&v, "Tonight lean mode for the build", "decision", &today, true);
    let gone = add(&v, "Last night lean mode for the build", "decision", &yesterday, true);
    let past = add(&v, "Lean mode until this morning", "note", "2020-01-01T08:00:00Z", false);
    let ctx = ops::context(&v, Some("app"), 15).unwrap();
    assert!(ctx.contains(&live.id) && !ctx.contains(&gone.id) && !ctx.contains(&past.id), "{ctx}");

    let r = ops::search(&v, "lean mode", &Scope::All, &ops::Filter::default()).unwrap();
    let ids: Vec<&str> = r.hits.iter().map(|h| h.note.id.as_str()).collect();
    assert_eq!(ids[0], live.id, "{ids:?}");
    assert!(r.hits[1..].iter().all(|h| h.lapsed == Some("expired")), "expired ones rank last");
    assert!(breadcrumb::format::search_rows(&r, Some("app")).contains(&format!("{} decision Last night lean mode for the build [pinned] [expired]", gone.id)));
}

#[test]
fn closing_the_item_expires_notes_waiting_for_it_in_the_same_commit() {
    let (_d, v) = common::vault();
    let task = common::add(&v, "Monkey test batch", "task", Some("app"), &[]);
    let permit = add(&v, "Promotion allowed during the monkey test batch", "decision", &format!("until:{}", task.id), true);
    let note = add(&v, "Full machine resources during the batch", "note", &format!("until:{}", task.id), false);
    assert!(ops::context(&v, Some("app"), 15).unwrap().contains(&permit.id));
    backlog::move_item(&v, &task.id, MoveReq { state: "in-progress".into(), ..Default::default() }).unwrap();
    assert!(v.get(&permit.id).unwrap().pinned, "only closing expires it");
    let commits = breadcrumb::git::commit_count(&v.root);
    backlog::move_item(&v, &task.id, MoveReq { state: "done".into(), ..Default::default() }).unwrap();
    assert_eq!(breadcrumb::git::commit_count(&v.root), commits + 1);
    let p = v.get(&permit.id).unwrap();
    assert!(!p.pinned && p.field("decision_status") == Some(&Value::String("superseded".into())), "{}", p.to_markdown());
    assert_eq!(p.updated, permit.updated, "an automatic mark is not an edit");
    let ctx = ops::context(&v, Some("app"), 15).unwrap();
    assert!(!ctx.contains(&permit.id) && !ctx.contains(&note.id), "{ctx}");
}

#[test]
fn doctor_lists_expired_notes_and_fix_marks_them() {
    let (_d, v) = common::vault();
    let pinned = add(&v, "Night permission", "note", &breadcrumb::util::fmt_time(&(now() - chrono::Duration::days(2))), true);
    let decided = add(&v, "Batch rule", "decision", "2026-01-01", false);
    let lapsed_long_ago = add(&v, "Old note", "note", "2020-01-01", false);
    let n = common::add(&v, "Hand written", "note", Some("app"), &[]);
    let mut raw = v.get(&n.id).unwrap();
    raw.set_field("expires", Value::String("next week".into()));
    v.write_note(&mut raw).unwrap();

    let msgs = |id: &str| breadcrumb::doctor::check(&v).into_iter().filter(|p| p.id.as_deref() == Some(id)).map(|p| format!("{} {}", p.severity, p.message)).collect::<Vec<_>>();
    assert!(msgs(&pinned.id).iter().any(|m| m.starts_with("warning expired") && m.contains("doctor --fix --yes")), "{:?}", msgs(&pinned.id));
    assert!(msgs(&decided.id).iter().any(|m| m.starts_with("warning")), "an accepted decision still reads as current");
    assert!(msgs(&lapsed_long_ago.id).iter().all(|m| !m.contains("expired")), "expired long ago and marked, doctor stays quiet");
    assert!(msgs(&n.id).iter().any(|m| m.contains("'next week'") && m.contains("never expires")));

    let plan = lifecycle::sweep_plan(&v);
    let ids: HashSet<&str> = plan.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, HashSet::from([pinned.id.as_str(), decided.id.as_str()]));
    assert!(lifecycle::sweep_text(&plan, false).contains("dry run: 2 expired notes"));
    let commits = breadcrumb::git::commit_count(&v.root);
    assert_eq!(lifecycle::sweep(&v).unwrap().len(), 2);
    assert_eq!(breadcrumb::git::commit_count(&v.root), commits + 1);
    assert!(lifecycle::sweep_plan(&v).is_empty());
    assert!(!v.get(&pinned.id).unwrap().pinned);
    assert!(msgs(&pinned.id).iter().any(|m| m.starts_with("info expired")), "{:?}", msgs(&pinned.id));
}

#[test]
fn file_notes_skip_expired_notes() {
    let (d, v) = common::vault();
    let proj = d.path().join("app");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    ops::project_link(&v, "app", &proj, None).unwrap();
    let req = |title: &str, expires: &str| {
        ops::add(&v, AddReq { title: title.into(), kind: Some("gotcha".into()), project: Some("app".into()), paths: vec!["src/**".into()], fields: vec![("expires".into(), FieldInput::Raw(expires.into()))], ..Default::default() }).unwrap()
    };
    let live = req("Live parser gotcha", "2d");
    let old = req("Old parser gotcha", "2020-01-01");
    let (line, _) = breadcrumb::recall::file_notes(&v, &proj, &proj.join("src").join("lib.rs"), &HashSet::new()).unwrap();
    assert!(line.contains(&live.id) && !line.contains(&old.id), "{line}");
}
