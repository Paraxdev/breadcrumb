//! MCP server: JSON-RPC 2.0 over newline-delimited stdio, no async runtime.

use crate::backlog::{self, BoardFilter, MoveReq, QueuePos};
use crate::format;
use crate::kind::{self, FieldDef, FieldType, KindDef};
use crate::note::Link;
use crate::ops::{self, AddReq, ContextOpts, FieldInput, Filter, ScopeSel, SortKey, StatusFilter, UpdateReq};
use crate::vault::Vault;
use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

pub const SUPPORTED_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];
pub const LATEST_VERSION: &str = "2025-06-18";

pub const INSTRUCTIONS: &str = "breadcrumb is your persistent memory and backlog across sessions. \
The project context is injected at session start, call context only if it is missing or you switch project. \
Search before add, prefer update or append over near duplicates, one fact per note, never store secrets. \
Save decisions, gotchas, user preferences and handoffs, pin preferences every session needs. Track work with move and next.";

const LIST_LIMIT: usize = 20;

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
        let p = s(a, "project");
        let is = |x: &str| p.as_deref() == Some(x);
        let sel = ScopeSel { project: p.clone().filter(|p| p != "global" && p != "all"), global: b(a, "global") || is("global"), all: b(a, "all") || is("all") };
        ops::resolve_scope(v, &sel, &self.cwd_of(a))
    }

    fn target_project(&self, v: &Vault, a: &Map<String, Value>) -> Result<Option<String>> {
        let p = s(a, "project");
        let global = b(a, "global") || p.as_deref() == Some("global");
        ops::target_project(v, p.as_deref().filter(|p| *p != "global"), global, &self.cwd_of(a))
    }

    /// Project the rows are relative to: an explicit slug, else the one detected from cwd.
    fn current(&self, v: &Vault, a: &Map<String, Value>) -> Option<String> {
        match s(a, "project").as_deref() {
            Some("global") => None,
            Some("all") | None if b(a, "global") => None,
            Some("all") | None => v.detect_project(&self.cwd_of(a)),
            Some(p) => Some(p.to_string()),
        }
    }

    pub fn call_tool(&self, name: &str, a: &Map<String, Value>) -> Result<String> {
        let v = self.vault()?;
        match name {
            "context" => {
                let p = self.target_project(&v, a)?;
                ops::context_with(&v, p.as_deref(), ContextOpts { recent: u(a, "limit").unwrap_or(15), kinds: b(a, "kinds"), ..Default::default() })
            }
            "search" => {
                let q = req_s(a, "query")?;
                let sc = self.scope(&v, a)?;
                let r = ops::search(&v, &q, &sc, &filter(a)?)?;
                Ok(format::search_rows(&r, self.current(&v, a).as_deref()))
            }
            "list" => {
                let sc = self.scope(&v, a)?;
                let mut f = filter(a)?;
                f.limit = Some(f.limit.unwrap_or(LIST_LIMIT));
                let (notes, total) = ops::list_counted(&v, &sc, &f)?;
                Ok(format::rows(&notes, total, self.current(&v, a).as_deref()))
            }
            "get" => Ok(format::note_compact(&v.get(&req_s(a, "id")?)?, b(a, "full"))),
            "add" => {
                let project = self.target_project(&v, a)?;
                let n = ops::add(
                    &v,
                    AddReq {
                        title: req_s(a, "title")?,
                        kind: s(a, "kind"),
                        project,
                        tags: strs(a, "tags"),
                        fields: note_fields(a)?,
                        pinned: b(a, "pinned"),
                        body: s(a, "body").map(|x| crate::util::normalize_newlines(&x)),
                        source: s(a, "source"),
                        links: links(a)?,
                        paths: strs(a, "paths"),
                        require_existing_project: true,
                    },
                )?;
                let mut out = format::ack("saved", &n);
                if ops::is_pref(&n) && !n.pinned {
                    out.push_str(&format!("\n{}", ops::PIN_HINT));
                }
                if let Some(h) = ops::similar_hint(&ops::similar(&v, &n)) {
                    out.push_str(&format!("\n{h}"));
                }
                Ok(out)
            }
            "update" if b(a, "verify") && a.keys().all(|k| k == "id" || k == "verify") => {
                let ns = ops::verify(&v, &[req_s(a, "id")?])?;
                Ok(format::ack("verified", &ns[0]))
            }
            "update" => {
                let mut tags: Vec<String> = strs(a, "tags").into_iter().map(|t| if t.starts_with('-') { t } else { format!("+{t}") }).collect();
                tags.extend(strs(a, "add_tags").into_iter().map(|t| format!("+{t}")));
                tags.extend(strs(a, "remove_tags").into_iter().map(|t| format!("-{t}")));
                let n = ops::update(
                    &v,
                    &req_s(a, "id")?,
                    UpdateReq {
                        title: s(a, "title"),
                        kind: s(a, "kind"),
                        tags,
                        fields: note_fields(a)?,
                        unset: strs(a, "unset"),
                        body: s(a, "body").map(|x| crate::util::normalize_newlines(&x)),
                        pinned: a.get("pinned").and_then(Value::as_bool),
                        status: s(a, "status"),
                        links: if a.contains_key("links") { Some(links(a)?) } else { None },
                        by: s(a, "by"),
                        set_paths: if a.contains_key("paths") { Some(strs(a, "paths")) } else { None },
                        refs: strs(a, "refs"),
                        ..Default::default()
                    },
                )?;
                Ok(format::ack(if n.is_archived() && s(a, "status").is_some() { "archived" } else { "updated" }, &n))
            }
            "append" => Ok(format::ack("appended", &ops::append(&v, &req_s(a, "id")?, &req_s(a, "text")?)?)),
            "link" => {
                let rel = s(a, "rel");
                let remove = b(a, "remove");
                let n = ops::link(&v, &req_s(a, "from")?, &req_s(a, "to")?, rel.as_deref(), remove)?;
                let to = v.resolve_id(&req_s(a, "to")?).unwrap_or_else(|_| s(a, "to").unwrap_or_default());
                let rel = rel.unwrap_or_else(|| crate::note::DEFAULT_REL.into());
                Ok(if remove { format!("unlinked {} -> {to}", n.id) } else { format!("linked {} {rel} {to}", n.id) })
            }
            "delete" => {
                if !b(a, "confirm") {
                    bail!("delete needs confirm: true, or archive instead with update status archived");
                }
                Ok(format::ack("deleted", &ops::delete(&v, &req_s(a, "id")?)?))
            }
            "move" => {
                let id = req_s(a, "id")?;
                let state = req_s(a, "state")?;
                let pos = match (s(a, "position").or_else(|| s(a, "after")), state.trim().eq_ignore_ascii_case("queued")) {
                    (Some(p), true) if p.eq_ignore_ascii_case("top") => Some(QueuePos::Top),
                    (Some(p), true) if p.eq_ignore_ascii_case("end") => Some(QueuePos::End),
                    (Some(p), true) => Some(QueuePos::After(p)),
                    (Some(_), false) => bail!("position only applies to state queued"),
                    (None, _) => None,
                };
                if let Some(pos) = pos {
                    let n = backlog::queue(&v, &id, pos, s(a, "by").as_deref())?;
                    return Ok(format::ack("queued", &n));
                }
                let check = s(a, "check").map(|c| backlog::Check::parse(&c)).transpose()?;
                let r = backlog::move_item(&v, &id, MoveReq { state, resolution: s(a, "resolution"), shipped_in: s(a, "shipped_in"), by: s(a, "by"), check, refs: strs(a, "refs") })?;
                let mut out = if r.changed { format!("{} {} -> {}", format::ack("moved", &r.note), r.from, r.to) } else { format!("{} already in {}", r.note.id, r.to) };
                if !r.unmet.is_empty() {
                    let unmet: Vec<String> = backlog::unmet_numbered(&r.note).into_iter().map(|(i, c)| format!("{i}. {c}")).collect();
                    out.push_str(&format!("\nwarning, acceptance criteria not marked [x], tick with move check \"all\" or \"2,3\": {}", unmet.join("; ")));
                }
                Ok(out)
            }
            "next" => {
                let sc = self.scope(&v, a)?;
                let me = backlog::session_name(s(a, "by").as_deref());
                let claim = if b(a, "claim") { Some(me.clone().unwrap_or_else(|| backlog::DEFAULT_ACTOR.into())) } else { None };
                match backlog::next_as(&v, &sc, s(a, "assignee").as_deref(), claim.as_deref(), me.as_deref())? {
                    Some(n) => Ok(format!("{}\n{}", if claim.is_some() { "claimed" } else { "next" }, format::note_compact(&n, false))),
                    None => Ok(format!("no unblocked queued items in {}", sc.label())),
                }
            }
            "claim" => {
                let id = req_s(a, "id")?;
                let by = backlog::session_name(s(a, "by").as_deref()).unwrap_or_else(|| backlog::DEFAULT_ACTOR.into());
                if b(a, "release") {
                    return Ok(match backlog::release(&v, &id, &by, b(a, "force"))? {
                        Some(n) => format::ack("released", &n),
                        None => format!("{id} has no claim to release"),
                    });
                }
                let n = backlog::claim(&v, &id, &by, b(a, "force"))?;
                Ok(format!("{} for {by}", format::ack("claimed", &n)))
            }
            "backlog" => match s(a, "view").as_deref().unwrap_or("board") {
                "board" => {
                    let sc = self.scope(&v, a)?;
                    let cols = backlog::board(&v, &sc, &BoardFilter { kind: s(a, "kind"), assignee: s(a, "assignee"), include_closed: b(a, "include_closed") })?;
                    Ok(backlog::board_compact(&cols))
                }
                "tree" => backlog::tree(&v, &req_s(a, "id")?),
                "deps" => {
                    let d = backlog::deps(&v, &req_s(a, "id")?)?;
                    let title = v.get(&d.id)?.title;
                    Ok(backlog::deps_text(&d, &title))
                }
                other => bail!("view must be board, tree or deps, got '{other}'"),
            },
            "project" => match s(a, "action").as_deref().unwrap_or("list") {
                "list" => {
                    let ps = v.load_projects();
                    let detected = v.detect_project(&self.cwd_of(a));
                    if ps.is_empty() {
                        return Ok("no projects yet, bind a directory with action link".into());
                    }
                    let mut out = format!("detected: {}\n", detected.as_deref().unwrap_or("none (global)"));
                    for p in ps {
                        out.push_str(&p.slug);
                        if !p.description.trim().is_empty() {
                            out.push_str(&format!(", {}", crate::util::one_line(&p.description)));
                        }
                        out.push('\n');
                    }
                    Ok(out)
                }
                "link" => {
                    let path = s(a, "path").map(PathBuf::from).unwrap_or_else(|| self.cwd_of(a));
                    let r = ops::project_link(&v, &req_s(a, "slug")?, &path, s(a, "description").as_deref())?;
                    Ok(format!("{} project {}, paths: {}", if r.created { "created" } else { "linked" }, r.project.slug, r.project.paths.join(", ")))
                }
                other => bail!("action must be list or link, got '{other}'"),
            },
            "kinds" => match a.get("define") {
                Some(Value::Object(d)) => {
                    let (k, created) = ops::define_kind(&v, kind_from_args(d)?, b(d, "replace"), &strs(d, "remove_fields"))?;
                    Ok(format!("{} kind {}", if created { "defined" } else { "updated" }, format::kind_text(&k).trim_end()))
                }
                Some(_) => bail!("define must be an object with at least a name"),
                None => Ok(format::kinds_compact(&v.load_kinds())),
            },
            "workflow" => {
                let cwd = self.cwd_of(a);
                match s(a, "action").as_deref().unwrap_or("list") {
                    "list" => {
                        let sc = self.scope(&v, a)?;
                        Ok(crate::workflow::list_text(&crate::workflow::list(&v, &sc), &sc.label()))
                    }
                    "save" => {
                        let paths: Vec<PathBuf> = strs(a, "paths").into_iter().map(PathBuf::from).collect();
                        if paths.is_empty() {
                            bail!("save needs paths");
                        }
                        let p = s(a, "project");
                        let global = b(a, "global") || p.as_deref() == Some("global");
                        let saved = crate::workflow::save(&v, &paths, p.as_deref().filter(|p| *p != "global"), global, &cwd)?;
                        Ok(crate::workflow::save_text(&saved))
                    }
                    act @ ("install" | "status") => {
                        let slug = crate::workflow::resolve_project(&v, s(a, "project").as_deref(), &cwd)?;
                        let dir = crate::workflow::target_dir(&v, &slug, s(a, "to").map(PathBuf::from).as_deref(), &cwd)?;
                        if act == "install" {
                            Ok(crate::workflow::install_text(&crate::workflow::install(&v, &slug, &dir, b(a, "force"))?))
                        } else {
                            Ok(crate::workflow::status_text(&dir, &crate::workflow::status(&v, &slug, &dir)?))
                        }
                    }
                    other => bail!("action must be list, save, install or status, got '{other}'"),
                }
            }
            "sync" => {
                let r = crate::export::sync(&v, s(a, "message").as_deref())?;
                Ok(format!("{}{}", if r.committed { "committed pending changes\n" } else { "" }, r.message))
            }
            "doctor" if b(a, "fix") => {
                let changes = crate::fix::plan(&v);
                let apply = b(a, "apply");
                let applied = if apply { Some(crate::fix::apply(&v, &changes)?) } else { None };
                let expired = if apply { crate::lifecycle::sweep(&v)? } else { crate::lifecycle::sweep_plan(&v) };
                Ok(format!("{}{}", crate::fix::text(&changes, applied), crate::lifecycle::sweep_text(&expired, apply)).replace("--yes", "apply: true"))
            }
            "doctor" => {
                let problems = crate::doctor::check(&v);
                Ok(crate::doctor::text(&problems, v.note_files().len()))
            }
            other => bail!("unknown tool '{other}'"),
        }
    }
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

/// `fields` plus the options that are fields under the hood.
fn note_fields(a: &Map<String, Value>) -> Result<Vec<(String, FieldInput)>> {
    let f = ops::with_field(fields(a, "fields")?, crate::digest::SUMMARY_KEY, s(a, "summary"));
    Ok(ops::with_field(f, crate::note::EXPIRES_KEY, s(a, "expires")))
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
    let mut o = json!({"type": "object", "properties": props});
    if !required.is_empty() {
        o["required"] = json!(required);
    }
    o
}

fn with(mut a: Value, b: Value) -> Value {
    if let (Value::Object(x), Value::Object(y)) = (&mut a, b) {
        x.extend(y);
    }
    a
}

fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({"name": name, "description": description, "inputSchema": schema})
}

pub fn tool_defs() -> Vec<Value> {
    let st = || json!({"type": "string"});
    let bl = || json!({"type": "boolean"});
    let int = || json!({"type": "integer"});
    let arr = || json!({"type": "array", "items": {"type": "string"}});
    let scope = || json!({"project": {"type": "string", "description": "slug, global or all"}});
    let target = || json!({"project": {"type": "string", "description": "slug or global"}});
    let filters = || json!({"kind": st(), "tags": arr(), "limit": int()});
    let fields = || json!({"type": "object", "description": "kind fields, summary: digest line of a pin, expires: date, 3d or until:id"});
    let links = || json!({"type": "array", "items": {"type": ["string", "object"]}, "description": "ids or {to, rel}"});
    let id = || json!({"id": st()});
    let refs = || json!({"type": "array", "items": {"type": "string"}, "description": "type:value, -ref drops"});
    let by = || json!({"type": "string", "description": "session name"});
    let paths = || json!({"type": "array", "items": {"type": "string"}, "description": "file globs, src/db/**"});
    vec![
        tool("context", "Reload the session start digest.", obj(with(target(), json!({"kinds": bl()})), &[])),
        tool("search", "Full-text search, project plus global.", obj(with(with(scope(), filters()), json!({"query": st()})), &["query"])),
        tool("list", "List notes, newest first.", obj(with(with(scope(), filters()), json!({
            "status": {"type": "string", "enum": ["active", "archived", "all"]},
            "where": {"type": ["object", "array"], "description": "{field: value} or k=v, ref=commit:sha"},
            "sort": {"type": "string", "enum": ["updated", "created", "title"]},
        })), &[])),
        tool("get", "Read a note, full adds default keys.", obj(with(id(), json!({"full": bl()})), &["id"])),
        tool("add", "Save one durable fact, decision, gotcha, preference, handoff or idea.", obj(with(target(), json!({
            "title": st(),
            "kind": {"type": "string", "description": "default note"},
            "body": st(),
            "tags": arr(),
            "fields": fields(),
            "links": links(),
            "pinned": bl(),
            "paths": paths(),
        })), &["title"])),
        tool("update", "Change a note.", obj(with(id(), json!({
            "title": st(),
            "kind": st(),
            "body": {"type": "string", "description": "replaces the body"},
            "tags": {"type": "array", "items": {"type": "string"}, "description": "tags to add, -tag removes"},
            "fields": fields(),
            "unset": {"type": "array", "items": {"type": "string"}, "description": "fields to remove"},
            "links": {"type": "array", "items": {"type": ["string", "object"]}, "description": "replaces all links"},
            "pinned": bl(),
            "status": {"type": "string", "enum": ["active", "archived"]},
            "paths": paths(),
            "refs": refs(),
            "verify": {"type": "boolean", "description": "alone: mark accurate"},
        })), &["id"])),
        tool("append", "Append a paragraph to a note.", obj(with(id(), json!({"text": st()})), &["id", "text"])),
        tool("link", "Add or remove a typed link between notes.", obj(json!({
            "from": st(),
            "to": st(),
            "rel": {"type": "string", "description": "relates (default), blocks, depends-on, supersedes"},
            "remove": bl(),
        }), &["from", "to"])),
        tool("delete", "Delete a note, prefer archiving.", obj(with(id(), json!({"confirm": bl()})), &["id", "confirm"])),
        tool("move", "Change a backlog item's state.", obj(with(id(), json!({
            "state": {"type": "string", "enum": backlog::STATES},
            "position": {"type": "string", "description": "queued: top or id to follow"},
            "resolution": {"type": "string", "description": "required for rejected"},
            "shipped_in": st(),
            "check": {"type": "string", "description": "tick acceptance: all or 2,3"},
            "refs": refs(),
            "by": st(),
        })), &["id", "state"])),
        tool("next", "Top unblocked queued item no other session claimed, claim starts it.", obj(with(json!({"project": st()}), json!({"claim": bl(), "by": by()})), &[])),
        tool("claim", "Claim an item for this session, release frees it.", obj(with(id(), json!({"by": by(), "release": bl(), "force": bl()})), &["id"])),
        tool("backlog", "Board, or tree or deps of one item.", obj(with(json!({"project": st()}), json!({
            "view": {"type": "string", "enum": ["board", "tree", "deps"]},
            "id": st(),
            "include_closed": bl(),
        })), &[])),
        tool("project", "List projects or bind a directory to one.", obj(json!({
            "action": {"type": "string", "enum": ["list", "link"]},
            "slug": st(),
            "path": {"type": "string", "description": "default cwd"},
            "description": st(),
        }), &[])),
        tool("kinds", "List kinds, or define one.", obj(json!({"define": obj(json!({
            "name": st(),
            "description": st(),
            "template": st(),
            "fields": {"type": "array", "items": {"type": ["string", "object"]}, "description": "name:type[:required][:a|b] or {name, type}"},
            "replace": bl(),
            "remove_fields": arr(),
        }), &["name"])}), &[])),
        tool("workflow", "Workflow scripts in the vault.", obj(with(json!({"project": st()}), json!({
            "action": {"type": "string", "enum": ["list", "save", "install", "status"]},
            "paths": {"type": "array", "items": {"type": "string"}, "description": "for save"},
            "to": st(),
            "force": bl(),
        })), &[])),
        tool("sync", "Commit, pull and push the vault.", obj(json!({"message": st()}), &[])),
        tool("doctor", "Validate notes, fix: repair stale paths, mark expired notes.", obj(json!({"fix": bl(), "apply": bl()}), &[])),
    ]
}
