mod common;

use breadcrumb::backlog;
use breadcrumb::vault::Vault;
use std::process::Command;

fn crumb(v: &Vault, args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_crumb")).args(args).env("BREADCRUMB_HOME", &v.root).env_remove("CLAUDE_CODE_SESSION_ID").env_remove("BREADCRUMB_SESSION").output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

#[test]
fn add_takes_the_title_as_a_word_or_a_flag_but_not_both() {
    let (_d, v) = common::vault();
    let (ok, out, err) = crumb(&v, &["add", "Positional title", "-k", "gotcha", "--global"]);
    assert!(ok && out.contains("Positional title"), "{err}");
    let (ok, out, err) = crumb(&v, &["add", "-t", "Flag title", "--global"]);
    assert!(ok && out.contains("Flag title"), "{err}");
    let (ok, _, err) = crumb(&v, &["add", "One", "--title", "Two", "--global"]);
    assert!(!ok && err.contains("cannot be used with"), "{err}");
    let (ok, _, err) = crumb(&v, &["add", "--global"]);
    assert!(!ok && err.contains("<TITLE>"), "{err}");
    let titles: Vec<String> = v.load_notes().into_iter().map(|n| n.title).collect();
    assert!(titles.len() == 2 && !titles.iter().any(|t| t == "One" || t == "Two"), "{titles:?}");
}

#[test]
fn add_and_idea_take_a_title_that_starts_with_a_hyphen() {
    let (_d, v) = common::vault();
    let (ok, _, err) = crumb(&v, &["add", "-k", "task", "-dash title", "--global"]);
    assert!(ok, "{err}");
    let (ok, _, err) = crumb(&v, &["add", "-lead title", "-k", "gotcha", "--body", "-dash body", "--global"]);
    assert!(ok, "{err}");
    let (ok, _, err) = crumb(&v, &["idea", "-dash idea", "--body", "-x", "--global"]);
    assert!(ok, "{err}");
    let notes = v.load_notes();
    let kind_of = |t: &str| notes.iter().find(|n| n.title == t).map(|n| n.kind.clone());
    assert_eq!(kind_of("-dash title").as_deref(), Some("task"));
    assert_eq!(kind_of("-lead title").as_deref(), Some("gotcha"));
    assert_eq!(kind_of("-dash idea").as_deref(), Some("idea"));
}

#[test]
fn text_that_reads_like_a_long_flag_is_refused_unless_after_a_lone_dash_dash() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "Thing", "task", Some("app"), &[]);
    let (ok, _, err) = crumb(&v, &["append", &a.id, "--weird"]);
    assert!(!ok && err.contains(&format!("unknown flag --weird, text that starts with -- goes last, after a lone --: `breadcrumb append {} -- \"--weird ...\"`", a.id)), "{err}");
    let (ok, _, err) = crumb(&v, &["add", "--tittle"]);
    assert!(!ok && err.contains("unknown flag --tittle") && err.contains("`breadcrumb add <flags> -- \"--tittle ...\"`"), "{err}");
    let (ok, _, err) = crumb(&v, &["idea", "--quick thought", "--global"]);
    assert!(!ok && err.contains("unknown flag --quick"), "{err}");
    let before = v.get(&a.id).unwrap().body;
    assert_eq!(v.load_notes().len(), 1, "nothing was saved");
    for text in ["- a bullet", "-5 degrees"] {
        let (ok, _, err) = crumb(&v, &["append", &a.id, text, "-p", "app"]);
        assert!(ok, "{err}");
    }
    let (ok, _, err) = crumb(&v, &["append", &a.id, "--", "--force breaks it"]);
    assert!(ok, "{err}");
    let (ok, _, err) = crumb(&v, &["add", "-k", "gotcha", "--global", "--", "--force pushes"]);
    assert!(ok, "{err}");
    let (ok, out, err) = crumb(&v, &["add", "-5 degrees", "-k", "gotcha", "--global", "--pin"]);
    assert!(ok && out.contains("-5 degrees"), "flags after the text still parse: {err}");
    let body = v.get(&a.id).unwrap().body;
    assert!(body.len() > before.len() && body.contains("\n- a bullet\n") && body.contains("\n-5 degrees") && body.contains("\n--force breaks it"), "{body}");
    let notes = v.load_notes();
    assert!(notes.iter().any(|n| n.title == "--force pushes"));
    assert!(notes.iter().any(|n| n.title == "-5 degrees" && n.pinned && n.kind == "gotcha"));
}

#[test]
fn slugs_takes_all_and_global_as_scopes_and_config_refuses_them() {
    let (_d, v) = common::vault();
    common::add(&v, "App fact here", "note", Some("app"), &[]);
    common::add(&v, "Global fact here", "note", None, &[]);
    let (_, all, _) = crumb(&v, &["slugs", "-p", "all"]);
    assert!(all.contains("App fact here") && all.contains("Global fact here"), "{all}");
    let (_, global, _) = crumb(&v, &["slugs", "-p", "Global"]);
    assert!(!global.contains("App fact here") && global.contains("Global fact here"), "{global}");
    let (_, app, _) = crumb(&v, &["slugs", "-p", "app"]);
    assert!(app.contains("App fact here") && !app.contains("Global fact here"), "{app}");
    let (_, out, _) = crumb(&v, &["review", "-p", "all"]);
    assert_eq!(out, "nothing in review in any project\n");
    for w in ["all", "global"] {
        let (ok, _, err) = crumb(&v, &["config", "-p", w]);
        assert!(!ok && err.contains(&format!("config has no {w} scope, drop -p for the vault's settings or name one project with -p <slug>")), "{err}");
    }
}

#[test]
fn commands_on_an_id_accept_and_ignore_a_project() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "Thing", "task", Some("app"), &[]);
    for args in [
        vec!["get", &a.id, "-p", "elsewhere"],
        vec!["update", &a.id, "--tag", "x", "--project", "elsewhere"],
        vec!["append", &a.id, "more", "-p", "elsewhere"],
        vec!["verify", &a.id, "-p", "elsewhere"],
        vec!["claim", &a.id, "--by", "me", "-p", "elsewhere"],
        vec!["release", &a.id, "--by", "me", "-p", "elsewhere"],
        vec!["move", &a.id, "review", "--step", "Try it", "-p", "elsewhere"],
        vec!["check", &a.id, "1", "-p", "elsewhere"],
        vec!["tree", &a.id, "-p", "elsewhere"],
        vec!["deps", &a.id, "-p", "elsewhere"],
        vec!["review", "pass", &a.id, "-p", "elsewhere"],
    ] {
        let (ok, _, err) = crumb(&v, &args);
        assert!(ok, "{args:?}: {err}");
    }
    let n = v.get(&a.id).unwrap();
    assert_eq!((backlog::state(&n).as_str(), n.project.as_deref()), ("done", Some("app")), "the project never moved or changed the lookup");
    let (_, help, _) = crumb(&v, &["get", "--help"]);
    assert!(!help.contains("--project"), "hidden: {help}");
}

#[test]
fn all_wins_over_a_project_and_project_all_reads_as_all() {
    let (_d, v) = common::vault();
    common::add(&v, "In app", "note", Some("app"), &[]);
    common::add(&v, "In web", "note", Some("web"), &[]);
    for args in [vec!["list", "--all", "-p", "app"], vec!["list", "-p", "all"], vec!["search", "In", "-p", "ALL"], vec!["board", "-p", "all"]] {
        let (ok, out, err) = crumb(&v, &args);
        assert!(ok, "{args:?}: {err}");
        if args[0] != "board" {
            assert!(out.contains("In app") && out.contains("In web"), "{args:?}: {out}");
        }
    }
    let (ok, out, _) = crumb(&v, &["list", "-p", "global"]);
    assert!(ok && !out.contains("In app"), "{out}");
    let (ok, _, err) = crumb(&v, &["workflow", "list", "--all", "-p", "app"]);
    assert!(ok, "{err}");
    for args in [vec!["add", "Global by word", "-p", "global"], vec!["idea", "Global idea", "-p", "Global"]] {
        let (ok, out, err) = crumb(&v, &args);
        assert!(ok && !out.contains("created new project"), "{args:?}: {out}{err}");
    }
    assert!(v.load_notes().iter().filter(|n| n.title.starts_with("Global")).all(|n| n.project.is_none()));
    assert!(v.load_project("global").unwrap().is_none());
    let (ok, out, _) = crumb(&v, &["context", "-p", "global"]);
    assert!(ok && out.contains("Global by word"), "{out}");
    let (ok, _, err) = crumb(&v, &["context", "-p", "All"]);
    assert!(!ok && err.contains("the digest is per project or global"), "{err}");
    let srv = breadcrumb::mcp::Server { vault_root: v.root.clone(), cwd: v.root.clone() };
    let a = serde_json::json!({"project": "all"}).as_object().unwrap().clone();
    assert!(srv.call_tool("context", &a).unwrap_err().to_string().contains("the digest is per project or global"));

    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("tidy.js");
    std::fs::write(&script, "export default async () => {}\n").unwrap();
    let (ok, out, err) = crumb(&v, &["workflow", "save", script.to_str().unwrap(), "-p", "global"]);
    assert!(ok && out.contains("tidy  global"), "{out}{err}");
    assert!(v.load_project("global").unwrap().is_none());
}

#[test]
fn free_text_may_start_with_a_hyphen() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "Thing", "task", Some("app"), &[]);
    for args in [vec!["append", &a.id, "- a bullet"], vec!["append", &a.id, "-p", "app", "- after a flag"], vec!["append", &a.id, "-p", "app", "--", "--dashes first"]] {
        let (ok, _, err) = crumb(&v, &args);
        assert!(ok, "{args:?}: {err}");
    }
    let body = v.get(&a.id).unwrap().body;
    assert!(body.contains("\n- a bullet") && body.contains("\n- after a flag") && body.contains("\n--dashes first"), "{body}");
    let (ok, _, err) = crumb(&v, &["ask", &a.id, "-- which one?", "-o", "a: x", "-o", "b: y"]);
    assert!(ok, "{err}");
}

#[test]
fn update_says_when_it_ignored_another_project() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "Thing", "task", Some("app"), &[]);
    let (_, out, _) = crumb(&v, &["update", &a.id, "--tag", "x", "-p", "web"]);
    assert!(out.ends_with("\n-p web was ignored, the note stays in app, no command moves a note to another project\n"), "{out}");
    let (_, out, _) = crumb(&v, &["update", &a.id, "--tag", "y", "-p", "App"]);
    assert!(!out.contains("ignored"), "{out}");
    assert_eq!(v.get(&a.id).unwrap().project.as_deref(), Some("app"));
}

#[test]
fn claim_and_release_read_the_project_of_a_track() {
    let (_d, v) = common::vault();
    crumb(&v, &["add", "Pick up the parser", "-k", "handoff", "--track", "main", "-p", "app"]);
    let (ok, _, err) = crumb(&v, &["claim", "track:main", "--by", "me"]);
    assert!(!ok && err.contains("no live handoff on track main in global, app has one, pass -p app (project in MCP)"), "{err}");
    let (ok, _, err) = crumb(&v, &["claim", "track:nope", "--by", "me"]);
    assert!(!ok && err.contains("no live handoff on track nope in global, pass -p <slug> (project in MCP) for another project's track"), "{err}");
    let (ok, out, err) = crumb(&v, &["claim", "track:main", "-p", "app", "--by", "me"]);
    assert!(ok && out.contains("took pick-parser"), "{err}");
    let (ok, out, err) = crumb(&v, &["release", "track:main", "--project", "app", "--by", "me"]);
    assert!(ok && out.contains("released pick-parser"), "{err}");
    let (ok, out, err) = crumb(&v, &["claim", "track:main", "-p", "APP", "--by", "me"]);
    assert!(ok && out.contains("took pick-parser"), "{err}");
    let (ok, _, err) = crumb(&v, &["claim", "track:main", "-p", "nosuch", "--by", "me"]);
    assert!(!ok && err.contains("unknown project 'nosuch', known projects: app"), "{err}");
}

#[test]
fn board_state_shows_only_those_columns() {
    let (_d, v) = common::vault();
    let q = common::add(&v, "Queued one", "task", Some("app"), &[]);
    crumb(&v, &["queue", &q.id]);
    common::add(&v, "Backlog one", "task", Some("app"), &[]);
    let (ok, out, err) = crumb(&v, &["board", "--all", "--state", "queued"]);
    assert!(ok, "{err}");
    assert!(out.contains("## queued (1)") && out.contains("Queued one") && !out.contains("## backlog") && !out.contains("Backlog one"), "{out}");
    let (_, out, _) = crumb(&v, &["board", "--all", "--state", "queued,backlog", "--state", "shipped"]);
    assert!(out.contains("## queued (1)") && out.contains("## backlog (1)") && out.contains("## shipped (0)") && !out.contains("## idea"), "{out}");
    let (_, out, _) = crumb(&v, &["board", "--all", "--state", "review", "--state", "done"]);
    assert!(out.contains("No items in review or done, without --state the board shows every column"), "{out}");
    let (ok, _, err) = crumb(&v, &["board", "--state", "soon"]);
    assert!(!ok && err.contains("unknown state 'soon'") && err.contains("being-built"), "{err}");
    let (_, out, _) = crumb(&v, &["list", "--all", "--where", "state=queued"]);
    assert!(out.contains("Queued one") && !out.contains("Backlog one"), "the flat list board help names: {out}");
    crumb(&v, &["update", &q.id, "--ref", "branch:lane/x"]);
    let (_, out, _) = crumb(&v, &["board", "--all", "--state", "Being-Built"]);
    assert!(out.contains("## being-built (1)") && out.contains("Queued one") && !out.contains("## queued"), "{out}");
    let asked = common::add(&v, "Asked one", "task", Some("app"), &[]);
    crumb(&v, &["queue", &asked.id]);
    crumb(&v, &["ask", &asked.id, "Which one?", "-o", "a: x", "-o", "b: y"]);
    let (_, out, _) = crumb(&v, &["board", "-p", "app", "--state", "queued"]);
    assert!(out.contains("No items in queued, its queued items sit in the being-built (1) and waiting-on-user (1) columns, add being-built,waiting-on-user to --state"), "{out}");
}

#[test]
fn mcp_help_says_what_it_speaks_and_tools_lists_every_tool_from_its_schema() {
    let (_d, v) = common::vault();
    let (_, help, _) = crumb(&v, &["mcp", "--help"]);
    assert!(help.contains("JSON-RPC 2.0") && help.contains("tools/call") && help.contains("stderr") && help.contains("--tools"), "{help}");
    let (ok, out, err) = crumb(&v, &["mcp", "--tools"]);
    assert!(ok, "{err}");
    for t in breadcrumb::mcp::tool_defs() {
        assert!(out.contains(&format!("\n{}: ", t["name"].as_str().unwrap())) || out.starts_with(&format!("{}: ", t["name"].as_str().unwrap())), "{out}");
    }
    assert!(out.contains("  state string: idea|backlog|queued|in-progress|review|done|shipped|rejected (required)"), "{out}");
    assert!(out.contains("  query string (required)") && out.contains("    name string (required)"), "nested define args are indented: {out}");
    let (_, json, _) = crumb(&v, &["--json", "mcp", "--tools"]);
    let j: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(j["tools"].as_array().unwrap().len(), breadcrumb::mcp::tool_defs().len());
}
