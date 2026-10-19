mod common;

use breadcrumb::ops::{self, UpdateReq};
use std::fs;

#[test]
fn doctor_fix_rewrites_moved_paths_and_marks_gone_ones() {
    let (d, v) = common::vault();
    let repo = d.path().join("demo");
    fs::create_dir_all(repo.join("src/engine")).unwrap();
    fs::create_dir_all(repo.join("lib")).unwrap();
    fs::write(repo.join("src/engine/core.rs"), "a").unwrap();
    fs::write(repo.join("src/engine/mesh.rs"), "b").unwrap();
    fs::write(repo.join("src/old_name.rs"), "fn x() {}\n".repeat(20)).unwrap();
    fs::write(repo.join("lib/helper.py"), "c").unwrap();
    common::git(&repo, &["init", "-q"]);
    common::git(&repo, &["-c", "user.name=t", "-c", "user.email=t@example.com", "add", "-A"]);
    common::git(&repo, &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "-m", "init"]);
    common::git(&repo, &["mv", "src/old_name.rs", "src/new_name.rs"]);
    common::git(&repo, &["mv", "src/engine", "src/kernel"]);
    common::git(&repo, &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "-m", "move"]);
    fs::create_dir_all(repo.join("tools")).unwrap();
    fs::rename(repo.join("lib/helper.py"), repo.join("tools/helper.py")).unwrap();
    ops::project_link(&v, "demo", &repo, None).unwrap();

    let body = "See `src/old_name.rs:12` and `lib/helper.py`, also `src/gone.rs` and `src/new_name.rs`.";
    let n = common::add(&v, "Engine notes", "note", Some("demo"), &[]);
    ops::update(&v, &n.id, UpdateReq { body: Some(body.into()), set_paths: Some(vec!["src/engine/**".into(), "src/vanished/**".into(), "*.rs".into()]), ..Default::default() }).unwrap();
    let warnings = |v: &breadcrumb::vault::Vault| breadcrumb::doctor::check(v).into_iter().filter(|p| p.message.contains("does not exist")).count();
    assert_eq!(warnings(&v), 5);

    let plan = breadcrumb::fix::plan(&v);
    let lines: Vec<String> = plan.iter().map(|c| c.line()).collect();
    assert_eq!(plan.len(), 5, "{lines:#?}");
    let text = breadcrumb::fix::text(&plan, None);
    assert!(text.contains("dry run") && text.contains("3 rewritten, 2 dropped or marked gone"), "{text}");
    let head = common::git(&v.root, &["rev-parse", "HEAD"]);
    assert_eq!(v.get(&n.id).unwrap().body, body, "dry run changes nothing");

    assert_eq!(breadcrumb::fix::apply(&v, &plan).unwrap(), 1);
    assert_eq!(common::git(&v.root, &["rev-list", "--count", &format!("{head}..HEAD")]), "1");
    let fixed = v.get(&n.id).unwrap();
    assert_eq!(fixed.body, "See `src/new_name.rs:12` and `tools/helper.py`, also `src/gone.rs` (gone) and `src/new_name.rs`.");
    assert_eq!(fixed.path_globs(), vec!["src/kernel/**", "*.rs"]);
    assert_eq!(warnings(&v), 0);
    assert!(breadcrumb::fix::plan(&v).is_empty());
}
