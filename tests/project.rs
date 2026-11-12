mod common;

use breadcrumb::ops::{self, Scope, ScopeSel};
use breadcrumb::project::{Project, detect, normalize_remote};
use std::path::Path;

#[test]
fn remote_normalization() {
    let want = "github.com/Paraxdev/Breadcrumb";
    for u in [
        "git@github.com:Paraxdev/Breadcrumb.git",
        "https://github.com/Paraxdev/Breadcrumb",
        "https://user:tok@GitHub.com/Paraxdev/Breadcrumb.git",
        "ssh://git@github.com/Paraxdev/Breadcrumb.git",
        "https://github.com/Paraxdev/Breadcrumb/",
        "git://github.com/Paraxdev/Breadcrumb.git",
    ] {
        assert_eq!(normalize_remote(u), want, "{u}");
    }
    assert_eq!(normalize_remote("C:/repos/thing.git"), "C:/repos/thing");
}

fn project(slug: &str, paths: &[&str], remotes: &[&str]) -> Project {
    let mut p = Project::new(slug, None);
    p.paths = paths.iter().map(|s| s.to_string()).collect();
    p.remotes = remotes.iter().map(|s| s.to_string()).collect();
    p
}

#[test]
fn detection_by_remote_then_longest_path_prefix() {
    let ps = vec![
        project("outer", &["/work/repo"], &[]),
        project("inner", &["/work/repo/sub"], &[]),
        project("byremote", &[], &["github.com/o/r"]),
    ];
    assert_eq!(detect(&ps, &["git@github.com:o/r.git".into()], Path::new("/elsewhere")).as_deref(), Some("byremote"));
    assert_eq!(detect(&ps, &[], Path::new("/work/repo/sub/deep")).as_deref(), Some("inner"));
    assert_eq!(detect(&ps, &[], Path::new("/work/repo/other")).as_deref(), Some("outer"));
    assert_eq!(detect(&ps, &[], Path::new("/work/repo")).as_deref(), Some("outer"));
    assert_eq!(detect(&ps, &[], Path::new("/work/repository")), None, "prefix must end at a path separator");
    assert_eq!(detect(&ps, &["https://github.com/x/y".into()], Path::new("/nowhere")), None);
}

#[cfg(windows)]
#[test]
fn windows_paths_are_case_and_separator_insensitive() {
    let ps = vec![project("para", &["D:/dev/webapp"], &[])];
    assert_eq!(detect(&ps, &[], Path::new(r"d:\Dev\WebApp\src")).as_deref(), Some("para"));
    assert_eq!(breadcrumb::project::normalize_path(r"\\?\D:\Dev\"), "d:/dev");
}

#[test]
fn project_link_binds_path_and_remote_and_scopes_follow() {
    let (d, v) = common::vault();
    let repo = d.path().join("code").join("my-app");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    common::git(&repo, &["init", "-q"]);
    common::git(&repo, &["remote", "add", "origin", "git@github.com:me/my-app.git"]);
    let r = ops::project_link(&v, "my-app", &repo, Some("The app")).unwrap();
    assert!(r.created);
    assert_eq!(r.added_remotes, vec!["github.com/me/my-app"]);
    let again = ops::project_link(&v, "my-app", &repo, None).unwrap();
    assert!(!again.created && again.added_path.is_none() && again.added_remotes.is_empty());

    assert_eq!(v.detect_project(&repo.join("src")).as_deref(), Some("my-app"));
    assert_eq!(v.detect_project(d.path()), None);

    let clone = d.path().join("other-pc-path");
    std::fs::create_dir_all(&clone).unwrap();
    common::git(&clone, &["init", "-q"]);
    common::git(&clone, &["remote", "add", "origin", "https://github.com/me/my-app"]);
    assert_eq!(v.detect_project(&clone).as_deref(), Some("my-app"), "remote match works from a different path");

    let scope = ops::resolve_scope(&v, &ScopeSel::default(), &repo).unwrap();
    assert_eq!(scope, Scope::Project { slug: "my-app".into(), with_global: true });
    assert_eq!(ops::resolve_scope(&v, &ScopeSel { global: true, ..Default::default() }, &repo).unwrap(), Scope::Global);
    assert!(ops::resolve_scope(&v, &ScopeSel { project: Some("nope".into()), ..Default::default() }, &repo).is_err());

    let target = ops::target_project(&v, None, false, &repo.join("src")).unwrap();
    assert_eq!(target.as_deref(), Some("my-app"));
    let n = common::add(&v, "scoped", "note", target.as_deref(), &[]);
    assert!(n.path.unwrap().starts_with(v.root.join("projects").join("my-app").join("notes")));
    assert!(ops::target_project(&v, Some("Bad Slug"), false, &repo).is_err());
}

fn crumb(v: &breadcrumb::vault::Vault, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_crumb")).args(args).env("BREADCRUMB_HOME", &v.root).env("CLAUDE_CODE_SESSION_ID", "").env("BREADCRUMB_SESSION", "").output().unwrap()
}

#[test]
fn linking_a_directory_another_project_holds_is_refused_and_move_takes_it() {
    let (d, v) = common::vault();
    let dir = d.path().join("shared");
    let child = dir.join("sub");
    std::fs::create_dir_all(&child).unwrap();
    ops::project_link(&v, "first", &dir, None).unwrap();

    let e = ops::project_link(&v, "second", &dir, None).unwrap_err().to_string();
    assert!(e.contains("project first") && e.contains("project link second --path") && e.contains("--move"), "{e}");
    assert!(v.load_project("second").unwrap().is_none(), "a refusal creates nothing");
    assert_eq!(v.load_project("first").unwrap().unwrap().paths.len(), 1);

    let again = ops::project_link(&v, "first", &dir, None).unwrap();
    assert!(again.added_path.is_none() && again.moved_from.is_empty(), "the holder linking it again is a no-op");

    ops::project_link(&v, "second", &child, None).unwrap();
    assert_eq!(v.detect_project(&child).as_deref(), Some("second"), "a child directory stays legal and wins by the longest path");
    assert_eq!(v.detect_project(&dir).as_deref(), Some("first"));

    let before = common::git(&v.root, &["rev-list", "--count", "HEAD"]).trim().parse::<u32>().unwrap();
    let out = crumb(&v, &["project", "link", "second", "--path", dir.to_str().unwrap()]);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("--move"));
    let out = crumb(&v, &["project", "link", "second", "--path", dir.to_str().unwrap(), "--move"]);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success() && text.starts_with("linked project second") && text.contains("moved the path from project first"), "{text}{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(common::git(&v.root, &["rev-list", "--count", "HEAD"]).trim().parse::<u32>().unwrap(), before + 1, "one commit");
    assert!(v.load_project("first").unwrap().unwrap().paths.is_empty());
    assert_eq!(v.detect_project(&dir).as_deref(), Some("second"));
}

#[test]
fn the_project_tool_refusal_names_the_cli_move() {
    let (d, v) = common::vault();
    let srv = breadcrumb::mcp::Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let dir = d.path().join("shared");
    std::fs::create_dir_all(&dir).unwrap();
    ops::project_link(&v, "first", &dir, None).unwrap();
    let args = serde_json::json!({"action": "link", "slug": "second", "path": dir.to_str().unwrap()}).as_object().unwrap().clone();
    let e = srv.call_tool("project", &args).err().unwrap().to_string();
    assert!(e.contains("project first") && e.contains("breadcrumb project link second --path") && e.contains("--move"), "{e}");
}
