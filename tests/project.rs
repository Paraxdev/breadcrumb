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
