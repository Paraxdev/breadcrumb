mod common;

use breadcrumb::backlog::{self, MoveReq, QueuePos};
use breadcrumb::export;
use breadcrumb::importer::{self, ClaudeOpts, FilesOpts, encode_claude_path};
use breadcrumb::mcp::Server;
use breadcrumb::note::Link;
use breadcrumb::ops::{self, FieldInput, Filter, Scope, UpdateReq};
use breadcrumb::serve::{App, Request};
use breadcrumb::vault::Vault;
use serde_json::{Value, json};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

fn write(p: &Path, s: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, s).unwrap();
}

fn commits(v: &Vault) -> usize {
    breadcrumb::git::commit_count(&v.root)
}

fn err<T: std::fmt::Debug>(r: anyhow::Result<T>) -> String {
    format!("{:#}", r.unwrap_err())
}

#[test]
fn link_remove_needs_a_real_prefix_and_still_removes_dangling_links() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "A", "note", None, &[]);
    let b = common::add(&v, "B", "note", None, &[]);
    let c = common::add(&v, "C", "note", None, &[]);
    ops::link(&v, &a.id, &b.id, None, false).unwrap();
    ops::link(&v, &a.id, &c.id, Some("blocks"), false).unwrap();
    assert!(err(ops::link(&v, &a.id, "", None, true)).contains("too short"));
    assert!(err(ops::link(&v, &a.id, &b.id[..1], None, true)).contains("too short"));
    assert_eq!(v.get(&a.id).unwrap().links.len(), 2, "short prefixes must not remove anything");

    ops::delete(&v, &b.id).unwrap();
    let n = ops::link(&v, &a.id, &b.id[..4], None, true).unwrap();
    assert_eq!(n.links, vec![Link { to: c.id.clone(), rel: "blocks".into() }]);
}

#[test]
fn ids_printed_by_list_resolve_for_files_without_a_matching_name_id() {
    let (_d, v) = common::vault();
    write(&v.root.join("global/notes/handwritten.md"), "# Handwritten heading\n\ntext\n");
    write(&v.root.join("global/notes/abcd9999-other.md"), "---\nid: zzzz9999\ntitle: Other\nkind: note\n---\nbody\n");
    let listed = ops::list(&v, &Scope::All, &Filter::default()).unwrap();
    let hand = listed.iter().find(|n| n.title == "Handwritten heading").unwrap();
    assert_eq!(v.get(&hand.id).unwrap().title, "Handwritten heading");
    let appended = ops::append(&v, &hand.id, "more").unwrap();
    assert!(appended.path.unwrap().ends_with("handwritten.md"));
    let other = v.get("zzzz9999").unwrap();
    assert!(other.path.unwrap().ends_with("abcd9999-other.md"));
    assert_eq!(v.resolve_id("zzzz").unwrap(), "zzzz9999");
}

#[test]
fn a_full_id_used_by_two_files_is_ambiguous() {
    let (_d, v) = common::vault();
    let n = common::add(&v, "orig", "note", None, &[]);
    let copy = v.root.join(format!("projects/p/notes/{}-copy.md", n.id));
    fs::create_dir_all(copy.parent().unwrap()).unwrap();
    fs::copy(n.path.as_ref().unwrap(), &copy).unwrap();
    let e = err(ops::delete(&v, &n.id));
    assert!(e.contains("several files") && e.contains("copy"), "{e}");
    assert!(n.path.unwrap().exists() && copy.exists());
}

#[test]
fn non_scalar_reserved_values_survive_a_rewrite() {
    let (_d, v) = common::vault();
    let p = v.root.join("global/notes/abcd2345-src.md");
    write(&p, "---\nid: abcd2345\ntitle: With source map\nkind: note\nsource: {url: http://x}\ncreated: 2026-01-01T00:00:00Z\nupdated: 2026-01-01T00:00:00Z\n---\nbody\n");
    let n = v.get("abcd2345").unwrap();
    assert!(n.warnings.iter().any(|w| w.contains("'source' is not a plain value")), "{:?}", n.warnings);
    ops::append(&v, "abcd2345", "more").unwrap();
    let raw = fs::read_to_string(&p).unwrap();
    assert!(raw.contains("source:") && raw.contains("url: http://x"), "{raw}");
    ops::update(&v, "abcd2345", UpdateReq { fields: vec![("source".into(), FieldInput::Raw("plain".into()))], ..Default::default() }).unwrap();
    let n = v.get("abcd2345").unwrap();
    assert_eq!(n.source.as_deref(), Some("plain"));
    assert!(n.warnings.is_empty(), "{:?}", n.warnings);
}

#[test]
fn append_goes_to_the_end_of_ordinary_notes_and_before_history_of_items() {
    let (_d, v) = common::vault();
    let n = ops::add(&v, ops::AddReq { title: "hist".into(), body: Some("Intro\n\n## History\n\nold\n\n## Notes\n\nlast".into()), ..Default::default() }).unwrap();
    let n = ops::append(&v, &n.id, "APPENDED").unwrap();
    let date = breadcrumb::util::fmt_date(&breadcrumb::util::now());
    assert!(n.body.ends_with(&format!("last\n\n{date}:\nAPPENDED")), "{}", n.body);
    let t = common::add(&v, "item", "task", None, &[]);
    backlog::queue(&v, &t.id, QueuePos::End, None).unwrap();
    let t = ops::append(&v, &t.id, "detail").unwrap();
    let (i, h) = (t.body.find("detail").unwrap(), t.body.find("## History").unwrap());
    assert!(i < h, "{}", t.body);
}

#[test]
fn import_files_keeps_links_status_pinned_id_and_provenance() {
    let (d, v) = common::vault();
    let target = common::add(&v, "target", "note", None, &[]);
    let src = d.path().join("src");
    write(
        &src.join("n.md"),
        &format!(
            "---\nid: wxyz2345\ntitle: Imported\nkind: note\nlinks: [{{to: {}, rel: blocks}}]\nstatus: archived\npinned: true\nsource: original provenance\n---\nbody\n",
            target.id
        ),
    );
    let rep = importer::import_files(&v, &FilesOpts { path: src.clone(), project: None, kind: None, tags: vec![], dry_run: false }).unwrap();
    assert_eq!(rep.counts()["global"].created, 1);
    let n = v.get("wxyz2345").unwrap();
    assert_eq!(n.links, vec![Link { to: target.id.clone(), rel: "blocks".into() }]);
    assert_eq!(n.status, "archived");
    assert!(n.pinned);
    assert_eq!(n.field("original_source").and_then(|x| x.as_str()), Some("original provenance"));
    assert!(n.source.unwrap().ends_with("src/n.md"));
}

fn req(method: &str, path: &str, body: Option<Value>) -> Request {
    let mut headers = vec![("Host".to_string(), "127.0.0.1:7878".to_string())];
    if body.is_some() {
        headers.push(("Content-Type".into(), "application/json".into()));
    }
    Request { method: method.into(), path: path.into(), query: vec![], headers, body: body.map(|b| serde_json::to_vec(&b).unwrap()).unwrap_or_default() }
}

#[test]
fn web_edit_of_a_stale_note_is_rejected_instead_of_reverting_agent_changes() {
    let (_d, v) = common::vault();
    let app = App::new(&v.root, 0);
    let n = ops::add(&v, ops::AddReq { title: "shared".into(), body: Some("original".into()), ..Default::default() }).unwrap();
    let loaded = app.handle(&req("GET", &format!("/api/note/{}", n.id), None)).body_json();
    let version = loaded["version"].as_str().unwrap().to_string();
    ops::append(&v, &n.id, "agent fact").unwrap();
    let r = app.handle(&req("PATCH", &format!("/api/note/{}", n.id), Some(json!({"body": "original", "expected_version": version}))));
    assert_eq!(r.status, 409, "{}", String::from_utf8_lossy(&r.body));
    assert!(v.get(&n.id).unwrap().body.contains("agent fact"));

    let fresh = app.handle(&req("GET", &format!("/api/note/{}", n.id), None)).body_json();
    let r = app.handle(&req("PATCH", &format!("/api/note/{}", n.id), Some(json!({"title": "shared 2", "tags": ["a", "b"], "expected_version": fresh["version"]}))));
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(v.get(&n.id).unwrap().tags, vec!["a", "b"]);
}

#[test]
fn web_edit_keeps_links_to_deleted_notes_and_rejected_needs_a_resolution() {
    let (_d, v) = common::vault();
    let app = App::new(&v.root, 0);
    let p = common::add(&v, "P", "note", None, &[]);
    let q = common::add(&v, "Q", "note", None, &[]);
    ops::link(&v, &p.id, &q.id, None, false).unwrap();
    ops::delete(&v, &q.id).unwrap();
    let r = app.handle(&req("PATCH", &format!("/api/note/{}", p.id), Some(json!({"title": "P edited", "links": [{"rel": "relates", "to": q.id}]}))));
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(v.get(&p.id).unwrap().links.len(), 1);

    let t = common::add(&v, "T", "task", None, &[]);
    let r = app.handle(&req("PATCH", &format!("/api/note/{}", t.id), Some(json!({"fields": {"state": "rejected"}}))));
    assert_eq!(r.status, 400);
    assert!(String::from_utf8_lossy(&r.body).contains("resolution"));
    assert!(err(ops::update(&v, &t.id, UpdateReq { fields: vec![("state".into(), FieldInput::Raw("rejected".into()))], ..Default::default() })).contains("resolution"));
    let ok = ops::update(
        &v,
        &t.id,
        UpdateReq { fields: vec![("state".into(), FieldInput::Raw("rejected".into())), ("resolution".into(), FieldInput::Raw("dup".into()))], ..Default::default() },
    )
    .unwrap();
    assert_eq!(backlog::state(&ok), "rejected");
}

#[test]
fn json_output_never_drops_fields_named_like_output_keys() {
    let (_d, v) = common::vault();
    let e = err(ops::add(&v, ops::AddReq { title: "k4".into(), fields: vec![("path".into(), FieldInput::Raw("src/main.rs".into()))], ..Default::default() }));
    assert!(e.contains("JSON output"), "{e}");
    write(&v.root.join("global/notes/abcd3456-k.md"), "---\nid: abcd3456\ntitle: k\nkind: note\npath: src/main.rs\nbody: hello\n---\nrealbody\n");
    let j = v.get("abcd3456").unwrap().to_json();
    assert_eq!(j["shadowed_fields"]["path"], "src/main.rs");
    assert_eq!(j["shadowed_fields"]["body"], "hello");
    assert_eq!(j["body"], "realbody");
}

#[test]
fn kind_file_name_must_match_its_declared_name() {
    let (_d, v) = common::vault();
    write(&v.kinds_dir().join("foo.toml"), "name = \"bar\"\ndescription = \"x\"\n");
    assert!(err(ops::add(&v, ops::AddReq { title: "x".into(), kind: Some("foo".into()), ..Default::default() })).contains("declares name 'bar'"));
    assert!(err(ops::add(&v, ops::AddReq { title: "x".into(), kind: Some("bar".into()), ..Default::default() })).contains("unknown kind"));
    assert!(breadcrumb::doctor::check(&v).iter().any(|p| p.message.contains("declares name 'bar'")));
    ops::define_kind(&v, breadcrumb::kind::KindDef::new("bar", "new"), false, &[]).unwrap();
    assert_eq!(v.load_kinds().iter().filter(|k| k.name == "bar").count(), 1);
    assert!(err(ops::add(&v, ops::AddReq { title: "x".into(), kind: Some("TODO".into()), ..Default::default() })).contains("did you mean 'todo'"));
}

#[test]
fn field_spec_defaults_and_descriptions_may_contain_colons() {
    let f = breadcrumb::kind::parse_field_spec("home:url:default=https://example.com").unwrap();
    assert_eq!(f.default, Some(toml::Value::String("https://example.com".into())));
    let f = breadcrumb::kind::parse_field_spec("at:date:required:default=2026-01-01T10:00:00Z:desc=when: exactly").unwrap();
    assert!(f.required);
    assert_eq!(f.default, Some(toml::Value::String("2026-01-01T10:00:00Z".into())));
    assert_eq!(f.description.as_deref(), Some("when: exactly"));
    assert!(breadcrumb::kind::parse_field_spec("x:string:bogus").is_err());
}

#[test]
fn nothing_is_written_while_a_git_rebase_is_unfinished() {
    let (_d, v) = common::vault();
    common::add(&v, "before", "note", None, &[]);
    let marker = v.root.join(".git/rebase-merge");
    fs::create_dir_all(&marker).unwrap();
    let files = v.note_files().len();
    assert!(err(ops::add(&v, ops::AddReq { title: "during rebase".into(), ..Default::default() })).contains("rebase is in progress"));
    assert_eq!(v.note_files().len(), files, "the note must not be written");
    assert!(err(export::sync(&v, None)).contains("rebase is in progress"));
    assert!(marker.exists(), "sync must not abort a rebase it did not start");
    fs::remove_dir_all(&marker).unwrap();
    common::add(&v, "after", "note", None, &[]);
}

#[test]
fn git_commands_never_fall_through_to_an_enclosing_repository() {
    let d = tempfile::tempdir().unwrap();
    let parent = d.path().join("parent");
    fs::create_dir_all(&parent).unwrap();
    common::git(&parent, &["init", "-q"]);
    write(&parent.join("a.txt"), "a");
    common::git(&parent, &["add", "-A"]);
    common::git(&parent, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "one"]);
    let vroot = parent.join("vault");
    write(&vroot.join(breadcrumb::vault::CONFIG_FILE), "version = 1\n");
    write(&parent.join("other.txt"), "unrelated");
    let v = Vault::open(&vroot).unwrap();
    assert!(err(export::sync(&v, None)).contains("not a git repository"));
    assert!(err(export::bundle(&v, &d.path().join("b.bundle"))).contains("not a git repository"));
    assert_eq!(breadcrumb::git::commit_count(&parent), 1);
    assert!(export::where_info(&vroot, d.path()).git.contains("not a git repository"));
}

#[test]
fn exports_into_the_vault_are_refused_and_restore_errors_are_clear() {
    let (d, v) = common::vault();
    common::add(&v, "x", "note", None, &[]);
    assert!(err(export::bundle(&v, &v.root.join("backup.bundle"))).contains("inside the vault"));
    assert!(err(export::jsonl(&v, &v.root.join("sub/dump.jsonl"))).contains("inside the vault"));
    let b = export::bundle(&v, &d.path().join("ok.bundle")).unwrap();
    assert_eq!(export::where_info(&v.root, d.path()).git, "ok");

    let file = d.path().join("r3");
    fs::write(&file, "f").unwrap();
    assert!(err(export::restore(&b, &file)).contains("is a file"));

    let plain = d.path().join("plain");
    fs::create_dir_all(&plain).unwrap();
    common::git(&plain, &["init", "-q"]);
    write(&plain.join("x.txt"), "x");
    common::git(&plain, &["add", "-A"]);
    common::git(&plain, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "x"]);
    let pb = d.path().join("plain.bundle");
    common::git(&plain, &["bundle", "create", &pb.to_string_lossy(), "--all"]);
    let target = d.path().join("r4");
    assert!(err(export::restore(&pb, &target)).contains("not a breadcrumb vault"));
    assert!(!target.exists(), "a failed restore must not leave the clone behind");
}

#[test]
fn relative_local_remote_is_recorded_as_an_absolute_path() {
    let d = tempfile::tempdir().unwrap();
    common::git(d.path(), &["init", "-q", "--bare", "remote.git"]);
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_breadcrumb"))
        .current_dir(d.path())
        .args(["--vault", "rel", "init", "--remote", "remote.git"])
        .env_remove("BREADCRUMB_HOME")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v = Vault::open(&d.path().join("rel")).unwrap();
    let remote = v.config.remote.clone().unwrap();
    assert!(Path::new(&remote).is_absolute(), "{remote}");
    common::add(&v, "x", "note", None, &[]);
    assert!(export::sync(&v, None).unwrap().pushed);
}

fn mcp_run(srv: &Server, input: Vec<u8>) -> Vec<Value> {
    let mut out = Vec::new();
    srv.serve(Cursor::new(input), &mut out).unwrap();
    String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn call(id: u64, name: &str, args: Value) -> Vec<u8> {
    let mut b = serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": args}})).unwrap();
    b.push(b'\n');
    b
}

fn text(v: &Value) -> String {
    v["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string()
}

#[test]
fn mcp_survives_invalid_utf8() {
    let (d, v) = common::vault();
    let srv = Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let mut input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"x\":\"\xff\xfe\"}\n".to_vec();
    input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\r\n");
    let out = mcp_run(&srv, input);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0]["error"]["code"], -32700);
    assert_eq!(out[1]["id"], 2);
}

#[test]
fn mcp_duplicate_hint_needs_two_shared_title_words_and_lists_exact_titles_first() {
    let (d, v) = common::vault();
    let srv = Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let mut input = call(1, "add", json!({"title": "Parser task"}));
    input.extend(call(2, "add", json!({"title": "Blocker task"})));
    for (i, t) in ["conc 1 1", "conc 2 1", "conc 3 1", "conc 4 1", "conc 5 1", "conc 1 11", "conc 1 1"].iter().enumerate() {
        input.extend(call(10 + i as u64, "add", json!({"title": t})));
    }
    let out = mcp_run(&srv, input);
    assert!(!text(&out[1]).contains("possible duplicates"), "one shared word is not a duplicate: {}", text(&out[1]));
    assert!(text(&out[3]).contains("possible duplicates"), "{}", text(&out[3]));
    let first_dup = text(&out[2]).split_whitespace().nth(1).unwrap().to_string();
    let last = text(&out[8]);
    let hint = last.lines().nth(1).unwrap_or_default();
    let first_hint = hint.split_once(": ").map(|x| x.1).unwrap_or_default();
    assert!(first_hint.starts_with(&format!("{first_dup} \"conc 1 1\"")), "{last}");
    assert_eq!(hint.matches('"').count(), 4, "at most 2 similar notes: {last}");
}

#[test]
fn mcp_list_and_search_say_when_results_are_cut() {
    let (d, v) = common::vault();
    for i in 0..3 {
        common::add(&v, &format!("concurrency note {i}"), "note", None, &[]);
    }
    let srv = Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let mut input = call(1, "list", json!({"limit": 2}));
    input.extend(call(2, "search", json!({"query": "concurrency", "limit": 2})));
    let out = mcp_run(&srv, input);
    assert!(text(&out[0]).ends_with("\n1 more, narrow with filters\n"), "{}", text(&out[0]));
    assert!(text(&out[1]).ends_with("\n1 more, refine the query or narrow with filters\n"), "{}", text(&out[1]));
    let ctx = breadcrumb::mcp::tool_defs().into_iter().find(|t| t["name"] == "context").unwrap();
    assert!(ctx["inputSchema"]["properties"].get("project").is_some());
}

#[test]
fn backlog_tools_treat_plain_notes_as_plain_notes() {
    let (_d, v) = common::vault();
    let g = common::add(&v, "Deploy uses docker compose", "gotcha", None, &[]);
    assert!(err(backlog::tree(&v, &g.id)).contains("not a backlog item"));
    assert!(err(backlog::deps(&v, &g.id)).contains("not a backlog item"));
    let e = err(backlog::move_item(&v, &g.id, MoveReq { state: "done".into(), ..Default::default() }));
    assert!(e.contains("update with kind") && !e.contains("with update or promote first"), "{e}");

    let r = common::add(&v, "R", "note", None, &[]);
    let t = common::add(&v, "D", "task", None, &[]);
    ops::link(&v, &t.id, &r.id, Some("depends-on"), false).unwrap();
    backlog::queue(&v, &t.id, QueuePos::End, None).unwrap();
    assert_eq!(backlog::next(&v, &Scope::Global, None, None).unwrap().map(|n| n.id), Some(t.id.clone()), "a plain note never blocks");
}

#[test]
fn noop_moves_do_not_commit_and_archived_children_do_not_count() {
    let (_d, v) = common::vault();
    let t = common::add(&v, "T", "task", None, &[]);
    let before = commits(&v);
    let r = backlog::move_item(&v, &t.id, MoveReq { state: "backlog".into(), ..Default::default() }).unwrap();
    assert_eq!((r.from.as_str(), r.to.as_str()), ("backlog", "backlog"));
    assert_eq!(commits(&v), before);

    let e = common::add(&v, "E", "epic", None, &[]);
    let a = common::add(&v, "A", "task", None, &[("parent", &e.id)]);
    common::add(&v, "B", "task", None, &[("parent", &e.id)]);
    ops::set_status(&v, &a.id, "archived").unwrap();
    let all = v.load_notes();
    assert_eq!(backlog::Index::new(&all).progress(&e.id), Some((0, 1)));
}

fn mem(c: &Path, dir: &str, file: &str, content: &str) {
    write(&c.join("projects").join(dir).join("memory").join(file), content);
}

fn encoded(p: &Path) -> String {
    encode_claude_path(&breadcrumb::util::path_str(&fs::canonicalize(p).unwrap()))
}

#[test]
fn claude_import_maps_dirs_to_existing_projects_and_never_merges_unrelated_ones() {
    let (d, v) = common::vault();
    let base = d.path().join("work");
    let cadapp = base.join("cadapp");
    let (src_a, src_b) = (base.join("repoa").join("src"), base.join("repob").join("src"));
    for p in [&cadapp, &src_a, &src_b] {
        fs::create_dir_all(p).unwrap();
    }
    ops::project_add(&v, "neo", None, None, &[breadcrumb::util::path_str(&fs::canonicalize(&cadapp).unwrap())]).unwrap();
    let c = d.path().join("claude");
    mem(&c, &encoded(&cadapp), "a.md", "---\nname: neo fact\ntype: project\n---\nx\n");
    mem(&c, &encoded(&src_a), "a.md", "---\nname: repo a fact\ntype: project\n---\na\n");
    mem(&c, &encoded(&src_b), "b.md", "---\nname: repo b fact\ntype: project\n---\nb\n");
    let rep = importer::import_claude(&v, &ClaudeOpts { claude_dir: c, dry_run: false, project_map: vec![] }).unwrap();
    assert!(!rep.projects_created.contains(&"cadapp".to_string()), "{:?}", rep.projects_created);
    let notes = v.load_notes();
    let proj = |t: &str| notes.iter().find(|n| n.title == t).unwrap().project.clone().unwrap();
    assert_eq!(proj("neo fact"), "neo");
    assert_ne!(proj("repo a fact"), proj("repo b fact"), "two unrelated src dirs must not share a project");
}

#[test]
fn claude_import_keeps_identical_memories_per_project_and_drops_stale_fields() {
    let (d, v) = common::vault();
    let c = d.path().join("claude");
    let same = "---\nname: no dashes\ndescription: OLDSUMMARY zebraword\ntype: feedback\n---\nUse commas.\n";
    mem(&c, "D--zz-nonexistent-alpha", "no-dashes.md", same);
    mem(&c, "D--zz-nonexistent-beta", "no-dashes.md", same);
    let opts = ClaudeOpts { claude_dir: c.clone(), dry_run: false, project_map: vec![] };
    let rep = importer::import_claude(&v, &opts).unwrap();
    let counts = rep.counts();
    assert_eq!((counts["alpha"].created, counts["beta"].created), (1, 1));

    mem(&c, "D--zz-nonexistent-alpha", "no-dashes.md", "---\nname: no dashes\ntype: user\n---\nUse commas.\n");
    let rep = importer::import_claude(&v, &opts).unwrap();
    assert_eq!(rep.counts()["alpha"].updated, 1);
    let n = v.load_notes().into_iter().find(|n| n.project.as_deref() == Some("alpha")).unwrap();
    assert_eq!(n.kind, "user");
    assert!(n.field("summary").is_none(), "summary removed from the source must go away");
    let _: PathBuf = n.path.unwrap();
}

#[test]
fn verbatim_unc_paths_normalize_to_the_form_the_shell_reports() {
    assert_eq!(breadcrumb::util::path_str(Path::new(r"\\?\UNC\localhost\C$\proj")), "//localhost/C$/proj");
    assert_eq!(breadcrumb::util::path_str(Path::new(r"\\?\D:\dev\x")), "D:/dev/x");
    assert_eq!(breadcrumb::project::normalize_path(r"\\?\UNC\Host\Share\P"), breadcrumb::project::normalize_path("//Host/Share/P"));
}

#[test]
fn unpinned_prefs_get_one_line_and_doctor_info() {
    let (_d, v) = common::vault();
    let old = common::add(&v, "Prefers tabs over spaces", "feedback", None, &[]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let new = common::add(&v, "Works on a laptop with a small screen", "user", None, &[]);
    let pinned = common::add(&v, "Never force push", "feedback", None, &[]);
    breadcrumb::ops::update(&v, &pinned.id, breadcrumb::ops::UpdateReq { pinned: Some(true), body: Some("Ask first.".into()), ..Default::default() }).unwrap();
    common::add(&v, "Plain fact", "note", None, &[]);
    let ctx = breadcrumb::ops::context(&v, None, 15).unwrap();
    let line = ctx.lines().find(|l| l.starts_with("Prefs")).unwrap_or_else(|| panic!("{ctx}"));
    assert!(line.find(&new.id).unwrap() < line.find(&old.id).unwrap(), "most recent first: {line}");
    assert!(!line.contains(&pinned.id) && ctx.contains(&format!("{} feedback Never force push\n", pinned.id)) && !ctx.contains("Ask first."));
    assert_eq!(ctx.matches(&old.id).count(), 1, "not repeated under Recent: {ctx}");

    for i in 0..30 {
        common::add(&v, &format!("Preference number {i} with a reasonably long title"), "feedback", None, &[]);
    }
    let ctx = breadcrumb::ops::context(&v, None, 15).unwrap();
    let line = ctx.lines().find(|l| l.starts_with("Prefs")).unwrap();
    assert!(line.chars().count() <= breadcrumb::ops::PREFS_LINE_MAX && line.ends_with("more"), "{line}");

    let probs = breadcrumb::doctor::check(&v);
    let info: Vec<_> = probs.iter().filter(|p| p.severity == "info").collect();
    assert_eq!(info.len(), 32);
    assert!(info.iter().all(|p| p.message.contains("--pin")));
    assert!(!probs.iter().any(|p| p.severity == "warning" && p.id.as_deref() == Some(old.id.as_str())));
    assert!(breadcrumb::doctor::text(&probs, 35).contains("32 info"));

    let srv = breadcrumb::mcp::Server { vault_root: v.root.clone(), cwd: v.root.clone() };
    let mut a = serde_json::Map::new();
    a.insert("title".into(), "Likes short commit subjects".into());
    a.insert("kind".into(), "feedback".into());
    a.insert("project".into(), "global".into());
    assert!(srv.call_tool("add", &a).unwrap().contains(breadcrumb::ops::PIN_HINT));
    a.insert("title".into(), "Some plain note".into());
    a.insert("kind".into(), "note".into());
    assert!(!srv.call_tool("add", &a).unwrap().contains("pin it"));
}
