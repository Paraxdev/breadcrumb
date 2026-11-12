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

fn age(v: &Vault, id: &str, days: i64) {
    let mut n = v.get(id).unwrap();
    n.created = breadcrumb::util::now() - chrono::Duration::days(days);
    v.write_note(&mut n).unwrap();
}

/// Every digest block in project `app`: a pin, two live handoffs (the older 3 days old), a pref, a machine fact, a todo,
/// an answered question, an item waiting for an agent review and 20 recent notes, plus one global note.
fn rich(v: &Vault, home: &Path) {
    let p = Some("app");
    let rule = common::add(v, "Tabs in the app code", "decision", p, &[]);
    ops::update(v, &rule.id, ops::UpdateReq { pinned: Some(true), ..Default::default() }).unwrap();
    common::add(v, "User likes terse output", "feedback", p, &[]);
    common::add(v, "Recordings dir", "env", None, &[("location", "C:/rec")]);
    common::add(v, "Fix the flaky test", "todo", p, &[]);
    for i in 0..20 {
        common::add(v, &format!("Fact {i} about the app"), "note", p, &[]);
    }
    common::add(v, "Global fact", "note", None, &[]);
    let older = common::add(v, "Older state", "handoff", p, &[]);
    age(v, &older.id, 3);
    common::add(v, "Current state", "handoff", p, &[]);
    let item = common::add(v, "Palette", "feature", p, &[]);
    assert!(crumb_in(v, home, &["ask", &item.id, "Which palette?", "-o", "warm", "-o", "cool"], "").0);
    assert!(crumb_in(v, home, &["answer", &format!("{}.1", item.id), "2"], "").0);
    config::set_in(v, "app", "review_by", "agent").unwrap();
    let r = common::add(v, "Review me", "feature", p, &[]);
    backlog::move_item(v, &r.id, MoveReq { state: "review".into(), steps: vec!["Run it".into()], by: Some("builder".into()), ..Default::default() }).unwrap();
}

fn recent_rows(d: &str) -> usize {
    d.lines().skip_while(|l| !l.starts_with("Recent")).skip(1).take_while(|l| !l.starts_with("Global:")).count()
}

fn at(d: &str, s: &str) -> usize {
    d.find(s).unwrap_or_else(|| panic!("no {s:?} in {d}"))
}

#[test]
fn the_digest_settings_default_to_the_digest_as_it_was() {
    let (d, mut v, _) = setup();
    rich(&v, d.path());
    let before = breadcrumb::digest::context(&v, Some("app"), 15).unwrap();
    let heads = ["Pinned", "Latest handoff", "Other live handoffs:", "Prefs (unpinned)", "Machine facts", "Todos:", "Backlog:", "answer arrived on", "Review: 1 waiting", "Recent (", "Global: 1 more note"];
    let pos: Vec<usize> = heads.iter().map(|h| at(&before, h)).collect();
    assert!(pos.windows(2).all(|w| w[0] < w[1]), "{before}");
    assert_eq!(recent_rows(&before), 14, "15 less one for the pref: {before}");
    assert!(!std::fs::read_to_string(v.root.join("breadcrumb.toml")).unwrap().contains("digest_sections"), "the default order is not written");

    config::set(&mut v, "digest_sections", &breadcrumb::digest::SECTIONS.join(",")).unwrap();
    config::set(&mut v, "digest_budget", "6000").unwrap();
    config::set(&mut v, "handoff_digest_days", "7").unwrap();
    assert_eq!(breadcrumb::digest::context(&v, Some("app"), 15).unwrap(), before);
    let (_, out, _) = crumb_in(&v, d.path(), &["config"], "");
    assert!(out.contains("  digest_sections = workflows,pins,handoff,handoffs,prefs,machine,landing,todos,backlog,recent\n") && out.contains("  digest_recent = unset\n"), "{out}");
}

#[test]
fn digest_sections_leave_blocks_out_and_reorder_them_but_calls_to_act_stay() {
    let (d, mut v, _) = setup();
    rich(&v, d.path());
    let (ok, out, err) = crumb_in(&v, d.path(), &["config", "set", "digest_sections", "recent, pins"], "");
    assert!(ok && out.starts_with("digest_sections: workflows,pins,") && out.ends_with(" -> recent,pins\n"), "{out}{err}");
    v = Vault::open(&v.root).unwrap();
    let dg = breadcrumb::digest::context(&v, Some("app"), 15).unwrap();
    for gone in ["Latest handoff", "Other live handoffs", "Prefs", "Machine facts", "Todos:", "Backlog:"] {
        assert!(!dg.contains(gone), "{gone}: {dg}");
    }
    assert!(dg.starts_with("breadcrumb context: app\nRecent ("), "{dg}");
    assert!(at(&dg, "Global: 1 more note") < at(&dg, "Pinned") && at(&dg, "Pinned") < at(&dg, "answer arrived on") && at(&dg, "answer arrived on") < at(&dg, "Review: 1 waiting"), "{dg}");
    assert_eq!(recent_rows(dg.split("Pinned").next().unwrap()), 15, "no pref block, so no row goes to it: {dg}");

    config::set(&mut v, "digest_sections", "todos").unwrap();
    let dg = breadcrumb::digest::context(&v, Some("app"), 15).unwrap();
    assert!(dg.starts_with("breadcrumb context: app\nTodos:\n"), "{dg}");
    assert!(at(&dg, "Todos:") < at(&dg, "answer arrived on") && dg.contains("Review: 1 waiting") && !dg.contains("Recent") && !dg.contains("Global:"), "{dg}");
}

#[test]
fn digest_recent_and_budget_shrink_the_digest_and_the_hook() {
    let (d, v, app) = setup();
    rich(&v, d.path());
    let hook = || {
        let (ok, out, err) = crumb_in(&v, &app, &["hook", "session-start"], &json!({"cwd": app}).to_string());
        assert!(ok, "{err}");
        serde_json::from_str::<Value>(&out).unwrap()["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_string()
    };
    let context = |args: &[&str]| crumb_in(&v, d.path(), &[&["context", "-p", "app"], args].concat(), "").1;
    let full = hook();
    assert_eq!((recent_rows(&context(&[])), recent_rows(&full)), (14, 9), "15 from context, 10 at session start, less one for the pref");
    let (out, err) = mcp(&v, d.path(), json!({"slug": "app", "key": "digest_recent", "value": 4}));
    assert!(!err && out == "digest_recent in app: unset -> 4\n", "{out}");
    assert_eq!((recent_rows(&context(&[])), recent_rows(&hook())), (3, 3), "4 less one for the pref");
    assert_eq!(recent_rows(&context(&["-n", "8"])), 7, "a count the call names wins");

    config::set_in(&v, "app", "digest_budget", "1000").unwrap();
    config::set_in(&v, "app", "digest_recent", "").unwrap();
    let small = context(&[]);
    assert!(small.chars().count() <= 1000 && recent_rows(&small) < 14, "{small}");
    let small = hook();
    assert!(small.chars().count() <= 1000 && small.chars().count() < full.chars().count(), "{small}");
}

#[test]
fn handoff_digest_days_is_the_window_for_other_handoffs_in_that_project_only() {
    let (d, v, _) = setup();
    rich(&v, d.path());
    let older = common::add(&v, "Web older state", "handoff", Some("web"), &[]);
    age(&v, &older.id, 3);
    common::add(&v, "Web state", "handoff", Some("web"), &[]);
    let others = |p: &str| breadcrumb::digest::context(&v, Some(p), 15).unwrap().contains("Other live handoffs:");
    assert!(others("app") && others("web"));
    let (ok, out, _) = crumb_in(&v, d.path(), &["config", "set", "handoff_digest_days", "2", "-p", "app"], "");
    assert!(ok && out == "handoff_digest_days in app: 7 -> 2\n", "{out}");
    assert!(!others("app") && others("web"));
    assert!(breadcrumb::digest::context(&v, Some("app"), 15).unwrap().contains(": Current state"), "the newest leads however old");

    config::set_in(&v, "web", "digest_sections", "recent").unwrap();
    assert!(!breadcrumb::digest::context(&v, Some("web"), 15).unwrap().contains("Latest handoff"));
    assert!(breadcrumb::digest::context(&v, Some("app"), 15).unwrap().contains("Latest handoff"));
    let (_, out, _) = crumb_in(&v, d.path(), &["config", "-p", "web"], "");
    assert!(out.contains("  digest_sections = recent (project, default workflows,pins,"), "{out}");
}

#[test]
fn bad_digest_values_are_refused_on_set_and_skipped_from_a_file() {
    let (d, mut v, _) = setup();
    let refused = |args: &[&str]| {
        let (ok, _, err) = crumb_in(&v, d.path(), &[&["config", "set"], args].concat(), "");
        assert!(!ok, "{args:?}");
        err.trim().trim_start_matches("error: ").to_string()
    };
    let names = "sections are: workflows, pins, handoff, handoffs, prefs, machine, landing, todos, backlog, recent";
    assert_eq!(refused(&["digest_sections", "pins,news"]), format!("unknown digest section 'news', {names}"));
    assert_eq!(refused(&["digest_sections", "pins,recent,pins", "-p", "app"]), format!("digest section 'pins' is listed twice, {names}"));
    assert_eq!(refused(&["digest_sections", ","]), format!("digest_sections needs at least one section, {names}"));
    assert_eq!(refused(&["digest_budget", "999"]), "digest_budget takes a whole number of 1000 or more, got 999");
    assert_eq!(refused(&["digest_recent", "0", "-p", "app"]), "digest_recent takes a whole number of 1 or more, got 0, leave recent out of digest_sections to hide the list");
    assert!(refused(&["handoff_digest_days", "soon"]).starts_with("handoff_digest_days takes a whole number of 0 or more, got 'soon'"));
    assert!(!v.root.join("projects/app/settings.toml").exists(), "refusals write nothing");

    rich(&v, d.path());
    let before = breadcrumb::digest::context(&v, Some("app"), 15).unwrap();
    std::fs::write(v.root.join("projects/app/settings.toml"), "review_by = \"agent\"\ndigest_sections = \"pins,bogus\"\ndigest_budget = 10\ndigest_recent = 0\nhandoff_digest_days = -1\nfile_notes = 1\n").unwrap();
    let c = config::effective(&v, Some("app"));
    assert_eq!((c.digest_sections.as_str(), c.digest_budget, c.digest_recent, c.handoff_digest_days, c.file_notes), (v.config.digest_sections.as_str(), 6000, None, 7, 1));
    assert_eq!(breadcrumb::digest::context(&v, Some("app"), 15).unwrap(), before);

    std::fs::write(v.root.join("projects/app/settings.toml"), "review_by = \"agent\"\n").unwrap();
    let path = v.root.join("breadcrumb.toml");
    let mut t: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    t.insert("digest_sections".into(), "nope,pins".into());
    t.insert("digest_budget".into(), toml::Value::Integer(5));
    std::fs::write(&path, toml::to_string(&t).unwrap()).unwrap();
    v = Vault::open(&v.root).unwrap();
    assert_eq!(breadcrumb::digest::context(&v, Some("app"), 15).unwrap(), before, "a broken vault value falls back to the default");
}

#[test]
fn listings_mark_overrides_that_do_not_hold_and_a_broken_file_names_its_fix() {
    let (d, v, app) = setup();
    std::fs::write(v.root.join("projects/app/settings.toml"), "review_by = \"agent\"\nstale_days = \"soon\"\njudge_project = \"x\"\nfuture_key = 3\n").unwrap();
    let (_, out, _) = crumb_in(&v, &app, &["config"], "");
    assert!(
        out.ends_with("Project app (detected here) overrides review_by = agent and ignores future_key = 3 (future_key is not a setting), judge_project = x (judge_project is vault only), stale_days = soon (stale_days takes a whole number of 0 or more), `breadcrumb config --project app` lists its settings\n"),
        "{out}"
    );
    let (_, out, _) = crumb_in(&v, d.path(), &["config", "--project", "app"], "");
    assert!(out.contains("  stale_days = 60 (project value soon ignored, stale_days takes a whole number of 0 or more)\n"), "{out}");
    assert!(out.contains("  review_by = agent (project, default user)\n"), "{out}");
    let (_, out, _) = crumb_in(&v, d.path(), &["config", "--project", "app", "--json"], "");
    assert!(out.contains("\"ignored\": \"soon ignored, stale_days takes a whole number of 0 or more\""), "{out}");

    std::fs::write(v.root.join("projects/app/settings.toml"), "not toml [").unwrap();
    let (_, out, _) = crumb_in(&v, &app, &["config"], "");
    assert!(out.contains("Project app (detected here) overrides nothing, ") && out.contains("settings.toml is not valid TOML"), "{out}");
    let (ok, _, err) = crumb_in(&v, d.path(), &["config", "set", "review_by", "agent", "-p", "app"], "");
    assert!(!ok && err.contains("projects/app/settings.toml is not valid TOML (") && err.contains("fix it or delete it to drop the project's overrides"), "{err}");
}

#[test]
fn default_kind_takes_only_a_kind_the_vault_has() {
    let (d, mut v, _) = setup();
    let e = config::set(&mut v, "default_kind", "nokind").unwrap_err().to_string();
    assert!(e.starts_with("default_kind takes a kind the vault has, 'nokind' is none, kinds: ") && e.contains("gotcha"), "{e}");
    let (ok, _, err) = crumb_in(&v, d.path(), &["config", "set", "default_kind", "nokind", "-p", "app"], "");
    assert!(!ok && err.contains("default_kind takes a kind the vault has"), "{err}");
    assert!(!v.root.join("projects/app/settings.toml").exists());

    std::fs::write(v.root.join("projects/app/settings.toml"), "default_kind = \"gone\"\n").unwrap();
    let e = ops::add(&v, AddReq { title: "x".into(), project: Some("app".into()), ..Default::default() }).unwrap_err().to_string();
    assert!(e.starts_with("the note has no kind and project app's default_kind setting names 'gone', which is no kind") && e.contains("`breadcrumb config set default_kind <kind> --project app`"), "{e}");
    let c = v.root.join("breadcrumb.toml");
    std::fs::write(&c, std::fs::read_to_string(&c).unwrap().replace("default_kind = \"note\"", "default_kind = \"gone\"")).unwrap();
    let v = Vault::open(&v.root).unwrap();
    let e = ops::add(&v, AddReq { title: "x".into(), project: Some("web".into()), ..Default::default() }).unwrap_err().to_string();
    assert!(e.starts_with("the note has no kind and the default_kind setting names 'gone'") && e.contains("`breadcrumb config set default_kind <kind>`."), "{e}");
    let e = ops::add(&v, AddReq { title: "x".into(), kind: Some("gone".into()), ..Default::default() }).unwrap_err().to_string();
    assert!(e.starts_with("unknown kind 'gone'"), "a kind given by the caller keeps its own error: {e}");
}

#[test]
fn mcp_refusal_of_a_vault_only_key_says_to_drop_slug() {
    let (d, v, _) = setup();
    let (out, err) = mcp(&v, d.path(), json!({"slug": "app", "key": "judge_project", "value": "x"}));
    assert!(err && out.contains("judge_project is vault only, a project cannot override it, call the same action without slug to set it for every project") && !out.contains("breadcrumb config"), "{out}");
}

#[test]
fn a_digest_value_in_breadcrumb_toml_that_does_not_hold_is_listed_as_ignored_and_doctor_reports_it() {
    let (d, v, _) = setup();
    let path = v.root.join("breadcrumb.toml");
    let mut t: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    t.insert("digest_budget".into(), toml::Value::Integer(10));
    std::fs::write(&path, toml::to_string(&t).unwrap()).unwrap();
    let v = Vault::open(&v.root).unwrap();
    let (_, out, _) = crumb_in(&v, d.path(), &["config"], "");
    assert!(out.contains("  digest_budget = 6000 (vault value 10 ignored, digest_budget takes a whole number of 1000 or more)\n"), "{out}");
    let (_, out, _) = crumb_in(&v, d.path(), &["config", "get", "digest_budget"], "");
    assert_eq!(out.trim(), "6000", "get prints the value that holds");
    let ps = breadcrumb::doctor::check_with(&v, None);
    assert!(
        ps.iter().any(|p| p.path == "breadcrumb.toml" && p.severity == "warning" && p.message == "digest_budget = 10 is ignored, digest_budget takes a whole number of 1000 or more, the default 6000 holds"),
        "{:?}",
        ps.iter().map(|p| &p.message).collect::<Vec<_>>()
    );

    std::fs::write(v.root.join("projects/app/settings.toml"), "digest_sections = [\"pins\", \"recent\"]\n").unwrap();
    let (_, out, _) = crumb_in(&v, d.path(), &["config", "--project", "app"], "");
    assert!(out.contains(" ignored, digest_sections takes text, section names separated by commas"), "{out}");
}
