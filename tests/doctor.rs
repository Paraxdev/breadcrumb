mod common;

use breadcrumb::ops::{self, UpdateReq};
use breadcrumb::vault::Vault;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

fn doctor(v: &Vault, cwd: &Path, args: &[&str]) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_breadcrumb"))
        .arg("--vault")
        .arg(&v.root)
        .arg("doctor")
        .args(args)
        .current_dir(cwd)
        .env_remove("BREADCRUMB_HOME")
        .env("BREADCRUMB_CLAUDE_DIR", v.root.with_extension("no-claude"))
        .output()
        .unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string())
}

fn checkout(d: &Path, name: &str, branch: &str) -> PathBuf {
    let dir = d.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    common::git(&dir, &["init", "-q", "-b", branch]);
    dir
}

fn body(v: &Vault, id: &str, text: &str) {
    ops::update(v, id, UpdateReq { body: Some(text.into()), ..Default::default() }).unwrap();
}

/// alpha and beta each hold a note with five missing paths, plus a global note and a note linked to nothing.
fn two_projects() -> (tempfile::TempDir, Vault, PathBuf, PathBuf) {
    let (d, v) = common::vault();
    let (alpha, beta) = (checkout(d.path(), "alpha", "main"), checkout(d.path(), "beta", "dev"));
    ops::project_link(&v, "alpha", &alpha, None).unwrap();
    ops::project_link(&v, "beta", &beta, None).unwrap();
    let paths = "`a/x.rs` `a/y.rs` `a/z.rs` `a/w.rs` `a/v.rs`";
    let n = common::add(&v, "Alpha paths", "note", Some("alpha"), &[]);
    body(&v, &n.id, paths);
    let n = common::add(&v, "Beta paths", "note", Some("beta"), &[]);
    body(&v, &n.id, paths);
    common::add(&v, "Global fact", "note", None, &[]);
    (d, v, alpha, beta)
}

#[test]
fn missing_paths_fold_to_one_line_naming_the_checkout_and_its_branch() {
    let (_d, v, alpha, _) = two_projects();
    let (_, out) = doctor(&v, &alpha, &[]);
    let line = out.lines().find(|l| l.contains("missing from")).unwrap_or_else(|| panic!("{out}"));
    assert!(line.starts_with("warning: ") && line.contains("mentions 5 paths missing from ") && line.ends_with(" (on main): a/x.rs, a/y.rs, a/z.rs (+2)"), "{line}");
    assert_eq!(out.matches("missing from").count(), 1, "{out}");
    let first = out.lines().next().unwrap();
    assert!(first.contains(": 0 errors, 1 warning"), "one folded line is one warning: {first}");
}

#[test]
fn doctor_reports_on_the_detected_project_and_global_and_all_checks_the_rest() {
    let (d, v, alpha, _) = two_projects();
    let (_, out) = doctor(&v, &alpha, &[]);
    let first = out.lines().next().unwrap();
    assert!(first.starts_with("checked 2 notes of alpha and global: 0 errors, 1 warning, `--all` checks the rest") && first.ends_with(", `--all` checks the rest"), "{first}");
    assert!(out.contains("projects/alpha/"), "{out}");
    assert!(!out.contains("projects/beta/"), "{out}");

    let (_, all) = doctor(&v, &alpha, &["--all"]);
    assert!(all.lines().next().unwrap().starts_with("checked 3 notes: 0 errors, 2 warnings") && !all.lines().next().unwrap().contains("checks the rest"), "{all}");
    assert!(all.contains("projects/beta/") && all.contains("(on dev)"), "{all}");

    let (_, beta) = doctor(&v, &alpha, &["-p", "beta"]);
    assert!(beta.starts_with("checked 2 notes of beta and global") && beta.contains("projects/beta/") && !beta.contains("projects/alpha/"), "{beta}");

    let (ok, _) = doctor(&v, &alpha, &["--project", "nope"]);
    assert!(!ok, "an unknown project is refused");

    let (_, everything) = doctor(&v, d.path(), &[]);
    assert!(everything.starts_with("checked 3 notes: 0 errors, 2 warnings"), "no detected project and no flag checks everything: {everything}");

    let (_, global) = doctor(&v, &alpha, &["--global"]);
    assert!(global.starts_with("checked 1 note of global: "), "{global}");
    assert_eq!(doctor(&v, &alpha, &["-p", "global"]).1, global, "-p global is --global");
    assert_eq!(doctor(&v, &alpha, &["-p", "all"]).1, all, "-p all is --all");
}

#[test]
fn checks_that_need_every_note_still_see_all_of_them() {
    let (_d, v, alpha, _) = two_projects();
    let other = common::add(&v, "Beta target", "note", Some("beta"), &[]);
    let mine = common::add(&v, "Alpha source", "note", Some("alpha"), &[]);
    ops::link(&v, &mine.id, &other.id, Some("relates"), false).unwrap();
    let stray = common::add(&v, "Beta stray", "note", Some("beta"), &[]);
    let mut n = v.get(&stray.id).unwrap();
    n.links.push(breadcrumb::note::Link { to: "zzzzzzzz".into(), rel: "relates".into() });
    v.write_note(&mut n).unwrap();
    let (ok, out) = doctor(&v, &alpha, &[]);
    assert!(ok && !out.contains("broken link"), "a link to a note of another project is fine and a broken one of another project is not reported: {out}");
    let (ok, all) = doctor(&v, &alpha, &["--all"]);
    assert!(!ok && all.contains("broken link to 'zzzzzzzz'"), "{all}");
}

#[test]
fn severity_shows_that_level_and_worse_and_the_summary_counts_everything() {
    let (_d, v, alpha, _) = two_projects();
    let old = common::add(&v, "Old fact", "note", Some("alpha"), &[]);
    let mut n = v.get(&old.id).unwrap();
    n.links.push(breadcrumb::note::Link { to: "zzzzzzzz".into(), rel: "relates".into() });
    v.write_note(&mut n).unwrap();
    let pinned = common::add(&v, "Pinned decision", "decision", Some("alpha"), &[]);
    ops::update(&v, &pinned.id, UpdateReq { pinned: Some(true), body: Some("Plan.\n\n2026-10-21: done.".into()), ..Default::default() }).unwrap();
    let (_, all) = doctor(&v, &alpha, &[]);
    assert!(all.contains("\ninfo: ") && all.contains("\nwarning: ") && all.contains("\nerror: "), "{all}");
    let (_, warn) = doctor(&v, &alpha, &["--severity", "warning"]);
    assert!(warn.contains("\nwarning: ") && warn.contains("\nerror: ") && !warn.contains("\ninfo: "), "{warn}");
    assert_eq!(warn.lines().next(), all.lines().next(), "the summary counts everything");
    let (_, err) = doctor(&v, &alpha, &["--severity", "error"]);
    assert!(err.contains("\nerror: ") && !err.contains("\nwarning: "), "{err}");
    assert!(!doctor(&v, &alpha, &["--severity", "loud"]).0);
}

#[test]
fn json_keeps_the_list_of_problems_with_the_folding_and_filter_applied() {
    let (_d, v, alpha, _) = two_projects();
    let o = Command::new(env!("CARGO_BIN_EXE_breadcrumb")).arg("--vault").arg(&v.root).args(["--json", "doctor", "--severity", "warning"]).current_dir(&alpha).env("BREADCRUMB_CLAUDE_DIR", v.root.with_extension("no-claude")).output().unwrap();
    let j: Value = serde_json::from_slice(&o.stdout).unwrap();
    let problems = j["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1, "{j}");
    assert!(problems[0]["message"].as_str().unwrap().contains("mentions 5 paths missing from") && problems[0]["severity"] == "warning");
    assert_eq!(j["notes"], 2);
}

#[test]
fn the_mcp_doctor_takes_project_and_severity() {
    let (_d, v, alpha, _) = two_projects();
    let srv = breadcrumb::mcp::Server { vault_root: v.root.clone(), cwd: alpha };
    let call = |args: &[(&str, &str)]| {
        let mut a = Map::new();
        for (k, x) in args {
            a.insert(k.to_string(), Value::String(x.to_string()));
        }
        srv.call_tool("doctor", &a).unwrap()
    };
    let scoped = call(&[]);
    assert!(scoped.starts_with("checked 2 notes of alpha and global: ") && scoped.lines().next().unwrap().ends_with(", `project: all` checks the rest") && !scoped.contains("projects/beta/"), "{scoped}");
    let all = call(&[("project", "all")]);
    assert!(all.starts_with("checked 3 notes: ") && all.contains("projects/beta/"), "{all}");
    assert!(call(&[("project", "beta"), ("severity", "error")]).lines().count() == 1);
    let mut a = Map::new();
    a.insert("severity".into(), Value::String("loud".into()));
    assert!(srv.call_tool("doctor", &a).is_err());
}

#[test]
fn doctor_fix_still_covers_the_whole_vault() {
    let (_d, v, alpha, _) = two_projects();
    let o = Command::new(env!("CARGO_BIN_EXE_breadcrumb")).arg("--vault").arg(&v.root).args(["doctor", "--fix"]).current_dir(&alpha).output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(o.status.success() && out.contains("dry run"), "{out}");
    assert!(out.matches("a/x.rs").count() >= 2, "alpha's and beta's note are both planned: {out}");
}

#[test]
fn doctor_reports_settings_overrides_that_do_not_hold_and_a_scope_slug() {
    let (_d, v) = common::vault();
    for slug in ["app", "web", "all"] {
        v.write_project(&breadcrumb::project::Project::new(slug, None)).unwrap();
    }
    std::fs::write(v.root.join("projects/app/settings.toml"), "stale_days = -3\nreview_by = \"robot\"\ntypo_key = 1\ndefault_kind = \"gone\"\n").unwrap();
    std::fs::write(v.root.join("projects/web/settings.toml"), "stale_days = [").unwrap();
    let ps = breadcrumb::doctor::check_with(&v, None);
    let at = |path: &str| ps.iter().filter(|p| p.path == path).map(|p| format!("{}: {}", p.severity, p.message)).collect::<Vec<_>>();
    let app = at("projects/app/settings.toml");
    assert!(app.contains(&"warning: stale_days = -3 is ignored, stale_days takes a whole number of 0 or more".to_string()), "{app:?}");
    assert!(app.contains(&"warning: review_by = \"robot\" is ignored, review_by is one of user, agent".to_string()), "{app:?}");
    assert!(app.contains(&"warning: typo_key = 1 is ignored, typo_key is not a setting".to_string()), "{app:?}");
    assert!(app.contains(&"error: default_kind = \"gone\" names no kind, so add without a kind fails in app".to_string()), "{app:?}");
    let web = at("projects/web/settings.toml");
    assert!(web.len() == 1 && web[0].starts_with("error: not valid TOML (") && web[0].ends_with("), none of its overrides hold, fix it or delete it"), "{web:?}");
    let all = at("projects/all/project.toml");
    assert!(all.len() == 1 && all[0].contains("project slug 'all' names a scope, so -p all never reaches its notes"), "{all:?}");
}
