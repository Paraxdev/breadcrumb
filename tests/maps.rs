mod common;

use breadcrumb::map::{self, PutReq};
use breadcrumb::mcp::Server;
use breadcrumb::vault::Vault;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

fn repo(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("app");
    std::fs::create_dir_all(&root).unwrap();
    common::git(&root, &["init", "-q"]);
    for (f, text) in files {
        write(&root, f, text);
    }
    (d, root)
}

fn write(root: &Path, f: &str, text: &str) {
    let p = root.join(f);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn setup(files: &[(&str, &str)]) -> (tempfile::TempDir, Vault, tempfile::TempDir, PathBuf) {
    let (vd, v) = common::vault();
    let (rd, root) = repo(files);
    breadcrumb::ops::project_link(&v, "app", &root, None).unwrap();
    (vd, v, rd, root)
}

fn put(v: &Vault, root: &Path, area: &str, lines: &[&str]) -> map::PutReport {
    let req = PutReq { area: area.into(), project: "app".into(), lines: lines.iter().map(|s| s.to_string()).collect(), ..Default::default() };
    map::put(v, req, root).unwrap()
}

fn get(v: &Vault, root: &Path, area: &str) -> String {
    map::get_text(v, "app", area, Some(root)).unwrap()
}

#[test]
fn an_edited_file_marks_its_lines_stale_and_a_deleted_one_gone() {
    let (_vd, v, _rd, root) = setup(&[("src/view/overlay.ts", "export function setOverlay() {}\n"), ("src/view/legend.ts", "export const legend = 1;\n")]);
    let r = put(&v, &root, "stress overlay", &["src/view/overlay.ts:1 setOverlay, turns the overlay on", "- src/view/legend.ts:1 legend, colour scale"]);
    assert!(r.created && r.added == 2, "{}", r.text());
    assert_eq!(common::git(&v.root, &["status", "--porcelain"]), "", "the map and its kind file are committed");
    let fresh = get(&v, &root, "stress overlay");
    assert!(fresh.starts_with(&format!("map stress overlay {}: 2 lines, all fresh\n", r.map.note.id)), "{fresh}");
    assert!(fresh.contains("\nsrc/view/overlay.ts:1 setOverlay, turns the overlay on\n"), "{fresh}");

    write(&root, "src/view/overlay.ts", "export function setOverlay(on: boolean) {}\n");
    let text = get(&v, &root, "Stress Overlay");
    let header = text.lines().next().unwrap();
    assert!(header.contains("1 stale (src/view/overlay.ts changed)"), "{text}");
    assert!(text.contains("[stale] src/view/overlay.ts:1 setOverlay"), "{text}");
    assert!(text.contains("\nsrc/view/legend.ts:1 legend"), "{text}");

    std::fs::remove_file(root.join("src/view/legend.ts")).unwrap();
    let text = get(&v, &root, "stress overlay");
    assert!(text.lines().next().unwrap().contains("2 stale (src/view/legend.ts gone, src/view/overlay.ts changed)"), "{text}");
    assert!(map::get_text(&v, "app", "stress overlay", None).unwrap().contains("freshness unknown"));
}

#[test]
fn two_areas_stay_apart_and_a_second_map_for_an_area_is_refused() {
    let (_vd, v, _rd, root) = setup(&[("src/view/overlay.ts", "a\n"), ("src/doc/session.ts", "b\n")]);
    put(&v, &root, "stress overlay", &["src/view/overlay.ts:1 setOverlay, turns it on"]);
    put(&v, &root, "document session", &["src/doc/session.ts:1 commitWithCascade, writes and rebuilds"]);
    let list = map::list_text(&v, "app", Some(&root));
    assert_eq!(list.lines().count(), 2, "{list}");
    assert!(list.contains("map document session") && list.contains("map stress overlay"), "{list}");
    let b = map::brief(&v, "app", &["document session".into()], Some(&root), map::BRIEF_BUDGET).unwrap();
    assert!(b.contains("commitWithCascade") && !b.contains("setOverlay"), "{b}");

    let dup = breadcrumb::ops::add(
        &v,
        breadcrumb::ops::AddReq { title: "overlay again".into(), kind: Some("map".into()), project: Some("app".into()), fields: vec![("area".into(), breadcrumb::ops::FieldInput::Raw("Stress overlay".into()))], ..Default::default() },
    );
    assert!(dup.unwrap_err().to_string().contains("map put"));
    assert_eq!(v.load_notes().iter().filter(|n| n.kind == "map").count(), 2);
}

#[test]
fn put_merges_by_file_and_symbol_and_keeps_each_line_its_own_hash() {
    let (_vd, v, _rd, root) = setup(&[("src/a.rs", "fn alpha() {}\nfn beta() {}\n")]);
    let first = put(&v, &root, "core", &["src/a.rs:1 alpha, old text", "src/a.rs:2 beta, second"]);
    let r = put(&v, &root, "core", &["src/a.rs:1 alpha, new text", "src/a.rs:1 gamma, third"]);
    assert_eq!((r.created, r.added, r.replaced, r.unchanged), (false, 1, 1, 0), "{}", r.text());
    assert_eq!(r.map.note.id, first.map.note.id);
    let text = get(&v, &root, "core");
    assert!(text.contains("alpha, new text") && !text.contains("old text") && text.contains("beta, second"), "{text}");
    assert_eq!(text.lines().count(), 4, "{text}");

    write(&root, "src/a.rs", "fn alpha(x: u8) {}\nfn beta() {}\nfn gamma() {}\n");
    let r = put(&v, &root, "core", &["src/a.rs:1 alpha, new text"]);
    assert_eq!((r.refreshed, r.added), (1, 0), "{}", r.text());
    let text = get(&v, &root, "core");
    assert!(text.contains("\nsrc/a.rs:1 alpha, new text") && text.contains("[stale] src/a.rs:2 beta") && text.contains("[stale] src/a.rs:1 gamma"), "{text}");

    let req = PutReq { area: "core".into(), project: "app".into(), drop: vec!["src/a.rs gamma".into()], ..Default::default() };
    assert_eq!(map::put(&v, req, &root).unwrap().dropped, 1);
    let req = PutReq { area: "core".into(), project: "app".into(), lines: vec!["src/missing.rs:3 nope".into()], ..Default::default() };
    assert!(map::put(&v, req, &root).is_err());
    let req = PutReq { area: "core".into(), project: "app".into(), lines: vec!["not a map line".into()], ..Default::default() };
    assert!(map::put(&v, req, &root).is_err());
    assert_eq!(get(&v, &root, "core").lines().count(), 3);
}

#[test]
fn brief_names_the_gotchas_whose_globs_match_and_not_others() {
    let (_vd, v, _rd, root) = setup(&[("src/view/overlay.ts", "a\n"), ("src/db/store.ts", "b\n")]);
    put(&v, &root, "stress overlay", &["src/view/overlay.ts:1 setOverlay, turns it on"]);
    let add = |title: &str, kind: &str, project: Option<&str>, glob: &str| {
        breadcrumb::ops::add(
            &v,
            breadcrumb::ops::AddReq { title: title.into(), kind: Some(kind.into()), project: project.map(Into::into), body: Some(format!("{title} body")), paths: vec![glob.into()], ..Default::default() },
        )
        .unwrap()
    };
    let hit = add("Overlay needs a redraw after the mesh swaps", "gotcha", Some("app"), "src/view/**");
    let dec = add("Overlay colours come from the legend", "decision", Some("app"), "overlay.ts");
    let fb = add("Keep overlay code small", "feedback", None, "src/view/*.ts");
    let other = add("Store writes are batched", "gotcha", Some("app"), "src/db/**");
    let elsewhere = add("Other project overlay trap", "gotcha", Some("web"), "src/view/**");
    let plain = add("Overlay history", "note", Some("app"), "src/view/**");

    let b = map::brief(&v, "app", &["src/view/overlay.ts".into()], Some(&root), map::BRIEF_BUDGET).unwrap();
    for n in [&hit, &dec, &fb] {
        assert!(b.contains(&n.id), "{} missing from\n{b}", n.title);
    }
    for n in [&other, &elsewhere, &plain] {
        assert!(!b.contains(&n.id), "{} should not be in\n{b}", n.title);
    }
    assert!(b.find(&hit.id).unwrap() < b.find(&dec.id).unwrap() && b.find(&dec.id).unwrap() < b.find(&fb.id).unwrap(), "gotchas rank first\n{b}");
    assert!(b.contains("setOverlay") && b.starts_with("brief for src/view/overlay.ts: 1 map, 3 notes\n"), "{b}");

    let by_area = map::brief(&v, "app", &["stress overlay".into()], Some(&root), map::BRIEF_BUDGET).unwrap();
    assert!(by_area.contains(&hit.id) && !by_area.contains(&other.id), "an area brings the gotchas of its files\n{by_area}");
    let none = map::brief(&v, "app", &["src/db/store.ts".into()], Some(&root), map::BRIEF_BUDGET).unwrap();
    assert!(none.contains("no map covers these") && none.contains(&other.id), "{none}");
}

#[test]
fn brief_keeps_to_its_budget_and_says_what_it_cut() {
    let files: Vec<(String, String)> = (0..40).map(|i| (format!("src/m{i:02}.rs"), format!("fn f{i}() {{}}\n"))).collect();
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let (_vd, v, _rd, root) = setup(&refs);
    let lines: Vec<String> = (0..40).map(|i| format!("src/m{i:02}.rs:1 f{i}, {}", "does a fairly long thing worth describing ".repeat(2))).collect();
    let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    put(&v, &root, "big", &line_refs);
    let g = breadcrumb::ops::add(
        &v,
        breadcrumb::ops::AddReq { title: "m39 has a trap".into(), kind: Some("gotcha".into()), project: Some("app".into()), paths: vec!["src/m39.rs".into()], ..Default::default() },
    )
    .unwrap();

    let b = map::brief(&v, "app", &["big".into(), "src/m39.rs".into()], Some(&root), 1500).unwrap();
    assert!(b.len() <= 1500 + 200, "{} chars\n{b}", b.len());
    assert!(b.contains("src/m39.rs:1 f39") && b.contains(&g.id), "the named file and its gotcha come first\n{b}");
    let cut = b.lines().last().unwrap();
    assert!(cut.starts_with("cut to fit 1500 chars: ") && cut.contains("map lines") && cut.contains("of big"), "{b}");
    let full = map::brief(&v, "app", &["big".into()], Some(&root), map::BRIEF_BUDGET).unwrap();
    assert!(!full.contains("cut to fit") && full.len() <= map::BRIEF_BUDGET, "{full}");
}

#[test]
fn the_mcp_map_tool_puts_and_briefs() {
    let (_vd, v, _rd, root) = setup(&[("src/a.ts", "x\n")]);
    let srv = Server { vault_root: v.root.clone(), cwd: root.clone() };
    let args = |x: Value| -> Map<String, Value> { x.as_object().unwrap().clone() };
    let put = srv.call_tool("map", &args(json!({"action": "put", "area": "core", "lines": ["src/a.ts:1 main, entry point"]}))).unwrap();
    assert!(put.starts_with("created map core "), "{put}");
    let again = srv.call_tool("map", &args(json!({"action": "put", "area": "core", "lines": "src/a.ts:1 main, entry point"}))).unwrap();
    assert!(again.contains("1 unchanged"), "{again}");
    let b = srv.call_tool("map", &args(json!({"targets": ["src/a.ts"]}))).unwrap();
    assert!(b.contains("src/a.ts:1 main, entry point") && b.contains("all fresh"), "{b}");
    assert!(srv.call_tool("map", &args(json!({"action": "list"}))).unwrap().contains("map core"));
    let names: Vec<String> = breadcrumb::mcp::tool_defs().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    assert!(names.contains(&"map".to_string()) && !names.contains(&"brief".to_string()), "one new tool");
}
