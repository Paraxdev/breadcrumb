mod common;

use breadcrumb::mcp;
use breadcrumb::ops;
use breadcrumb::vault::Vault;
use serde_json::json;
use std::process::Command;

fn crumb(v: &Vault, args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_crumb")).args(args).args(["--vault", v.root.to_str().unwrap()]).output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

fn ok(v: &Vault, args: &[&str]) -> String {
    let (ok, out, err) = crumb(v, args);
    assert!(ok, "{args:?}: {err}");
    out
}

fn imported(v: &Vault) -> String {
    let n = common::add(v, "Imported", "reference", Some("app"), &[]);
    let path = v.resolve_path(&n.id).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let extra = "source: claude:projects/x.md\nmetadata:\n  node_type: memory\n  originSessionId: da55\nsource_hash: abc123\nmemory_name: imported\nverified:";
    std::fs::write(&path, text.replacen("verified:", extra, 1)).unwrap();
    n.id
}

const IMPORT: [&str; 5] = ["source:", "node_type", "originSessionId", "source_hash", "memory_name"];

#[test]
fn get_hides_import_keys_and_full_and_json_keep_them() {
    let (_d, v) = common::vault();
    let id = imported(&v);
    let before = std::fs::read_to_string(v.resolve_path(&id).unwrap()).unwrap();
    let plain = ok(&v, &["get", &id]);
    let full = ok(&v, &["get", &id, "--full"]);
    let as_json = ok(&v, &["get", &id, "--json"]);
    for k in IMPORT {
        assert!(!plain.contains(k), "{k} in {plain}");
        assert!(full.contains(k), "{k} in {full}");
    }
    let parsed: serde_json::Value = serde_json::from_str(&as_json).unwrap();
    for k in ["source", "metadata", "source_hash", "memory_name", "verified", "body", "path"] {
        assert!(parsed.get(k).is_some(), "{k} in {as_json}");
    }
    assert_eq!(before, std::fs::read_to_string(v.resolve_path(&id).unwrap()).unwrap());
}

#[test]
fn a_source_set_by_hand_stays_visible() {
    let (_d, v) = common::vault();
    let n = ops::add(&v, ops::AddReq { title: "From a page".into(), project: Some("app".into()), source: Some("https://example.com".into()), ..Default::default() }).unwrap();
    assert!(ok(&v, &["get", &n.id]).contains("source: https://example.com"));
}

#[test]
fn get_says_what_superseded_a_note_and_what_links_to_it() {
    let (_d, v) = common::vault();
    let old = common::add(&v, "Old", "note", Some("app"), &[]).id;
    let new = common::add(&v, "New", "note", Some("app"), &[]).id;
    let other = common::add(&v, "Other", "note", Some("app"), &[]).id;
    ok(&v, &["link", &new, &old, "--rel", "supersedes"]);
    ok(&v, &["link", &other, &old]);
    let out = ok(&v, &["get", &old]);
    assert!(out.lines().any(|l| l.starts_with("superseded by") && l.contains(&new) && l.contains("\"New\"")), "{out}");
    assert!(out.contains("linked from:") && out.lines().any(|l| l.starts_with("  relates") && l.contains(&other)), "{out}");
    let parsed: serde_json::Value = serde_json::from_str(&ok(&v, &["get", &old, "--json"])).unwrap();
    assert_eq!(parsed["inbound"].as_array().unwrap().len(), 2, "{parsed}");
    assert!(!ok(&v, &["get", &new]).contains("superseded by"));
    assert!(!ok(&v, &["get", &old, "--body"]).contains("superseded by"));
}

#[test]
fn an_archived_note_supersedes_nothing() {
    let (_d, v) = common::vault();
    let old = common::add(&v, "Old", "note", Some("app"), &[]).id;
    let new = common::add(&v, "New", "note", Some("app"), &[]).id;
    ok(&v, &["link", &new, &old, "--rel", "supersedes"]);
    ok(&v, &["archive", &new]);
    let out = ok(&v, &["get", &old]);
    assert!(!out.contains("superseded by") && out.lines().any(|l| l.starts_with("  supersedes") && l.contains(&new)), "{out}");
}

fn long_item(v: &Vault) -> String {
    let id = common::add(v, "Long", "task", Some("app"), &[]).id;
    let body: Vec<String> = (1..=10).map(|i| format!("log {i}")).collect();
    ok(v, &["update", &id, "--body", &body.join("\n")]);
    ok(v, &["move", &id, "queued"]);
    id
}

#[test]
fn tail_keeps_the_last_lines_says_how_many_went_and_keeps_history() {
    let (_d, v) = common::vault();
    let id = long_item(&v);
    let out = ok(&v, &["get", &id, "--tail", "3"]);
    assert!(out.contains("(7 earlier body lines cut"), "{out}");
    assert!(out.contains("log 8\nlog 9\nlog 10") && !out.contains("log 7"), "{out}");
    assert!(out.contains("## History") && out.contains("-> queued"), "{out}");
    assert!(out.starts_with("---\n"), "{out}");

    let all = ok(&v, &["get", &id, "--tail", "50"]);
    assert!(!all.contains("cut") && all.contains("log 1\n"), "{all}");
    let parsed: serde_json::Value = serde_json::from_str(&ok(&v, &["get", &id, "--tail", "3", "--json"])).unwrap();
    assert!(parsed["body"].as_str().unwrap().contains("log 1\n"), "--json keeps the whole body");
}

#[test]
fn body_prints_the_text_alone() {
    let (_d, v) = common::vault();
    let id = long_item(&v);
    let out = ok(&v, &["get", &id, "--body"]);
    assert!(out.starts_with("log 1\n") && out.ends_with("log 10\n"), "{out}");
    assert!(!out.contains("History") && !out.contains("---"), "{out}");

    let (_, piped, err) = crumb(&v, &["get", &id, "--body", "--tail", "2"]);
    assert_eq!(piped, "log 9\nlog 10\n");
    assert!(err.contains("(8 earlier body lines cut"), "{err}");
}

#[test]
fn a_tail_or_body_changes_nothing_in_the_file() {
    let (_d, v) = common::vault();
    let id = long_item(&v);
    let before = std::fs::read_to_string(v.resolve_path(&id).unwrap()).unwrap();
    ok(&v, &["get", &id, "--tail", "1"]);
    ok(&v, &["get", &id, "--body"]);
    assert_eq!(before, std::fs::read_to_string(v.resolve_path(&id).unwrap()).unwrap());
}

#[test]
fn the_mcp_get_takes_the_same_options() {
    let (_d, v) = common::vault();
    let id = imported(&v);
    ok(&v, &["update", &id, "--body", "one\ntwo\nthree"]);
    let later = common::add(&v, "Later", "note", Some("app"), &[]).id;
    ok(&v, &["link", &later, &id, "--rel", "supersedes"]);
    let srv = mcp::Server { vault_root: v.root.clone(), cwd: v.root.clone() };
    let get = |a: serde_json::Value| srv.call_tool("get", a.as_object().unwrap()).unwrap();

    let plain = get(json!({"id": id}));
    assert!(IMPORT.iter().all(|k| !plain.contains(k)) && plain.lines().any(|l| l.starts_with("superseded by") && l.contains(&later)), "{plain}");
    let full = get(json!({"id": id, "full": true}));
    assert!(IMPORT.iter().all(|k| full.contains(k)), "{full}");
    assert!(get(json!({"id": id, "tail": 1})).contains("(2 earlier body lines cut"));
    assert_eq!(get(json!({"id": id, "body": true})), "one\ntwo\nthree");
    let tailed = get(json!({"id": id, "body": true, "tail": 1}));
    assert!(tailed.starts_with("(2 earlier") && tailed.ends_with("\nthree"), "{tailed}");
}
