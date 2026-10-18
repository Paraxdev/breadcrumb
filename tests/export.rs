mod common;

use breadcrumb::export;
use breadcrumb::ops::{self, UpdateReq};
use breadcrumb::vault::Vault;

#[test]
fn bundle_restore_and_jsonl() {
    let (d, mut v) = common::vault();
    let n = common::add(&v, "Survives the move", "decision", Some("app"), &[("extra", "kept")]);
    export::remote_set(&mut v, "https://github.com/me/vault.git").unwrap();
    std::fs::write(v.root.join("global/notes/uncommitted.md"), "# Pending\n\nnot committed yet\n").unwrap();

    let bundle = d.path().join("out").join("vault.bundle");
    export::bundle(&v, &bundle).unwrap();
    assert!(bundle.is_file());
    assert_eq!(common::git(&v.root, &["status", "--porcelain"]), "", "bundle commits pending changes first");

    let target = d.path().join("restored");
    let (dir, remote) = export::restore(&bundle, &target).unwrap();
    assert_eq!(remote.as_deref(), Some("https://github.com/me/vault.git"));
    assert_eq!(common::git(&dir, &["remote", "get-url", "origin"]), "https://github.com/me/vault.git");
    let r = Vault::open(&dir).unwrap();
    let got = r.get(&n.id).unwrap();
    assert_eq!(got.title, "Survives the move");
    assert_eq!(got.field("extra").and_then(|x| x.as_str()), Some("kept"));
    assert!(r.load_notes().iter().any(|x| x.title == "Pending"));
    assert!(export::restore(&bundle, &target).unwrap_err().to_string().contains("not empty"));

    let jl = d.path().join("dump.jsonl");
    let (_, count) = export::jsonl(&v, &jl).unwrap();
    assert_eq!(count, 2);
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(&jl).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert!(lines.iter().any(|l| l["record"] == "kind" && l["name"] == "todo"));
    assert!(lines.iter().any(|l| l["record"] == "project" && l["slug"] == "app"));
    let note = lines.iter().find(|l| l["record"] == "note" && l["id"] == n.id.as_str()).unwrap();
    assert_eq!(note["extra"], "kept");
    assert!(note["path"].as_str().unwrap().starts_with("projects/app/notes/"));
}

#[test]
fn sync_pushes_pulls_and_aborts_conflicts_without_data_loss() {
    let (d, mut a) = common::vault();
    let bare = d.path().join("remote.git");
    common::git(d.path(), &["init", "-q", "--bare", "-b", "main", &bare.to_string_lossy()]);
    let n = common::add(&a, "Shared note", "note", None, &[]);
    export::remote_set(&mut a, &bare.to_string_lossy()).unwrap();
    let r = export::sync(&a, None).unwrap();
    assert!(r.pushed && !r.pulled);

    let b_root = d.path().join("b");
    common::git(d.path(), &["clone", "-q", &bare.to_string_lossy(), &b_root.to_string_lossy()]);
    let b = Vault::open(&b_root).unwrap();
    let from_b = common::add(&b, "Written on PC b", "note", None, &[]);
    assert!(export::sync(&b, Some("sync from b")).unwrap().pushed);
    let r = export::sync(&a, None).unwrap();
    assert!(r.pulled && r.pushed);
    assert!(a.get(&from_b.id).is_ok(), "a sees b's note after sync");

    ops::update(&a, &n.id, UpdateReq { body: Some("edited on a".into()), ..Default::default() }).unwrap();
    export::sync(&a, None).unwrap();
    ops::update(&b, &n.id, UpdateReq { body: Some("edited on b".into()), ..Default::default() }).unwrap();
    let b_head = common::git(&b_root, &["rev-parse", "HEAD"]);
    let err = export::sync(&b, None).unwrap_err().to_string();
    assert!(err.contains("conflict") && err.contains(&n.id), "{err}");
    assert_eq!(common::git(&b_root, &["rev-parse", "HEAD"]), b_head, "local commit intact");
    assert!(!b_root.join(".git/rebase-merge").exists() && !b_root.join(".git/rebase-apply").exists());
    assert_eq!(b.get(&n.id).unwrap().body, "edited on b");
}

#[test]
fn sync_without_remote_commits_locally() {
    let (_d, v) = common::vault();
    std::fs::write(v.root.join("global/notes/hand-written.md"), "# Hand written\n").unwrap();
    let r = export::sync(&v, Some("manual edits")).unwrap();
    assert!(r.committed && !r.pushed);
    assert!(r.message.contains("no origin"));
    assert_eq!(common::git(&v.root, &["log", "-1", "--format=%s"]), "manual edits");
}

#[test]
fn doctor_reports_problems() {
    let (_d, v) = common::vault();
    let ok = common::add(&v, "fine", "note", None, &[]);
    let dir = v.root.join("global/notes");
    let w = |name: &str, s: &str| std::fs::write(dir.join(name), s).unwrap();
    w("aaaa1111-unknown.md", "---\nid: aaaa1111\ntitle: x\nkind: nosuchkind\ncreated: 2026-01-01T00:00:00Z\n---\n");
    w("bbbb2222-broken.md", "---\nid: bbbb2222\ntitle: x\nkind: note\nlinks: [{to: zzzzzzzz}]\ncreated: 2026-01-01T00:00:00Z\n---\n");
    w(&format!("{}-dupe.md", ok.id), &format!("---\nid: {}\ntitle: dupe\nkind: note\ncreated: 2026-01-01T00:00:00Z\n---\n", ok.id));
    w("cccc3333-bad.md", "---\ntitle: [unclosed\n---\n");
    w("dddd4444-todo.md", "---\nid: dddd4444\ntitle: x\nkind: todo\npriority: urgent\ncreated: 2026-01-01T00:00:00Z\n---\n");
    let mut k = breadcrumb::kind::KindDef::new("strict", "needs a field");
    k.fields.push(breadcrumb::kind::parse_field_spec("must:string:required").unwrap());
    ops::define_kind(&v, k, false, &[]).unwrap();
    w("eeee5555-strict.md", "---\nid: eeee5555\ntitle: x\nkind: strict\ncreated: 2026-01-01T00:00:00Z\n---\n");

    let problems = breadcrumb::doctor::check(&v);
    let has = |needle: &str| problems.iter().any(|p| p.severity == "error" && p.message.contains(needle));
    assert!(has("unknown kind 'nosuchkind'"));
    assert!(has("broken link to 'zzzzzzzz'"));
    assert!(has("duplicate id"));
    assert!(problems.iter().any(|p| p.path.ends_with("cccc3333-bad.md")));
    assert!(has("must be one of"));
    assert!(has("missing required field 'must'"));
    assert!(breadcrumb::doctor::text(&problems, 7).contains("errors"));
}
