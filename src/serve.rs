//! Local web UI: a minimal HTTP/1.1 server on 127.0.0.1 with a JSON API over the same ops the CLI uses.

use crate::backlog::{self, BoardFilter, MoveReq};
use crate::kind;
use crate::note::{Link, Note};
use crate::question;
use crate::ops::{self, AddReq, FieldInput, Filter, Scope, SortKey, StatusFilter, UpdateReq};
use crate::vault::Vault;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value, json};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

const INDEX_HTML: &str = include_str!("web/index.html");
const QUESTIONS_HTML: &str = include_str!("web/questions.html");
const LOGIN_HTML: &str = "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\"><title>breadcrumb</title><body style=\"font:16px system-ui;margin:2em auto;max-width:30em;padding:0 16px\"><h1>breadcrumb</h1><p>Sign in with the login link that <code>breadcrumb serve --remote</code> printed when it started.</p>";
pub const COOKIE: &str = "crumb_token";
const MAX_BODY: usize = 4 * 1024 * 1024;
const MAX_HEADERS: usize = 100;

#[derive(Debug, Clone, Default)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn param(&self, name: &str) -> Option<&str> {
        self.query.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).filter(|v| !v.is_empty())
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    pub headers: Vec<(&'static str, String)>,
}

impl Response {
    pub fn json(status: u16, v: &Value) -> Response {
        Response { status, content_type: "application/json; charset=utf-8", body: serde_json::to_vec(v).unwrap_or_default(), headers: vec![] }
    }

    fn html(status: u16, body: &str) -> Response {
        Response { status, content_type: "text/html; charset=utf-8", body: body.as_bytes().to_vec(), headers: vec![] }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    fn error(status: u16, msg: impl Into<String>) -> Response {
        Response::json(status, &json!({"error": msg.into()}))
    }

    pub fn body_json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        303 => "See Other",
        401 => "Unauthorized",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        503 => "Service Unavailable",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        _ => "Internal Server Error",
    }
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() => {
                let hex = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
                out.push(hex(b[i + 1]) * 16 + hex(b[i + 2]));
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(p), String::new()),
        })
        .collect()
}

pub fn read_request<R: BufRead>(r: &mut R) -> Result<Option<Request>> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().context("empty request line")?.to_string();
    let target = parts.next().context("request line without target")?.to_string();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), parse_query(q)),
        None => (target.clone(), vec![]),
    };
    let mut headers = vec![];
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 {
            break;
        }
        let h = h.trim_end_matches(['\r', '\n']);
        if h.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            bail!("too many headers");
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let mut req = Request { method, path: percent_decode(&path.replace('+', "%2B")), query, headers, body: vec![] };
    let len: usize = req.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    if len > MAX_BODY {
        bail!("request body too large");
    }
    if len > 0 {
        let mut body = vec![0u8; len];
        r.read_exact(&mut body)?;
        req.body = body;
    }
    Ok(Some(req))
}

pub fn write_response<W: Write>(w: &mut W, resp: &Response) -> Result<()> {
    write!(
        w,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n",
        resp.status,
        reason(resp.status),
        resp.content_type,
        resp.body.len()
    )?;
    for (k, v) in &resp.headers {
        write!(w, "{k}: {v}\r\n")?;
    }
    w.write_all(b"\r\n")?;
    w.write_all(&resp.body)?;
    w.flush()?;
    Ok(())
}

fn host_of(authority: &str) -> &str {
    let a = authority.trim();
    if let Some(rest) = a.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("");
    }
    a.rsplit_once(':').map(|(h, _)| h).unwrap_or(a)
}

fn is_local_host(h: &str) -> bool {
    matches!(h.to_ascii_lowercase().as_str(), "localhost" | "127.0.0.1" | "::1")
}

/// Remote mode: extra Host names behind a TLS proxy and the one token that unlocks them.
pub struct Remote {
    pub hosts: Vec<String>,
    pub token: String,
}

pub struct App {
    pub vault_root: PathBuf,
    pub port: u16,
    pub remote: Option<Remote>,
}

/// Compares SHA-256 digests of both sides, so neither the length nor the first differing byte shows in the timing.
pub fn token_eq(a: &str, b: &str) -> bool {
    let (da, db) = (Sha256::digest(a.as_bytes()), Sha256::digest(b.as_bytes()));
    da.iter().zip(db.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn cookie_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').filter_map(|c| c.trim().split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| v)
}

/// Lowercases and strips a scheme, port and trailing slash, so `https://PC.ts.net:443/` and `pc.ts.net` are one name.
pub fn normalize_host(raw: &str) -> Result<String> {
    let h = raw.trim().trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/');
    let h = host_of(h).to_ascii_lowercase();
    if h.is_empty() || h.contains(['/', ' ', '@', '?', '#']) {
        bail!("'{raw}' is not a host name");
    }
    Ok(h)
}

impl App {
    pub fn new(vault_root: &Path, port: u16) -> App {
        App { vault_root: vault_root.to_path_buf(), port, remote: None }
    }

    pub fn with_remote(vault_root: &Path, port: u16, hosts: Vec<String>, token: String) -> App {
        App { vault_root: vault_root.to_path_buf(), port, remote: Some(Remote { hosts, token }) }
    }

    fn allowed_host(&self, host: &str) -> bool {
        is_local_host(host) || self.remote.as_ref().is_some_and(|r| r.hosts.iter().any(|h| h.eq_ignore_ascii_case(host)))
    }

    /// DNS rebinding and CSRF defenses: local Host only, and writes need a JSON body from a local origin.
    fn guard(&self, req: &Request) -> Option<Response> {
        match req.header("host") {
            Some(h) if self.allowed_host(host_of(h)) => {}
            _ => return Some(Response::error(403, "forbidden host, use http://127.0.0.1 or http://localhost")),
        }
        if matches!(req.method.as_str(), "POST" | "PATCH" | "PUT" | "DELETE") {
            let ct = req.header("content-type").unwrap_or("").to_ascii_lowercase();
            if !ct.starts_with("application/json") {
                return Some(Response::error(415, "writes require Content-Type: application/json"));
            }
            if let Some(origin) = req.header("origin") {
                let local = origin
                    .strip_prefix("http://")
                    .map(|rest| is_local_host(host_of(rest)) && (self.port == 0 || rest.ends_with(&format!(":{}", self.port))))
                    .unwrap_or(false);
                let remote = self.remote.as_ref().is_some_and(|r| r.hosts.iter().any(|h| origin.eq_ignore_ascii_case(&format!("https://{h}"))));
                let ok = local || remote;
                if !ok {
                    return Some(Response::error(403, "cross-origin write rejected"));
                }
            }
            if req.header("sec-fetch-site").is_some_and(|s| s.eq_ignore_ascii_case("cross-site")) {
                return Some(Response::error(403, "cross-site write rejected"));
            }
        }
        None
    }

    /// Remote mode only: the login route, then the token cookie, then the short list of allowed routes.
    fn remote_gate(&self, r: &Remote, req: &Request, segs: &[&str]) -> Option<Response> {
        if req.method == "GET" && segs == ["login"] {
            if !req.param("token").is_some_and(|t| token_eq(t, &r.token)) {
                return Some(Response::html(401, LOGIN_HTML));
            }
            let mut resp = Response::html(303, "");
            resp.headers.push(("Location", "/".into()));
            resp.headers.push(("Set-Cookie", format!("{COOKIE}={}; Path=/; Max-Age=31536000; HttpOnly; Secure; SameSite=Strict", r.token)));
            return Some(resp);
        }
        let authed = req.header("cookie").and_then(|c| cookie_value(c, COOKIE)).is_some_and(|t| token_eq(t, &r.token));
        if !authed {
            return Some(Response::html(401, LOGIN_HTML));
        }
        let allowed = matches!(
            (req.method.as_str(), segs),
            ("GET", [] | ["questions"] | ["board"] | ["api", "questions" | "board" | "projects" | "kinds"]) | ("POST", ["api", "question", _, "answer"])
        );
        if allowed { None } else { Some(Response::error(403, "not available in remote mode")) }
    }

    pub fn handle(&self, req: &Request) -> Response {
        let segs: Vec<&str> = req.path.trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();
        if !matches!(req.header("host"), Some(h) if self.allowed_host(host_of(h))) {
            return Response::error(403, "forbidden host, use http://127.0.0.1 or http://localhost");
        }
        if let Some(r) = &self.remote
            && let Some(resp) = self.remote_gate(r, req, &segs)
        {
            return resp;
        }
        if let Some(r) = self.guard(req) {
            return r;
        }
        let res = match (req.method.as_str(), segs.as_slice()) {
            ("GET", []) if self.remote.is_some() => return Response::html(200, QUESTIONS_HTML),
            ("GET", []) | ("GET", ["index.html"]) | ("GET", ["board"]) => {
                let page = if self.remote.is_some() { INDEX_HTML.replacen("<html lang=\"en\">", "<html lang=\"en\" data-remote=\"1\">", 1) } else { INDEX_HTML.to_string() };
                return Response::html(200, &page);
            }
            ("GET", ["questions"]) => return Response::html(200, QUESTIONS_HTML),
            ("GET", ["api", "questions"]) => self.questions(),
            ("POST", ["api", "question", qid, "answer"]) => self.answer(qid, req),
            ("GET", ["api", "projects"]) => self.projects(),
            ("GET", ["api", "kinds"]) => self.kinds(),
            ("GET", ["api", "board"]) => self.board(req),
            ("GET", ["api", "notes"]) => self.notes(req),
            ("GET", ["api", "note", id]) => self.note(id),
            ("POST", ["api", "note"]) => self.create(req),
            ("PATCH", ["api", "note", id]) => self.patch(id, req),
            ("POST", ["api", "note", id, "move"]) => self.move_item(id, req),
            (_, ["api", ..]) => return Response::error(404, format!("no route {} {}", req.method, req.path)),
            _ => return Response::error(404, "not found"),
        };
        match res {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("{e:#}");
                let status = if msg.starts_with("no note with id") {
                    404
                } else if msg.starts_with(ops::CONFLICT) {
                    409
                } else {
                    400
                };
                Response::error(status, msg)
            }
        }
    }

    fn questions(&self) -> Result<Response> {
        let v = self.vault()?;
        let notes: Vec<Note> = v.load_notes().into_iter().filter(|n| !n.is_archived()).collect();
        let row = |n: &Note, q: &question::Question| {
            json!({"qid": question::qid(n, q), "item": n.id, "title": n.title, "project": n.project, "state": backlog::state(n), "question": q, "answer": q.answer_text()})
        };
        let open: Vec<Value> = question::open_in(&notes).iter().map(|(n, q)| row(n, q)).collect();
        let mut unseen = vec![];
        for n in notes.iter().filter(|n| backlog::is_item(n)) {
            unseen.extend(question::questions(n).iter().filter(|q| q.unseen()).map(|q| row(n, q)));
        }
        Ok(Response::json(200, &json!({"open": open, "unseen": unseen})))
    }

    fn answer(&self, qid: &str, req: &Request) -> Result<Response> {
        let v = self.vault()?;
        let b = body_obj(req)?;
        let mut choices: Vec<usize> = vec![];
        if let Some(Value::Array(xs)) = b.get("choices") {
            for x in xs {
                match x.as_u64() {
                    Some(i) if i > 0 => {
                        if !choices.contains(&(i as usize)) {
                            choices.push(i as usize);
                        }
                    }
                    _ => bail!("choices must be option numbers starting at 1"),
                }
            }
        }
        let by = str_of(&b, "by").unwrap_or_else(|| "web".to_string());
        let (n, q) = question::answer(&v, qid, &choices, b.get("text").and_then(Value::as_str), &by)?;
        Ok(Response::json(200, &json!({"qid": question::qid(&n, &q), "answer": q.answer_text(), "question": q})))
    }

    fn vault(&self) -> Result<Vault> {
        Vault::open(&self.vault_root)
    }

    fn scope(&self, v: &Vault, req: &Request) -> Result<Scope> {
        match req.param("project") {
            None | Some("all") => Ok(Scope::All),
            Some("global") => Ok(Scope::Global),
            Some(p) => ops::resolve_scope(v, &ops::ScopeSel { project: Some(p.to_string()), ..Default::default() }, Path::new(".")),
        }
    }

    fn projects(&self) -> Result<Response> {
        let v = self.vault()?;
        let ps: Vec<Value> = v.load_projects().iter().map(|p| json!({"slug": p.slug, "name": p.name, "description": p.description})).collect();
        Ok(Response::json(200, &json!({"vault": crate::util::path_str(&v.root), "projects": ps})))
    }

    fn kinds(&self) -> Result<Response> {
        let v = self.vault()?;
        let ks: Vec<Value> = v.load_kinds().iter().map(crate::format::kind_json).collect();
        Ok(Response::json(200, &json!({"kinds": ks, "states": backlog::STATES, "priorities": backlog::PRIORITIES, "backlog_kinds": backlog::KINDS})))
    }

    fn board(&self, req: &Request) -> Result<Response> {
        let v = self.vault()?;
        let sc = self.scope(&v, req)?;
        let f = BoardFilter {
            kind: req.param("kind").map(str::to_string),
            assignee: req.param("assignee").map(str::to_string),
            include_closed: req.param("include_closed").is_some_and(|x| x != "0" && x != "false"),
        };
        let cols = backlog::board(&v, &sc, &f)?;
        Ok(Response::json(200, &json!({"scope": sc.label(), "columns": cols})))
    }

    fn notes(&self, req: &Request) -> Result<Response> {
        let v = self.vault()?;
        let sc = self.scope(&v, req)?;
        let f = Filter {
            kind: req.param("kind").map(str::to_string),
            status: match req.param("status") {
                Some(s) => StatusFilter::parse(s)?,
                None => StatusFilter::Active,
            },
            limit: Some(req.param("limit").and_then(|l| l.parse().ok()).unwrap_or(200)),
            sort: SortKey::Updated,
            ..Default::default()
        };
        let summary = |n: &Note, snippet: &str| {
            json!({
                "id": n.id, "title": n.title, "kind": n.kind, "project": n.project, "tags": n.tags,
                "updated": crate::util::fmt_time(&n.updated), "pinned": n.pinned, "status": n.status,
                "state": if backlog::is_item(n) { Some(backlog::state(n)) } else { None }, "snippet": snippet,
            })
        };
        let (items, fell_back) = match req.param("q") {
            Some(q) => {
                let r = ops::search(&v, q, &sc, &f)?;
                (r.hits.iter().map(|h| summary(&h.note, &h.snippet)).collect::<Vec<_>>(), r.fell_back_to_or)
            }
            None => (ops::list(&v, &sc, &f)?.iter().map(|n| summary(n, "")).collect(), false),
        };
        Ok(Response::json(200, &json!({"scope": sc.label(), "fell_back_to_or": fell_back, "notes": items})))
    }

    fn note_payload(&self, v: &Vault, n: &Note) -> Value {
        let all = v.load_notes();
        let idx = backlog::Index::new(&all);
        let is_item = backlog::is_item(n);
        let children: Vec<Value> = idx.children(&n.id).iter().map(|c| json!({"id": c.id, "title": c.title, "kind": c.kind, "state": backlog::state_label(c)})).collect();
        json!({
            "note": n.to_json(),
            "version": ops::version(n),
            "markdown": n.to_markdown(),
            "key": backlog::key(n),
            "item": is_item,
            "state": if is_item { Some(backlog::state(n)) } else { None },
            "blocked_by": if is_item { idx.open_blockers(n) } else { vec![] },
            "children": children,
            "unmet_acceptance": backlog::unmet_acceptance(n),
        })
    }

    fn note(&self, id: &str) -> Result<Response> {
        let v = self.vault()?;
        let n = v.get(id)?;
        Ok(Response::json(200, &self.note_payload(&v, &n)))
    }

    fn create(&self, req: &Request) -> Result<Response> {
        let v = self.vault()?;
        let b = body_obj(req)?;
        let project = match str_of(&b, "project").as_deref() {
            None | Some("global") | Some("all") => None,
            Some(p) => Some(p.to_string()),
        };
        let (fields, _) = split_fields(&b)?;
        let n = ops::add(
            &v,
            AddReq {
                title: str_of(&b, "title").ok_or_else(|| anyhow!("title is required"))?,
                kind: str_of(&b, "kind"),
                project,
                tags: strs_of(&b, "tags"),
                fields,
                pinned: b.get("pinned").and_then(Value::as_bool).unwrap_or(false),
                body: str_of(&b, "body").map(|s| crate::util::normalize_newlines(&s)),
                source: None,
                links: links_of(&b)?.unwrap_or_default(),
                paths: vec![],
                slug: None,
                require_existing_project: false,
            },
        )?;
        Ok(Response::json(201, &self.note_payload(&v, &n)))
    }

    fn patch(&self, id: &str, req: &Request) -> Result<Response> {
        let v = self.vault()?;
        let b = body_obj(req)?;
        let (fields, mut unset) = split_fields(&b)?;
        unset.extend(strs_of(&b, "unset"));
        let n = ops::update(
            &v,
            id,
            UpdateReq {
                title: str_of(&b, "title"),
                kind: str_of(&b, "kind"),
                set_tags: b.get("tags").map(|_| strs_of(&b, "tags")),
                expected_version: str_of(&b, "expected_version"),
                fields,
                unset,
                body: b.get("body").and_then(Value::as_str).map(crate::util::normalize_newlines),
                pinned: b.get("pinned").and_then(Value::as_bool),
                status: str_of(&b, "status"),
                links: links_of(&b)?,
                by: str_of(&b, "by"),
                ..Default::default()
            },
        )?;
        Ok(Response::json(200, &self.note_payload(&v, &n)))
    }

    fn move_item(&self, id: &str, req: &Request) -> Result<Response> {
        let v = self.vault()?;
        let b = body_obj(req)?;
        let r = backlog::move_item(
            &v,
            id,
            MoveReq {
                state: str_of(&b, "state").ok_or_else(|| anyhow!("state is required"))?,
                resolution: str_of(&b, "resolution"),
                shipped_in: str_of(&b, "shipped_in"),
                by: str_of(&b, "by"),
                ..Default::default()
            },
        )?;
        let mut payload = self.note_payload(&v, &r.note);
        payload["from"] = r.from.clone().into();
        payload["to"] = r.to.clone().into();
        payload["unmet_acceptance"] = json!(r.unmet);
        Ok(Response::json(200, &payload))
    }
}

fn body_obj(req: &Request) -> Result<Map<String, Value>> {
    match serde_json::from_slice::<Value>(&req.body).context("invalid JSON body")? {
        Value::Object(m) => Ok(m),
        _ => bail!("JSON body must be an object"),
    }
}

fn str_of(b: &Map<String, Value>, k: &str) -> Option<String> {
    b.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

fn strs_of(b: &Map<String, Value>, k: &str) -> Vec<String> {
    match b.get(k) {
        Some(Value::Array(xs)) => xs.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect(),
        Some(Value::String(s)) => s.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect(),
        _ => vec![],
    }
}

/// Splits `fields` into values to set and keys to unset (null or empty string from a cleared form input).
type FieldSplit = (Vec<(String, FieldInput)>, Vec<String>);

fn split_fields(b: &Map<String, Value>) -> Result<FieldSplit> {
    let mut set = vec![];
    let mut unset = vec![];
    match b.get("fields") {
        None | Some(Value::Null) => {}
        Some(Value::Object(m)) => {
            for (k, v) in m {
                match v {
                    Value::Null => unset.push(k.clone()),
                    Value::String(s) if s.trim().is_empty() => unset.push(k.clone()),
                    other => set.push((k.clone(), FieldInput::Value(kind::json_to_yaml(other)))),
                }
            }
        }
        Some(_) => bail!("fields must be an object"),
    }
    Ok((set, unset))
}

fn links_of(b: &Map<String, Value>) -> Result<Option<Vec<Link>>> {
    match b.get("links") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(xs)) => {
            let mut out = vec![];
            for x in xs {
                match x {
                    Value::String(to) => out.push(Link { to: to.clone(), rel: crate::note::DEFAULT_REL.into() }),
                    Value::Object(m) => {
                        let to = m.get("to").and_then(Value::as_str).ok_or_else(|| anyhow!("link needs 'to'"))?;
                        let rel = m.get("rel").and_then(Value::as_str).map(str::trim).filter(|r| !r.is_empty()).unwrap_or(crate::note::DEFAULT_REL);
                        out.push(Link { to: to.trim().to_string(), rel: rel.to_string() });
                    }
                    _ => bail!("links must be ids or {{to, rel}} objects"),
                }
            }
            Ok(Some(out))
        }
        Some(_) => bail!("links must be an array"),
    }
}

fn handle_conn(app: &App, stream: TcpStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(15)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut w = stream;
    let mut busy = None;
    let resp = match read_request(&mut reader) {
        Ok(Some(req)) => {
            busy = Some(InFlight::enter());
            if STOPPING.load(Ordering::SeqCst) { Response::error(503, "server is shutting down") } else { app.handle(&req) }
        }
        Ok(None) => return Ok(()),
        Err(e) => {
            let status = if e.to_string().contains("too large") { 413 } else { 400 };
            Response::error(status, format!("{e:#}"))
        }
    };
    write_response(&mut w, &resp)?;
    drop(busy);
    // Note: on Windows, git children spawned by concurrent requests can inherit this socket handle, so dropping
    // ours does not reliably end the connection. Shut down explicitly and drain so the peer gets a clean FIN.
    let _ = w.shutdown(std::net::Shutdown::Write);
    w.set_read_timeout(Some(Duration::from_millis(500)))?;
    let mut sink = [0u8; 1024];
    while matches!(std::io::Read::read(&mut w, &mut sink), Ok(n) if n > 0) {}
    Ok(())
}

static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static STOPPING: AtomicBool = AtomicBool::new(false);

struct InFlight;

impl InFlight {
    fn enter() -> InFlight {
        IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
        InFlight
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Lets requests that are already running finish, bounded so a stuck client cannot hold the exit.
fn drain_and_exit() -> ! {
    STOPPING.store(true, Ordering::SeqCst);
    let start = std::time::Instant::now();
    while IN_FLIGHT.load(Ordering::SeqCst) > 0 && start.elapsed() < Duration::from_secs(4) {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::process::exit(0)
}

#[cfg(windows)]
fn install_ctrl_handler() {
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
    }
    // Windows runs this on its own thread and kills the process when it returns (or after ~5 s on console close).
    unsafe extern "system" fn on_ctrl(_event: u32) -> i32 {
        drain_and_exit()
    }
    unsafe {
        SetConsoleCtrlHandler(Some(on_ctrl), 1);
    }
}

#[cfg(not(windows))]
fn install_ctrl_handler() {}

/// Accepts connections forever, one thread per connection. Writes serialize on the vault lock.
pub fn serve_listener(listener: TcpListener, app: Arc<App>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let app = Arc::clone(&app);
        std::thread::spawn(move || {
            if let Err(e) = handle_conn(&app, stream) {
                eprintln!("breadcrumb serve: {e:#}");
            }
        });
    }
}

fn open_browser(url: &str) {
    let r = if cfg!(windows) {
        crate::util::hidden(&mut std::process::Command::new("cmd")).args(["/C", "start", "", url]).spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    if let Err(e) = r {
        eprintln!("could not open a browser ({e}), visit {url}");
    }
}

/// Reads the saved token or makes a new 256 bit one, stored outside the vault so git never tracks it.
pub fn load_or_create_token(dir: &Path) -> Result<String> {
    let file = dir.join("serve-token");
    if let Ok(t) = std::fs::read_to_string(&file) {
        let t = t.trim();
        if t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(t.to_string());
        }
    }
    let token: String = crate::util::random_bytes::<32>().iter().map(|b| format!("{b:02x}")).collect();
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(&file).and_then(|mut f| f.write_all(token.as_bytes())).with_context(|| format!("saving {}", file.display()))?;
    Ok(token)
}

pub fn run(vault_root: &Path, port: u16, open: bool, remote_hosts: Option<Vec<String>>) -> Result<()> {
    crate::session::disable();
    let listener = TcpListener::bind(("127.0.0.1", port)).with_context(|| format!("binding 127.0.0.1:{port}, is it already in use? try --port"))?;
    let port = listener.local_addr()?.port();
    let url = format!("http://127.0.0.1:{port}/");
    eprintln!("breadcrumb web UI on {url} (vault {}), Ctrl+C to stop", crate::util::path_str(vault_root));
    let app = match remote_hosts {
        Some(hosts) => {
            let dir = dirs::data_local_dir().context("cannot find the app data directory")?.join("breadcrumb");
            let token = load_or_create_token(&dir)?;
            eprintln!("remote mode on, token file {}", dir.join("serve-token").display());
            eprintln!("log in once per device: https://{}/login?token={token}", hosts[0]);
            App::with_remote(vault_root, port, hosts, token)
        }
        None => {
            if open {
                open_browser(&url);
            }
            App::new(vault_root, port)
        }
    };
    install_ctrl_handler();
    serve_listener(listener, Arc::new(app));
    Ok(())
}
