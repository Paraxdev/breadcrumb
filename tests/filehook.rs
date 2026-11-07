mod common;

use breadcrumb::map::{self, PutReq};
use breadcrumb::ops::{self, AddReq, FieldInput};
use breadcrumb::vault::Vault;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn setup() -> (tempfile::TempDir, Vault, PathBuf) {
    let (d, v) = common::vault();
    let root = d.path().join("app");
    std::fs::create_dir_all(root.join("src")).unwrap();
    common::git(&root, &["init", "-q"]);
    for f in ["a.ts", "b.ts"] {
        std::fs::write(root.join("src").join(f), "export const x = 1;\n").unwrap();
    }
    ops::project_link(&v, "app", &root, None).unwrap();
    (d, v, root)
}

fn add(v: &Vault, title: &str, kind: &str, state: Option<&str>, summary: Option<&str>, paths: &[&str]) -> breadcrumb::note::Note {
    let mut fields = vec![];
    if let Some(s) = state {
        fields.push(("state".to_string(), FieldInput::Raw(s.into())));
    }
    if let Some(s) = summary {
        fields.push(("summary".to_string(), FieldInput::Raw(s.into())));
    }
    ops::add(v, AddReq { title: title.into(), kind: Some(kind.into()), project: Some("app".into()), fields, paths: paths.iter().map(|p| p.to_string()).collect(), ..Default::default() }).unwrap()
}

fn map_of(v: &Vault, root: &Path, area: &str, file: &str) {
    let req = PutReq { area: area.into(), project: "app".into(), lines: vec![format!("{file}:1 x, what it does")], ..Default::default() };
    map::put(v, req, root).unwrap();
}

fn touch(v: &Vault, root: &Path, file: &str, session: &str) -> Option<String> {
    let input = json!({"cwd": root, "session_id": session, "hook_event_name": "PreToolUse", "tool_name": "Edit", "tool_input": {"file_path": root.join(file)}});
    let mut c = Command::new(env!("CARGO_BIN_EXE_breadcrumb"));
    c.arg("--vault").arg(&v.root).args(["hook", "file-touch"]).current_dir(root).env_remove("BREADCRUMB_HOME");
    c.env("BREADCRUMB_CLAUDE_DIR", v.root.with_extension("no-claude"));
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    if out.trim().is_empty() {
        return None;
    }
    let j: Value = serde_json::from_str(&out).unwrap();
    Some(j["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_string())
}

#[test]
fn gotchas_and_decisions_lead_with_a_why_then_open_items_and_maps_and_closed_items_are_counted() {
    let (_d, v, root) = setup();
    let item = add(&v, "Translate the docs site", "feature", Some("queued"), None, &["src/**"]);
    let done = add(&v, "Old finished feature", "feature", Some("done"), None, &["src/**"]);
    let shipped = add(&v, "Older shipped feature", "feature", Some("shipped"), None, &["src/**"]);
    let gotcha = add(&v, "The loader reads the cache twice", "gotcha", None, Some("Never call load() from a hook, it deadlocks on the cache lock"), &["src/a.ts"]);
    let decision = add(&v, "Overlays use one shared palette", "decision", None, None, &["src/**"]);
    let feedback = add(&v, "Keep functions short", "feedback", None, None, &["src/**"]);
    let maps: Vec<String> = (1..=4)
        .map(|i| {
            let area = format!("review {i}");
            map_of(&v, &root, &area, "src/a.ts");
            area
        })
        .collect();

    let line = touch(&v, &root, "src/a.ts", "s1").expect("notes match");
    assert!(!line.contains('\n'), "{line}");
    let at = |id: &str| line.find(id).unwrap_or_else(|| panic!("{id} missing: {line}"));
    assert!(at(&gotcha.id).max(at(&decision.id)) < at(&feedback.id), "{line}");
    assert!(at(&feedback.id) < at(&item.id), "{line}");
    assert!(line.contains(&format!("{} gotcha The loader reads the cache twice: Never call load() from a hook", gotcha.id)), "the summary is the why: {line}");
    assert!(line.contains(&format!("{} decision Overlays use one shared palette;", decision.id)), "a title that says it all is not repeated: {line}");
    assert!(line.contains("(+4 maps, +2 closed)"), "{line}");
    assert!(line.contains("`crumb brief src/a.ts` (or the map tool)"), "{line}");
    assert!(!line.contains(&done.id) && !line.contains(&shipped.id), "{line}");
    assert!(maps.iter().all(|a| !line.contains(a.as_str())), "{line}");

    assert_eq!(touch(&v, &root, "src/a.ts", "s1"), None, "everything counted counts as shown");
    assert!(touch(&v, &root, "src/a.ts", "s2").is_some(), "another session starts fresh");
}

#[test]
fn a_file_with_only_maps_gets_the_brief_pointer_once_per_map_and_closed_items_alone_say_nothing() {
    let (_d, v, root) = setup();
    map_of(&v, &root, "review 1", "src/a.ts");
    add(&v, "Finished work", "feature", Some("done"), None, &["src/b.ts"]);

    let line = touch(&v, &root, "src/a.ts", "s1").unwrap();
    assert!(line.starts_with("breadcrumb notes for src/a.ts: +1 map. A code map covers it"), "{line}");
    assert_eq!(touch(&v, &root, "src/a.ts", "s1"), None);
    assert_eq!(touch(&v, &root, "src/b.ts", "s1"), None, "a closed item alone is not worth a line");
}

#[test]
fn no_map_means_no_brief_pointer_and_a_plain_gotcha_line_stays_short() {
    let (_d, v, root) = setup();
    let gotcha = add(&v, "Never rename the cache dir. It is hashed into ids", "gotcha", None, None, &["src/a.ts"]);
    let line = touch(&v, &root, "src/a.ts", "s1").unwrap();
    assert_eq!(line, format!("breadcrumb notes for src/a.ts: {} gotcha Never rename the cache dir. It is hashed into ids", gotcha.id));
}

fn crumb(v: &Vault, args: &[&str]) -> String {
    let o = Command::new(env!("CARGO_BIN_EXE_crumb")).args(args).env("BREADCRUMB_HOME", &v.root).env("BREADCRUMB_CLAUDE_DIR", v.root.with_extension("no-claude")).output().unwrap();
    assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).to_string()
}

#[test]
fn adding_a_project_gotcha_or_decision_without_a_path_glob_hints_and_still_saves() {
    let (_d, v, _root) = setup();
    for kind in ["gotcha", "decision"] {
        let out = crumb(&v, &["add", "-k", kind, "-t", "Cache dir is hashed into ids", "-p", "app"]);
        assert!(out.starts_with("added "), "{out}");
        assert!(out.contains(&format!("no path glob, so `brief` and the file hook cannot find this {kind}")), "{out}");
    }

    let with_glob = crumb(&v, &["add", "-k", "gotcha", "-t", "Scoped gotcha", "-p", "app", "--path-glob", "src/**"]);
    assert!(!with_glob.contains("no path glob"), "{with_glob}");
    let plain = crumb(&v, &["add", "-k", "note", "-t", "Plain note", "-p", "app"]);
    assert!(!plain.contains("no path glob"), "{plain}");
    let global = crumb(&v, &["add", "-k", "gotcha", "-t", "Global gotcha", "--global"]);
    assert!(!global.contains("no path glob"), "a global note has no files to match: {global}");
}

#[test]
fn the_mcp_add_result_carries_the_path_glob_hint() {
    let (d, v, _root) = setup();
    let srv = breadcrumb::mcp::Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let add = |args: Value| srv.call_tool("add", args.as_object().unwrap()).unwrap();
    let out = add(json!({"title": "Loader deadlocks", "kind": "gotcha", "project": "app"}));
    assert!(out.starts_with("saved ") && out.contains("\nno path glob, so `brief` and the file hook cannot find this gotcha"), "{out}");
    let out = add(json!({"title": "Loader deadlocks twice", "kind": "gotcha", "project": "app", "paths": ["src/**"]}));
    assert!(!out.contains("no path glob"), "{out}");
}
