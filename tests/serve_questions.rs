mod common;

use breadcrumb::question::{self, AskReq, Opt};
use breadcrumb::serve::{App, Request, Response, load_or_create_token, normalize_host, token_eq};
use breadcrumb::vault::Vault;
use serde_json::{Value, json};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const HOST: &str = "pc.tailnet.ts.net";

fn req(method: &str, path: &str, host: &str, body: Option<Value>) -> Request {
    let mut headers = vec![("Host".to_string(), host.to_string())];
    if body.is_some() {
        headers.push(("Content-Type".into(), "application/json".into()));
    }
    let (path, query) = match path.split_once('?') {
        Some((p, q)) => (p.to_string(), q.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect()),
        None => (path.to_string(), vec![]),
    };
    Request { method: method.into(), path, query, headers, body: body.map(|b| serde_json::to_vec(&b).unwrap()).unwrap_or_default() }
}

fn authed(mut r: Request) -> Request {
    r.headers.push(("Cookie".into(), format!("other=1; crumb_token={TOKEN}")));
    r
}

fn ask(v: &Vault, id: &str, multi: bool, free: bool) -> String {
    let options = ["red", "blue", "green"].iter().map(|l| Opt { label: l.to_string(), desc: format!("{l} way") }).collect();
    let (n, q) = question::ask(v, AskReq { id: id.into(), text: "Which colour?".into(), options, multi, free, by: Some("agent-1".into()) }).unwrap();
    question::qid(&n, &q)
}

fn setup() -> (tempfile::TempDir, Vault, String) {
    let (d, v) = common::vault();
    let id = common::add(&v, "Pick a palette", "task", Some("app"), &[("state", "queued")]).id;
    (d, v, id)
}

fn post(app: &App, path: &str, body: Value) -> Response {
    app.handle(&req("POST", path, "127.0.0.1", Some(body)))
}

fn msg(r: Response) -> String {
    r.body_json()["error"].as_str().unwrap().to_string()
}

#[test]
fn list_and_answer_endpoints() {
    let (_d, v, id) = setup();
    let app = App::new(&v.root, 0);
    let qid = ask(&v, &id, true, true);

    let j = app.handle(&req("GET", "/api/questions", "127.0.0.1", None)).body_json();
    assert_eq!(j["open"][0]["qid"], qid.as_str());
    assert_eq!(j["open"][0]["title"], "Pick a palette");
    assert_eq!(j["open"][0]["project"], "app");
    assert_eq!(j["open"][0]["question"]["options"][1]["label"], "blue");
    assert_eq!(j["open"][0]["question"]["options"][1]["desc"], "blue way");
    assert_eq!(j["unseen"], json!([]));

    let r = post(&app, &format!("/api/question/{qid}/answer"), json!({"choices": [1, 3], "text": "and teal", "by": "phone"}));
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(r.body_json()["answer"], "red, green, and teal");

    let j = app.handle(&req("GET", "/api/questions", "127.0.0.1", None)).body_json();
    assert_eq!(j["open"], json!([]));
    assert_eq!(j["unseen"][0]["qid"], qid.as_str());
    assert_eq!(j["unseen"][0]["answer"], "red, green, and teal");
    assert_eq!(j["unseen"][0]["question"]["answer"]["by"], "phone");
    assert_eq!(app.handle(&req("GET", "/questions", "localhost", None)).status, 200);
}

#[test]
fn answer_validation_matches_the_cli() {
    let (_d, v, id) = setup();
    let app = App::new(&v.root, 0);
    let single = ask(&v, &id, false, false);
    let path = format!("/api/question/{single}/answer");

    let r = post(&app, &path, json!({"choices": [9]}));
    assert_eq!(r.status, 400);
    assert!(msg(r).contains("has 3 options, not 9"));
    assert!(msg(post(&app, &path, json!({"choices": [1, 2]}))).contains("takes a single option"));
    assert!(msg(post(&app, &path, json!({"text": "teal"}))).contains("takes no free text"));
    assert!(msg(post(&app, &path, json!({}))).contains("pick an option number"));
    assert!(msg(post(&app, &path, json!({"choices": [0]}))).contains("starting at 1"));
    assert_eq!(post(&app, &path, json!({"choices": [2]})).status, 200);
    assert!(msg(post(&app, &path, json!({"choices": [1]}))).contains("already answered"));
}

#[test]
fn local_mode_is_unchanged() {
    let (_d, v, _id) = setup();
    let app = App::new(&v.root, 7878);
    assert_eq!(app.handle(&req("GET", "/api/projects", HOST, None)).status, 403, "extra host refused without --remote");
    assert_eq!(app.handle(&req("GET", "/api/projects", "127.0.0.1:7878", None)).status, 200);
    assert_eq!(app.handle(&req("GET", "/login?token=x", "127.0.0.1:7878", None)).status, 404);
    let mut w = req("POST", "/api/note", "127.0.0.1:7878", Some(json!({"title": "kept"})));
    w.headers.push(("Origin".into(), format!("https://{HOST}")));
    assert_eq!(app.handle(&w).status, 403, "https origin refused without --remote");
    let page = app.handle(&req("GET", "/", "127.0.0.1:7878", None));
    let html = String::from_utf8_lossy(&page.body);
    assert!(html.contains("<title>breadcrumb</title>") && !html.contains("data-remote=\"1\""));
}

fn remote_app(v: &Vault) -> App {
    App::with_remote(&v.root, 7878, vec![HOST.to_string()], TOKEN.to_string())
}

#[test]
fn remote_requires_a_known_host_and_the_token() {
    let (_d, v, id) = setup();
    ask(&v, &id, false, false);
    let app = remote_app(&v);
    assert_eq!(app.handle(&req("GET", "/api/questions", "evil.example", None)).status, 403);
    assert_eq!(app.handle(&authed(req("GET", "/api/questions", "evil.example", None))).status, 403);

    let r = app.handle(&req("GET", "/api/questions", HOST, None));
    assert_eq!(r.status, 401);
    assert!(!String::from_utf8_lossy(&r.body).contains("Pick a palette"));
    let mut bad = req("GET", "/api/questions", HOST, None);
    bad.headers.push(("Cookie".into(), format!("crumb_token={}", &TOKEN[..63])));
    assert_eq!(app.handle(&bad).status, 401);
    assert_eq!(app.handle(&req("GET", "/", HOST, None)).status, 401);
    assert_eq!(app.handle(&req("GET", "/login?token=nope", HOST, None)).status, 401);
    assert_eq!(app.handle(&req("GET", "/login", HOST, None)).status, 401);

    assert_eq!(app.handle(&authed(req("GET", "/api/questions", HOST, None))).status, 200);
    assert_eq!(app.handle(&authed(req("GET", "/api/questions", "localhost:7878", None))).status, 200);
}

#[test]
fn login_sets_a_hardened_cookie() {
    let (_d, v, _id) = setup();
    let app = remote_app(&v);
    let r = app.handle(&req("GET", &format!("/login?token={TOKEN}"), HOST, None));
    assert_eq!(r.status, 303);
    assert_eq!(r.header("location"), Some("/"));
    let c = r.header("set-cookie").unwrap();
    assert!(c.starts_with(&format!("crumb_token={TOKEN};")));
    for part in ["HttpOnly", "Secure", "SameSite=Strict"] {
        assert!(c.contains(part), "{c}");
    }
    let mut wire = vec![];
    breadcrumb::serve::write_response(&mut wire, &r).unwrap();
    assert!(String::from_utf8(wire).unwrap().contains("Set-Cookie: crumb_token="));
}

#[test]
fn remote_allows_questions_and_a_read_only_board_only() {
    let (_d, v, id) = setup();
    let app = remote_app(&v);
    let qid = ask(&v, &id, false, false);

    let page = app.handle(&authed(req("GET", "/", HOST, None)));
    assert!(String::from_utf8_lossy(&page.body).contains("<title>breadcrumb questions</title>"));
    let board = app.handle(&authed(req("GET", "/board", HOST, None)));
    assert!(String::from_utf8_lossy(&board.body).contains("data-remote"));
    assert_eq!(app.handle(&authed(req("GET", "/api/board", HOST, None))).status, 200);

    let note = format!("/api/note/{id}");
    let mv = format!("/api/note/{id}/move");
    for (m, p) in [("GET", "/api/notes"), ("GET", note.as_str()), ("POST", "/api/note"), ("PATCH", note.as_str()), ("POST", mv.as_str())] {
        let body = (m != "GET").then(|| json!({"title": "x", "state": "done"}));
        let mut r = authed(req(m, p, HOST, body));
        r.headers.push(("Origin".into(), format!("https://{HOST}")));
        assert_eq!(app.handle(&r).status, 403, "{m} {p}");
    }
    assert!(v.load_notes().iter().all(|n| n.title != "x"));

    let answer = format!("/api/question/{qid}/answer");
    let mut again = authed(req("POST", &answer, HOST, Some(json!({"choices": [2]}))));
    again.headers.push(("Origin".into(), "https://evil.example".into()));
    assert_eq!(app.handle(&again).status, 403, "other origins stay refused");
    let mut cross = authed(req("POST", &answer, HOST, Some(json!({"choices": [2]}))));
    cross.headers.push(("Sec-Fetch-Site".into(), "cross-site".into()));
    assert_eq!(app.handle(&cross).status, 403);
    let mut ok = authed(req("POST", &answer, HOST, Some(json!({"choices": [2]}))));
    ok.headers.push(("Origin".into(), format!("https://{HOST}")));
    assert_eq!(app.handle(&ok).status, 200);
}

#[test]
fn token_helpers() {
    assert!(token_eq(TOKEN, TOKEN));
    assert!(!token_eq(TOKEN, &TOKEN[..63]));
    assert!(!token_eq(TOKEN, ""));
    assert_eq!(normalize_host("https://PC.Tailnet.ts.net:443/").unwrap(), HOST);
    assert!(normalize_host("a/b").is_err() && normalize_host("").is_err());

    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("app");
    let t = load_or_create_token(&dir).unwrap();
    assert_eq!(t.len(), 64);
    assert_eq!(load_or_create_token(&dir).unwrap(), t, "saved token is reused");
    std::fs::write(dir.join("serve-token"), "short").unwrap();
    assert_ne!(load_or_create_token(&dir).unwrap(), "short");
}
