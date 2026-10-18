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
    assert!(init["result"]["instructions"].as_str().unwrap().contains("context"));

    let tools = out[1]["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for n in ["context", "search", "list", "get", "add", "update", "append", "link", "archive", "delete", "projects", "project_link", "kinds", "define_kind", "sync", "doctor", "board", "next", "move", "queue", "capture_idea", "tree", "deps"] {
        assert!(names.contains(&n), "missing tool {n}");
    }
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
        assert!(t["description"].as_str().is_some_and(|s| !s.is_empty()));
    }

    assert_eq!(out[2]["result"]["isError"], false);
    let added = text(&out[2]);
    assert!(added.starts_with("added "), "{added}");
    let id = added.split_whitespace().nth(1).unwrap().to_string();
    let note = v.get(&id).unwrap();
    assert_eq!(note.kind, "gotcha");
    assert_eq!(note.field("severity"), Some(&serde_yaml::Value::Number(3.into())));

    let found = text(&out[3]);
    assert!(found.contains(&id) && found.contains("Release checklist"), "{found}");
    assert_eq!(out[4]["result"], json!({}));
    assert_eq!(out[5]["error"]["code"], -32601);
    assert_eq!(out[6]["result"]["isError"], true);
    assert!(text(&out[6]).contains("define_kind"));
    assert!(text(&out[7]).contains("Release checklist"));
    assert_eq!(out[8]["error"]["code"], -32602);
    assert_eq!(out[9]["error"]["code"], -32700);
}

#[test]
fn version_negotiation_and_get_returns_frontmatter() {
    let (d, v) = common::vault();
    let srv = Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let n = common::add(&v, "Has fields", "todo", None, &[("priority", "p1")]);
    let out = run(
        &srv,
        &[
            json!({"jsonrpc": "2.0", "id": "a", "method": "initialize", "params": {"protocolVersion": "1999-01-01"}}),
            json!({"jsonrpc": "2.0", "id": "b", "method": "initialize", "params": {"protocolVersion": "2025-03-26"}}),
            call(1, "get", json!({"id": &n.id[..4]})),
            call(2, "delete", json!({"id": n.id})),
            call(3, "delete", json!({"id": n.id, "confirm": true})),
        ],
        &[],
    );
    assert_eq!(out[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(out[1]["result"]["protocolVersion"], "2025-03-26");
    let got = text(&out[2]);
    assert!(got.starts_with("---\n") && got.contains("priority: p1") && got.contains(&format!("id: {}", n.id)), "{got}");
    assert_eq!(out[3]["result"]["isError"], true);
    assert_eq!(out[4]["result"]["isError"], false);
    assert!(v.get(&n.id).is_err());
}

#[test]
fn backlog_tools_and_define_kind() {
    let (d, v) = common::vault();
    let srv = Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let out = run(&srv, &[call(1, "capture_idea", json!({"title": "Offline mode", "project": "app"})), call(2, "add", json!({"title": "x", "project": "app"}))], &[]);
    for o in &out {
        assert_eq!(o["result"]["isError"], true, "an unknown project must not be created by add or capture_idea");
        assert!(text(o).contains("unknown project 'app'") && text(o).contains("project_link"), "{}", text(o));
    }
    assert!(v.load_project("app").unwrap().is_none());
    let out = run(&srv, &[call(1, "project_link", json!({"slug": "app", "path": d.path().join("appdir").to_string_lossy()}))], &[]);
    assert_eq!(out[0]["result"]["isError"], false, "{}", text(&out[0]));
    let out = run(
        &srv,
        &[
            call(1, "capture_idea", json!({"title": "Offline mode", "project": "app"})),
            call(2, "define_kind", json!({"name": "runbook", "description": "Ops runbook", "fields": [{"name": "service", "type": "string", "required": true}, "severity:enum:low|high"]})),
            call(3, "add", json!({"title": "Restart api", "kind": "runbook"})),
            call(4, "add", json!({"title": "Restart api", "kind": "runbook", "fields": {"service": "api", "severity": "high"}})),
            call(5, "board", json!({"project": "app"})),
        ],
        &[],
    );
    assert!(text(&out[0]).contains("APP-"));
    assert!(text(&out[1]).contains("defined kind runbook"));
    assert_eq!(out[2]["result"]["isError"], true);
    assert_eq!(out[3]["result"]["isError"], false);
    assert!(text(&out[4]).contains("Offline mode"));
    let idea = v.load_notes().into_iter().find(|n| n.title == "Offline mode").unwrap();
    let out = run(&srv, &[call(1, "queue", json!({"id": idea.id})), call(2, "next", json!({"project": "app", "claim": true, "by": "agent-7"})), call(3, "move", json!({"id": idea.id, "state": "rejected"}))], &[]);
    assert!(text(&out[0]).contains("queued"));
    assert!(text(&out[1]).contains("claimed") && text(&out[1]).contains("@agent-7"));
    assert_eq!(out[2]["result"]["isError"], true, "rejected without resolution must fail");
}
