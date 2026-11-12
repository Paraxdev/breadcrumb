mod common;

use breadcrumb::note::Note;
use breadcrumb::vault::Vault;
use chrono::{DateTime, Duration, Utc};
use std::path::{Path, PathBuf};
use std::process::Command;

struct Env {
    _d: tempfile::TempDir,
    v: Vault,
    work: PathBuf,
}

/// A vault and a git checkout bound to project app.
fn setup() -> Env {
    let (d, v) = common::vault();
    let work = d.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    common::git(&work, &["init", "-q"]);
    breadcrumb::ops::project_link(&v, "app", &work, None).unwrap();
    Env { _d: d, v, work }
}

fn days_ago(d: i64) -> DateTime<Utc> {
    breadcrumb::util::now() - Duration::days(d)
}

/// Commits a change to `file` dated `at`, returns the short sha.
fn commit(dir: &Path, file: &str, msg: &str, at: DateTime<Utc>) -> String {
    let p = dir.join(file);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let old = std::fs::read_to_string(&p).unwrap_or_default();
    std::fs::write(&p, format!("{old}{msg}\n")).unwrap();
    common::git(dir, &["add", "-A"]);
    let date = at.to_rfc3339();
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "-m", msg])
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    common::git(dir, &["rev-parse", "--short", "HEAD"])
}

/// A gotcha with `globs`, created, written and verified `age` days ago, with only the `verified` day older binaries write.
fn note(v: &Vault, project: Option<&str>, title: &str, globs: &[&str], age: i64) -> Note {
    let req = breadcrumb::ops::AddReq { title: title.into(), kind: Some("gotcha".into()), project: project.map(Into::into), body: Some(format!("{title} body")), paths: globs.iter().map(|g| g.to_string()).collect(), ..Default::default() };
    let mut n = breadcrumb::ops::add(v, req).unwrap();
    n.created = days_ago(age);
    n.updated = days_ago(age);
    n.set_field("verified", serde_yaml::Value::String(breadcrumb::util::fmt_date(&days_ago(age))));
    n.unset_field("verified_at");
    v.write_note(&mut n).unwrap();
    v.get(&n.id).unwrap()
}

fn tick() {
    std::thread::sleep(std::time::Duration::from_millis(1100));
}

fn today_start() -> DateTime<Utc> {
    breadcrumb::util::now().date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc()
}

/// The rules are about the UTC day, keep clear of midnight so `today` is the day writes stamp.
fn away_from_midnight() {
    if breadcrumb::util::now() - today_start() < Duration::seconds(30) {
        std::thread::sleep(std::time::Duration::from_secs(31));
    }
}

fn crumb(e: &Env, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_crumb")).args(args).current_dir(&e.work).env("BREADCRUMB_HOME", &e.v.root).output().unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn get(e: &Env, id: &str) -> String {
    crumb(e, &["get", id])
}

fn date(age: i64) -> String {
    breadcrumb::util::fmt_date(&days_ago(age))
}

#[test]
fn get_counts_commits_touching_the_globs_since_the_note_was_verified() {
    let e = setup();
    commit(&e.work, "src/engine/a.rs", "base", days_ago(20));
    let n = note(&e.v, Some("app"), "Engine ticks twice", &["src/engine/**"], 10);
    let bare = note(&e.v, Some("app"), "No globs here", &[], 10);
    assert!(!get(&e, &n.id).contains("paths changed"), "0 commits shows nothing: {}", get(&e, &n.id));

    let a = commit(&e.work, "src/engine/a.rs", "engine fix", days_ago(5));
    commit(&e.work, "docs/x.md", "docs only", days_ago(4));
    let line = format!("paths changed: 1 commit since verified {}, newest {a} \"engine fix\"\n", date(10));
    assert!(get(&e, &n.id).ends_with(&line), "{}", get(&e, &n.id));

    let c = commit(&e.work, "src/engine/sub/b.rs", "deeper change", days_ago(3));
    assert!(get(&e, &n.id).ends_with(&format!("paths changed: 2 commits since verified {}, newest {c} \"deeper change\"\n", date(10))), "{}", get(&e, &n.id));
    let json: serde_json::Value = serde_json::from_str(&crumb(&e, &["get", &n.id, "--json"])).unwrap();
    assert!(json["paths_changed"].as_str().unwrap().starts_with("paths changed: 2 commits"), "{json}");
    assert!(!crumb(&e, &["get", &n.id, "--body"]).contains("paths changed"), "the body alone stays clean");
    assert!(!get(&e, &bare.id).contains("paths changed"), "a note without paths gets no line");

    common::git(&e.work, &["checkout", "-q", "--detach"]);
    assert!(get(&e, &n.id).contains("paths changed: 2 commits"), "a detached HEAD counts the same: {}", get(&e, &n.id));

    // A note without `verified` counts from `updated`.
    let mut old = e.v.get(&n.id).unwrap();
    old.unset_field("verified");
    old.updated = days_ago(4);
    e.v.write_note(&mut old).unwrap();
    assert!(get(&e, &n.id).ends_with(&format!("paths changed: 1 commit since updated {}, newest {c} \"deeper change\"\n", date(4))), "{}", get(&e, &n.id));

    let srv = breadcrumb::mcp::Server { vault_root: e.v.root.clone(), cwd: e.work.clone() };
    let mcp = srv.call_tool("get", serde_json::json!({"id": n.id}).as_object().unwrap()).unwrap();
    assert!(mcp.ends_with(&format!("paths changed: 1 commit since updated {}, newest {c} \"deeper change\"\n", date(4))), "{mcp}");
}

#[test]
fn verify_clears_the_line_at_once_and_a_later_commit_brings_it_back() {
    away_from_midnight();
    let e = setup();
    let today = breadcrumb::util::fmt_date(&today_start());
    commit(&e.work, "src/a.rs", "base", days_ago(20));
    let n = note(&e.v, Some("app"), "Parser keeps state", &["src/*.rs"], 10);
    commit(&e.work, "src/a.rs", "parser rewrite", breadcrumb::util::now());
    assert!(get(&e, &n.id).contains("paths changed: 1 commit"), "{}", get(&e, &n.id));
    assert!(crumb(&e, &["doctor"]).contains("paths changed: 1 commit"));

    tick();
    crumb(&e, &["verify", &n.id]);
    let stored = e.v.get(&n.id).unwrap();
    let at = stored.field("verified_at").and_then(breadcrumb::note::yaml_scalar_string).unwrap();
    assert!(at.starts_with(&format!("{today}T")) && at.ends_with('Z') && stored.verified_at().is_some(), "{at}");
    assert!(!get(&e, &n.id).contains("paths changed"), "a verify the same day clears it at once: {}", get(&e, &n.id));
    assert!(!crumb(&e, &["doctor"]).contains("paths changed"));

    tick();
    let later = commit(&e.work, "src/b.rs", "after verify", breadcrumb::util::now());
    let line = format!("paths changed: 1 commit since verified {today}, newest {later} \"after verify\"\n");
    assert!(get(&e, &n.id).ends_with(&line), "{}", get(&e, &n.id));
    assert!(crumb(&e, &["doctor"]).contains(&format!("{}, reread it", line.trim_end())));

    tick();
    crumb(&e, &["update", &n.id, "--body", "Parser keeps state across files"]);
    assert!(!get(&e, &n.id).contains("paths changed"), "an update verifies too: {}", get(&e, &n.id));
    assert!(!crumb(&e, &["doctor"]).contains("paths changed"));
    let refused = Command::new(env!("CARGO_BIN_EXE_crumb")).args(["update", &n.id, "--field", "verified_at=2026-01-01T00:00:00Z"]).current_dir(&e.work).env("BREADCRUMB_HOME", &e.v.root).output().unwrap();
    assert!(!refused.status.success() && String::from_utf8_lossy(&refused.stderr).contains("'verified_at' is set by breadcrumb"));
}

#[test]
fn a_note_with_only_the_verified_day_counts_from_the_start_of_that_day() {
    away_from_midnight();
    let e = setup();
    let start = today_start();
    let today = breadcrumb::util::fmt_date(&start);
    commit(&e.work, "src/a.rs", "base", days_ago(20));
    let mut n = note(&e.v, Some("app"), "Parser keeps state", &["src/*.rs"], 10);
    n.set_field("verified", serde_yaml::Value::String(today.clone()));
    e.v.write_note(&mut n).unwrap();
    commit(&e.work, "src/a.rs", "parser rewrite", days_ago(5));
    assert!(!get(&e, &n.id).contains("paths changed"), "{}", get(&e, &n.id));

    // A day has no time to tell a commit made earlier that day apart from one made after the verify, so it counts.
    let early = commit(&e.work, "src/b.rs", "early today", start + Duration::seconds(1));
    let line = format!("paths changed: 1 commit since verified {today}, newest {early} \"early today\"\n");
    assert!(get(&e, &n.id).ends_with(&line), "{}", get(&e, &n.id));

    // A `verified_at` on another day than `verified`, from a hand edit or an older binary's write, is not used.
    n.set_field("verified_at", serde_yaml::Value::String(breadcrumb::util::fmt_time(&breadcrumb::util::now())));
    n.set_field("verified", serde_yaml::Value::String(date(3)));
    e.v.write_note(&mut n).unwrap();
    assert!(get(&e, &n.id).contains(&format!("paths changed: 1 commit since verified {}, newest {early}", date(3))), "{}", get(&e, &n.id));

    // A note written today never counts commits from before it existed.
    tick();
    let fresh = breadcrumb::ops::add(&e.v, breadcrumb::ops::AddReq { title: "Fresh parser note".into(), kind: Some("gotcha".into()), project: Some("app".into()), paths: vec!["src/*.rs".into()], ..Default::default() }).unwrap();
    assert!(!get(&e, &fresh.id).contains("paths changed"), "{}", get(&e, &fresh.id));
}

#[test]
fn nothing_shows_without_a_checkout_commits_or_a_project() {
    let e = setup();
    let n = note(&e.v, Some("app"), "Engine ticks twice", &["src/**"], 10);
    assert!(!get(&e, &n.id).contains("paths changed"), "a repo without commits counts nothing");
    commit(&e.work, "src/a.rs", "change", days_ago(2));
    assert!(get(&e, &n.id).contains("paths changed: 1 commit"), "{}", get(&e, &n.id));
    let global = note(&e.v, None, "Global engine note", &["src/**"], 10);
    assert!(!get(&e, &global.id).contains("paths changed"), "a global note has no checkout");
    std::fs::rename(e.work.join(".git"), e.work.join("git-off")).unwrap();
    assert!(!get(&e, &n.id).contains("paths changed"), "no checkout, no line");
}

#[test]
fn globs_mean_the_same_to_git_as_to_the_file_hook() {
    let e = setup();
    commit(&e.work, "README.md", "base", days_ago(20));
    let cases: [(&str, &str, bool); 9] = [
        ("src/**/mod.rs", "src/ui/deep/mod.rs", true),
        ("src/**/mod.rs", "src/mod.rs", true),
        ("Cargo.toml", "sub/Cargo.toml", true),
        ("src/engine", "src/engine/a.rs", true),
        ("src/engine/", "src/engine/x/y.rs", true),
        ("DOCS/*.md", "docs/Guide.md", true),
        ("src/*.rs", "src/engine/a.rs", false),
        ("*.hxx", "include/deep/a.hxx", true),
        ("src/engine", "src/engineer.rs", false),
    ];
    let notes: Vec<Note> = cases.iter().enumerate().map(|(i, (g, _, _))| note(&e.v, Some("app"), &format!("glob case {i}"), &[g], 10)).collect();
    // Written by hand, `add` would turn the backslashes into slashes.
    let mut win = note(&e.v, Some("app"), "Windows style glob", &[], 10);
    win.set_field("paths", serde_yaml::Value::Sequence(vec![serde_yaml::Value::String(r"src\engine\**".into())]));
    e.v.write_note(&mut win).unwrap();
    for (i, (g, file, hit)) in cases.iter().enumerate() {
        assert_eq!(breadcrumb::recall::glob_match(g, file), *hit, "the file hook reads {g} on {file} differently");
        let sha = commit(&e.work, file, &format!("touch {i}"), days_ago(5) + Duration::minutes(i as i64));
        let out = get(&e, &notes[i].id);
        assert_eq!(out.contains(&format!("newest {sha} \"touch {i}\"")), *hit, "{g} on {file}: {out}");
    }
    let out = get(&e, &win.id);
    assert!(out.contains("paths changed: 3 commits") && out.contains("\"touch 6\""), "{out}");
}

#[test]
fn doctor_warns_for_the_scoped_project_only() {
    let e = setup();
    commit(&e.work, "src/a.rs", "base", days_ago(20));
    commit(&e.work, "docs/x.md", "docs base", days_ago(19));
    let n = note(&e.v, Some("app"), "Engine ticks twice", &["src/**"], 10);
    let quiet = note(&e.v, Some("app"), "Docs are generated", &["docs/**"], 10);
    let mut gone = note(&e.v, Some("app"), "Archived engine note", &["src/**"], 10);
    gone.status = "archived".into();
    e.v.write_note(&mut gone).unwrap();
    let sha = commit(&e.work, "src/a.rs", "engine fix", days_ago(2));

    let other_dir = e.work.parent().unwrap().join("other");
    std::fs::create_dir_all(&other_dir).unwrap();
    common::git(&other_dir, &["init", "-q"]);
    breadcrumb::ops::project_link(&e.v, "other", &other_dir, None).unwrap();
    let o = note(&e.v, Some("other"), "Other project note", &["lib/**"], 10);
    commit(&other_dir, "lib/x.rs", "other change", days_ago(2));

    let d = crumb(&e, &["doctor"]);
    let path = e.v.rel(e.v.get(&n.id).unwrap().path.as_ref().unwrap());
    let want = format!("warning: {path}: paths changed: 1 commit since verified {}, newest {sha} \"engine fix\", reread it, then `breadcrumb verify {}` or update the note\n", date(10), n.id);
    assert!(d.contains(&want), "{d}");
    assert!(!d.contains(&quiet.id) && !d.contains(&gone.id) && !d.contains(&o.id), "only notes of app with changed paths: {d}");
    assert!(crumb(&e, &["doctor", "-p", "other"]).contains(&format!("verify {}`", o.id)));
    assert!(!crumb(&e, &["doctor", "--all"]).contains("paths changed"), "--all checks no checkout");

    let srv = breadcrumb::mcp::Server { vault_root: e.v.root.clone(), cwd: e.work.clone() };
    let mcp = srv.call_tool("doctor", serde_json::json!({}).as_object().unwrap()).unwrap();
    assert!(mcp.contains(&want), "{mcp}");

    let skipped = breadcrumb::drift::doctor_problems(&e.v, "app", &e.work, std::time::Duration::ZERO);
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].message, "paths check stopped after 0s, 2 notes with paths not checked, `breadcrumb get <id>` checks one");
    assert_eq!(skipped[0].severity, "info");
}

#[test]
fn brief_marks_the_notes_it_shows() {
    let e = setup();
    commit(&e.work, "src/view/overlay.ts", "base", days_ago(20));
    let n = note(&e.v, Some("app"), "Overlay needs a redraw", &["src/view/**"], 10);
    let still = note(&e.v, Some("app"), "Overlay colours", &["overlay.ts"], 1);
    commit(&e.work, "src/view/overlay.ts", "overlay rework", days_ago(3));
    commit(&e.work, "src/view/overlay.ts", "overlay again", days_ago(2));
    let b = breadcrumb::map::brief(&e.v, "app", &["src/view/overlay.ts".into()], Some(&e.work), breadcrumb::map::BRIEF_BUDGET).unwrap();
    assert!(b.contains(&format!("gotcha {} Overlay needs a redraw [2 commits since verified]: Overlay needs a redraw body\n", n.id)), "{b}");
    assert!(b.contains(&format!("gotcha {} Overlay colours: Overlay colours body\n", still.id)), "{b}");
    let none = breadcrumb::map::brief(&e.v, "app", &["src/view/overlay.ts".into()], None, breadcrumb::map::BRIEF_BUDGET).unwrap();
    assert!(!none.contains("since verified"), "{none}");
}
