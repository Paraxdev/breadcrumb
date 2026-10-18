mod common;

use breadcrumb::serve::{App, Request, read_request, serve_listener};
use serde_json::{Value, json};
use std::io::{BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

fn req(method: &str, path: &str, body: Option<Value>) -> Request {
    let (path, query) = match path.split_once('?') {
        Some((p, q)) => (p.to_string(), q.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), breadcrumb::serve::percent_decode(v))).collect()),
        None => (path.to_string(), vec![]),
    };
    let mut headers = vec![("Host".to_string(), "127.0.0.1:7878".to_string())];
    if body.is_some() {
        headers.push(("Content-Type".into(), "application/json".into()));
    }
    Request { method: method.into(), path, query, headers, body: body.map(|b| serde_json::to_vec(&b).unwrap()).unwrap_or_default() }
}

#[test]
fn api_create_move_patch_round_trip() {
    let (_d, v) = common::vault();
    let app = App::new(&v.root, 0);

    let r = app.handle(&req("POST", "/api/note", Some(json!({"title": "Kanban", "kind": "feature", "project": "web", "tags": ["ui"], "fields": {"priority": "p1", "acceptance": ["drag works"]}, "body": "first"}))));
    assert_eq!(r.status, 201, "{}", String::from_utf8_lossy(&r.body));
    let created = r.body_json();
    let id = created["note"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["key"], format!("WEB-{id}"));
    assert_eq!(created["state"], "backlog");

    let board = app.handle(&req("GET", "/api/board?project=web", None)).body_json();
    let backlog_col = board["columns"].as_array().unwrap().iter().find(|c| c["state"] == "backlog").unwrap();
    assert_eq!(backlog_col["items"][0]["id"], id.as_str());

    let r = app.handle(&req("POST", &format!("/api/note/{id}/move"), Some(json!({"state": "done", "by": "web-user"}))));
    assert_eq!(r.status, 200);
    let moved = r.body_json();
    assert_eq!(moved["from"], "backlog");
    assert_eq!(moved["to"], "done");
    assert_eq!(moved["unmet_acceptance"], json!(["drag works"]));
    assert!(moved["markdown"].as_str().unwrap().contains("state backlog -> done (by web-user)"));

    let r = app.handle(&req("PATCH", &format!("/api/note/{}", &id[..5]), Some(json!({"title": "Kanban board", "tags": ["ux"], "fields": {"assignee": "ann", "priority": ""}}))));
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    let n = v.get(&id).unwrap();
    assert_eq!(n.title, "Kanban board");
    assert_eq!(n.tags, vec!["ux"]);
    assert_eq!(n.field("assignee").and_then(|x| x.as_str()), Some("ann"));
    assert_eq!(n.field("priority").and_then(|x| x.as_str()), Some("p2"), "cleared field falls back to the kind default");

    let got = app.handle(&req("GET", &format!("/api/note/{id}"), None)).body_json();
    assert_eq!(got["note"]["title"], "Kanban board");
    let found = app.handle(&req("GET", "/api/notes?q=kanban&project=all", None)).body_json();
    assert_eq!(found["notes"][0]["id"], id.as_str());
    let projects = app.handle(&req("GET", "/api/projects", None)).body_json();
    assert_eq!(projects["projects"][0]["slug"], "web");
    assert!(app.handle(&req("GET", "/api/kinds", None)).body_json()["kinds"].as_array().unwrap().len() >= 15);

    assert_eq!(app.handle(&req("POST", &format!("/api/note/{id}/move"), Some(json!({"state": "rejected"})))).status, 400);
    assert_eq!(app.handle(&req("GET", "/api/note/zzzzzzzz", None)).status, 404);
    assert_eq!(app.handle(&req("GET", "/api/nope", None)).status, 404);
    let log = common::git(&v.root, &["log", "--format=%s"]);
    assert!(log.contains("breadcrumb: move") && log.contains("breadcrumb: update"));
}

#[test]
fn host_and_content_type_guards() {
    let (_d, v) = common::vault();
    let app = App::new(&v.root, 7878);
    let mut r = req("GET", "/api/projects", None);
    r.headers = vec![("Host".into(), "evil.example:7878".into())];
    assert_eq!(app.handle(&r).status, 403);
    r.headers = vec![("Host".into(), "localhost:7878".into())];
    assert_eq!(app.handle(&r).status, 200);
    r.headers = vec![("Host".into(), "[::1]:7878".into())];
    assert_eq!(app.handle(&r).status, 200);
    r.headers = vec![];
    assert_eq!(app.handle(&r).status, 403);

    let mut w = req("POST", "/api/note", Some(json!({"title": "x"})));
    w.headers.retain(|(k, _)| k != "Content-Type");
    w.headers.push(("Content-Type".into(), "text/plain".into()));
    assert_eq!(app.handle(&w).status, 415);
    let mut w = req("POST", "/api/note", Some(json!({"title": "x"})));
    w.headers.push(("Origin".into(), "http://evil.example".into()));
    assert_eq!(app.handle(&w).status, 403);
    let mut w = req("POST", "/api/note", Some(json!({"title": "same origin"})));
    w.headers.push(("Origin".into(), "http://127.0.0.1:7878".into()));
    assert_eq!(app.handle(&w).status, 201);
    assert!(v.load_notes().iter().any(|n| n.title == "same origin"));
}

fn http(port: u16, raw: &str) -> (u16, Value) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(raw.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status: u16 = out.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = out.split_once("\r\n\r\n").unwrap().1;
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

#[test]
fn real_tcp_round_trip() {
    let (_d, v) = common::vault();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Arc::new(App::new(&v.root, port));
    std::thread::spawn(move || serve_listener(listener, app));

    let body = r#"{"title":"Idea from é web","kind":"idea","project":"global"}"#;
    let (st, j) = http(port, &format!("POST /api/note HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()));
    assert_eq!(st, 201, "{j}");
    let id = j["note"]["id"].as_str().unwrap().to_string();
    let mv = r#"{"state":"queued"}"#;
    let (st, j) = http(port, &format!("POST /api/note/{id}/move HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\n\r\n{mv}", mv.len()));
    assert_eq!(st, 200, "{j}");
    assert_eq!(j["to"], "queued");
    let (st, j) = http(port, &format!("GET /api/board?project=global HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"));
    assert_eq!(st, 200);
    let queued = j["columns"].as_array().unwrap().iter().find(|c| c["state"] == "queued").unwrap();
    assert_eq!(queued["items"][0]["title"], "Idea from \u{e9} web");
    let (st, _) = http(port, &format!("GET / HTTP/1.1\r\nHost: attacker.test:{port}\r\n\r\n"));
    assert_eq!(st, 403);

    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes()).unwrap();
    let mut page = String::new();
    s.read_to_string(&mut page).unwrap();
    assert!(page.starts_with("HTTP/1.1 200") && page.contains("<title>breadcrumb</title>"));
}

#[test]
fn request_parsing() {
    let raw = "PATCH /api/note/abcd%20x?q=a+b%21&project=web HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\n{}";
    let r = read_request(&mut BufReader::new(raw.as_bytes())).unwrap().unwrap();
    assert_eq!(r.method, "PATCH");
    assert_eq!(r.path, "/api/note/abcd x");
    assert_eq!(r.param("q"), Some("a b!"));
    assert_eq!(r.param("project"), Some("web"));
    assert_eq!(r.body, b"{}");
}
