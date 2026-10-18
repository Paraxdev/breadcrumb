//! MCP server: JSON-RPC 2.0 over newline-delimited stdio, no async runtime.

use crate::backlog::{self, BoardFilter, MoveReq, QueuePos};
use crate::format;
use crate::kind::{self, FieldDef, FieldType, KindDef};
use crate::note::Link;
use crate::ops::{self, AddReq, FieldInput, Filter, ScopeSel, SortKey, StatusFilter, UpdateReq};
use crate::vault::Vault;
use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

pub const SUPPORTED_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];
pub const LATEST_VERSION: &str = "2025-06-18";

const INSTRUCTIONS: &str = "breadcrumb is a persistent, git-backed memory shared across agent sessions. \
Call the context tool first in every session to load the project digest (pinned notes, open todos, backlog, kinds). \
Search before adding to avoid duplicates, prefer update or append over near-duplicate notes, and save durable knowledge \
(decisions, gotchas, user preferences, end of session handoffs) with add. One fact per note, never store secrets. \
Define new kinds with define_kind before using them.";

pub struct Server {
    pub vault_root: PathBuf,
    pub cwd: PathBuf,
}

pub fn run_stdio(vault_root: PathBuf, cwd: PathBuf) -> Result<()> {
    eprintln!("breadcrumb mcp: serving vault {} on stdio", crate::util::path_str(&vault_root));
    let srv = Server { vault_root, cwd };
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    srv.serve(stdin.lock(), stdout.lock())
}

fn rpc_error(id: Value, code: i64, msg: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}})
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn text_result(text: String, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

impl Server {
    pub fn serve<R: BufRead, W: Write>(&self, mut reader: R, mut writer: W) -> Result<()> {
        let mut buf = Vec::new();
        loop {
            buf.clear();
            if reader.read_until(b'\n', &mut buf)? == 0 {
                return Ok(());
            }
            // Invalid UTF-8 is a parse error for that one message, the server keeps going.
            let resp = match std::str::from_utf8(&buf) {
                Ok(line) if line.trim().is_empty() => continue,
                Ok(line) => self.handle_line(line.trim_end_matches(['\r', '\n'])),
                Err(_) => Some(rpc_error(Value::Null, -32700, "Parse error")),
            };
            if let Some(resp) = resp {
                writeln!(writer, "{}", serde_json::to_string(&resp)?)?;
                writer.flush()?;
            }
        }
    }

    pub fn handle_line(&self, line: &str) -> Option<Value> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return Some(rpc_error(Value::Null, -32700, "Parse error")),
        };
        match msg {
            Value::Array(items) => {
                if items.is_empty() {
                    return Some(rpc_error(Value::Null, -32600, "Invalid Request"));
                }
                let out: Vec<Value> = items.into_iter().filter_map(|m| self.handle_message(m)).collect();
                if out.is_empty() { None } else { Some(Value::Array(out)) }
            }
            m => self.handle_message(m),
        }
    }

    pub fn handle_message(&self, msg: Value) -> Option<Value> {
        let Value::Object(obj) = msg else {
            return Some(rpc_error(Value::Null, -32600, "Invalid Request"));
        };
        let id = obj.get("id").cloned();
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            // A response from the client to a request we never send, or garbage.
            if obj.contains_key("result") || obj.contains_key("error") {
                return None;
            }
            return Some(rpc_error(id.unwrap_or(Value::Null), -32600, "Invalid Request"));
        };
        // Notifications carry no id and get no response.
        let id = id?;
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        Some(match method {
            "initialize" => {
                let requested = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
                let version = if SUPPORTED_VERSIONS.contains(&requested) { requested } else { LATEST_VERSION };
                rpc_result(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {"name": "breadcrumb", "title": "breadcrumb knowledge vault", "version": env!("CARGO_PKG_VERSION")},
                        "instructions": INSTRUCTIONS,
                    }),
                )
            }
            "ping" => rpc_result(id, json!({})),
            "tools/list" => rpc_result(id, json!({"tools": tool_defs()})),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                if !tool_defs().iter().any(|t| t["name"] == name) {
                    return Some(rpc_error(id, -32602, &format!("Unknown tool: {name}")));
                }
                let args = match params.get("arguments") {
                    Some(Value::Object(m)) => m.clone(),
                    _ => Map::new(),
                };
                let res = match self.call_tool(name, &args) {
                    Ok(text) => text_result(text, false),
                    Err(e) => {
                        eprintln!("breadcrumb mcp: tool {name} failed: {e:#}");
                        text_result(format!("error: {e:#}"), true)
                    }
                };
                rpc_result(id, res)
            }
            _ => rpc_error(id, -32601, &format!("Method not found: {method}")),
        })
    }

    fn vault(&self) -> Result<Vault> {
        Vault::open(&self.vault_root)
    }

    fn cwd_of(&self, a: &Map<String, Value>) -> PathBuf {
        match s(a, "cwd") {
            Some(c) => crate::util::absolutize(Path::new(&c)),
            None => self.cwd.clone(),
        }
    }

    fn scope(&self, v: &Vault, a: &Map<String, Value>) -> Result<ops::Scope> {
        let sel = ScopeSel { project: s(a, "project").filter(|p| p != "global"), global: b(a, "global") || s(a, "project").as_deref() == Some("global"), all: b(a, "all") };
        ops::resolve_scope(v, &sel, &self.cwd_of(a))
    }

    fn target_project(&self, v: &Vault, a: &Map<String, Value>) -> Result<Option<String>> {
        let p = s(a, "project");
        let global = b(a, "global") || p.as_deref() == Some("global");
        ops::target_project(v, p.as_deref().filter(|p| *p != "global"), global, &self.cwd_of(a))
    }

    pub fn call_tool(&self, name: &str, a: &Map<String, Value>) -> Result<String> {
        let v = self.vault()?;
        match name {
            "context" => {
                let p = self.target_project(&v, a)?;
                ops::context(&v, p.as_deref(), u(a, "limit").unwrap_or(15))
            }
            "search" => {
                let q = req_s(a, "query")?;
                let sc = self.scope(&v, a)?;
                let r = ops::search(&v, &q, &sc, &filter(a)?)?;
                Ok(format::search_text(&r, &sc.label()))
            }
            "list" => {
                let sc = self.scope(&v, a)?;
                let mut f = filter(a)?;
                if f.limit.is_none() {
                    f.limit = Some(50);
                }
                let (notes, total) = ops::list_counted(&v, &sc, &f)?;
                let count = if total > notes.len() {
                    format!("showing {} of {total} notes, raise limit or narrow the filters to see the rest", notes.len())
                } else {
                    format!("{total} notes")
                };
                Ok(format!("{} ({count})\n{}", sc.label(), format::notes_text(&notes)))
            }
            "get" => Ok(format::note_full(&v.get(&req_s(a, "id")?)?)),
            "add" => {
                let project = self.target_project(&v, a)?;
                let n = ops::add(
                    &v,
                    AddReq {
                        title: req_s(a, "title")?,
                        kind: s(a, "kind"),
                        project,
                        tags: strs(a, "tags"),
                        fields: fields(a, "fields")?,
                        pinned: b(a, "pinned"),
                        body: s(a, "body").map(|x| crate::util::normalize_newlines(&x)),
                        source: s(a, "source"),
                        links: links(a)?,
                        require_existing_project: true,
                    },
                )?;
                let mut out = format!("added {} ({}, {}) {}\n", n.id, n.kind, n.scope_label(), n.title);
                let similar = similar_notes(&v, &n);
                if !similar.is_empty() {
                    out.push_str("possibly related existing notes (merge or link if duplicate):\n");
                    for x in similar {
                        out.push_str(&format!("- {} {} {} ({})\n", x.id, x.kind, x.title, x.scope_label()));
                    }
                }
                Ok(out)
            }
            "update" => {
                let mut tags: Vec<String> = strs(a, "add_tags").into_iter().map(|t| format!("+{t}")).collect();
                tags.extend(strs(a, "remove_tags").into_iter().map(|t| format!("-{t}")));
                let n = ops::update(
                    &v,
                    &req_s(a, "id")?,
                    UpdateReq {
                        title: s(a, "title"),
                        kind: s(a, "kind"),
                        tags,
                        fields: fields(a, "fields")?,
                        unset: strs(a, "unset"),
                        body: s(a, "body").map(|x| crate::util::normalize_newlines(&x)),
                        pinned: a.get("pinned").and_then(Value::as_bool),
                        status: s(a, "status"),
                        links: if a.contains_key("links") { Some(links(a)?) } else { None },
                        by: s(a, "by"),
                        ..Default::default()
                    },
                )?;
                Ok(format!("updated {} {}", n.id, n.title))
            }
            "append" => {
                let n = ops::append(&v, &req_s(a, "id")?, &req_s(a, "text")?)?;
                Ok(format!("appended to {} {}", n.id, n.title))
            }
            "link" => {
                let n = ops::link(&v, &req_s(a, "from")?, &req_s(a, "to")?, s(a, "rel").as_deref(), b(a, "remove"))?;
                let ls: Vec<String> = n.links.iter().map(|l| format!("{} {}", l.rel, l.to)).collect();
                Ok(format!("{} links: {}", n.id, if ls.is_empty() { "(none)".into() } else { ls.join(", ") }))
            }
            "archive" => {
                let st = if b(a, "unarchive") { "active" } else { "archived" };
                let n = ops::set_status(&v, &req_s(a, "id")?, st)?;
                Ok(format!("{} is now {}: {}", n.id, n.status, n.title))
            }
            "delete" => {
                if !b(a, "confirm") {
                    bail!("delete requires confirm: true (consider archive instead, it keeps the note searchable with status archived)");
                }
                let n = ops::delete(&v, &req_s(a, "id")?)?;
                Ok(format!("deleted {} {} (still in git history)", n.id, n.title))
            }
            "projects" => {
                let ps = v.load_projects();
                let detected = v.detect_project(&self.cwd_of(a));
                if ps.is_empty() {
                    return Ok("no projects yet, bind a directory with project_link".into());
                }
                let mut out = format!("detected for this directory: {}\n", detected.as_deref().unwrap_or("none (global)"));
                for p in ps {
                    out.push_str(&format!("- {}", format::project_text(&p)));
                }
                Ok(out)
            }
            "project_link" => {
                let path = s(a, "path").map(PathBuf::from).unwrap_or_else(|| self.cwd_of(a));
                let r = ops::project_link(&v, &req_s(a, "slug")?, &path, s(a, "description").as_deref())?;
                Ok(format!(
                    "{} project {}\npaths: {}\nremotes: {}",
                    if r.created { "created" } else { "linked" },
                    r.project.slug,
                    r.project.paths.join(", "),
                    r.project.remotes.join(", ")
                ))
            }
            "kinds" => Ok(format::kinds_text(&v.load_kinds())),
            "define_kind" => {
                let def = kind_from_args(a)?;
                let (k, created) = ops::define_kind(&v, def, b(a, "replace"), &strs(a, "remove_fields"))?;
                Ok(format!("{} kind {}\n{}", if created { "defined" } else { "updated" }, k.name, format::kind_text(&k)))
            }
            "sync" => {
                let r = crate::export::sync(&v, s(a, "message").as_deref())?;
                Ok(format!("{}{}", if r.committed { "committed pending changes\n" } else { "" }, r.message))
            }
            "doctor" => {
                let problems = crate::doctor::check(&v);
                Ok(crate::doctor::text(&problems, v.note_files().len()))
            }
            "board" => {
                let sc = self.scope(&v, a)?;
                let cols = backlog::board(&v, &sc, &BoardFilter { kind: s(a, "kind"), assignee: s(a, "assignee"), include_closed: b(a, "include_closed") })?;
                Ok(backlog::board_text(&cols, &sc.label()))
            }
            "next" => {
                let sc = self.scope(&v, a)?;
                let claim = if b(a, "claim") { Some(s(a, "by").unwrap_or_else(|| backlog::DEFAULT_ACTOR.into())) } else { None };
                match backlog::next(&v, &sc, s(a, "assignee").as_deref(), claim.as_deref())? {
                    Some(n) => Ok(format!("{}: {}\n\n{}", if claim.is_some() { "claimed" } else { "next" }, backlog::item_summary(&n), format::note_full(&n))),
                    None => Ok(format!("no unblocked queued items in {}", sc.label())),
                }
            }
            "move" => {
                let r = backlog::move_item(
                    &v,
                    &req_s(a, "id")?,
                    MoveReq { state: req_s(a, "state")?, resolution: s(a, "resolution"), shipped_in: s(a, "shipped_in"), by: s(a, "by") },
                )?;
                Ok(backlog::move_text(&r))
            }
            "queue" => {
                let pos = match (s(a, "after"), s(a, "position").as_deref()) {
                    (Some(x), _) => QueuePos::After(x),
                    (None, Some("top")) => QueuePos::Top,
                    _ => QueuePos::End,
                };
                let n = backlog::queue(&v, &req_s(a, "id")?, pos, s(a, "by").as_deref())?;
                Ok(format!("queued {}", backlog::item_summary(&n)))
            }
            "capture_idea" => {
                let project = self.target_project(&v, a)?;
                let n = backlog::capture_idea(&v, &req_s(a, "title")?, s(a, "body"), project, strs(a, "tags"), true)?;
                Ok(format!("captured idea {} {}", backlog::key(&n), n.title))
            }
            "tree" => backlog::tree(&v, &req_s(a, "id")?),
            "deps" => {
                let d = backlog::deps(&v, &req_s(a, "id")?)?;
                let title = v.get(&d.id)?.title;
                Ok(backlog::deps_text(&d, &title))
            }
            "workflow_list" => {
                let sc = self.scope(&v, a)?;
                Ok(crate::workflow::list_text(&crate::workflow::list(&v, &sc), &sc.label()))
            }
            "workflow_save" => {
                let paths: Vec<PathBuf> = strs(a, "paths").into_iter().map(PathBuf::from).collect();
                let p = s(a, "project");
                let global = b(a, "global") || p.as_deref() == Some("global");
                let saved = crate::workflow::save(&v, &paths, p.as_deref().filter(|p| *p != "global"), global, &self.cwd_of(a))?;
                Ok(crate::workflow::save_text(&saved))
            }
            "workflow_install" => {
                let cwd = self.cwd_of(a);
                let slug = crate::workflow::resolve_project(&v, s(a, "project").as_deref(), &cwd)?;
                let dir = crate::workflow::target_dir(&v, &slug, s(a, "to").map(PathBuf::from).as_deref(), &cwd)?;
                Ok(crate::workflow::install_text(&crate::workflow::install(&v, &slug, &dir, b(a, "force"))?))
            }
            "workflow_status" => {
                let cwd = self.cwd_of(a);
                let slug = crate::workflow::resolve_project(&v, s(a, "project").as_deref(), &cwd)?;
                let dir = crate::workflow::target_dir(&v, &slug, s(a, "to").map(PathBuf::from).as_deref(), &cwd)?;
                Ok(crate::workflow::status_text(&dir, &crate::workflow::status(&v, &slug, &dir)?))
            }
            other => bail!("unknown tool '{other}'"),
        }
    }
}

/// Near duplicates of a freshly added note: notes in its project or global that match every title term,
/// exact titles first. Any-term matches are ignored, one shared common word is not a duplicate.
fn similar_notes(v: &Vault, n: &crate::note::Note) -> Vec<crate::note::Note> {
    let pool: Vec<_> = v
        .load_notes()
        .into_iter()
        .filter(|x| x.id != n.id && !x.is_archived() && (x.project == n.project || x.project.is_none()))
        .collect();
    let r = crate::search::search(pool, &n.title);
    if r.fell_back_to_or {
        return vec![];
    }
    let title = crate::util::one_line(&n.title).to_lowercase();
    let terms = crate::search::tokenize(&n.title);
    let mut hits: Vec<(bool, usize, f64, crate::note::Note)> = r
        .hits
        .into_iter()
        .filter(|h| h.score >= 6.0)
        .map(|h| {
            let exact = crate::util::one_line(&h.note.title).to_lowercase() == title;
            let toks = crate::search::tokenize(&h.note.title);
            let in_title = terms.iter().filter(|t| toks.contains(t)).count();
            (exact, in_title, h.score, h.note)
        })
        .filter(|(exact, in_title, _, _)| *exact || *in_title * 2 >= terms.len().max(1))
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal)));
    hits.into_iter().take(3).map(|h| h.3).collect()
}

fn s(a: &Map<String, Value>, k: &str) -> Option<String> {
    match a.get(k)? {
        Value::String(x) => Some(x.clone()).filter(|x| !x.trim().is_empty()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn req_s(a: &Map<String, Value>, k: &str) -> Result<String> {
    s(a, k).ok_or_else(|| anyhow!("missing required argument '{k}'"))
}

fn b(a: &Map<String, Value>, k: &str) -> bool {
    match a.get(k) {
        Some(Value::Bool(x)) => *x,
        Some(Value::String(x)) => matches!(x.to_ascii_lowercase().as_str(), "true" | "yes" | "1"),
        _ => false,
    }
}

fn u(a: &Map<String, Value>, k: &str) -> Option<usize> {
    match a.get(k)? {
        Value::Number(n) => n.as_u64().map(|x| x as usize),
        Value::String(x) => x.trim().parse().ok(),
        _ => None,
    }
}

/// Accepts a JSON array of strings or a comma separated string.
fn strs(a: &Map<String, Value>, k: &str) -> Vec<String> {
    match a.get(k) {
        Some(Value::Array(xs)) => xs.iter().filter_map(|x| x.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect(),
        Some(Value::String(x)) => x.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        _ => vec![],
    }
}

fn fields(a: &Map<String, Value>, k: &str) -> Result<Vec<(String, FieldInput)>> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::Object(m)) => Ok(m.iter().map(|(k, v)| (k.clone(), FieldInput::Value(kind::json_to_yaml(v)))).collect()),
        Some(_) => bail!("'{k}' must be an object of field name to value"),
    }
}

fn links(a: &Map<String, Value>) -> Result<Vec<Link>> {
    let mut out = vec![];
    if let Some(Value::Array(xs)) = a.get("links") {
        for x in xs {
            match x {
                Value::String(to) => out.push(Link { to: to.clone(), rel: crate::note::DEFAULT_REL.into() }),
                Value::Object(m) => {
                    let to = m.get("to").and_then(Value::as_str).ok_or_else(|| anyhow!("link needs 'to'"))?;
                    let rel = m.get("rel").and_then(Value::as_str).filter(|r| !r.trim().is_empty()).unwrap_or(crate::note::DEFAULT_REL);
                    out.push(Link { to: to.to_string(), rel: rel.to_string() });
                }
                _ => bail!("links must be ids or {{to, rel}} objects"),
            }
        }
    }
    Ok(out)
}

fn filter(a: &Map<String, Value>) -> Result<Filter> {
    let mut wheres = strs(a, "where");
    if let Some(Value::Object(m)) = a.get("where") {
        wheres = m.iter().map(|(k, v)| format!("{k}={}", v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))).collect();
    }
    Ok(Filter {
        kind: s(a, "kind"),
        tags: strs(a, "tags"),
        status: match s(a, "status") {
            Some(x) => StatusFilter::parse(&x)?,
            None => StatusFilter::Active,
        },
        wheres,
        pinned: b(a, "pinned"),
        limit: u(a, "limit"),
        sort: match s(a, "sort") {
            Some(x) => SortKey::parse(&x)?,
            None => SortKey::Updated,
        },
    })
}

fn kind_from_args(a: &Map<String, Value>) -> Result<KindDef> {
    let mut k = KindDef::new(&req_s(a, "name")?, &s(a, "description").unwrap_or_default());
    k.template = s(a, "template");
    if let Some(Value::Array(fs)) = a.get("fields") {
        for f in fs {
            let fd = match f {
                Value::String(spec) => kind::parse_field_spec(spec)?,
                Value::Object(m) => {
                    let name = m.get("name").and_then(Value::as_str).ok_or_else(|| anyhow!("field needs a name"))?;
                    let ty = FieldType::parse(m.get("type").and_then(Value::as_str).unwrap_or("string"))?;
                    let mut fd = FieldDef::new(name, ty);
                    fd.required = m.get("required").and_then(Value::as_bool).unwrap_or(false);
                    fd.description = m.get("description").and_then(Value::as_str).map(str::to_string);
                    fd.values = m.get("values").and_then(Value::as_array).map(|xs| xs.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
                    if let Some(d) = m.get("default").filter(|d| !d.is_null()) {
                        let y = kind::json_to_yaml(d);
                        let y = match &y {
                            serde_yaml::Value::String(raw) => fd.parse_value(raw).unwrap_or(y),
                            _ => y,
                        };
                        fd.default = kind::yaml_to_toml(&y);
                    }
                    fd
                }
                _ => bail!("fields must be objects {{name, type, ...}} or spec strings name:type"),
            };
            k.fields.push(fd);
        }
    }
    Ok(k)
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

fn scope_props(extra: Value) -> Value {
    let mut m = json!({
        "project": {"type": "string", "description": "project slug, or 'global'; default is detected from cwd"},
        "global": {"type": "boolean", "description": "only global notes"},
        "all": {"type": "boolean", "description": "every project"},
        "cwd": {"type": "string", "description": "directory used for project detection, default is the server's cwd"},
    });
    if let (Value::Object(m), Value::Object(e)) = (&mut m, extra) {
        m.extend(e);
    }
    m
}

fn filter_props() -> Value {
    json!({
        "kind": {"type": "string"},
        "tags": {"type": "array", "items": {"type": "string"}, "description": "all must match"},
        "status": {"type": "string", "enum": ["active", "archived", "all"]},
        "where": {"description": "field filters, object {field: value} or array of 'k=v' / 'k!=v'", "anyOf": [{"type": "object"}, {"type": "array", "items": {"type": "string"}}]},
        "pinned": {"type": "boolean"},
        "limit": {"type": "integer", "minimum": 1},
        "sort": {"type": "string", "enum": ["updated", "created", "title"]},
    })
}

fn merge(a: Value, b: Value) -> Value {
    match (a, b) {
        (Value::Object(mut x), Value::Object(y)) => {
            x.extend(y);
            Value::Object(x)
        }
        (x, _) => x,
    }
}

fn tool(name: &str, title: &str, description: &str, schema: Value) -> Value {
    json!({"name": name, "title": title, "description": description, "inputSchema": schema})
}

pub fn tool_defs() -> Vec<Value> {
    let id = json!({"id": {"type": "string", "description": "note id or unique prefix (4+ chars)"}});
    let by = json!({"by": {"type": "string", "description": "actor name for the backlog history, default 'user'"}});
    let fields_p = json!({"type": "object", "description": "kind fields and free-form extra fields, e.g. {\"priority\": \"p1\"}", "additionalProperties": true});
    let links_p = json!({"type": "array", "items": {"anyOf": [{"type": "string"}, {"type": "object", "properties": {"to": {"type": "string"}, "rel": {"type": "string"}}, "required": ["to"]}]}, "description": "links to other notes, rel defaults to 'relates' (blocks, depends-on, implements, duplicates, ...)"});
    vec![
        tool("context", "Session context", "Call first in every session. Markdown digest of the current project: pinned notes in full, open todos, backlog summary, recent notes, available kinds.", obj(json!({
            "project": {"type": "string", "description": "project slug or 'global', default detected from cwd"},
            "global": {"type": "boolean", "description": "global digest regardless of cwd"},
            "cwd": {"type": "string"},
            "limit": {"type": "integer", "description": "recent notes to list, default 15"},
        }), &[])),
        tool("search", "Search notes", "Ranked full-text search over titles, tags, fields and bodies. Default scope is the current project plus global. Use \"quoted phrases\" for exact matches.", obj(merge(scope_props(json!({"query": {"type": "string"}})), filter_props()), &["query"])),
        tool("list", "List notes", "List notes with filters, newest first.", obj(merge(scope_props(json!({})), filter_props()), &[])),
        tool("get", "Get note", "Full note with frontmatter fields and body.", obj(id.clone(), &["id"])),
        tool("add", "Add note", "Save durable knowledge. Search first to avoid duplicates. Project defaults to the one detected from cwd.", obj(json!({
            "title": {"type": "string", "description": "short specific title"},
            "kind": {"type": "string", "description": "note kind, see kinds tool; default note"},
            "project": {"type": "string", "description": "existing project slug or 'global', create projects with project_link"},
            "global": {"type": "boolean"},
            "cwd": {"type": "string"},
            "tags": {"type": "array", "items": {"type": "string"}},
            "fields": fields_p.clone(),
            "pinned": {"type": "boolean", "description": "include in full in every context call"},
            "body": {"type": "string", "description": "markdown body"},
            "links": links_p.clone(),
            "source": {"type": "string", "description": "provenance"},
        }), &["title"])),
        tool("update", "Update note", "Change title, kind, tags, fields, body, pin or status of a note. Backlog state changes are recorded in the history.", obj(merge(id.clone(), merge(by.clone(), json!({
            "title": {"type": "string"},
            "kind": {"type": "string"},
            "add_tags": {"type": "array", "items": {"type": "string"}},
            "remove_tags": {"type": "array", "items": {"type": "string"}},
            "fields": fields_p.clone(),
            "unset": {"type": "array", "items": {"type": "string"}, "description": "field names to remove"},
            "body": {"type": "string", "description": "replaces the whole body"},
            "pinned": {"type": "boolean"},
            "status": {"type": "string", "enum": ["active", "archived"]},
            "links": merge(links_p.clone(), json!({"description": "replaces all links"})),
        }))), &["id"])),
        tool("append", "Append to note", "Append a paragraph to a note body.", obj(merge(id.clone(), json!({"text": {"type": "string"}})), &["id", "text"])),
        tool("link", "Link notes", "Add (or remove) a typed link between notes. Deduplicated.", obj(json!({
            "from": {"type": "string"},
            "to": {"type": "string"},
            "rel": {"type": "string", "description": "relates (default), blocks, depends-on, implements, duplicates, supersedes, ..."},
            "remove": {"type": "boolean"},
        }), &["from", "to"])),
        tool("archive", "Archive note", "Archive a note (or unarchive with unarchive: true). Archived notes are hidden from default lists.", obj(merge(id.clone(), json!({"unarchive": {"type": "boolean"}})), &["id"])),
        tool("delete", "Delete note", "Delete a note file, git history keeps it. Requires confirm: true. Prefer archive.", obj(merge(id.clone(), json!({"confirm": {"type": "boolean"}})), &["id", "confirm"])),
        tool("projects", "List projects", "Known projects and which one is detected for the cwd.", obj(json!({"cwd": {"type": "string"}}), &[])),
        tool("project_link", "Link project", "Bind a directory (default cwd) and its git remotes to a project, creating it if needed.", obj(json!({
            "slug": {"type": "string", "description": "lowercase [a-z0-9-]"},
            "path": {"type": "string"},
            "description": {"type": "string"},
            "cwd": {"type": "string"},
        }), &["slug"])),
        tool("kinds", "List kinds", "Available note kinds with their typed fields.", obj(json!({}), &[])),
        tool("define_kind", "Define kind", "Create or extend a note kind. Fields merge by name unless replace is true.", obj(json!({
            "name": {"type": "string"},
            "description": {"type": "string"},
            "template": {"type": "string", "description": "markdown body used when a note's body is empty"},
            "fields": {"type": "array", "items": {"anyOf": [
                {"type": "string", "description": "name:type[:required][:a|b|c]"},
                {"type": "object", "properties": {
                    "name": {"type": "string"},
                    "type": {"type": "string", "enum": ["string", "int", "float", "bool", "date", "enum", "list", "ref", "url"]},
                    "required": {"type": "boolean"},
                    "description": {"type": "string"},
                    "default": {},
                    "values": {"type": "array", "items": {"type": "string"}},
                }, "required": ["name", "type"]},
            ]}},
            "replace": {"type": "boolean"},
            "remove_fields": {"type": "array", "items": {"type": "string"}},
        }), &["name"])),
        tool("sync", "Sync vault", "Commit pending changes, pull --rebase and push to the git remote.", obj(json!({"message": {"type": "string"}}), &[])),
        tool("doctor", "Check vault", "Validate every note: parse errors, unknown kinds, broken links, duplicate ids, missing required fields.", obj(json!({}), &[])),
        tool("board", "Backlog board", "Backlog items (idea, epic, feature, task, bug) grouped by state in workflow order.", obj(scope_props(json!({
            "kind": {"type": "string"},
            "assignee": {"type": "string"},
            "include_closed": {"type": "boolean"},
        })), &[])),
        tool("next", "Next item", "Highest priority, lowest rank queued item that is not blocked. claim: true moves it to in-progress and assigns it to 'by'.", obj(scope_props(merge(by.clone(), json!({
            "assignee": {"type": "string"},
            "claim": {"type": "boolean"},
        }))), &[])),
        tool("move", "Move item", "Change a backlog item's state: idea, backlog, queued, in-progress, review, done, shipped, rejected. rejected requires resolution.", obj(merge(id.clone(), merge(by.clone(), json!({
            "state": {"type": "string", "enum": backlog::STATES},
            "resolution": {"type": "string"},
            "shipped_in": {"type": "string", "description": "version, commit or PR"},
        }))), &["id", "state"])),
        tool("queue", "Queue item", "Set state queued and rank it (end of queue by default).", obj(merge(id.clone(), merge(by.clone(), json!({
            "position": {"type": "string", "enum": ["end", "top"]},
            "after": {"type": "string", "description": "place right after this queued item"},
        }))), &["id"])),
        tool("capture_idea", "Capture idea", "Quickly record an idea in the backlog (kind idea, state idea).", obj(json!({
            "title": {"type": "string"},
            "body": {"type": "string"},
            "project": {"type": "string", "description": "existing project slug or 'global'"},
            "global": {"type": "boolean"},
            "cwd": {"type": "string"},
            "tags": {"type": "array", "items": {"type": "string"}},
        }), &["title"])),
        tool("tree", "Item tree", "Epic, feature and task hierarchy under an item, with states.", obj(id.clone(), &["id"])),
        tool("deps", "Item dependencies", "What an item blocks and what blocks it, transitively.", obj(id, &["id"])),
        tool("workflow_list", "List workflows", "Claude Code workflow scripts stored in the vault, for the current project plus global by default.", obj(json!({
            "project": {"type": "string", "description": "project slug or 'global', default detected from cwd"},
            "global": {"type": "boolean", "description": "only global workflows"},
            "all": {"type": "boolean", "description": "every project"},
            "cwd": {"type": "string"},
        }), &[])),
        tool("workflow_save", "Save workflows", "Back up .claude/workflows/<name>.js scripts into the vault and record a workflow note for each. Unchanged scripts are not committed again.", obj(json!({
            "paths": {"type": "array", "items": {"type": "string"}, "minItems": 1, "description": "workflow .js files, relative paths resolve against cwd"},
            "project": {"type": "string", "description": "existing project slug or 'global', default detected from the file location or cwd"},
            "global": {"type": "boolean"},
            "cwd": {"type": "string"},
        }), &["paths"])),
        tool("workflow_install", "Install workflows", "Copy the project's stored workflows into <to>/.claude/workflows. Local copies that differ are kept unless force is true.", obj(json!({
            "project": {"type": "string", "description": "project slug, default detected from cwd"},
            "to": {"type": "string", "description": "project directory, default the project containing cwd, else its first recorded path"},
            "force": {"type": "boolean", "description": "overwrite local copies that differ"},
            "cwd": {"type": "string"},
        }), &[])),
        tool("workflow_status", "Workflow status", "Per workflow: same, local newer, vault newer, missing locally or only local.", obj(json!({
            "project": {"type": "string", "description": "project slug, default detected from cwd"},
            "to": {"type": "string", "description": "project directory, default as for workflow_install"},
            "cwd": {"type": "string"},
        }), &[])),
    ]
}
