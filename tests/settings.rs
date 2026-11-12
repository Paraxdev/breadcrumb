mod common;

use breadcrumb::backlog::{self, MoveReq, ReviewBy};
use breadcrumb::config;
use breadcrumb::mcp::Server;
use breadcrumb::ops::{self, AddReq};
use breadcrumb::vault::Vault;
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn crumb_in(v: &Vault, cwd: &Path, args: &[&str], stdin: &str) -> (bool, String, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_crumb"));
    c.args(args).current_dir(cwd).env("BREADCRUMB_HOME", &v.root).env("BREADCRUMB_CLAUDE_DIR", v.root.with_extension("no-claude"));
    c.env_remove("CLAUDE_CODE_SESSION_ID").env_remove("BREADCRUMB_SESSION");
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let o = child.wait_with_output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

/// A vault with project `app` linked to <tmp>/app and project `web`, both with a src dir.
fn setup() -> (tempfile::TempDir, Vault, PathBuf) {
    let (d, v) = common::vault();
    let app = d.path().join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::create_dir_all(d.path().join("web")).unwrap();
    ops::project_link(&v, "app", &app, None).unwrap();
    ops::project_link(&v, "web", &d.path().join("web"), None).unwrap();
    (d, v, app)
}

fn mcp(v: &Vault, cwd: &Path, args: Value) -> (String, bool) {
    let srv = Server { vault_root: v.root.clone(), cwd: cwd.to_path_buf() };
    let mut a: Map<String, Value> = args.as_object().unwrap().clone();
    a.insert("action".into(), json!("config"));
    let req = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "project", "arguments": a}});
    let resp = srv.handle_message(req).unwrap();
    (resp["result"]["content"][0]["text"].as_str().unwrap().to_string(), resp["result"]["isError"] == true)
}

#[test]
fn a_project_overrides_a_setting_and_the_vault_value_stays() {
    let (d, v, app) = setup();
    let home = d.path();
    let (ok, out, err) = crumb_in(&v, home, &["config", "set", "review_by", "agent", "--project", "app"], "");
    assert!(ok, "{err}");
    assert_eq!(out, "review_by in app: user -> agent\n");
    assert!(crumb_in(&v, home, &["config", "set", "stale_days", "20", "-p", "app"], "").0);
    assert!(v.root.join("projects/app/settings.toml").exists());
    assert!(!std::fs::read_to_string(v.root.join("projects/app/project.toml")).unwrap().contains("review_by"), "project.toml stays as old binaries write it");
    assert_eq!(Vault::open(&v.root).unwrap().config.review_by, "user", "the vault value is untouched");

    assert_eq!(crumb_in(&v, home, &["config", "get", "review_by", "--project", "app"], "").1, "agent\n");
    assert_eq!(crumb_in(&v, home, &["config", "get", "review_by", "--project", "web"], "").1, "user\n");
    assert_eq!(crumb_in(&v, &app, &["config", "get", "review_by"], "").1, "user\n", "without --project the vault value, even inside a project");

    assert!(crumb_in(&v, home, &["config", "set", "pin_review_days", "7"], "").0);
    let (_, out, _) = crumb_in(&v, home, &["config", "--project", "app"], "");
    assert!(out.starts_with("Settings for project app, `breadcrumb config set <key> <value> --project app` overrides one"), "{out}");
    assert!(out.contains("  review_by = agent (project, default user)\n"), "{out}");
    assert!(out.contains("  stale_days = 20 (project, default 60)\n"), "{out}");
    assert!(out.contains("  pin_review_days = 7 (vault, default 30)\n"), "{out}");
    assert!(out.contains("  judge_nudge = off\n"), "{out}");
    assert!(out.contains("  judge_project = breadcrumb (vault only)\n"), "{out}");

    let (_, out, _) = crumb_in(&v, home, &["config"], "");
    assert!(out.lines().next().unwrap().contains("a project overrides any not marked vault only with `--project <slug>`"), "{out}");
    assert!(out.contains("  review_by = user\n") && !out.contains("detected here"), "{out}");
    let (_, out, _) = crumb_in(&v, &app, &["config"], "");
    assert!(out.ends_with("Project app (detected here) overrides review_by = agent, stale_days = 20, `breadcrumb config --project app` lists its settings\n"), "{out}");

    let (ok, out, _) = crumb_in(&v, &app, &["config", "set", "default_kind", "gotcha"], "");
    assert!(ok && out == "default_kind: note -> gotcha\n", "a set inside a project without --project is vault wide: {out}");
    assert_eq!(Vault::open(&v.root).unwrap().config.default_kind, "gotcha");
    assert!(!std::fs::read_to_string(v.root.join("projects/app/settings.toml")).unwrap().contains("default_kind"));

    let log = common::git(&v.root, &["log", "--format=%s"]);
    assert!(log.contains("breadcrumb: config review_by = agent in app\n"), "{log}");
    assert!(log.contains("breadcrumb: config stale_days = 20 in app\n"), "{log}");
}

#[test]
fn refusals_name_the_reason() {
    let (d, v, _) = setup();
    let (ok, _, err) = crumb_in(&v, d.path(), &["config", "set", "judge_project", "x", "--project", "app"], "");
    assert!(!ok && err.contains("judge_project is vault only, a project cannot override it"), "{err}");
    let (ok, _, err) = crumb_in(&v, d.path(), &["config", "set", "review_by", "agent", "--project", "nope"], "");
    assert!(!ok && err.contains("unknown project 'nope'") && err.contains("app, web"), "{err}");
    let (ok, _, err) = crumb_in(&v, d.path(), &["config", "--project", "nope"], "");
    assert!(!ok && err.contains("unknown project 'nope'"), "{err}");
    let (ok, _, err) = crumb_in(&v, d.path(), &["config", "set", "review_by", "robot", "--project", "app"], "");
    assert!(!ok && err.contains("user, agent"), "{err}");
    let (ok, _, err) = crumb_in(&v, d.path(), &["config", "set", "stale_days", "soon", "--project", "app"], "");
    assert!(!ok && err.contains("whole number"), "{err}");
    assert!(!v.root.join("projects/app/settings.toml").exists(), "refusals write nothing");
}

#[test]
fn unset_drops_an_override_or_resets_the_vault_value() {
    let (d, mut v, _) = setup();
    let home = d.path();
    config::set_in(&v, "app", "review_by", "agent").unwrap();
    config::set_in(&v, "app", "stale_days", "20").unwrap();
    let (ok, out, err) = crumb_in(&v, home, &["config", "unset", "review_by", "--project", "app"], "");
    assert!(ok, "{err}");
    assert_eq!(out, "review_by in app: agent -> user\n");
    assert!(crumb_in(&v, home, &["config", "set", "stale_days", "", "--project", "app"], "").0, "an empty value drops the override too");
    assert!(!v.root.join("projects/app/settings.toml").exists(), "the file goes with its last override");

    config::set(&mut v, "stale_days", "90").unwrap();
    let (ok, out, _) = crumb_in(&v, home, &["config", "unset", "stale_days"], "");
    assert!(ok && out == "stale_days: 90 -> 60\n", "{out}");
    assert_eq!(Vault::open(&v.root).unwrap().config.stale_days, 60);
    let log = common::git(&v.root, &["log", "--format=%s"]);
    assert!(log.contains("breadcrumb: config unset review_by in app\n") && log.contains("breadcrumb: config unset stale_days\n"), "{log}");
}

#[test]
fn overrides_reach_review_rules_kinds_staleness_and_the_nudge() {
    let (_d, v, _) = setup();
    config::set_in(&v, "app", "review_by", "agent").unwrap();
    let by = |who: &str| ReviewBy { by: Some(who.into()), evidence: None, by_user: false };
    for p in ["app", "web"] {
        let a = common::add(&v, "Thing", "feature", Some(p), &[]);
        backlog::move_item(&v, &a.id, MoveReq { state: "review".into(), steps: vec!["Run it".into()], by: Some("builder".into()), ..Default::default() }).unwrap();
        let r = backlog::review_pass(&v, &a.id, by("builder"));
        assert_eq!(r.is_err(), p == "app", "agent rules only where the project asks for them: {p}");
    }
    assert!(breadcrumb::digest::context(&v, Some("app"), 15).unwrap().contains("Review: 1 waiting for an agent check"));
    assert!(!breadcrumb::digest::context(&v, Some("web"), 15).unwrap().contains("Review:"));

    config::set_in(&v, "app", "default_kind", "gotcha").unwrap();
    let add = |p: &str| ops::add(&v, AddReq { title: format!("Untyped {p}"), project: Some(p.into()), ..Default::default() }).unwrap().kind;
    assert_eq!((add("app"), add("web")), ("gotcha".to_string(), "note".to_string()));

    config::set_in(&v, "app", "judge_nudge", "session").unwrap();
    assert!(breadcrumb::digest::context(&v, Some("app"), 15).unwrap().contains("Judge breadcrumb"));
    assert!(!breadcrumb::digest::context(&v, Some("web"), 15).unwrap().contains("Judge breadcrumb"));

    for p in ["app", "web"] {
        let n = common::add(&v, &format!("Old fact {p}"), "note", Some(p), &[]);
        let mut n = v.get(&n.id).unwrap();
        n.updated = breadcrumb::util::now() - chrono::Duration::days(30);
        n.unset_field("verified");
        v.write_note(&mut n).unwrap();
    }
    config::set_in(&v, "app", "stale_days", "20").unwrap();
    assert!(breadcrumb::digest::context(&v, Some("app"), 15).unwrap().contains("Old fact app (stale?)"));
    assert!(!breadcrumb::digest::context(&v, Some("web"), 15).unwrap().contains("(stale?)"));
    let doc = breadcrumb::doctor::check(&v);
    let stale: Vec<String> = doc.iter().filter(|p| p.message.contains("not verified for")).map(|p| p.message.clone()).collect();
    assert_eq!(stale.len(), 1, "doctor uses each note's project: {stale:?}");
}

#[test]
fn hook_counts_follow_the_project_setting_and_stats_agree() {
    let (_d, v, app) = setup();
    for i in 0..4 {
        ops::add(&v, AddReq { title: format!("Shader cache invalidation on driver update {i}"), kind: Some("gotcha".into()), project: Some("app".into()), body: Some("The shader cache keeps stale binaries after a driver update.".into()), paths: vec!["src/**".into()], ..Default::default() }).unwrap();
    }
    let prompt = "Why does the shader cache break after a driver update?";
    let file = app.join("src").join("x.rs");
    let none = HashSet::new();
    let recalled = || breadcrumb::recall::recall(&v, Some("app"), prompt, &none).len();
    let filed = || breadcrumb::recall::file_notes(&v, &app, &file, &none).map(|(l, _)| l.matches(" gotcha ").count()).unwrap_or(0);
    assert_eq!((recalled(), filed()), (3, 4), "defaults 3 and 5");
    config::set_in(&v, "app", "recall_notes", "1").unwrap();
    config::set_in(&v, "app", "file_notes", "2").unwrap();
    assert_eq!((recalled(), filed()), (1, 2));

    let hook = |sub: &str, input: Value| {
        let (ok, out, err) = crumb_in(&v, &app, &["hook", sub], &input.to_string());
        assert!(ok, "{err}");
        if out.trim().is_empty() { String::new() } else { serde_json::from_str::<Value>(&out).unwrap()["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_string() }
    };
    let p_line = hook("prompt", json!({"cwd": app, "session_id": "s1", "prompt": prompt}));
    let f_line = hook("file-touch", json!({"cwd": app, "tool_name": "Read", "tool_input": {"file_path": file}}));
    assert_eq!(p_line.matches(" gotcha ").count(), 1, "{p_line}");
    assert_eq!(f_line.matches(" gotcha ").count(), 2, "{f_line}");
    let st = breadcrumb::stats::collect(&v.root, &app, Some(prompt), Some(&file)).unwrap();
    let chars = |name: &str| st.iter().find(|s| s.name == name).unwrap().chars;
    assert_eq!((chars("hook prompt"), chars("hook file")), (p_line.chars().count(), f_line.chars().count()), "stats shows what the hooks inject");

    config::set_in(&v, "app", "recall_notes", "0").unwrap();
    config::set_in(&v, "app", "file_notes", "0").unwrap();
    assert_eq!(hook("prompt", json!({"cwd": app, "session_id": "s2", "prompt": prompt})), "", "0 silences the prompt hook");
    assert_eq!(hook("file-touch", json!({"cwd": app, "tool_name": "Read", "tool_input": {"file_path": file}})), "", "0 silences the file hook");
    let st = breadcrumb::stats::collect(&v.root, &app, Some(prompt), Some(&file)).unwrap();
    assert!(st.iter().filter(|s| s.per_trigger).all(|s| s.chars == 0));
}

#[test]
fn a_broken_or_newer_settings_file_never_breaks_a_hook() {
    let (_d, v, app) = setup();
    std::fs::write(v.root.join("projects/app/settings.toml"), "review_by = \"robot\"\nstale_days = \"soon\"\nfuture_key = 3\nfile_notes = 1\n").unwrap();
    let c = config::effective(&v, Some("app"));
    assert_eq!((c.review_by.as_str(), c.stale_days, c.file_notes), ("user", 60, 1), "bad values and unknown keys are skipped, good ones hold");
    std::fs::write(v.root.join("projects/app/settings.toml"), "not toml [").unwrap();
    assert_eq!(config::effective(&v, Some("app")).file_notes, 5);
    let (ok, _, err) = crumb_in(&v, &app, &["hook", "file-touch"], &json!({"cwd": app, "tool_name": "Read", "tool_input": {"file_path": app.join("src/x.rs")}}).to_string());
    assert!(ok, "{err}");
    assert!(breadcrumb::vault::Vault::open(&v.root).unwrap().load_project("app").unwrap().is_some());
}

#[test]
fn mcp_project_config_lists_reads_sets_and_drops() {
    let (d, v, app) = setup();
    let (out, err) = mcp(&v, d.path(), json!({}));
    assert!(!err && out.starts_with("Vault settings, set one with key and value, a project overrides any not marked vault only when slug is given:"), "{out}");
    let (out, err) = mcp(&v, d.path(), json!({"slug": "app", "key": "review_by", "value": "agent"}));
    assert!(!err && out == "review_by in app: user -> agent\n", "{out}");
    assert_eq!(mcp(&v, d.path(), json!({"slug": "app", "key": "review_by"})).0, "review_by = agent (project, default user)\n");
    assert_eq!(mcp(&v, d.path(), json!({"key": "review_by"})).0, "review_by = user\n");
    let (out, _) = mcp(&v, d.path(), json!({"slug": "app"}));
    assert!(out.starts_with("Settings for project app, set one with key and value to override it here, an empty value drops the override:") && out.contains("review_by = agent (project"), "{out}");
    let (out, _) = mcp(&v, &app, json!({}));
    assert!(out.ends_with("Project app (detected here) overrides review_by = agent, slug app lists its settings\n"), "{out}");

    let (out, err) = mcp(&v, d.path(), json!({"key": "stale_days", "value": 45}));
    assert!(!err && out == "stale_days: 60 -> 45\n", "a number is taken as the value: {out}");
    assert_eq!(Vault::open(&v.root).unwrap().config.stale_days, 45);

    let (out, err) = mcp(&v, d.path(), json!({"slug": "app", "key": "review_by", "value": ""}));
    assert!(!err && out == "review_by in app: agent -> user\n", "{out}");
    assert!(!v.root.join("projects/app/settings.toml").exists());

    let (out, err) = mcp(&v, d.path(), json!({"slug": "app", "key": "remote", "value": "x"}));
    assert!(err && out.contains("remote is vault only"), "{out}");
    let (out, err) = mcp(&v, d.path(), json!({"slug": "nope", "key": "review_by", "value": "agent"}));
    assert!(err && out.contains("unknown project 'nope'"), "{out}");
    let log = common::git(&v.root, &["log", "--format=%s"]);
    assert!(log.contains("breadcrumb: config review_by = agent in app\n") && log.contains("breadcrumb: config stale_days = 45\n"), "{log}");
}
