mod common;

use breadcrumb::ops::{self, AddReq, Filter, Scope, UpdateReq};
use breadcrumb::slug::{self, Backfill};
use breadcrumb::{backlog, digest};

fn add(v: &breadcrumb::vault::Vault, title: &str, kind: &str) -> breadcrumb::note::Note {
    common::add(v, title, kind, Some("app"), &[])
}

#[test]
fn names_skip_filler_and_always_hold_a_hyphen() {
    let name = |t: &str, k: &str| slug::from_title(t, k, "k3m9x2qa");
    assert_eq!(name("Items get a short readable name that is unique", "feature"), "items-short-readable-name");
    assert_eq!(name("Handoff 2026-10-30 (session 998b32ba neocad-e3)", "handoff"), "handoff-session-neocad-e3");
    assert_eq!(name("SourceForge mirror grows by about 0.56 GB", "bug"), "sourceforge-mirror-grows-gb");
    assert_eq!(name("Material rework 5: remove the node graph", "task"), "material-rework-5-remove");
    assert_eq!(name("Upgrade to 3", "task"), "upgrade-3");
    assert_eq!(name("Stress", "bug"), "stress-bug");
    assert_eq!(name("Bug", "bug"), "bug-k3m9");
    assert_eq!(name("!!!", "note"), "note-k3m9");
    assert!(name("internationalization localization containerization orchestration", "note").len() <= 40);
}

#[test]
fn the_same_title_gets_different_names_and_a_retitle_keeps_the_name() {
    let (_d, v) = common::vault();
    let a = add(&v, "Stress auto mesh", "feature");
    let b = add(&v, "Stress auto mesh", "feature");
    let c = common::add(&v, "Stress auto mesh", "note", Some("other"), &[]);
    assert_eq!(a.slug().as_deref(), Some("stress-auto-mesh"));
    assert_eq!(b.slug().as_deref(), Some("stress-auto-mesh-2"));
    assert_eq!(c.slug().as_deref(), Some("stress-auto-mesh-3"), "unique across the vault");
    let a = ops::update(&v, &a.id, UpdateReq { title: Some("Something else entirely".into()), ..Default::default() }).unwrap();
    assert_eq!(a.slug().as_deref(), Some("stress-auto-mesh"));
    assert_eq!(a.title, "Something else entirely");
}

#[test]
fn a_given_name_wins_and_bad_ones_are_refused() {
    let (_d, v) = common::vault();
    let add_as = |title: &str, s: &str| ops::add(&v, AddReq { title: title.into(), project: Some("app".into()), slug: Some(s.into()), ..Default::default() });
    assert_eq!(add_as("Long title about meshing", "Thin-Walls").unwrap().slug().as_deref(), Some("thin-walls"));
    assert_eq!(add_as("Another", "thin-walls").unwrap().slug().as_deref(), Some("thin-walls-2"));
    for bad in ["oneword", "with space", "-leading", "trailing-", "a--b", "one-two-three-four-five", "12-34", "app-k3m9x2qa"] {
        let e = add_as("x y", bad).unwrap_err().to_string();
        assert!(e.contains("slug"), "{bad}: {e}");
    }
    let n = add(&v, "Field slug", "note");
    let r = ops::update(&v, &n.id, UpdateReq { slug: Some("thin-walls".into()), ..Default::default() });
    assert!(r.unwrap_err().to_string().contains("taken"));
    let n = ops::update(&v, &n.id, UpdateReq { slug: Some("field-renamed".into()), ..Default::default() }).unwrap();
    assert_eq!(v.resolve_id("field-renamed").unwrap(), n.id);
    let r = ops::update(&v, &n.id, UpdateReq { unset: vec!["slug".into()], ..Default::default() });
    assert!(r.is_err(), "a slug cannot be unset");
}

#[test]
fn every_command_takes_the_slug_and_the_old_id() {
    let (_d, v) = common::vault();
    let a = add(&v, "Stress auto mesh", "feature");
    let b = add(&v, "Fillet preview round", "bug");
    let (sa, sb) = ("stress-auto-mesh", "fillet-preview-round");

    assert_eq!(v.get(sa).unwrap().id, a.id);
    assert_eq!(v.get(&a.id).unwrap().id, a.id);
    assert_eq!(v.get(&a.id[..5]).unwrap().id, a.id, "id prefixes keep working");
    assert_eq!(v.get("Stress-Auto-Mesh").unwrap().id, a.id);
    assert!(v.get("stress-auto").unwrap_err().to_string().contains("no note with id or slug"), "a slug has no prefix form");

    let linked = ops::link(&v, sa, sb, Some("blocks"), false).unwrap();
    assert_eq!(linked.links[0].to, b.id, "links store the id");
    ops::link(&v, &a.id, &b.id, Some("blocks"), true).unwrap();
    ops::link(&v, &a.id, sb, Some("relates"), false).unwrap();

    backlog::move_item(&v, sa, backlog::MoveReq { state: "queued".into(), ..Default::default() }).unwrap();
    backlog::move_item(&v, &a.id, backlog::MoveReq { state: "in-progress".into(), ..Default::default() }).unwrap();
    assert_eq!(backlog::state(&v.get(sa).unwrap()), "in-progress");
    ops::append(&v, sa, "more").unwrap();
    backlog::claim(&v, sa, "me", false).unwrap();

    let child = common::add(&v, "Child part", "task", Some("app"), &[("parent", sa)]);
    assert_eq!(backlog::parent(&child).as_deref(), Some(a.id.as_str()), "ref fields store the id");

    for q in [sa, a.id.as_str(), "stress"] {
        let r = ops::search(&v, q, &Scope::All, &Filter::default()).unwrap();
        assert_eq!(r.hits[0].note.id, a.id, "search for {q}");
    }
}

#[test]
fn output_puts_the_slug_first_and_keeps_the_id() {
    let (_d, v) = common::vault();
    let a = add(&v, "Stress auto mesh", "feature");
    let pinned = ops::add(&v, AddReq { title: "Pinned rule here".into(), kind: Some("decision".into()), project: Some("app".into()), pinned: true, ..Default::default() }).unwrap();
    let want = format!("stress-auto-mesh {}", a.id);
    let scope = Scope::Project { slug: "app".into(), with_global: false };

    let d = digest::context(&v, Some("app"), 15).unwrap();
    assert!(d.contains(&format!("pinned-rule {} decision", pinned.id)), "{d}");
    backlog::move_item(&v, &a.id, backlog::MoveReq { state: "in-progress".into(), ..Default::default() }).unwrap();
    assert!(digest::context(&v, Some("app"), 15).unwrap().contains(&want));

    let cols = backlog::board(&v, &scope, &Default::default()).unwrap();
    assert!(backlog::board_text(&cols, "app").contains(&format!("stress-auto-mesh  APP-{}", a.id)));
    assert!(backlog::board_compact(&cols).contains(&want));

    let r = ops::search(&v, "stress", &scope, &Filter::default()).unwrap();
    assert!(breadcrumb::format::search_rows(&r, Some("app")).contains(&want));
    assert!(breadcrumb::format::search_text(&r, "app").contains(&want));

    let m = backlog::move_item(&v, "stress-auto-mesh", backlog::MoveReq { state: "review".into(), ..Default::default() }).unwrap();
    assert!(backlog::move_text(&m).starts_with(&format!("stress-auto-mesh APP-{} ", a.id)));
    assert!(breadcrumb::format::ack("moved", &m.note).starts_with(&format!("moved {want} ")));
}

#[test]
fn notes_without_a_slug_show_the_id_alone() {
    let (_d, v) = common::vault();
    let n = add(&v, "Old note", "feature");
    let mut old = n.clone();
    old.unset_field("slug");
    let p = v.write_note(&mut old).unwrap();
    common::git(&v.root, &["add", "-A"]);
    common::git(&v.root, &["commit", "-qm", "note from an old binary"]);
    let n = v.get(p.file_name().unwrap().to_string_lossy().split('-').next().unwrap()).unwrap();
    assert!(n.slug().is_none());
    assert_eq!(n.handle(), n.id);
    assert!(breadcrumb::format::note_line(&n).starts_with(&n.id));
}

#[test]
fn a_hand_written_slug_without_a_hyphen_is_ignored() {
    let (_d, v) = common::vault();
    let mut n = add(&v, "Some item", "feature");
    n.set_field("slug", serde_yaml::Value::String("abcd2345".into()));
    v.write_note(&mut n).unwrap();
    let got = v.get(&n.id).unwrap();
    assert!(got.slug().is_none());
    assert_eq!(got.handle(), got.id);
}

#[test]
fn backfill_is_dry_by_default_repeatable_and_leaves_updated_alone() {
    let (_d, v) = common::vault();
    let open = add(&v, "Open item name", "feature");
    let closed = add(&v, "Open item name", "feature");
    backlog::move_item(&v, &closed.id, backlog::MoveReq { state: "shipped".into(), ..Default::default() }).unwrap();
    let plain = common::add(&v, "Plain fact here", "note", Some("other"), &[]);
    for id in [&open.id, &closed.id, &plain.id] {
        let mut n = v.get(id).unwrap();
        n.unset_field("slug");
        v.write_note(&mut n).unwrap();
    }
    common::git(&v.root, &["add", "-A"]);
    common::git(&v.root, &["commit", "-qm", "notes from an old binary"]);
    let before = (breadcrumb::git::commit_count(&v.root), v.get(&open.id).unwrap().updated);

    let dry = slug::backfill(&v, &Backfill::default()).unwrap();
    assert_eq!(dry.len(), 3);
    assert_eq!(breadcrumb::git::commit_count(&v.root), before.0, "dry writes nothing");
    assert!(v.get(&open.id).unwrap().slug().is_none());
    assert!(slug::backfill_text(&dry, false).contains("nothing was written"));

    let only = slug::backfill(&v, &Backfill { open_only: true, ..Default::default() }).unwrap();
    assert_eq!(only.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), vec![open.id.as_str()]);
    let scoped = slug::backfill(&v, &Backfill { project: Some("other".into()), ..Default::default() }).unwrap();
    assert_eq!(scoped.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), vec![plain.id.as_str()]);

    let done = slug::backfill(&v, &Backfill { write: true, ..Default::default() }).unwrap();
    assert_eq!(done.len(), 3);
    assert_eq!(breadcrumb::git::commit_count(&v.root), before.0 + 1);
    let got = v.get(&open.id).unwrap();
    assert_eq!(got.slug().as_deref(), Some("open-item-name"), "open items get the bare name");
    assert_eq!(got.updated, before.1, "a new name is not news");
    assert_eq!(v.get(&closed.id).unwrap().slug().as_deref(), Some("open-item-name-2"));

    assert!(slug::backfill(&v, &Backfill { write: true, ..Default::default() }).unwrap().is_empty());
    assert_eq!(breadcrumb::git::commit_count(&v.root), before.0 + 1, "a second run changes nothing");
    assert!(common::git(&v.root, &["status", "--porcelain"]).is_empty());
}

#[test]
fn doctor_warns_about_a_slug_two_notes_share_and_stays_quiet_otherwise() {
    let (_d, v) = common::vault();
    let a = add(&v, "Twin name one", "note");
    let mut b = add(&v, "Other thing", "note");
    assert!(!breadcrumb::doctor::check(&v).iter().any(|p| p.message.contains("slug")));
    b.set_field("slug", serde_yaml::Value::String(a.slug().unwrap()));
    v.write_note(&mut b).unwrap();
    assert!(breadcrumb::doctor::check(&v).iter().any(|p| p.message.contains("used by several")));
    assert!(v.get(&a.slug().unwrap()).unwrap_err().to_string().contains("several"));
    assert_eq!(v.get(&a.id).unwrap().id, a.id, "ids still resolve");
}
