mod common;

use breadcrumb::ops::{self, AddReq, UpdateReq};
use breadcrumb::recall::{glob_match, normalize_globs, rel_to_root};
use breadcrumb::vault::Vault;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Instant;

fn breadcrumb(vault: &Path, args: &[&str], stdin: &str, cwd: &Path) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_breadcrumb"));
    c.arg("--vault").arg(vault).args(args).current_dir(cwd).env_remove("BREADCRUMB_HOME");
    c.env("BREADCRUMB_CLAUDE_DIR", vault.with_extension("no-claude"));
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn stdout(o: &Output) -> String {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).to_string()
}

/// additionalContext of a hook's output, None when the hook printed nothing.
fn context(o: &Output, event: &str) -> Option<String> {
    let out = stdout(o);
    if out.trim().is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], event);
    Some(v["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_string())
}

fn touch(v: &Vault, cwd: &Path, file: &str, session: Option<&str>) -> Option<String> {
    let mut input = json!({"cwd": cwd, "hook_event_name": "PreToolUse", "tool_name": "Read", "tool_input": {"file_path": file}});
    if let Some(s) = session {
        input["session_id"] = json!(s);
    }
    context(&breadcrumb(&v.root, &["hook", "file-touch"], &input.to_string(), cwd), "PreToolUse")
}

fn prompt(v: &Vault, cwd: &Path, text: &str, session: &str) -> Option<String> {
    let input = json!({"cwd": cwd, "session_id": session, "hook_event_name": "UserPromptSubmit", "prompt": text});
    context(&breadcrumb(&v.root, &["hook", "prompt"], &input.to_string(), cwd), "UserPromptSubmit")
}

fn add_scoped(v: &Vault, title: &str, kind: &str, project: Option<&str>, paths: &[&str]) -> breadcrumb::note::Note {
    ops::add(
        v,
        AddReq {
            title: title.into(),
            kind: Some(kind.into()),
            project: project.map(Into::into),
            paths: paths.iter().map(|p| p.to_string()).collect(),
            ..Default::default()
        },
    )
    .unwrap()
}

/// A vault with project `demo` linked to <tmp>/work/demo, which has a src/engine directory.
fn project_vault() -> (tempfile::TempDir, Vault, PathBuf) {
    let (d, v) = common::vault();
    let proj = d.path().join("work").join("demo");
    std::fs::create_dir_all(proj.join("src").join("engine")).unwrap();
    ops::project_link(&v, "demo", &proj, None).unwrap();
    (d, v, proj)
}

fn age(v: &Vault, id: &str, days: i64) {
    let mut n = v.get(id).unwrap();
    n.updated = breadcrumb::util::now() - chrono::Duration::days(days);
    n.unset_field("verified");
    v.write_note(&mut n).unwrap();
}

#[test]
fn glob_matching() {
    let m = |p: &str, f: &str| glob_match(p, f);
    assert!(m("src/engine/**", "src/engine/a/b.cpp"));
    assert!(m("src/engine/**", "src/engine/x.h"));
    assert!(!m("src/engine/**", "src/engined/x.h"));
    assert!(!m("src/engine/**", "lib/src/engine/x.h"));
    assert!(m("*.hxx", "deep/dir/Widget.HXX"));
    assert!(m("*.hxx", "top.hxx"));
    assert!(!m("*.hxx", "top.hxx.bak"));
    assert!(m("src/*.rs", "src/main.rs"));
    assert!(!m("src/*.rs", "src/a/main.rs"));
    assert!(m("src/**/*.rs", "src/main.rs"));
    assert!(m("src/**/*.rs", "src/a/b/c.rs"));
    assert!(m("src/engine", "src/engine/core/x.cpp"), "a literal directory covers everything below it");
    assert!(m("Cargo.toml", "Cargo.toml"));
    assert!(m("cargo.toml", "sub/Cargo.toml"), "a pattern without a slash matches the name at any depth");
    assert!(m("src/a?.rs", "src/ab.rs"));
    assert!(!m("src/a?.rs", "src/abc.rs"));
    assert!(m("src\\engine\\**", "src\\engine\\x.cpp"), "backslashes are separators");
    assert!(m("SRC/Engine/**", "src/engine/x.cpp"));
    assert!(m("**/test_*.py", "a/b/test_util.py"));

    assert_eq!(normalize_globs(&[".\\src\\engine\\".into(), "/docs/*.md".into(), "  ".into(), "*.hxx".into(), "*.hxx".into()]).unwrap(), vec![
        "src/engine/**",
        "docs/*.md",
        "*.hxx"
    ]);
    assert!(normalize_globs(&["C:/abs/path".into()]).is_err());
    assert!(normalize_globs(&["../outside/**".into()]).is_err());
}

#[test]
fn relpath_handles_separators_case_and_prefixes() {
    assert_eq!(rel_to_root("C:\\Work\\Demo\\src\\Engine\\a.cpp", "c:/work/demo").as_deref(), Some("src/Engine/a.cpp"));
    assert_eq!(rel_to_root("c:/work/demo/src/a.rs", "C:\\Work\\Demo\\").as_deref(), Some("src/a.rs"));
    assert_eq!(rel_to_root("\\\\?\\C:\\work\\demo\\x.rs", "C:/work/demo").as_deref(), Some("x.rs"));
    assert_eq!(rel_to_root("/home/u/demo/a/b.rs", "/home/u/demo").as_deref(), Some("a/b.rs"));
    assert_eq!(rel_to_root("/home/u/demo-two/a.rs", "/home/u/demo"), None);
    assert_eq!(rel_to_root("/home/u/demo", "/home/u/demo"), None);
    assert_eq!(rel_to_root("/elsewhere/a.rs", "/home/u/demo"), None);
}

#[test]
fn file_touch_injects_matching_notes_once_per_session() {
    let (d, v, proj) = project_vault();
    let plain = add_scoped(&v, "Engine uses fixed timestep", "note", Some("demo"), &["src/engine/**"]);
    let gotcha = add_scoped(&v, "Engine allocator is not thread safe", "gotcha", Some("demo"), &["src/engine/**"]);
    let header = add_scoped(&v, "Headers use pragma once", "feedback", None, &["*.hxx"]);
    add_scoped(&v, "Unscoped note", "note", Some("demo"), &[]);
    add_scoped(&v, "Other area", "gotcha", Some("demo"), &["docs/**"]);

    let file = proj.join("src").join("engine").join("loop.cpp");
    let win_style = file.to_string_lossy().replace('/', "\\").to_uppercase();
    let line = touch(&v, &proj, &win_style, None).expect("matching notes");
    assert!(line.starts_with("breadcrumb notes for SRC/ENGINE/LOOP.CPP: "), "{line}");
    let gi = line.find(&gotcha.id).unwrap();
    let pi = line.find(&plain.id).unwrap();
    assert!(gi < pi, "gotchas come first: {line}");
    assert!(line.contains(&format!("{} gotcha Engine allocator is not thread safe", gotcha.id)));
    assert!(!line.contains("Unscoped") && !line.contains("Other area") && !line.contains('\n'));

    let file = file.to_string_lossy().to_string();
    let first = touch(&v, &proj, &file, Some("s1")).unwrap();
    assert_eq!(first.matches("; ").count(), 1, "{first}");
    assert_eq!(touch(&v, &proj, &file, Some("s1")), None, "same session never repeats a note");
    let hxx = proj.join("src").join("engine").join("Widget.hxx");
    let h = touch(&v, &proj, &hxx.to_string_lossy(), Some("s1")).unwrap();
    assert!(h.contains(&header.id) && !h.contains(&gotcha.id), "only the not yet shown global note: {h}");
    assert!(touch(&v, &proj, &file, Some("s2")).is_some(), "a new session starts fresh");

    let rel = touch(&v, &proj, "src/engine/rel.cpp", Some("s3")).unwrap();
    assert!(rel.starts_with("breadcrumb notes for src/engine/rel.cpp: "), "relative paths resolve against cwd: {rel}");
    assert_eq!(touch(&v, &proj, &proj.join("README.md").to_string_lossy(), Some("s4")), None);
    assert_eq!(touch(&v, &proj, &d.path().join("elsewhere.cpp").to_string_lossy(), Some("s4")), None);

    for i in 0..7 {
        add_scoped(&v, &format!("Extra engine fact {i}"), "note", Some("demo"), &["src/engine/**"]);
    }
    let many = touch(&v, &proj, &file, Some("s5")).unwrap();
    assert_eq!(many.matches("; ").count(), 4, "at most five notes: {many}");
    assert!(many.contains(&gotcha.id));

    let sessions = v.root.join(".local").join("sessions");
    assert!(sessions.join("s1.json").exists());
    let o = breadcrumb(&v.root, &["hook", "file-touch"], "{not json", &proj);
    assert!(o.status.success() && o.stdout.is_empty());
    let input = json!({"session_id": "../../evil", "cwd": proj, "tool_input": {"file_path": file}}).to_string();
    let o = breadcrumb(&v.root, &["hook", "file-touch"], &input, &proj);
    assert!(o.status.success());
    assert!(!v.root.join("evil.json").exists());
}

#[test]
fn notebook_path_and_cli_path_globs() {
    let (_d, v, proj) = project_vault();
    let o = breadcrumb(&v.root, &["add", "-t", "Notebook kernels need restart", "-k", "gotcha", "-p", "demo", "--path-glob", "notebooks/", "--path-glob", "*.ipynb"], "", &proj);
    let out = stdout(&o);
    let id = out.split_whitespace().nth(1).unwrap().to_string();
    let n = v.get(&id).unwrap();
    assert_eq!(n.path_globs(), vec!["notebooks/**", "*.ipynb"]);
    stdout(&breadcrumb(&v.root, &["update", &id, "--path-glob", "-*.ipynb", "--path-glob", "lab/**"], "", &proj));
    assert_eq!(v.get(&id).unwrap().path_globs(), vec!["notebooks/**", "lab/**"]);

    let nb = proj.join("lab").join("a.ipynb");
    let input = json!({"cwd": proj, "session_id": "nb", "tool_name": "NotebookEdit", "tool_input": {"notebook_path": nb}}).to_string();
    let line = context(&breadcrumb(&v.root, &["hook", "file-touch"], &input, &proj), "PreToolUse").unwrap();
    assert!(line.contains(&id), "{line}");

    let o = breadcrumb(&v.root, &["add", "-t", "Bad", "-p", "demo", "--path-glob", "C:/abs/**"], "", &proj);
    assert!(!o.status.success());
}

#[test]
fn mcp_paths_and_verify() {
    let (d, v, proj) = project_vault();
    let srv = breadcrumb::mcp::Server { vault_root: v.root.clone(), cwd: proj.clone() };
    let args = |j: Value| j.as_object().unwrap().clone();
    let out = srv.call_tool("add", &args(json!({"title": "Shaders compile at startup", "kind": "gotcha", "paths": ["src/render/**", "*.glsl"]}))).unwrap();
    let id = out.split_whitespace().nth(1).unwrap().to_string();
    assert_eq!(v.get(&id).unwrap().path_globs(), vec!["src/render/**", "*.glsl"]);
    srv.call_tool("update", &args(json!({"id": id, "paths": ["src/gfx/**"]}))).unwrap();
    assert_eq!(v.get(&id).unwrap().path_globs(), vec!["src/gfx/**"]);

    age(&v, &id, 100);
    let before = v.get(&id).unwrap();
    let out = srv.call_tool("update", &args(json!({"id": id, "verify": true}))).unwrap();
    assert!(out.starts_with("verified "), "{out}");
    let after = v.get(&id).unwrap();
    assert_eq!(after.updated, before.updated, "verify only bumps verified");
    assert_eq!(after.verified().unwrap().date_naive(), breadcrumb::util::now().date_naive());
    assert!(!breadcrumb::recall::is_stale(&after, 60));
    let _ = d;
}

#[test]
fn first_prompt_recall_only_once_and_skips_digest_notes() {
    let (_d, v, proj) = project_vault();
    let shader = add_scoped(&v, "Shader cache invalidation on driver update", "gotcha", Some("demo"), &[]);
    ops::update(&v, &shader.id, UpdateReq { body: Some("The shader cache keeps stale binaries after a driver update.".into()), ..Default::default() }).unwrap();
    age(&v, &shader.id, 10);
    let pinned = add_scoped(&v, "Shader compiler flags", "note", Some("demo"), &[]);
    ops::update(&v, &pinned.id, UpdateReq { pinned: Some(true), ..Default::default() }).unwrap();
    for i in 0..15 {
        add_scoped(&v, &format!("Unrelated filler {i}"), "note", Some("demo"), &[]);
    }

    let start = json!({"cwd": proj, "session_id": "p1", "source": "startup"}).to_string();
    let digest = context(&breadcrumb(&v.root, &["hook", "session-start"], &start, &proj), "SessionStart").unwrap();
    assert!(digest.contains(&pinned.id));

    assert_eq!(prompt(&v, &proj, "hi there", "p1"), None, "short prompts are skipped");
    assert_eq!(prompt(&v, &proj, "/compact keep the shader stuff", "p1"), None, "slash commands are skipped");
    let line = prompt(&v, &proj, "Why does the shader cache break after a driver update?", "p1").expect("recall");
    assert!(line.starts_with("breadcrumb may be relevant: "), "{line}");
    assert!(line.contains(&format!("{} gotcha Shader cache invalidation on driver update", shader.id)), "{line}");
    assert!(!line.contains(&pinned.id), "notes in the digest are not repeated: {line}");
    assert!(!line.contains("filler"));
    assert_eq!(prompt(&v, &proj, "Why does the shader cache break after a driver update?", "p1"), None, "only the first prompt");

    assert_eq!(prompt(&v, &proj, "please write a haiku about autumn leaves", "p2"), None, "silent when nothing scores well");
    assert_eq!(prompt(&v, &proj, "shader cache driver update problem again", "p2"), None, "the first prompt slot was used");
}

#[test]
fn stale_notes_are_marked_in_digest_hooks_and_doctor() {
    let (_d, v, proj) = project_vault();
    let old = add_scoped(&v, "Old engine fact", "note", Some("demo"), &["src/engine/**"]);
    let fresh = add_scoped(&v, "Fresh engine fact", "note", Some("demo"), &["src/engine/**"]);
    let todo = add_scoped(&v, "Old todo", "todo", Some("demo"), &[]);
    assert!(v.get(&fresh.id).unwrap().verified().is_some(), "create sets verified");
    age(&v, &old.id, 90);
    age(&v, &todo.id, 90);

    let digest = ops::context(&v, Some("demo"), 15).unwrap();
    let line_of = |id: &str| digest.lines().find(|l| l.contains(id)).unwrap().to_string();
    assert!(line_of(&old.id).ends_with(" (stale?)"), "{digest}");
    assert!(!line_of(&fresh.id).contains("stale"));
    assert!(!line_of(&todo.id).contains("stale"), "todos are exempt");

    let file = proj.join("src").join("engine").join("x.cpp");
    let line = touch(&v, &proj, &file.to_string_lossy(), None).unwrap();
    assert!(line.contains("Old engine fact (stale?)") && !line.contains("Fresh engine fact (stale?)"), "{line}");

    let doc = stdout(&breadcrumb(&v.root, &["doctor"], "", &proj));
    assert!(doc.contains(&format!("run `breadcrumb verify {}`", old.id)), "{doc}");
    assert!(!doc.contains(&format!("breadcrumb verify {}`", todo.id)));

    std::fs::write(v.root.join("breadcrumb.toml"), std::fs::read_to_string(v.root.join("breadcrumb.toml")).unwrap().replace("stale_days = 60", "stale_days = 120")).unwrap();
    let v2 = Vault::open(&v.root).unwrap();
    assert!(!ops::context(&v2, Some("demo"), 15).unwrap().contains("(stale?)"));

    let before = v.get(&old.id).unwrap();
    stdout(&breadcrumb(&v.root, &["verify", &old.id[..6]], "", &proj));
    let after = v.get(&old.id).unwrap();
    assert_eq!(after.updated, before.updated);
    assert!(!ops::context(&v, Some("demo"), 15).unwrap().contains("(stale?)"));

    age(&v, &old.id, 90);
    ops::append(&v, &old.id, "Still true.").unwrap();
    assert!(!breadcrumb::recall::is_stale(&v.get(&old.id).unwrap(), 60), "append counts as verification");
}

#[test]
fn doctor_reports_missing_repo_paths() {
    let (_d, v, proj) = project_vault();
    std::fs::write(proj.join("src").join("real.rs"), "").unwrap();
    let n = add_scoped(&v, "File refs", "note", Some("demo"), &["src/engine/**", "gone_dir/**", "*.hxx"]);
    let body = "See `src/real.rs:42` and `src/gone.rs`, docs at `https://example.com/a/b.html`, run `cargo test`, \
                glob `src/**/*.rs`, abs `/etc/hosts.conf`, dir `src/engine/`.";
    ops::update(&v, &n.id, UpdateReq { body: Some(body.into()), ..Default::default() }).unwrap();
    assert_eq!(breadcrumb::doctor::file_refs(body), vec!["src/real.rs", "src/gone.rs"]);

    let doc = stdout(&breadcrumb(&v.root, &["doctor"], "", &proj));
    assert!(doc.contains("mentions `src/gone.rs`"), "{doc}");
    assert!(doc.contains("mentions `gone_dir`"), "{doc}");
    assert!(!doc.contains("src/real.rs") && !doc.contains("example.com") && !doc.contains("src/engine"), "{doc}");

    ops::project_add(&v, "nowhere", None, None, &["/definitely/not/here/proj".into()]).unwrap();
    let m = add_scoped(&v, "Elsewhere", "note", Some("nowhere"), &[]);
    ops::update(&v, &m.id, UpdateReq { body: Some("`src/gone.rs`".into()), ..Default::default() }).unwrap();
    let doc = stdout(&breadcrumb(&v.root, &["doctor"], "", &proj));
    assert_eq!(doc.matches("mentions `src/gone.rs`").count(), 1, "skipped when the project has no checkout here: {doc}");
}

fn write_memory(dir: &Path, file: &str, name: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(file), format!("---\nname: {name}\ndescription: about {name}\ntype: project\n---\n\n{body}\n")).unwrap();
}

#[test]
fn wiki_links_become_links_idempotently() {
    let (d, v) = common::vault();
    let claude = d.path().join("claude-home");
    let mem = claude.join("projects").join("-demo").join("memory");
    write_memory(&mem, "alpha.md", "alpha-notes", "Depends on [[beta-notes]] and [[Gamma]], also [[missing]].");
    write_memory(&mem, "beta.md", "beta-notes", "Plain.");
    write_memory(&mem, "gamma.md", "Gamma", "Back to [[alpha-notes|the alpha]].");
    let cd = claude.to_string_lossy().to_string();
    let import = |quiet: bool| {
        let mut args = vec!["import", "claude", "--claude-dir", cd.as_str()];
        if quiet {
            args.push("--quiet");
        }
        stdout(&breadcrumb(&v.root, &args, "", d.path()))
    };
    import(false);
    let notes = v.load_notes();
    let by = |name: &str| notes.iter().find(|n| n.field("memory_name").and_then(|x| x.as_str()) == Some(name)).unwrap().clone();
    let (a, b, g) = (by("alpha-notes"), by("beta-notes"), by("Gamma"));
    let mut targets: Vec<&str> = a.links.iter().map(|l| l.to.as_str()).collect();
    targets.sort();
    let mut want = vec![b.id.as_str(), g.id.as_str()];
    want.sort();
    assert_eq!(targets, want);
    assert!(a.body.contains("[[beta-notes]]"), "text stays as is");
    assert_eq!(g.links.iter().map(|l| l.to.clone()).collect::<Vec<_>>(), vec![a.id.clone()]);
    assert!(b.links.is_empty());

    let head = common::git(&v.root, &["rev-parse", "HEAD"]);
    assert_eq!(import(true), "", "quiet no-op prints nothing");
    assert_eq!(common::git(&v.root, &["rev-parse", "HEAD"]), head);

    // An import from before memory_name existed: drop the field and the links, the next import restores both.
    let mut old = v.get(&a.id).unwrap();
    old.links.clear();
    old.unset_field("memory_name");
    v.write_note(&mut old).unwrap();
    let out = import(true);
    assert_eq!(out.trim(), "0 created, 1 updated");
    let a2 = v.get(&a.id).unwrap();
    assert_eq!(a2.links.len(), 2);
    assert_eq!(a2.field("memory_name").and_then(|x| x.as_str()), Some("alpha-notes"));
    assert_eq!(import(true), "");
}

#[test]
fn session_end_imports_memory_in_background() {
    let (d, v) = common::vault();
    let claude = d.path().join("claude-home");
    write_memory(&claude.join("projects").join("-demo").join("memory"), "one.md", "one", "Remember [[two]].");
    write_memory(&claude.join("projects").join("-demo").join("memory"), "two.md", "two", "Second.");
    let run = |args: &[&str]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_breadcrumb"));
        c.arg("--vault").arg(&v.root).args(args).current_dir(d.path()).env_remove("BREADCRUMB_HOME").env("BREADCRUMB_CLAUDE_DIR", &claude);
        c.stdin(Stdio::null()).output().unwrap()
    };
    assert!(run(&["hook", "session-end", "--no-import"]).status.success());
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(v.load_notes().is_empty(), "--no-import skips the import");

    assert!(run(&["hook", "session-end"]).status.success());
    let start = Instant::now();
    let sig = v.root.join(".local").join("claude-import.sig");
    while !sig.exists() && start.elapsed().as_secs() < 20 {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let notes = v.load_notes();
    assert_eq!(notes.len(), 2);
    assert!(notes.iter().any(|n| !n.links.is_empty()));
}

fn settings_fixture() -> Value {
    json!({
        "hooks": {
            "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "guard.sh"}]}],
            "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "other prompt hook"}]}]
        }
    })
}

fn read(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn entries_with(s: &Value, event: &str, suffix: &str) -> Vec<Value> {
    s["hooks"][event]
        .as_array()
        .map(|es| es.iter().filter(|e| e["hooks"].as_array().unwrap().iter().any(|h| h["command"].as_str().unwrap().contains(suffix))).cloned().collect())
        .unwrap_or_default()
}

#[test]
fn hooks_install_registers_recall_hooks_idempotently() {
    let (d, v) = common::vault();
    let settings = d.path().join("settings.json");
    std::fs::write(&settings, serde_json::to_string_pretty(&settings_fixture()).unwrap()).unwrap();
    let sp = settings.to_str().unwrap();
    let install = |extra: &[&str]| {
        let mut args = vec!["hooks", "install", "--settings", sp];
        args.extend_from_slice(extra);
        stdout(&breadcrumb(&v.root, &args, "", d.path()))
    };
    install(&[]);
    let s = read(&settings);
    let file = entries_with(&s, "PreToolUse", "hook file-touch");
    assert_eq!(file.len(), 1);
    assert_eq!(file[0]["matcher"], "Read|Edit|Write|MultiEdit|NotebookEdit");
    assert_eq!(file[0]["hooks"][0]["timeout"], 5);
    let pr = entries_with(&s, "UserPromptSubmit", "hook prompt");
    assert_eq!(pr.len(), 1);
    assert!(pr[0].get("matcher").is_none());
    assert_eq!(pr[0]["hooks"][0]["timeout"], 5);
    assert_eq!(s["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "guard.sh");
    assert_eq!(s["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"], "other prompt hook");
    assert!(entries_with(&s, "SessionEnd", "hook session-end")[0]["hooks"][0]["command"].as_str().unwrap().ends_with("hook session-end"));

    let text = std::fs::read_to_string(&settings).unwrap();
    assert!(install(&[]).contains("no change"));
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), text);

    install(&["--no-import"]);
    let s = read(&settings);
    let end = entries_with(&s, "SessionEnd", "hook session-end");
    assert_eq!(end.len(), 1);
    assert!(end[0]["hooks"][0]["command"].as_str().unwrap().ends_with("hook session-end --no-import"));
    assert!(install(&["--no-import"]).contains("no change"));
    install(&[]);
    let s = read(&settings);
    assert_eq!(entries_with(&s, "SessionEnd", "hook session-end").len(), 1);
    assert!(entries_with(&s, "SessionEnd", "hook session-end")[0]["hooks"][0]["command"].as_str().unwrap().ends_with("hook session-end"));

    install(&["--no-recall", "--no-file-notes"]);
    let s = read(&settings);
    assert!(entries_with(&s, "PreToolUse", "hook file-touch").is_empty());
    assert!(entries_with(&s, "UserPromptSubmit", "hook prompt").is_empty());
    assert_eq!(s["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);

    install(&[]);
    let st: Value = serde_json::from_str(&stdout(&breadcrumb(&v.root, &["--json", "hooks", "status", "--settings", sp], "", d.path()))).unwrap();
    assert!(st["prompt"].as_str().unwrap().ends_with("hook prompt"));
    assert!(st["file_touch"].as_str().unwrap().ends_with("hook file-touch"));

    stdout(&breadcrumb(&v.root, &["hooks", "uninstall", "--settings", sp], "", d.path()));
    assert_eq!(read(&settings), settings_fixture());
}

/// Writes notes straight to disk, committing 500 notes one by one would dominate the test time.
fn generate_vault(v: &Vault, n: usize) {
    let dir = v.notes_dir(Some("demo"));
    std::fs::create_dir_all(&dir).unwrap();
    let body = "Some body text about the subsystem and its quirks. ".repeat(12);
    for i in 0..n {
        let id = format!("t{i:07}");
        let paths = if i % 10 == 0 { format!("paths:\n- src/mod{}/**\n- '*.x{i}'\n", i % 50) } else { String::new() };
        let text = format!(
            "---\nid: {id}\ntitle: Generated note {i}\nkind: note\nproject: demo\ntags: []\nlinks: []\nstatus: active\npinned: false\ncreated: 2026-01-01T00:00:00Z\nupdated: 2026-01-01T00:00:00Z\n{paths}---\n\n{body}\n"
        );
        std::fs::write(dir.join(format!("{id}-generated-note-{i}.md")), text).unwrap();
    }
}

#[test]
fn file_touch_is_fast_on_500_notes() {
    let (_d, v, proj) = project_vault();
    generate_vault(&v, 500);
    let file = proj.join("src").join("mod20").join("a.cpp").to_string_lossy().to_string();
    let input = |sid: &str| json!({"cwd": proj, "session_id": sid, "tool_input": {"file_path": file}}).to_string();
    let t = Instant::now();
    let first = context(&breadcrumb(&v.root, &["hook", "file-touch"], &input("t0"), &proj), "PreToolUse").unwrap();
    let cold = t.elapsed();
    assert!(first.contains("Generated note 20"), "{first}");
    let mut warm = vec![];
    for i in 0..5 {
        let t = Instant::now();
        let o = breadcrumb(&v.root, &["hook", "file-touch"], &input(&format!("t{}", i + 1)), &proj);
        warm.push(t.elapsed());
        assert!(context(&o, "PreToolUse").is_some());
    }
    warm.sort();
    let median = warm[warm.len() / 2];
    let miss = Instant::now();
    let o = breadcrumb(&v.root, &["hook", "file-touch"], &json!({"cwd": proj, "session_id": "m", "tool_input": {"file_path": proj.join("zzz.txt")}}).to_string(), &proj);
    let miss = miss.elapsed();
    assert!(context(&o, "PreToolUse").is_none());
    println!("file-touch on 500 notes: cold {cold:?}, warm median {median:?}, no match {miss:?} (debug build, includes process start)");
    assert!(median.as_millis() < 1500, "warm file-touch took {median:?}");
}

#[test]
fn placeholder_paths_are_not_file_refs() {
    let refs = breadcrumb::doctor::ref_spans("see `.github/release-notes/vX.Y.Z.md` and `logs/YYYY-run.txt` but `src/real.rs`");
    let names: Vec<&str> = refs.iter().map(|(_, _, r)| r.as_str()).collect();
    assert_eq!(names, vec!["src/real.rs"]);
}
