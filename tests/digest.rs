mod common;

use breadcrumb::digest::{self, ContextOpts};
use breadcrumb::note::Link;
use breadcrumb::ops::{self, AddReq, FieldInput, UpdateReq};
use serde_yaml::Value;
use std::time::Duration;

fn pin(v: &breadcrumb::vault::Vault, id: &str) {
    ops::update(v, id, UpdateReq { pinned: Some(true), ..Default::default() }).unwrap();
}

fn tick() {
    std::thread::sleep(Duration::from_millis(1100));
}

#[test]
fn summaries_come_from_the_field_or_the_titles_first_sentence() {
    let mut n = breadcrumb::note::Note::new("aaaa0001", "Use tabs. Spaces broke the parser, e.g. in tests.", "decision");
    assert_eq!(digest::summary(&n), "Use tabs.");
    n.title = "Prefer e.g. short names. Always".into();
    assert_eq!(digest::summary(&n), "Prefer e.g. short names.");
    n.title = "word ".repeat(40);
    assert_eq!(digest::summary(&n).chars().count(), digest::SUMMARY_MAX);
    n.set_field(digest::SUMMARY_KEY, Value::String("Own  summary\nline".into()));
    assert_eq!(digest::summary(&n), "Own summary line");
}

#[test]
fn summary_is_an_option_of_add_and_update() {
    let (d, v) = common::vault();
    let srv = breadcrumb::mcp::Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let mut a = serde_json::Map::new();
    a.insert("title".into(), "Release checklist".into());
    a.insert("project".into(), "global".into());
    a.insert("pinned".into(), true.into());
    a.insert("summary".into(), "Tag, build,\n then publish".into());
    srv.call_tool("add", &a).unwrap();
    let n = v.load_notes().into_iter().find(|n| n.title == "Release checklist").unwrap();
    assert_eq!(digest::summary(&n), "Tag, build, then publish");
    let mut u = serde_json::Map::new();
    u.insert("id".into(), n.id.clone().into());
    u.insert("summary".into(), "Tag and publish".into());
    srv.call_tool("update", &u).unwrap();
    assert!(ops::context(&v, None, 15).unwrap().contains(&format!("{} note Tag and publish\n", n.id)));
}

#[test]
fn pins_are_one_line_each_rules_first_and_lapsed_notes_never_show() {
    let (_d, v) = common::vault();
    let pinned = |title: &str, kind: &str, fields: &[(&str, &str)]| {
        ops::add(
            &v,
            AddReq {
                title: title.into(),
                kind: Some(kind.into()),
                project: Some("app".into()),
                pinned: true,
                body: Some("Body text that stays out of the digest.".into()),
                fields: fields.iter().map(|(k, x)| (k.to_string(), FieldInput::Raw(x.to_string()))).collect(),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let old_rule = pinned("Old rule", "decision", &[("summary", "Short own summary")]);
    tick();
    let gotcha = pinned("Pinned gotcha", "gotcha", &[]);
    tick();
    let new_rule = pinned("New rule", "feedback", &[]);
    let replaced = pinned("Replaced decision", "decision", &[("decision_status", "superseded")]);
    let linked = pinned("Superseded by a link", "note", &[]);
    let tagged = pinned("Tagged superseded", "note", &[]);
    let archived = pinned("Archived pin", "note", &[]);
    let newer = common::add(&v, "Newer fact", "note", Some("app"), &[]);
    ops::link(&v, &newer.id, &linked.id, Some("supersedes"), false).unwrap();
    ops::update(&v, &tagged.id, UpdateReq { tags: vec!["superseded".into()], ..Default::default() }).unwrap();
    ops::set_status(&v, &archived.id, "archived").unwrap();

    let ctx = ops::context(&v, Some("app"), 15).unwrap();
    let pins: Vec<&str> = ctx.lines().skip_while(|l| !l.starts_with("Pinned")).skip(1).take(3).collect();
    assert_eq!(
        pins,
        vec![
            format!("{} feedback New rule", new_rule.id).as_str(),
            format!("{} decision Short own summary", old_rule.id).as_str(),
            format!("{} gotcha Pinned gotcha", gotcha.id).as_str()
        ],
        "{ctx}"
    );
    assert!(!ctx.contains("Body text"), "{ctx}");
    for gone in [&replaced, &linked, &tagged, &archived] {
        assert!(!ctx.contains(&gone.id), "{} must not appear: {ctx}", gone.title);
    }
    assert!(ctx.contains(&newer.id));
}

#[test]
fn a_new_handoff_supersedes_the_live_one_and_unpins_older_ones() {
    let (_d, v) = common::vault();
    let first = common::add(&v, "Handoff one", "handoff", Some("app"), &[]);
    pin(&v, &first.id);
    tick();
    let second = common::add(&v, "Handoff two", "handoff", Some("app"), &[]);
    assert_eq!(second.links, vec![Link { to: first.id.clone(), rel: "supersedes".into() }]);
    assert!(!v.get(&first.id).unwrap().pinned, "the superseded handoff is unpinned");
    pin(&v, &second.id);
    tick();
    // Touching an old handoff later must not make it the live one again.
    ops::append(&v, &first.id, "late remark").unwrap();
    let other = common::add(&v, "Other project handoff", "handoff", Some("web"), &[]);
    assert!(other.links.is_empty(), "handoffs of another project are not linked");
    let commits = breadcrumb::git::commit_count(&v.root);
    let third = ops::add(&v, AddReq { title: "Handoff three".into(), kind: Some("handoff".into()), project: Some("app".into()), body: Some("## Done\n\nParser.\n\n## Next\n\nLexer.".into()), ..Default::default() }).unwrap();
    assert_eq!(breadcrumb::git::commit_count(&v.root), commits + 1, "the unpin lands in the same commit");
    assert_eq!(third.links, vec![Link { to: second.id.clone(), rel: "supersedes".into() }]);
    assert!(!v.get(&second.id).unwrap().pinned);

    let ctx = ops::context(&v, Some("app"), 15).unwrap();
    assert!(ctx.contains(&format!("Latest handoff {} (", third.id)) && ctx.contains(&format!("previous {}): Handoff three\nDone:\nParser.\nNext:\nLexer.\n", second.id)), "{ctx}");
    assert!(!ctx.contains(&first.id) && !ctx.contains(&other.id) && !ctx.contains('#'), "{ctx}");
}

#[test]
fn the_digest_fits_its_budget_with_every_pin_and_a_cut_handoff() {
    let (_d, v) = common::vault();
    for i in 0..20 {
        let n = common::add(&v, &format!("Standing rule number {i} with a title long enough to matter for the budget"), "decision", Some("app"), &[]);
        pin(&v, &n.id);
    }
    for i in 0..30 {
        common::add(&v, &format!("Fact {i}"), "note", Some("app"), &[]);
    }
    let body: String = (0..200).map(|i| format!("- step {i} of a long handoff that goes on\n")).collect();
    let h = ops::add(&v, AddReq { title: "Long handoff".into(), kind: Some("handoff".into()), project: Some("app".into()), body: Some(body), ..Default::default() }).unwrap();
    let ctx = ops::context(&v, Some("app"), 15).unwrap();
    assert!(ctx.chars().count() <= digest::DIGEST_BUDGET, "{} chars", ctx.chars().count());
    assert_eq!(ctx.matches(" decision Standing rule number").count(), 20, "{ctx}");
    assert!(ctx.contains(&format!("... (get {} for more)\n", h.id)) && ctx.contains("- step 0 of"), "{ctx}");
    assert!(ctx.contains("Recent (") && ctx.contains("Fact 29"), "recent notes still get room: {ctx}");

    let small = ops::context_with(&v, Some("app"), ContextOpts { budget: 2000, ..Default::default() }).unwrap();
    assert!(small.contains("more pinned: ") && small.contains(&h.id), "{small}");
}
