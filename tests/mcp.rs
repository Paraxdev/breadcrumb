mod common;

use breadcrumb::mcp::Server;
use serde_json::{Value, json};
use std::io::Cursor;

fn run(srv: &Server, msgs: &[Value], raw_extra: &[&str]) -> Vec<Value> {
    let mut input = String::new();
    for m in msgs {
        input.push_str(&serde_json::to_string(m).unwrap());
        input.push('\n');
    }
    for r in raw_extra {
        input.push_str(r);
        input.push('\n');
    }
    let mut out = Vec::new();
    srv.serve(Cursor::new(input), &mut out).unwrap();
    String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).expect("stdout must carry only JSON-RPC")).collect()
}

fn call(id: u64, name: &str, args: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": args}})
}

fn text(resp: &Value) -> &str {
    resp["result"]["content"][0]["text"].as_str().unwrap()
}

#[test]
fn handshake_list_add_search() {
    let (d, v) = common::vault();
    let srv = Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let out = run(
        &srv,
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            call(3, "add", json!({"title": "Release checklist for breadcrumb", "kind": "gotcha", "tags": ["release"], "body": "Tag before pushing.", "fields": {"severity": 3}})),
            call(4, "search", json!({"query": "release checklist"})),
            json!({"jsonrpc": "2.0", "id": 5, "method": "ping"}),
            json!({"jsonrpc": "2.0", "id": 6, "method": "resources/list"}),
            call(7, "add", json!({"title": "x", "kind": "nonexistent"})),
            call(8, "context", json!({})),
            call(9, "no_such_tool", json!({})),
        ],
        &["{not json"],
    );
    assert_eq!(out.len(), 10, "the notification must not get a response: {out:#?}");

    let init = &out[0];
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(init["result"]["capabilities"]["tools"]["listChanged"], false);
    assert_eq!(init["result"]["serverInfo"]["name"], "breadcrumb");
    let instructions = init["result"]["instructions"].as_str().unwrap();
    assert!(instructions.contains("context") && instructions.chars().count() < 400, "{instructions}");

    let tools = out[1]["result"]["tools"].as_array().unwrap();
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort();
    assert_eq!(
        names,
        ["add", "append", "backlog", "context", "delete", "doctor", "get", "kinds", "link", "list", "move", "next", "project", "search", "sync", "update", "workflow"]
    );
    assert!(serde_json::to_string(&out[1]["result"]).unwrap().len() < 6000);
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
        assert!(t["inputSchema"]["properties"].is_object(), "{}", t["name"]);
        assert!(t["description"].as_str().is_some_and(|s| !s.is_empty() && s.matches(". ").count() == 0), "{}", t["name"]);
    }

    assert_eq!(out[2]["result"]["isError"], false);
    let added = text(&out[2]);
    assert!(added.starts_with("saved ") && added.ends_with("\"Release checklist for breadcrumb\"") && !added.contains('\n'), "{added}");
    let id = added.split_whitespace().nth(1).unwrap().to_string();
    let note = v.get(&id).unwrap();
    assert_eq!(note.kind, "gotcha");
    assert_eq!(note.field("severity"), Some(&serde_yaml::Value::Number(3.into())));

    let found = text(&out[3]);
    assert!(found.starts_with(&format!("{id} gotcha Release checklist for breadcrumb\n")), "{found}");
    assert_eq!(out[4]["result"], json!({}));
    assert_eq!(out[5]["error"]["code"], -32601);
    assert_eq!(out[6]["result"]["isError"], true);
    assert!(text(&out[6]).contains("kind"));
    assert!(text(&out[7]).contains("Release checklist"));
    assert_eq!(out[8]["error"]["code"], -32602);
    assert_eq!(out[9]["error"]["code"], -32700);
}

#[test]
fn version_negotiation_and_get_omits_defaults() {
    let (d, v) = common::vault();
    let srv = Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let n = common::add(&v, "Has fields", "todo", None, &[("priority", "p1")]);
    let out = run(
        &srv,
        &[
            json!({"jsonrpc": "2.0", "id": "a", "method": "initialize", "params": {"protocolVersion": "1999-01-01"}}),
            json!({"jsonrpc": "2.0", "id": "b", "method": "initialize", "params": {"protocolVersion": "2025-03-26"}}),
            call(1, "get", json!({"id": &n.id[..4]})),
            call(2, "get", json!({"id": n.id, "full": true})),
            call(3, "delete", json!({"id": n.id})),
            call(4, "delete", json!({"id": n.id, "confirm": true})),
        ],
        &[],
    );
    assert_eq!(out[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(out[1]["result"]["protocolVersion"], "2025-03-26");
    let got = text(&out[2]);
    assert!(got.starts_with("---\n") && got.contains("priority: p1") && got.contains(&format!("id: {}", n.id)), "{got}");
    for k in ["links:", "tags:", "status:", "pinned:", "created:"] {
        assert!(!got.contains(k), "{k} holds a default: {got}");
    }
    let full = text(&out[3]);
    assert!(full.contains("links: []") && full.contains("pinned: false") && full.contains("created:"), "{full}");
    assert_eq!(out[4]["result"]["isError"], true);
    assert_eq!(text(&out[5]), format!("deleted {} \"Has fields\"", n.id));
    assert!(v.get(&n.id).is_err());
}

#[test]
fn backlog_tools_and_kinds_define() {
    let (d, v) = common::vault();
    let srv = Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let out = run(&srv, &[call(1, "add", json!({"title": "Offline mode", "kind": "idea", "project": "app"})), call(2, "add", json!({"title": "x", "project": "app"}))], &[]);
    for o in &out {
        assert_eq!(o["result"]["isError"], true, "an unknown project must not be created by add");
        assert!(text(o).contains("unknown project 'app'"), "{}", text(o));
    }
    assert!(v.load_project("app").unwrap().is_none());
    let out = run(&srv, &[call(1, "project", json!({"action": "link", "slug": "app", "path": d.path().join("appdir").to_string_lossy()})), call(2, "project", json!({}))], &[]);
    assert_eq!(out[0]["result"]["isError"], false, "{}", text(&out[0]));
    assert!(text(&out[0]).starts_with("created project app"));
    assert!(text(&out[1]).contains("\napp\n"), "{}", text(&out[1]));
    let out = run(
        &srv,
        &[
            call(1, "add", json!({"title": "Offline mode", "kind": "idea", "project": "app"})),
            call(2, "kinds", json!({"define": {"name": "runbook", "description": "Ops runbook", "fields": [{"name": "service", "type": "string", "required": true}, "severity:enum:low|high"]}})),
            call(3, "add", json!({"title": "Restart api", "kind": "runbook"})),
            call(4, "add", json!({"title": "Restart api", "kind": "runbook", "fields": {"service": "api", "severity": "high"}})),
            call(5, "backlog", json!({"project": "app"})),
            call(6, "kinds", json!({})),
        ],
        &[],
    );
    assert!(text(&out[0]).starts_with("saved "));
    assert!(text(&out[1]).contains("defined kind runbook"));
    assert_eq!(out[2]["result"]["isError"], true);
    assert_eq!(out[3]["result"]["isError"], false);
    assert!(text(&out[4]).starts_with("idea (1):\n") && text(&out[4]).contains("Offline mode"), "{}", text(&out[4]));
    assert!(text(&out[5]).contains("runbook: Ops runbook ["));
    let idea = v.load_notes().into_iter().find(|n| n.title == "Offline mode").unwrap();
    assert_eq!(breadcrumb::backlog::state(&idea), "idea");
    let other = common::add(&v, "Second", "task", Some("app"), &[]);
    let out = run(
        &srv,
        &[
            call(1, "move", json!({"id": idea.id, "state": "queued"})),
            call(2, "move", json!({"id": other.id, "state": "queued", "position": "top"})),
            call(3, "next", json!({"project": "app", "claim": true, "by": "agent-7"})),
            call(4, "move", json!({"id": idea.id, "state": "rejected"})),
            call(5, "move", json!({"id": idea.id, "state": "done", "position": "top"})),
            call(6, "backlog", json!({"view": "deps", "id": idea.id})),
            call(7, "update", json!({"id": idea.id, "status": "archived"})),
        ],
        &[],
    );
    assert!(text(&out[0]).ends_with("idea -> queued"), "{}", text(&out[0]));
    assert!(text(&out[1]).starts_with(&format!("queued {}", other.id)));
    assert!(text(&out[2]).starts_with("claimed\n---\n") && text(&out[2]).contains("assignee: agent-7") && text(&out[2]).contains("Second"), "{}", text(&out[2]));
    assert_eq!(out[3]["result"]["isError"], true, "rejected without resolution must fail");
    assert_eq!(out[4]["result"]["isError"], true, "position only applies to queued");
    assert!(text(&out[5]).contains("blocked by"));
    assert!(text(&out[6]).starts_with("archived "));
    assert!(v.get(&idea.id).unwrap().is_archived());
}

#[test]
fn compact_rows_and_write_acks() {
    let (d, v) = common::vault();
    let proj = d.path().join("app");
    std::fs::create_dir_all(&proj).unwrap();
    breadcrumb::ops::project_link(&v, "app", &proj, None).unwrap();
    let srv = Server { vault_root: v.root.clone(), cwd: proj.clone() };
    let long = "word ".repeat(30);
    let a = common::add(&v, long.trim(), "decision", Some("app"), &[]);
    let b = common::add(&v, "Global preference", "feedback", None, &[]);
    breadcrumb::ops::update(&v, &b.id, breadcrumb::ops::UpdateReq { body: Some(format!("first line\n\n{}", "lorem ipsum ".repeat(20))), ..Default::default() }).unwrap();
    for i in 0..25 {
        common::add(&v, &format!("Filler {i}"), "note", Some("app"), &[]);
    }
    let out = run(
        &srv,
        &[
            call(1, "list", json!({})),
            call(2, "search", json!({"query": "lorem"})),
            call(3, "append", json!({"id": a.id, "text": "more"})),
            call(4, "link", json!({"from": a.id, "to": b.id, "rel": "depends-on"})),
            call(5, "update", json!({"id": a.id, "tags": ["x", "y"]})),
            call(6, "update", json!({"id": a.id, "tags": ["-x"]})),
            call(7, "context", json!({"kinds": true})),
            call(8, "list", json!({"kind": "feedback"})),
        ],
        &[],
    );
    let list = text(&out[0]);
    let lines: Vec<&str> = list.lines().collect();
    assert_eq!(lines.len(), 21, "{list}");
    assert_eq!(lines[20], "7 more, narrow with filters");
    assert!(lines.iter().all(|l| !l.contains("  ")), "{list}");
    assert!(!list.contains("(app)"));
    let fb = text(&out[7]);
    assert!(fb.starts_with(&format!("{} feedback Global preference (global) 20", b.id)), "{fb}");
    let hit = text(&out[1]);
    let snippet = hit.lines().nth(1).unwrap();
    assert!(snippet.starts_with("  lorem") && snippet.trim().chars().count() <= 100, "{hit}");
    let title = breadcrumb::format::short_title(&a.title);
    assert!(title.chars().count() == 80);
    assert_eq!(text(&out[2]), format!("appended {} \"{title}\"", a.id));
    assert_eq!(text(&out[3]), format!("linked {} depends-on {}", a.id, b.id));
    assert!(text(&out[4]).starts_with("updated "));
    assert_eq!(v.get(&a.id).unwrap().tags, vec!["y".to_string()]);
    let ctx = text(&out[6]);
    assert!(ctx.starts_with("breadcrumb context: app\n") && ctx.contains("\nKinds:\n") && ctx.contains("Recent (14 of "), "{ctx}");
    assert!(!ctx.contains("\n\n") && !ctx.contains('#'), "{ctx}");
}
