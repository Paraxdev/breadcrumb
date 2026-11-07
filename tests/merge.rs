mod common;

use breadcrumb::mcp::Server;
use breadcrumb::merge;
use breadcrumb::note::{Link, Note};
use breadcrumb::ops::{self, AddReq, FieldInput, Filter, Scope, StatusFilter, UpdateReq};
use breadcrumb::vault::Vault;
use serde_json::{Value, json};
use std::process::Command;

fn note(v: &Vault, title: &str, kind: &str, body: &str, tags: &[&str], paths: &[&str]) -> Note {
    ops::add(
        v,
        AddReq {
            title: title.into(),
            kind: Some(kind.into()),
            project: Some("app".into()),
            body: Some(body.into()),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            paths: paths.iter().map(|p| p.to_string()).collect(),
            ..Default::default()
        },
    )
    .unwrap()
}

fn link(v: &Vault, from: &Note, to: &Note, rel: &str) {
    ops::link(v, &from.id, &to.id, Some(rel), false).unwrap();
}

fn file(v: &Vault, id: &str) -> String {
    std::fs::read_to_string(v.get(id).unwrap().path.unwrap()).unwrap()
}

fn crumb(v: &Vault, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_crumb")).args(args).env("BREADCRUMB_HOME", &v.root).env("CLAUDE_CODE_SESSION_ID", "").env("BREADCRUMB_SESSION", "").output().unwrap()
}

fn crumb_ok(v: &Vault, args: &[&str]) -> String {
    let out = crumb(v, args);
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn hits(v: &Vault, q: &str) -> Vec<String> {
    ops::search(v, q, &Scope::All, &Filter::default()).unwrap().hits.into_iter().map(|h| h.note.id).collect()
}

fn listed(v: &Vault) -> Vec<String> {
    ops::list(v, &Scope::All, &Filter { status: StatusFilter::Active, ..Default::default() }).unwrap().into_iter().map(|n| n.id).collect()
}

#[test]
fn merge_moves_body_tags_paths_and_links_and_hides_the_duplicate() {
    let (_d, v) = common::vault();
    let keep = note(&v, "Build script rebuilds on every edit", "gotcha", "Set the rerun-if-changed lines.", &["build"], &["build.rs"]);
    let dup = note(&v, "build.rs reruns each time anything changes", "gotcha", "Cargo reruns build.rs unless rerun-if-changed is set.", &["build", "cargo"], &["build.rs", "Cargo.toml"]);
    let other = note(&v, "Cargo features", "note", "x", &[], &[]);
    let pointing = note(&v, "Release notes", "note", "y", &[], &[]);
    let also = note(&v, "Packaging", "note", "z", &[], &[]);
    link(&v, &dup, &other, "relates");
    link(&v, &dup, &keep, "relates");
    link(&v, &pointing, &dup, "depends-on");
    link(&v, &also, &dup, "relates");
    link(&v, &also, &keep, "relates");
    link(&v, &keep, &dup, "relates");
    let before = common::git(&v.root, &["rev-list", "--count", "HEAD"]);

    let m = merge::merge(&v, &dup.id, &keep.id).unwrap();
    assert_eq!(common::git(&v.root, &["rev-list", "--count", "HEAD"]).trim().parse::<u32>().unwrap(), before.trim().parse::<u32>().unwrap() + 1, "one commit per merge");
    let subject = common::git(&v.root, &["log", "-1", "--format=%s"]);
    assert_eq!(subject.trim(), format!("breadcrumb: merge {} into {} build.rs reruns each time anything changes", dup.id, keep.id));

    let keep = v.get(&keep.id).unwrap();
    assert!(keep.body.starts_with("Set the rerun-if-changed lines.\n\n"), "{}", keep.body);
    assert!(keep.body.contains(&format!("Merged from {}", dup.id)) && keep.body.contains("Cargo reruns build.rs unless rerun-if-changed is set."), "{}", keep.body);
    assert_eq!(keep.tags, vec!["build", "cargo"]);
    assert_eq!(keep.path_globs(), vec!["build.rs", "Cargo.toml"]);
    assert!(keep.links.contains(&Link { to: other.id.clone(), rel: "relates".into() }));
    assert!(!keep.links.iter().any(|l| l.to == keep.id || l.to == dup.id), "no self link, none to the duplicate: {:?}", keep.links);
    let pointing = v.get(&pointing.id).unwrap();
    assert_eq!(pointing.links, vec![Link { to: keep.id.clone(), rel: "depends-on".into() }], "retargeted");
    let also = v.get(&also.id).unwrap();
    assert_eq!(also.links.iter().filter(|l| l.to == keep.id).count(), 1, "no duplicate link: {:?}", also.links);
    assert!(!also.links.iter().any(|l| l.to == dup.id));

    let dup = v.get(&dup.id).unwrap();
    assert!(dup.is_archived());
    assert_eq!(merge::merged_into(&dup).as_deref(), Some(keep.id.as_str()));
    assert!(m.dup.body.contains("Cargo reruns"), "the duplicate keeps its own text");

    assert!(!hits(&v, "rerun").contains(&dup.id) && hits(&v, "rerun").contains(&keep.id));
    assert!(!listed(&v).contains(&dup.id) && listed(&v).contains(&keep.id));
    assert!(!ops::context(&v, Some("app"), 15).unwrap().contains("reruns each time"));
    let indexed: Vec<String> = breadcrumb::recall::path_index(&v).into_iter().filter(|e| e.holds()).map(|e| e.id).collect();
    assert!(indexed.contains(&keep.id) && !indexed.contains(&dup.id), "{indexed:?}");
}

#[test]
fn a_merged_note_stays_out_even_when_unarchived_and_search_all_labels_it() {
    let (_d, v) = common::vault();
    let keep = note(&v, "Keep this fact about parsing", "note", "k", &[], &[]);
    let dup = note(&v, "Parsing fact written twice", "note", "d", &[], &[]);
    merge::merge(&v, &dup.id, &keep.id).unwrap();
    let mut n = v.get(&dup.id).unwrap();
    n.status = "active".into();
    v.write_note(&mut n).unwrap();
    assert!(!hits(&v, "parsing").contains(&dup.id));
    assert!(!ops::context(&v, Some("app"), 15).unwrap().contains("written twice"));
    let r = ops::search(&v, "parsing", &Scope::All, &Filter { status: StatusFilter::All, ..Default::default() }).unwrap();
    let h = r.hits.iter().find(|h| h.note.id == dup.id).expect("shown with --status all");
    assert_eq!(h.lapsed, Some("merged"));
}

#[test]
fn get_answers_for_the_duplicate_and_points_to_the_kept_note() {
    let (_d, v) = common::vault();
    let keep = note(&v, "Kept fact", "note", "k", &[], &[]);
    let dup = note(&v, "Duplicate fact", "note", "d", &[], &[]);
    merge::merge(&v, &dup.id, &keep.id).unwrap();
    let text = crumb_ok(&v, &["get", &dup.id]);
    assert!(text.starts_with(&format!("merged into {}", v.get(&keep.id).unwrap().handle())), "{text}");
    assert!(text.contains("Duplicate fact") && text.contains(&format!("merge --undo {}", dup.id)), "{text}");
    let json: Value = serde_json::from_str(&crumb_ok(&v, &["--json", "get", &dup.id])).unwrap();
    assert_eq!(json["merged_into"], keep.id);
    let kept = crumb_ok(&v, &["get", &keep.id]);
    assert!(!kept.starts_with("merged into") && !kept.contains("linked from"), "{kept}");
    let slug = v.get(&dup.id).unwrap().slug().unwrap();
    assert!(crumb_ok(&v, &["get", &slug]).starts_with("merged into"), "a slug still resolves");
    let body = crumb(&v, &["get", "--body", &dup.id]);
    assert_eq!(String::from_utf8_lossy(&body.stdout).trim(), "d", "piped body stays clean");
    assert!(String::from_utf8_lossy(&body.stderr).contains("merged into"));
}

#[test]
fn undo_restores_both_notes_byte_for_byte_after_later_commits() {
    let (_d, v) = common::vault();
    let keep = note(&v, "Kept fact about caches", "decision", "k body", &["cache"], &["src/cache/**"]);
    let dup = note(&v, "Duplicate fact about caches", "gotcha", "d body", &["cache", "perf"], &["src/perf/**"]);
    let pointing = note(&v, "Links to dup", "note", "p", &[], &[]);
    let also = note(&v, "Links to both", "note", "a", &[], &[]);
    let other = note(&v, "Target", "note", "t", &[], &[]);
    link(&v, &pointing, &dup, "relates");
    link(&v, &also, &dup, "relates");
    link(&v, &also, &keep, "relates");
    link(&v, &keep, &dup, "blocks");
    link(&v, &dup, &other, "relates");
    ops::update(&v, &dup.id, UpdateReq { pinned: Some(true), ..Default::default() }).unwrap();
    let snapshot: Vec<(String, String)> = [&keep, &dup, &pointing, &also, &other].iter().map(|n| (n.id.clone(), file(&v, &n.id))).collect();

    merge::merge(&v, &dup.id, &keep.id).unwrap();
    assert!(v.get(&keep.id).unwrap().pinned, "a pin is not lost");
    ops::append(&v, &other.id, "later work on an unrelated note").unwrap();
    let u = merge::unmerge(&v, &dup.id).unwrap();
    assert!(u.left.is_empty(), "{:?}", u.left);
    for (id, was) in &snapshot {
        if *id == other.id {
            continue;
        }
        assert_eq!(&file(&v, id), was, "{id} is back as it was");
    }
    assert!(file(&v, &other.id).contains("later work"), "later edits stay");
    let subject = common::git(&v.root, &["log", "-1", "--format=%s"]);
    assert!(subject.contains("unmerge") && subject.contains(&dup.id), "{subject}");
    assert!(hits(&v, "duplicate fact").contains(&dup.id));
}

#[test]
fn undo_keeps_what_was_edited_since_and_says_so() {
    let (_d, v) = common::vault();
    let keep = note(&v, "Kept fact", "note", "k body", &[], &[]);
    let dup = note(&v, "Duplicate fact", "note", "d body", &[], &["a/**"]);
    merge::merge(&v, &dup.id, &keep.id).unwrap();
    let body = v.get(&keep.id).unwrap().body.replace("d body", "d body, reworded by hand");
    ops::update(&v, &keep.id, UpdateReq { body: Some(body.clone()), ..Default::default() }).unwrap();
    let u = merge::unmerge(&v, &dup.id).unwrap();
    assert_eq!(u.left.len(), 1, "{:?}", u.left);
    let k = v.get(&keep.id).unwrap();
    assert_eq!(k.body, body.trim_end(), "the edited text is never cut");
    assert!(k.path_globs().is_empty(), "what is recorded still comes off");
    assert!(!v.get(&dup.id).unwrap().is_archived());
}

#[test]
fn merge_refuses_what_would_lose_or_hide_data() {
    let (_d, v) = common::vault();
    let a = note(&v, "Alpha note", "note", "a", &[], &[]);
    let b = note(&v, "Beta note", "note", "b", &[], &[]);
    let c = note(&v, "Gamma note", "note", "c", &[], &[]);
    let err = |r: anyhow::Result<merge::Merged>| r.err().expect("refused").to_string();

    assert!(err(merge::merge(&v, &a.id, &a.id)).contains("itself"));
    let slug = v.get(&a.id).unwrap().slug().unwrap();
    assert!(err(merge::merge(&v, &slug, &a.id)).contains("itself"), "a slug and an id of one note are the same note");

    merge::merge(&v, &a.id, &b.id).unwrap();
    assert!(err(merge::merge(&v, &a.id, &c.id)).contains("already merged"));
    assert!(err(merge::merge(&v, &c.id, &a.id)).contains("merge into that note"), "into a merged note");
    assert!(err(merge::merge(&v, &c.id, &a.id)).contains(&b.id));

    ops::set_status(&v, &c.id, "archived").unwrap();
    let d = note(&v, "Delta note", "note", "d", &[], &[]);
    assert!(err(merge::merge(&v, &d.id, &c.id)).contains("archived"));

    let item = common::add(&v, "Open task", "task", Some("app"), &[]);
    let fact = note(&v, "A fact", "note", "f", &[], &[]);
    let e = err(merge::merge(&v, &item.id, &fact.id));
    assert!(e.contains("backlog item") && e.contains("state"), "{e}");
    let other = common::add(&v, "Elsewhere", "note", Some("other"), &[]);
    let e = err(merge::merge(&v, &other.id, &fact.id));
    assert!(e.contains("project"), "{e}");

    assert!(merge::merge(&v, &fact.id, &item.id).is_ok(), "a fact can fold into a backlog item");
    assert!(merge::unmerge(&v, &d.id).err().unwrap().to_string().contains("not merged"));
}

#[test]
fn backlog_items_merge_into_items_before_the_history_and_name_their_state() {
    let (_d, v) = common::vault();
    let keep = common::add(&v, "Add export", "feature", Some("app"), &[]);
    let dup = common::add(&v, "Export to csv", "task", Some("app"), &[]);
    ops::append(&v, &dup.id, "needs a header row").unwrap();
    breadcrumb::backlog::move_item(&v, &dup.id, breadcrumb::backlog::MoveReq { state: "queued".into(), ..Default::default() }).unwrap();
    merge::merge(&v, &dup.id, &keep.id).unwrap();
    let k = v.get(&keep.id).unwrap();
    let merged_at = k.body.find("Merged from").unwrap();
    assert!(k.body.contains("task queued"), "{}", k.body);
    if let Some(h) = k.body.find("## History") {
        assert!(merged_at < h, "{}", k.body);
    }
    assert!(!k.body.contains("\n## History\n\n2") || k.body.matches("\n## History").count() == 1, "the duplicate's history heading is demoted: {}", k.body);
    assert!(k.body.contains("needs a header row"));
    let e = ops::append(&v, &dup.id, "late note").err().unwrap().to_string();
    assert!(e.contains("merged into") && e.contains(&keep.id), "{e}");
    assert!(breadcrumb::backlog::claim(&v, &dup.id, "x", false).is_err(), "a merged item cannot be claimed");
}

#[test]
fn writing_to_a_merged_note_is_refused_with_a_pointer() {
    let (_d, v) = common::vault();
    let keep = note(&v, "Kept fact", "note", "k", &[], &[]);
    let dup = note(&v, "Duplicate fact", "note", "d", &[], &[]);
    let third = note(&v, "Third", "note", "t", &[], &[]);
    merge::merge(&v, &dup.id, &keep.id).unwrap();
    for r in [
        ops::append(&v, &dup.id, "more").map(|_| ()),
        ops::update(&v, &dup.id, UpdateReq { title: Some("x".into()), ..Default::default() }).map(|_| ()),
        ops::link(&v, &dup.id, &third.id, None, false).map(|_| ()),
        ops::link(&v, &third.id, &dup.id, None, false).map(|_| ()),
        ops::update(&v, &third.id, UpdateReq { fields: vec![("merged_into".into(), FieldInput::Raw(keep.id.clone()))], ..Default::default() }).map(|_| ()),
    ] {
        assert!(r.is_err());
    }
    assert!(ops::append(&v, &dup.id, "more").err().unwrap().to_string().contains(&keep.id));
}

#[test]
fn doctor_lists_likely_duplicates_with_the_merge_command_and_changes_nothing() {
    let (_d, v) = common::vault();
    let hdri = "The native tracer refuses HDRI files above 8192 pixels wide on Linux, because the Mesa driver caps texture size there, so large environment maps fail to load and the scene renders black.";
    let other = "Plugins read their manifest from the bundle root, a manifest in a subfolder is ignored and the plugin never shows in the list, which looks like a broken install to users.";
    let old = note(&v, "HDRI size limits on Linux", "gotcha", hdri, &["render"], &[]);
    let new = note(&v, "HDRI limit for big files on Linux", "gotcha", &format!("{hdri} Seen again today."), &["render"], &[]);
    let globs_a = note(&v, "Cache invalidation order", "note", "c", &[], &["src/cache/**"]);
    let globs_b = note(&v, "Order of cache eviction", "note", "d", &[], &["src/cache/**"]);
    let unrelated = note(&v, "Packaging needs a desktop file", "note", "e", &[], &["src/cache/**"]);
    let apart = note(&v, "HDRI limit for big plugin files on Linux", "gotcha", other, &["render"], &[]);
    let linked_a = note(&v, "Shader cache path on Linux builds", "gotcha", hdri, &[], &[]);
    let linked_b = note(&v, "Shader cache path for Linux builds", "gotcha", hdri, &[], &[]);
    ops::link(&v, &linked_b.id, &linked_a.id, Some("relates"), false).unwrap();
    let told = note(&v, "Packaging step", "note", &format!("Duplicate of {}, read that one.", unrelated.id), &[], &[]);
    let map_a = common::add(&v, "docs translations de batch", "map", Some("app"), &[("area", "docs")]);
    let map_b = common::add(&v, "docs translations fr batch", "map", Some("app"), &[("area", "docs-fr")]);
    for (older, newer) in [(&old, &new), (&globs_a, &globs_b)] {
        let mut n = v.get(&older.id).unwrap();
        n.created = newer.created - chrono::Duration::days(1);
        v.write_note(&mut n).unwrap();
    }
    let before: Vec<String> = [&old, &new, &globs_a, &globs_b, &unrelated].iter().map(|n| file(&v, &n.id)).collect();
    let problems = breadcrumb::doctor::check_with(&v, None);
    let lines: Vec<&breadcrumb::doctor::Problem> = problems.iter().filter(|p| p.message.contains("may duplicate")).collect();
    let for_id = |id: &str| lines.iter().find(|p| p.id.as_deref() == Some(id)).map(|p| p.message.clone());
    let m = for_id(&new.id).expect("the newer note is named");
    assert!(m.contains(&old.id) && m.contains(&format!("breadcrumb merge {} {}", new.id, old.id)), "{m}");
    assert!(lines.iter().all(|p| p.severity == "info"));
    assert!(for_id(&globs_b.id).is_some_and(|m| m.contains(&globs_a.id)), "same globs and a shared title word");
    assert!(for_id(&unrelated.id).is_none(), "same globs alone are not enough");
    assert!(for_id(&apart.id).is_none(), "a lookalike title with a different text is a separate fact");
    assert!(for_id(&linked_a.id).is_none() && for_id(&linked_b.id).is_none(), "a linked pair was looked at and kept");
    assert!(for_id(&told.id).is_some_and(|m| m.contains(&unrelated.id)), "a note that says it duplicates another is listed");
    assert!(for_id(&map_a.id).is_none() && for_id(&map_b.id).is_none(), "code maps are not listed");
    assert_eq!(lines[0].id.as_deref(), Some(told.id.as_str()), "strongest first");
    let after: Vec<String> = [&old, &new, &globs_a, &globs_b, &unrelated].iter().map(|n| file(&v, &n.id)).collect();
    assert_eq!(before, after);

    merge::merge(&v, &new.id, &old.id).unwrap();
    let again = breadcrumb::doctor::check_with(&v, None);
    assert!(!again.iter().any(|p| p.message.contains("may duplicate") && p.id.as_deref() == Some(new.id.as_str())), "a merged note is not listed again");
}

#[test]
fn cli_merges_with_or_without_into_and_undoes() {
    let (_d, v) = common::vault();
    let keep = note(&v, "Kept fact", "note", "k", &["x"], &[]);
    let dup = note(&v, "Duplicate fact", "note", "d", &["y"], &[]);
    let out = crumb_ok(&v, &["merge", &dup.id, "into", &keep.id]);
    assert!(out.starts_with("merged ") && out.contains(&format!("merge --undo {}", dup.id)), "{out}");
    let handle = v.get(&dup.id).unwrap().handle();
    let rows = |q: &str| crumb_ok(&v, &["search", "--all", q]).lines().filter(|l| l.starts_with(&handle)).count();
    assert_eq!(rows("duplicate"), 0);
    let again = crumb(&v, &["merge", &dup.id, &keep.id]);
    assert!(!again.status.success() && String::from_utf8_lossy(&again.stderr).contains("already merged"));
    assert!(!crumb(&v, &["merge", &dup.id]).status.success());
    assert!(!crumb(&v, &["merge", "--undo", &dup.id, &keep.id]).status.success());
    let out = crumb_ok(&v, &["merge", "--undo", &dup.id]);
    assert!(out.starts_with("restored "), "{out}");
    assert_eq!(rows("duplicate"), 1);
    assert_eq!(v.get(&keep.id).unwrap().tags, vec!["x"]);
}

#[test]
fn the_merge_tool_merges_and_undoes() {
    let (d, v) = common::vault();
    let srv = Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let keep = note(&v, "Kept fact", "note", "k", &[], &[]);
    let dup = note(&v, "Duplicate fact", "note", "d", &[], &[]);
    let args = |x: Value| x.as_object().unwrap().clone();
    let e = srv.call_tool("merge", &args(json!({"id": dup.id}))).err().unwrap().to_string();
    assert!(e.contains("into"), "{e}");
    let text = srv.call_tool("merge", &args(json!({"id": dup.id, "into": keep.id}))).unwrap();
    assert!(text.starts_with("merged ") && !text.contains('\n'), "{text}");
    let got = srv.call_tool("get", &args(json!({"id": dup.id}))).unwrap();
    assert!(got.starts_with("merged into"), "{got}");
    let text = srv.call_tool("merge", &args(json!({"id": dup.id, "undo": true}))).unwrap();
    assert!(text.starts_with("restored "), "{text}");
    assert!(!v.get(&dup.id).unwrap().is_archived());
}
