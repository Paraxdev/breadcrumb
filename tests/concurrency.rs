mod common;

use breadcrumb::vault::Vault;
use std::process::Command;

#[test]
fn parallel_threads_never_lose_notes_or_commits() {
    let (_d, v) = common::vault();
    let base = breadcrumb::git::commit_count(&v.root);
    let threads = 6;
    let per = 4;
    let handles: Vec<_> = (0..threads)
        .map(|t| {
            let root = v.root.clone();
            std::thread::spawn(move || {
                let v = Vault::open(&root).unwrap();
                let mut ids = vec![];
                for i in 0..per {
                    let project = if i % 2 == 0 { Some("shared") } else { None };
                    ids.push(common::add(&v, &format!("thread {t} note {i}"), "note", project, &[]).id);
                }
                ids
            })
        })
        .collect();
    let ids: Vec<String> = handles.into_iter().flat_map(|h| h.join().unwrap()).collect();
    assert_eq!(ids.len(), threads * per);
    let notes = v.load_notes();
    assert_eq!(notes.len(), threads * per);
    for id in &ids {
        assert!(notes.iter().any(|n| &n.id == id));
    }
    let commits = breadcrumb::git::commit_count(&v.root);
    assert_eq!(commits - base, threads * per);
    assert_eq!(common::git(&v.root, &["status", "--porcelain"]), "", "working tree must be clean");
    assert!(breadcrumb::doctor::check(&v).iter().all(|p| p.severity != "error"));
}

#[test]
fn parallel_processes_share_one_vault() {
    let (_d, v) = common::vault();
    let base = breadcrumb::git::commit_count(&v.root);
    let exe = env!("CARGO_BIN_EXE_breadcrumb");
    let procs = 5;
    let per = 2;
    let children: Vec<_> = (0..procs)
        .map(|p| {
            let root = v.root.clone();
            std::thread::spawn(move || {
                for i in 0..per {
                    let out = Command::new(exe)
                        .args(["--vault"])
                        .arg(&root)
                        .args(["add", "--global", "--title", &format!("process {p} note {i}"), "--kind", "todo", "--field", "priority=p1"])
                        .output()
                        .unwrap();
                    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
                }
            })
        })
        .collect();
    for c in children {
        c.join().unwrap();
    }
    let notes = v.load_notes();
    assert_eq!(notes.len(), procs * per);
    assert_eq!(breadcrumb::git::commit_count(&v.root) - base, procs * per);
    let log = common::git(&v.root, &["log", "--format=%s"]);
    assert_eq!(log.lines().filter(|l| l.starts_with("breadcrumb: add ")).count(), procs * per);
    assert_eq!(common::git(&v.root, &["status", "--porcelain"]), "");
}
