mod common;

use breadcrumb::mcp::Server;
use breadcrumb::vault::Vault;
use serde_json::{Value, json};
use std::process::{Command, Output};

fn crumb(v: &Vault, session: Option<&str>, args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_crumb"));
    c.args(args).env("BREADCRUMB_HOME", &v.root).env_remove("CLAUDE_CODE_SESSION_ID").env_remove("BREADCRUMB_SESSION");
    if let Some(s) = session {
        c.env("BREADCRUMB_SESSION", s);
    }
    c.output().unwrap()
}

fn raw(v: &Vault) -> String {
    std::fs::read_to_string(v.root.join(".local").join("usage.log")).unwrap_or_default()
}

fn lines(v: &Vault) -> Vec<Value> {
    raw(v).lines().map(|l| serde_json::from_str(l).expect("every line is JSON")).collect()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn added_id(o: &Output) -> String {
    stdout(o).split_whitespace().nth(1).unwrap().to_string()
}

#[test]
fn a_command_logs_its_path_outcome_project_session_and_chars_but_no_text() {
    let (_d, v) = common::vault();
    let o = crumb(&v, Some("night-shift"), &["add", "-k", "note", "-p", "app", "-t", "Secret plan for the launch", "--body", "hidden words inside"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id = added_id(&o);
    crumb(&v, None, &["get", "nosuchid"]);
    crumb(&v, None, &["project", "list"]);
    let l = lines(&v);
    assert_eq!(l.len(), 3, "{l:#?}");
    assert_eq!((l[0]["cmd"].as_str(), l[0]["outcome"].as_str(), l[0]["project"].as_str(), l[0]["session"].as_str()), (Some("add"), Some("ok"), Some("app"), Some("night-shift")));
    assert!(l[0]["chars"].as_u64().unwrap() > 0 && l[0]["time"].as_str().unwrap().ends_with('Z'));
    assert_eq!((l[1]["cmd"].as_str(), l[1]["outcome"].as_str()), (Some("get"), Some("not_found")));
    assert_eq!(l[2]["cmd"], "project.list");
    let text = raw(&v);
    assert!(!text.contains("Secret") && !text.contains("hidden") && !text.contains(&id), "{text}");
    let tracked = common::git(&v.root, &["ls-files"]);
    assert!(!tracked.contains("usage"), "{tracked}");
    assert!(common::git(&v.root, &["status", "--porcelain"]).trim().is_empty());
}

#[test]
fn a_refused_line_keeps_only_the_word_and_the_suggestion() {
    let (_d, v) = common::vault();
    crumb(&v, None, &["gren", "abc1234"]);
    crumb(&v, None, &["add", "-t", "Private words here", "--text", "private body"]);
    crumb(&v, None, &["list", "--status", "actve"]);
    crumb(&v, None, &["Fix the login bug please"]);
    let l = lines(&v);
    assert_eq!(l.len(), 4, "{l:#?}");
    assert_eq!((l[0]["cmd"].as_str(), l[0]["outcome"].as_str(), l[0]["miss"].as_str(), l[0]["hint"].as_str()), (Some("cli"), Some("unknown_command"), Some("gren"), Some("land green")));
    assert_eq!((l[1]["cmd"].as_str(), l[1]["miss"].as_str(), l[1]["hint"].as_str()), (Some("add"), Some("--text"), Some("--body")));
    assert_eq!((l[2]["cmd"].as_str(), l[2]["outcome"].as_str(), l[2]["miss"].as_str(), l[2]["hint"].as_str()), (Some("list"), Some("bad_value"), Some("actve"), Some("active")));
    assert_eq!(l[3]["miss"], "<text>");
    let text = raw(&v);
    assert!(!text.contains("Private") && !text.contains("private") && !text.contains("login"), "{text}");
}

#[test]
fn a_line_read_the_way_it_was_meant_logs_the_command_it_ran_as() {
    let (_d, v) = common::vault();
    let id = added_id(&crumb(&v, None, &["add", "-k", "note", "-t", "A note"]));
    let o = crumb(&v, None, &["get", "--id", &id]);
    assert!(o.status.success());
    let l = lines(&v);
    assert_eq!((l[1]["cmd"].as_str(), l[1]["outcome"].as_str(), l[1]["miss"].as_str(), l[1]["hint"].as_str()), (Some("get"), Some("ok"), Some("--id"), Some("read as get")));
}

#[test]
fn no_log_when_the_vault_does_not_ignore_local() {
    let (_d, v) = common::vault();
    std::fs::write(v.root.join(".gitignore"), "*.lock\n").unwrap();
    crumb(&v, None, &["list"]);
    assert!(!v.root.join(".local").join("usage.log").exists());
}

#[test]
fn sessions_appending_at_once_each_get_one_whole_line() {
    let (_d, v) = common::vault();
    let kids: Vec<_> = (0..8)
        .map(|i| {
            Command::new(env!("CARGO_BIN_EXE_crumb"))
                .args(["list"])
                .env("BREADCRUMB_HOME", &v.root)
                .env_remove("CLAUDE_CODE_SESSION_ID")
                .env("BREADCRUMB_SESSION", format!("s{i}"))
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for mut k in kids {
        k.wait().unwrap();
    }
    let l = lines(&v);
    assert_eq!(l.len(), 8);
    assert!(l.iter().all(|x| x["cmd"] == "list"));
}

#[test]
fn a_full_log_rotates_and_usage_reads_both_files() {
    let (_d, v) = common::vault();
    let dir = v.root.join(".local");
    std::fs::create_dir_all(&dir).unwrap();
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let one = format!("{}\n", json!({"time": now, "cmd": "search", "outcome": "ok", "chars": 10}));
    std::fs::write(dir.join("usage.log"), one.repeat(2 * 1024 * 1024 / one.len() + 10)).unwrap();
    crumb(&v, None, &["list"]);
    assert!(dir.join("usage.log.1").exists());
    assert_eq!(lines(&v).len(), 1);
    let o = crumb(&v, None, &["usage", "--json"]);
    let r: Value = serde_json::from_str(&stdout(&o)).unwrap();
    let search = r["commands"].as_array().unwrap().iter().find(|c| c["cmd"] == "search").unwrap();
    assert!(search["calls"].as_u64().unwrap() > 1000);
}

#[test]
fn usage_shows_calls_errors_misses_unused_commands_and_the_digest() {
    let (_d, v) = common::vault();
    crumb(&v, Some("a"), &["context"]);
    crumb(&v, Some("a"), &["get", "nosuchid"]);
    crumb(&v, Some("b"), &["get", "nosuchid2"]);
    crumb(&v, Some("b"), &["gren", "abc1234"]);
    let o = crumb(&v, None, &["usage", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let r: Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(r["days"], 7);
    assert_eq!(r["sessions"], 2);
    let get = r["commands"].as_array().unwrap().iter().find(|c| c["cmd"] == "get").unwrap();
    assert_eq!((get["calls"].as_u64(), get["errors"].as_u64()), (Some(2), Some(2)));
    assert_eq!(r["misses"][0]["miss"], "gren");
    assert_eq!(r["misses"][0]["hint"], "land green");
    let unused: Vec<&str> = r["unused"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(unused.contains(&"archive") && unused.contains(&"mcp:map") && !unused.contains(&"get") && !unused.contains(&"hook"), "{unused:?}");
    assert_eq!(r["digest"]["built"], 1);
    assert_eq!(r["digest"]["budget"], 6000);
    assert_eq!(r["digest"]["cut"], 0);
    let text = stdout(&crumb(&v, None, &["usage"]));
    assert!(text.contains("usage in the last 7 days:") && text.contains("2 (100%)") && text.contains("breadcrumb gren  1 hit in 1 session, refused, suggested land green"), "{text}");
    assert!(text.contains("never used: ") && text.contains("digest: built 1 time"), "{text}");
    let none = stdout(&crumb(&v, None, &["usage", "-p", "elsewhere"]));
    assert!(none.starts_with("no calls logged in elsewhere in the last 7 days"), "{none}");
}

#[test]
fn a_cut_digest_is_logged_as_cut() {
    use breadcrumb::ops::{self, AddReq, UpdateReq};
    use breadcrumb::question::{self, AskReq, Opt};
    let (_d, mut v) = common::vault();
    breadcrumb::config::set(&mut v, "review_by", "agent").unwrap();
    for i in 0..6 {
        let n = common::add(&v, &format!("Decision {i:02} that every session has to know about, written out at its full length here"), "decision", Some("app"), &[]);
        ops::update(&v, &n.id, UpdateReq { pinned: Some(true), ..Default::default() }).unwrap();
    }
    for t in 1..=3 {
        common::add(&v, &format!("State of track {t}, a handoff title that goes on for quite a while longer"), "handoff", Some("app"), &[("track", &format!("o{t}"))]);
    }
    std::thread::sleep(std::time::Duration::from_millis(1100));
    ops::add(&v, AddReq { title: "Where it stands:".into(), kind: Some("handoff".into()), project: Some("app".into()), body: Some("Line of the body.
".repeat(40)), ..Default::default() }).unwrap();
    for i in 0..4 {
        let t = common::add(&v, &format!("Task {i} with a question"), "task", Some("app"), &[("state", "queued")]);
        let options = ["this way", "that way"].iter().map(|l| Opt { label: l.to_string(), desc: String::new() }).collect();
        let text = format!("Question {i}: which of the two ways should this be built, given what the last session found out about it?");
        let (n, q) = question::ask(&v, AskReq { id: t.id.clone(), text, options, multi: false, free: false, by: None }).unwrap();
        if i < 3 {
            question::answer(&v, &question::qid(&n, &q), &[2], None, "user").unwrap();
        }
    }
    let built = common::add(&v, "Thing to check", "feature", Some("app"), &[]);
    breadcrumb::backlog::move_item(&v, &built.id, breadcrumb::backlog::MoveReq { state: "review".into(), steps: vec!["Run it".into()], by: Some("builder".into()), ..Default::default() }).unwrap();
    breadcrumb::config::set(&mut v, "digest_budget", "1000").unwrap();

    crumb(&v, None, &["context", "-p", "app"]);
    let l = lines(&v);
    let ctx = l.iter().find(|x| x["cmd"] == "context").unwrap();
    assert_eq!((ctx["budget"].as_u64(), ctx["cut"].as_bool(), ctx["project"].as_str()), (Some(1000), Some(true), Some("app")), "{ctx}");
    assert!(ctx["digest"].as_u64().unwrap() <= 1000);
    crumb(&v, None, &["context", "--global"]);
    let quiet = lines(&v).into_iter().filter(|x| x["cmd"] == "context").nth(1).unwrap();
    assert!(quiet.get("cut").is_none() && quiet["budget"] == 1000, "{quiet}");
}

#[test]
fn a_miss_two_sessions_repeat_is_filed_once_as_a_judgment() {
    let (_d, v) = common::vault();
    crumb(&v, Some("a"), &["gren", "abc1234"]);
    crumb(&v, Some("a"), &["gren", "abc1234"]);
    let judgments = |v: &Vault| v.load_notes().into_iter().filter(|n| n.kind == "judgment").collect::<Vec<_>>();
    assert!(judgments(&v).is_empty());
    crumb(&v, Some("a"), &["gren", "abc1234"]);
    assert!(judgments(&v).is_empty(), "one session alone never files");
    crumb(&v, Some("b"), &["gren", "abc1234"]);
    let j = judgments(&v);
    assert_eq!(j.len(), 1);
    let n = &j[0];
    assert!(n.tags.contains(&"usage-miss".to_string()));
    assert_eq!(breadcrumb::judge::verdict(n), "confusing");
    assert_eq!(breadcrumb::judge::about(n), "cli");
    assert!(n.title.contains("`breadcrumb gren` was refused 4 times in 2 sessions"), "{}", n.title);
    assert!(n.body.contains("maps cleanly to `land green`"), "{}", n.body);
    crumb(&v, Some("c"), &["gren", "abc1234"]);
    std::fs::remove_file(v.root.join(".local").join("usage-filed")).unwrap();
    crumb(&v, Some("c"), &["gren", "abc1234"]);
    assert_eq!(judgments(&v).len(), 1, "never filed twice");
}

#[test]
fn adapt_threshold_sets_the_hits_and_zero_turns_filing_off() {
    let (_d, v) = common::vault();
    crumb(&v, None, &["config", "set", "adapt_threshold", "0"]);
    for s in ["a", "b", "c", "d"] {
        crumb(&v, Some(s), &["list", "--status", "actve"]);
    }
    assert!(v.load_notes().iter().all(|n| n.kind != "judgment"));
    crumb(&v, None, &["config", "set", "adapt_threshold", "5"]);
    crumb(&v, Some("e"), &["list", "--status", "actve"]);
    let j: Vec<_> = v.load_notes().into_iter().filter(|n| n.kind == "judgment").collect();
    assert_eq!(j.len(), 1);
    assert!(j[0].title.contains("`breadcrumb list actve`") && j[0].body.contains("`active`"), "{}", j[0].body);
}

#[test]
fn mcp_calls_are_logged_with_their_action_and_misses() {
    let (d, v) = common::vault();
    let srv = Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let call = |name: &str, args: Value| srv.handle_message(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}})).unwrap();
    call("list", json!({"project": "app"}));
    call("project", json!({"action": "list"}));
    call("lsit", json!({}));
    call("get", json!({"id": "nosuchid"}));
    call("search", json!({"query": "secret words", "knd": "note"}));
    call("question", json!({"action": "aks"}));
    let l = lines(&v);
    assert_eq!(l.len(), 6, "{l:#?}");
    assert_eq!((l[0]["cmd"].as_str(), l[0]["outcome"].as_str(), l[0]["project"].as_str()), (Some("mcp:list"), Some("ok"), Some("app")));
    assert!(l[0]["chars"].as_u64().unwrap() > 0);
    assert_eq!(l[1]["cmd"], "mcp:project.list");
    assert_eq!((l[2]["cmd"].as_str(), l[2]["outcome"].as_str(), l[2]["miss"].as_str(), l[2]["hint"].as_str()), (Some("mcp"), Some("unknown_tool"), Some("lsit"), Some("list")));
    assert_eq!((l[3]["cmd"].as_str(), l[3]["outcome"].as_str()), (Some("mcp:get"), Some("not_found")));
    assert_eq!((l[4]["outcome"].as_str(), l[4]["miss"].as_str(), l[4]["hint"].as_str()), (Some("ok"), Some("knd"), Some("kind")));
    assert_eq!((l[5]["cmd"].as_str(), l[5]["miss"].as_str(), l[5]["hint"].as_str()), (Some("mcp:question"), Some("aks"), Some("ask")));
    assert!(!raw(&v).contains("secret"));
    let r = call("project", json!({"action": "usage", "days": 3}));
    let text = r["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("usage in the last 3 days: 6 calls") && text.contains("MCP tool lsit  1 hit in 0 sessions, refused, suggested list"), "{text}");
}

#[test]
fn doctor_names_a_command_that_fails_more_than_a_third_of_its_calls() {
    let (_d, v) = common::vault();
    let dir = v.root.join(".local");
    std::fs::create_dir_all(&dir).unwrap();
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let old = (chrono::Utc::now() - chrono::Duration::days(9)).format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let line = |t: &str, cmd: &str, outcome: &str| format!("{}\n", json!({"time": t, "cmd": cmd, "outcome": outcome}));
    let mut log = String::new();
    for i in 0..6 {
        log.push_str(&line(&now, "land.read", if i < 3 { "bad_input" } else { "ok" }));
        log.push_str(&line(&now, "list", if i < 2 { "bad_value" } else { "ok" }));
        log.push_str(&line(&old, "move", "bad_input"));
    }
    log.push_str(&line(&now, "get", "not_found"));
    log.push_str("{cut short\n");
    std::fs::write(dir.join("usage.log"), log).unwrap();
    let o = crumb(&v, None, &["doctor"]);
    let text = stdout(&o);
    assert!(text.contains("info: .local/usage.log: land.read failed 3 of 6 calls in the last 7 days (50%), `breadcrumb usage` shows the misses"), "{text}");
    assert!(!text.contains("list failed") && !text.contains("move failed") && !text.contains("get failed"), "{text}");
}

#[test]
fn in_process_tests_never_see_the_shell_session() {
    let (_d, v) = common::vault();
    assert!(std::env::var("CLAUDE_CODE_SESSION_ID").is_err() && std::env::var("BREADCRUMB_SESSION").is_err());
    let n = common::add(&v, "Written in a test", "note", None, &[]);
    assert!(n.field("session").is_none(), "{:?}", n.field("session"));
}
