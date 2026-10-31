mod common;

use breadcrumb::backlog::{self, MoveReq};
use breadcrumb::ops::Scope;
use breadcrumb::serve::{App, Request, Response};
use breadcrumb::vault::Vault;
use serde_json::{Value, json};
use std::process::Command;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const HOST: &str = "pc.tailnet.ts.net";

fn to_review(v: &Vault, id: &str, steps: &[&str]) -> backlog::MoveResult {
    backlog::move_item(v, id, MoveReq { state: "review".into(), steps: steps.iter().map(|s| s.to_string()).collect(), ..Default::default() }).unwrap()
}

fn req(method: &str, path: &str, host: &str, body: Option<Value>) -> Request {
    let mut headers = vec![("Host".to_string(), host.to_string())];
    if body.is_some() {
        headers.push(("Content-Type".into(), "application/json".into()));
    }
    Request { method: method.into(), path: path.to_string(), query: vec![], headers, body: body.map(|b| serde_json::to_vec(&b).unwrap()).unwrap_or_default() }
}

fn authed(mut r: Request) -> Request {
    r.headers.push(("Cookie".into(), format!("crumb_token={TOKEN}")));
    r
}

fn err(r: Response) -> String {
    r.body_json()["error"].as_str().unwrap().to_string()
}

fn crumb(v: &Vault, args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_crumb")).args(args).env("BREADCRUMB_HOME", &v.root).env_remove("CLAUDE_CODE_SESSION_ID").env_remove("BREADCRUMB_SESSION").output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

#[test]
fn steps_are_acceptance_criteria_and_moving_without_them_only_warns() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "No steps", "bug", Some("app"), &[]);
    let r = to_review(&v, &a.id, &[]);
    assert_eq!(r.to, "review", "never a refusal");
    assert!(r.warnings[0].contains("no steps") && backlog::move_text(&r).contains("warning: "), "{:?}", r.warnings);

    let r = to_review(&v, &a.id, &["Open the app", "Press the button"]);
    assert!(r.changed && r.warnings.is_empty());
    assert_eq!(backlog::review_steps(&r.note), ["Open the app", "Press the button"]);
    assert_eq!(backlog::unmet_acceptance(&r.note).len(), 2);
    assert!(!to_review(&v, &a.id, &["open the APP"]).changed, "a known step is not added twice");

    let f = common::add(&v, "Has criteria", "feature", Some("app"), &[("acceptance", "['[x] builds', renders]")]);
    let r = to_review(&v, &f.id, &[]);
    assert!(r.warnings.is_empty(), "open criteria are the steps");
    assert_eq!(backlog::review_steps(&r.note), ["renders"]);
    let r = to_review(&v, &f.id, &["builds"]);
    assert_eq!(backlog::review_steps(&r.note), ["builds", "renders"], "asking for a met one again opens it");

    let t = common::add(&v, "Elsewhere", "task", Some("app"), &[]);
    let e = backlog::move_item(&v, &t.id, MoveReq { state: "queued".into(), steps: vec!["x".into()], ..Default::default() }).unwrap_err();
    assert!(e.to_string().contains("moving to review"), "{e}");
}

#[test]
fn check_only_ticks_and_step_only_adds() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "Literal three", "task", Some("app"), &[]);
    let (ok, out, _) = crumb(&v, &["move", &a.id, "review", "--step", "3", "--step", "Open it", "--step", "Close it"]);
    assert!(ok, "{out}");
    assert_eq!(backlog::review_steps(&v.get(&a.id).unwrap()), ["3", "Open it", "Close it"], "a step that reads 3 ticks nothing");

    let (ok, _, err) = crumb(&v, &["move", &a.id, "review", "--check", "Try it"]);
    assert!(!ok && err.contains("invalid criterion 'Try'") && err.contains("--step"), "{err}");
    assert_eq!(backlog::review_steps(&v.get(&a.id).unwrap()).len(), 3);

    let (ok, out, _) = crumb(&v, &["move", &a.id, "review", "--check", "1,3"]);
    assert!(ok, "{out}");
    assert_eq!(backlog::review_steps(&v.get(&a.id).unwrap()), ["Open it"]);
    let (ok, out, _) = crumb(&v, &["move", &a.id, "review", "--check", "all"]);
    assert!(ok, "{out}");
    assert!(backlog::review_steps(&v.get(&a.id).unwrap()).is_empty());

    let b = common::add(&v, "Elsewhere", "task", Some("app"), &[]);
    let (ok, _, err) = crumb(&v, &["move", &b.id, "done", "--check", "Try it"]);
    assert!(!ok && err.contains("invalid criterion"), "{err}");
    let (ok, _, err) = crumb(&v, &["move", &b.id, "done", "--step", "Try it"]);
    assert!(!ok && err.contains("moving to review"), "{err}");
    let (ok, _, err) = crumb(&v, &["check", &b.id, "Try"]);
    assert!(!ok && err.contains("--step"), "{err}");
}

#[test]
fn pass_and_fail_through_the_cli() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "Pass me", "task", Some("app"), &[("priority", "p1")]);
    let (ok, out, _) = crumb(&v, &["move", &a.id, "review", "--step", "Open it", "--step", "Click it"]);
    assert!(ok && !out.contains("warning"), "{out}");
    let (_, out, _) = crumb(&v, &["review", "--project", "app"]);
    assert!(out.contains("1. Open it") && out.contains("2. Click it"), "{out}");

    let (ok, _, _) = crumb(&v, &["review", "pass", &a.id, "--by", "paul"]);
    assert!(ok);
    let n = v.get(&a.id).unwrap();
    assert_eq!(backlog::state(&n), "done");
    assert!(backlog::unmet_acceptance(&n).is_empty(), "a pass marks the steps met");
    assert!(n.body.contains("review -> done (by paul)"), "{}", n.body);
    let (ok, _, e) = crumb(&v, &["review", "pass", &a.id]);
    assert!(!ok && e.contains("not in review"), "{e}");

    let b = common::add(&v, "Fail me", "bug", Some("app"), &[("priority", "p0")]);
    backlog::claim(&v, &b.id, "lane-1", false).unwrap();
    to_review(&v, &b.id, &["Try it"]);
    let (ok, _, e) = crumb(&v, &["review", "fail", &b.id]);
    assert!(!ok && e.contains("--reason"), "{e}");
    let (ok, _, e) = crumb(&v, &["review", "fail", &b.id, "--reason", "  "]);
    assert!(!ok && e.contains("needs a reason"), "{e}");
    let (ok, _, _) = crumb(&v, &["review", "fail", &b.id, "--reason", "button does nothing", "--by", "paul"]);
    assert!(ok);
    let n = v.get(&b.id).unwrap();
    assert_eq!((backlog::state(&n).as_str(), backlog::priority(&n).as_str()), ("queued", "p0"));
    assert!(n.body.contains("paul:\nreview failed: button does nothing"), "{}", n.body);
    assert!(n.body.contains("review -> queued (by paul)"), "{}", n.body);
    assert!(n.body.find("review failed").unwrap() < n.body.find("## History").unwrap());
    assert_eq!(backlog::assignee(&n), None, "the queue can hand it out again");
    assert_eq!(backlog::review_steps(&n), ["Try it"], "the steps stay for the next try");
}

#[test]
fn web_endpoints_match_the_cli() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "One", "task", Some("app"), &[]);
    let b = common::add(&v, "Two", "task", Some("app"), &[]);
    to_review(&v, &a.id, &["Step one"]);
    to_review(&v, &b.id, &[]);
    let app = App::new(&v.root, 0);
    let j = app.handle(&req("GET", "/api/review", "127.0.0.1", None)).body_json();
    assert_eq!(j["items"].as_array().unwrap().len(), 2);
    let with_steps = j["items"].as_array().unwrap().iter().find(|i| i["id"] == a.id.as_str()).unwrap();
    assert_eq!(with_steps["steps"], json!(["Step one"]));
    let page = app.handle(&req("GET", "/review", "127.0.0.1", None));
    assert!(String::from_utf8_lossy(&page.body).contains("<title>breadcrumb review</title>"));

    let fail = format!("/api/review/{}/fail", a.id);
    assert!(err(app.handle(&req("POST", &fail, "127.0.0.1", Some(json!({}))))).contains("needs a reason"));
    assert_eq!(backlog::state(&v.get(&a.id).unwrap()), "review");
    assert_eq!(app.handle(&req("POST", &fail, "127.0.0.1", Some(json!({"reason": "broken"})))).status, 200);
    let n = v.get(&a.id).unwrap();
    assert_eq!(backlog::state(&n), "queued");
    assert!(n.body.contains("web:\nreview failed: broken") && n.body.contains("(by web"), "{}", n.body);
    let again = app.handle(&req("POST", &fail, "127.0.0.1", Some(json!({"reason": "broken"}))));
    assert_eq!(again.status, 400);
    assert!(err(again).contains("not in review"));

    let pass = format!("/api/review/{}/pass", b.id);
    let r = app.handle(&req("POST", &pass, "127.0.0.1", Some(json!({})))).body_json();
    assert_eq!((r["from"].as_str(), r["to"].as_str()), (Some("review"), Some("done")));
    assert_eq!(app.handle(&req("POST", "/api/review/nothere/pass", "127.0.0.1", Some(json!({})))).status, 404);
    assert_eq!(app.handle(&req("POST", &format!("/api/review/{}/reopen", b.id), "127.0.0.1", Some(json!({})))).status, 404);
}

#[test]
fn remote_review_is_as_protected_as_answering() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "Secret title", "task", Some("app"), &[]);
    to_review(&v, &a.id, &["Secret step"]);
    let app = App::with_remote(&v.root, 7878, vec![HOST.to_string()], TOKEN.to_string());
    let pass = format!("/api/review/{}/pass", a.id);
    let fail = format!("/api/review/{}/fail", a.id);

    for path in ["/review", "/api/review"] {
        let r = app.handle(&req("GET", path, HOST, None));
        assert_eq!(r.status, 401, "{path}");
        assert!(!String::from_utf8_lossy(&r.body).contains("Secret"));
    }
    assert_eq!(app.handle(&req("POST", &pass, HOST, Some(json!({})))).status, 401);
    assert_eq!(app.handle(&req("POST", &fail, HOST, Some(json!({"reason": "no"})))).status, 401);
    let mut bad = req("POST", &pass, HOST, Some(json!({})));
    bad.headers.push(("Cookie".into(), format!("crumb_token={}", &TOKEN[..63])));
    assert_eq!(app.handle(&bad).status, 401);
    assert_eq!(app.handle(&authed(req("GET", "/api/review", "evil.example", None))).status, 403);

    let mut evil = authed(req("POST", &pass, HOST, Some(json!({}))));
    evil.headers.push(("Origin".into(), "https://evil.example".into()));
    assert_eq!(app.handle(&evil).status, 403);
    let mut cross = authed(req("POST", &pass, HOST, Some(json!({}))));
    cross.headers.push(("Sec-Fetch-Site".into(), "cross-site".into()));
    assert_eq!(app.handle(&cross).status, 403);
    let mut form = authed(req("POST", &pass, HOST, None));
    form.headers.push(("Content-Type".into(), "application/x-www-form-urlencoded".into()));
    assert_eq!(app.handle(&form).status, 415);
    assert_eq!(app.handle(&authed(req("GET", &pass, HOST, None))).status, 403, "a verdict is never a GET");
    assert_eq!(app.handle(&authed(req("POST", &format!("/api/review/{}/reopen", a.id), HOST, Some(json!({}))))).status, 403);
    assert_eq!(backlog::state(&v.get(&a.id).unwrap()), "review", "no refused request changed it");

    assert_eq!(app.handle(&authed(req("GET", "/review", HOST, None))).status, 200);
    assert_eq!(app.handle(&authed(req("GET", "/api/review", HOST, None))).body_json()["items"][0]["steps"], json!(["Secret step"]));
    let mut ok = authed(req("POST", &pass, HOST, Some(json!({}))));
    ok.headers.push(("Origin".into(), format!("https://{HOST}")));
    assert_eq!(app.handle(&ok).status, 200);
    assert_eq!(backlog::state(&v.get(&a.id).unwrap()), "done");
}
