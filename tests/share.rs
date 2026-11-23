mod common;

use breadcrumb::note::Note;
use breadcrumb::ops;
use breadcrumb::share::{self, PushOpts};
use breadcrumb::vault::Vault;
use std::path::{Path, PathBuf};

fn repo(dir: &Path) -> PathBuf {
    let r = dir.join("repo");
    std::fs::create_dir_all(&r).unwrap();
    common::git(&r, &["init", "-q"]);
    common::git(&r, &["remote", "add", "origin", "https://github.com/example/shared.git"]);
    r
}

fn second_vault(dir: &Path) -> Vault {
    Vault::init(&dir.join("vault2"), None).unwrap().0
}

fn repo_files(r: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(r.join(".breadcrumb/notes")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    v.sort();
    v
}

fn repo_note(r: &Path, id: &str) -> (PathBuf, Note) {
    let name = repo_files(r).into_iter().find(|f| f.starts_with(id)).unwrap();
    let p = r.join(".breadcrumb/notes").join(&name);
    let n = Note::parse(&std::fs::read_to_string(&p).unwrap(), Some(&name)).unwrap();
    (p, n)
}

#[test]
fn init_pushes_shared_kinds_only_and_a_second_vault_pulls_them() {
    let (dir, v) = common::vault();
    let r = repo(dir.path());
    ops::project_link(&v, "shared", &r, Some("a shared repo")).unwrap();
    let task = common::add(&v, "Login times out", "task", Some("shared"), &[]);
    let dec = common::add(&v, "Use sqlite for the cache", "decision", Some("shared"), &[]);
    let fb = common::add(&v, "Keep replies short", "feedback", Some("shared"), &[]);
    let private = common::add(&v, "Scratch idea", "note", Some("shared"), &[("share", "false")]);
    let forced = common::add(&v, "Env of the CI box", "env", Some("shared"), &[("share", "true")]);
    let secret = common::add(&v, "Deploy key", "note", Some("shared"), &[("hint", "ghp_abcdefghijklmnopqrstuvwxyz0123456789")]);

    let rep = share::init(&v, None, Some(&r), "sync", &r).unwrap();
    assert!(rep.created);
    assert!(r.join(".breadcrumb/project.toml").is_file());
    assert!(r.join(".breadcrumb/README.md").is_file());
    let files = repo_files(&r).join(" ");
    for n in [&task, &dec, &forced] {
        assert!(files.contains(&n.id), "{} missing from {files}", n.title);
    }
    for n in [&fb, &private, &secret] {
        assert!(!files.contains(&n.id), "{} should stay in the vault", n.title);
    }
    assert!(rep.push.skipped.iter().any(|(id, why)| *id == secret.id && why.contains("GitHub token")));

    let v2 = second_vault(dir.path());
    let pulled = share::pull(&v2, &r, false).unwrap();
    assert!(pulled.bound);
    assert_eq!(pulled.project, "shared");
    assert_eq!(pulled.added.len(), 3);
    assert_eq!(v2.detect_project(&r).as_deref(), Some("shared"));
    assert_eq!(v2.get(&task.id).unwrap().title, "Login times out");
    assert_eq!(v2.get(&task.id).unwrap().project.as_deref(), Some("shared"));

    let again = share::pull(&v2, &r, false).unwrap();
    assert_eq!(again.changed(), 0);
    assert_eq!(again.unchanged, 3);
    let back = share::push(&v2, &r, &PushOpts::default()).unwrap();
    assert_eq!(back.changed(), 0, "{}", back.text());
}

#[test]
fn the_newer_copy_wins_both_ways() {
    let (dir, v) = common::vault();
    let r = repo(dir.path());
    ops::project_link(&v, "shared", &r, None).unwrap();
    let task = common::add(&v, "Login times out", "task", Some("shared"), &[]);
    share::init(&v, None, Some(&r), "sync", &r).unwrap();

    let v2 = second_vault(dir.path());
    share::pull(&v2, &r, false).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    ops::append(&v2, &task.id, "Reproduced on a fresh profile").unwrap();
    let pushed = share::push(&v2, &r, &PushOpts::default()).unwrap();
    assert_eq!(pushed.updated.len(), 1);
    assert!(repo_note(&r, &task.id).1.body.contains("fresh profile"));

    let stale = share::push(&v, &r, &PushOpts::default()).unwrap();
    assert_eq!(stale.updated.len(), 0);
    assert_eq!(stale.newer_there.len(), 1);
    assert!(repo_note(&r, &task.id).1.body.contains("fresh profile"), "an older vault copy must not overwrite the repo");

    let pulled = share::pull(&v, &r, false).unwrap();
    assert_eq!(pulled.updated.len(), 1);
    assert!(v.get(&task.id).unwrap().body.contains("fresh profile"));
}

#[test]
fn prune_removes_notes_no_longer_shared_and_status_changes_nothing() {
    let (dir, v) = common::vault();
    let r = repo(dir.path());
    ops::project_link(&v, "shared", &r, None).unwrap();
    let a = common::add(&v, "First task", "task", Some("shared"), &[]);
    let b = common::add(&v, "Second task", "task", Some("shared"), &[]);
    share::init(&v, None, Some(&r), "sync", &r).unwrap();
    assert_eq!(repo_files(&r).len(), 2);

    let path = v.get(&b.id).unwrap().path.unwrap();
    let text = std::fs::read_to_string(&path).unwrap().replacen("---\n", "---\nshare: false\n", 1);
    std::fs::write(&path, text).unwrap();
    let dry = share::push(&v, &r, &PushOpts { dry_run: true, prune: true, ..Default::default() }).unwrap();
    assert_eq!(dry.removed.len(), 1);
    assert_eq!(repo_files(&r).len(), 2);
    let done = share::push(&v, &r, &PushOpts { prune: true, ..Default::default() }).unwrap();
    assert_eq!(done.removed.len(), 1);
    let left = repo_files(&r);
    assert_eq!(left.len(), 1);
    assert!(left[0].starts_with(&a.id));
}

#[test]
fn find_repo_stops_at_the_git_root() {
    let (dir, v) = common::vault();
    let r = repo(dir.path());
    ops::project_link(&v, "shared", &r, None).unwrap();
    share::init(&v, None, Some(&r), "pull", &r).unwrap();
    let deep = r.join("src/a");
    std::fs::create_dir_all(&deep).unwrap();
    assert_eq!(share::find_repo(&deep).as_deref(), Some(r.as_path()));
    let nested = r.join("vendor/other");
    std::fs::create_dir_all(nested.join(".git")).unwrap();
    assert_eq!(share::find_repo(&nested), None);
    assert_eq!(share::load_manifest(&r).unwrap().auto, "pull");
}

#[test]
fn auto_pull_brings_notes_in_and_names_them_for_the_digest() {
    let (dir, v) = common::vault();
    let r = repo(dir.path());
    ops::project_link(&v, "shared", &r, None).unwrap();
    common::add(&v, "Login times out", "task", Some("shared"), &[]);
    share::init(&v, None, Some(&r), "sync", &r).unwrap();
    let v2 = second_vault(dir.path());
    let line = share::auto_pull(&v2.root, &r.join("src")).unwrap_or_default();
    assert!(line.contains("pulled 1 new"), "{line}");
    assert!(line.contains("bound this repo to project shared"), "{line}");
    assert_eq!(share::auto_pull(&v2.root, &r), None);
}

#[test]
fn secrets_are_recognised() {
    for s in [
        "token ghp_abcdefghijklmnopqrstuvwxyz0123456789",
        "key sk-ant-abcdefghijklmnopqrstuvwxyz",
        "AKIAABCDEFGHIJKLMNOP",
        "-----BEGIN OPENSSH PRIVATE KEY-----",
        "password: hunter2hunter2",
        "postgres://app:s3cret@db.internal/app",
    ] {
        assert!(share::secret_in(s).is_some(), "{s}");
    }
    for s in ["the login token expires after 30s", "https://github.com/example/shared", "set password_min to 8"] {
        assert!(share::secret_in(s).is_none(), "{s}");
    }
}
