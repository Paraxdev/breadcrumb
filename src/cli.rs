use crate::backlog::{self, BoardFilter, MoveReq, QueuePos};
use crate::export;
use crate::format;
use crate::importer;
use crate::kind::{self, KindDef};
use crate::ops::{self, AddReq, Filter, Scope, ScopeSel, SortKey, StatusFilter, UpdateReq};
use crate::util::{path_str, read_body_arg};
use crate::vault::{Vault, resolve_vault_path};
use crate::workflow;
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use serde_json::{Value as Json, json};
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(name = "breadcrumb", version, about = "Local-first, git-backed knowledge vault for AI coding agents")]
pub struct Cli {
    /// Vault directory (default: $BREADCRUMB_HOME or ~/breadcrumb)
    #[arg(long, global = true, value_name = "PATH")]
    pub vault: Option<PathBuf>,
    /// Machine readable JSON output
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Args, Debug, Clone, Default)]
pub struct ScopeArgs {
    /// Project slug (default: detected from the current directory)
    #[arg(long, short = 'p')]
    pub project: Option<String>,
    /// Only global notes
    #[arg(long, conflicts_with_all = ["project", "all"])]
    pub global: bool,
    /// Every project and global
    #[arg(long, conflicts_with = "project")]
    pub all: bool,
}

impl ScopeArgs {
    fn sel(&self) -> ScopeSel {
        ScopeSel { project: self.project.clone(), global: self.global, all: self.all }
    }
}

#[derive(Args, Debug, Clone, Default)]
pub struct FilterArgs {
    #[arg(long, short = 'k')]
    pub kind: Option<String>,
    /// Require this tag (repeatable)
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    /// active, archived or all
    #[arg(long, default_value = "active")]
    pub status: String,
    /// Field filter key=value or key!=value (repeatable)
    #[arg(long = "where", value_name = "K=V")]
    pub wheres: Vec<String>,
    /// Only pinned notes
    #[arg(long)]
    pub pinned: bool,
    #[arg(long, short = 'n')]
    pub limit: Option<usize>,
    /// updated, created or title
    #[arg(long, default_value = "updated")]
    pub sort: String,
}

impl FilterArgs {
    fn filter(&self) -> Result<Filter> {
        Ok(Filter {
            kind: self.kind.clone(),
            tags: self.tags.clone(),
            status: StatusFilter::parse(&self.status)?,
            wheres: self.wheres.clone(),
            pinned: self.pinned,
            limit: self.limit,
            sort: SortKey::parse(&self.sort)?,
        })
    }
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Create a vault, or add missing files to an existing one
    Init {
        #[arg(long)]
        remote: Option<String>,
    },
    /// Show vault path, detected project, remote and dirty state
    Where,
    /// Manage projects
    #[command(subcommand)]
    Project(ProjectCmd),
    /// Add a note
    Add(AddArgs),
    /// Print a full note
    Get { id: String },
    /// Change a note
    Update(UpdateArgs),
    /// Append a paragraph to a note body ("-" reads stdin)
    Append { id: String, text: String },
    /// Link two notes
    Link {
        from: String,
        to: String,
        #[arg(long)]
        rel: Option<String>,
        #[arg(long)]
        remove: bool,
    },
    /// Archive a note
    Archive { id: String },
    /// Restore an archived note to active
    Unarchive { id: String },
    /// Delete a note file (git history keeps it)
    Delete {
        id: String,
        #[arg(long)]
        yes: bool,
    },
    /// List notes
    List {
        #[command(flatten)]
        scope: ScopeArgs,
        #[command(flatten)]
        filter: FilterArgs,
    },
    /// Ranked full-text search
    Search {
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        #[command(flatten)]
        scope: ScopeArgs,
        #[command(flatten)]
        filter: FilterArgs,
    },
    /// Markdown digest for agents to read at session start
    Context {
        #[arg(long, short = 'p')]
        project: Option<String>,
        #[arg(long, conflicts_with = "project")]
        global: bool,
        #[arg(long, short = 'n', default_value_t = 15)]
        limit: usize,
    },
    /// Note kinds (schemas)
    #[command(subcommand)]
    Kind(KindCmd),
    /// Import Claude Code memory or arbitrary files
    #[command(subcommand)]
    Import(ImportCmd),
    /// Export the vault
    #[command(subcommand)]
    Export(ExportCmd),
    /// Clone a bundle into a new vault location
    Restore {
        bundle: PathBuf,
        #[arg(long)]
        to: Option<PathBuf>,
    },
    /// Configure the git remote
    #[command(subcommand)]
    Remote(RemoteCmd),
    /// Commit, pull --rebase and push
    Sync {
        #[arg(short = 'm', long)]
        message: Option<String>,
    },
    /// Run the MCP server on stdio
    Mcp,
    /// Print the command that registers the MCP server with Claude Code
    McpConfig,
    /// Validate every note and report problems
    Doctor,
    /// Print the agent guide (AGENTS.md)
    Guide,
    /// Capture an idea quickly
    Idea {
        title: String,
        #[arg(long)]
        body: Option<String>,
        #[arg(long, short = 'p')]
        project: Option<String>,
        #[arg(long, conflicts_with = "project")]
        global: bool,
        #[arg(long = "tag")]
        tags: Vec<String>,
    },
    /// Backlog items grouped by state
    Board {
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long, short = 'k')]
        kind: Option<String>,
        #[arg(long)]
        assignee: Option<String>,
        #[arg(long)]
        include_closed: bool,
    },
    /// Queue an item (end of queue by default)
    Queue {
        id: String,
        #[arg(long, conflicts_with = "after")]
        top: bool,
        #[arg(long)]
        after: Option<String>,
        #[arg(long)]
        by: Option<String>,
    },
    /// Highest priority unblocked queued item
    Next {
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long)]
        assignee: Option<String>,
        /// Move it to in-progress and assign it
        #[arg(long)]
        claim: bool,
        #[arg(long)]
        by: Option<String>,
    },
    /// Change the state of a backlog item
    Move {
        id: String,
        state: String,
        #[arg(long)]
        resolution: Option<String>,
        #[arg(long)]
        shipped_in: Option<String>,
        #[arg(long)]
        by: Option<String>,
    },
    /// Epic, feature and task hierarchy
    Tree { id: String },
    /// What an item blocks and what blocks it, transitively
    Deps { id: String },
    /// Change the kind of an item, keeping id, links and history
    Promote {
        id: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        by: Option<String>,
    },
    /// Back up and restore Claude Code workflow scripts (.claude/workflows)
    #[command(subcommand)]
    Workflow(WorkflowCmd),
    /// Local web UI on 127.0.0.1
    Serve {
        #[arg(long, default_value_t = 7878)]
        port: u16,
        #[arg(long)]
        open: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ProjectCmd {
    List,
    Show {
        slug: String,
    },
    Add {
        slug: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long = "path")]
        paths: Vec<String>,
    },
    /// Bind a directory and its git remotes to a project, creating it if needed
    Link {
        slug: String,
        #[arg(long)]
        description: Option<String>,
        /// Directory to bind (default: current directory)
        #[arg(long)]
        path: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum WorkflowCmd {
    /// Copy scripts into the vault and record a workflow note for each
    Save {
        #[arg(required = true, num_args = 1..)]
        files: Vec<PathBuf>,
        #[arg(long, short = 'p')]
        project: Option<String>,
        #[arg(long, conflicts_with = "project")]
        global: bool,
    },
    /// Copy the project's stored workflows into <DIR>/.claude/workflows
    Install {
        #[arg(long, short = 'p')]
        project: Option<String>,
        /// Project directory (default: the project containing the cwd, else its first recorded path)
        #[arg(long, value_name = "DIR")]
        to: Option<PathBuf>,
        /// Overwrite local copies that differ from the vault
        #[arg(long)]
        force: bool,
    },
    /// Compare stored workflows with the local .claude/workflows
    Status {
        #[arg(long, short = 'p')]
        project: Option<String>,
        #[arg(long, value_name = "DIR")]
        to: Option<PathBuf>,
    },
    /// Stored workflows of the current project and global
    List {
        #[arg(long, short = 'p')]
        project: Option<String>,
        /// Every project
        #[arg(long, conflicts_with = "project")]
        all: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum KindCmd {
    List,
    Show {
        name: String,
    },
    /// Create or update a kind from flags or a toml file
    Define {
        #[arg(long, required_unless_present = "file")]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
        /// name:type[:required][:a|b|c][:default=x], repeatable
        #[arg(long = "field")]
        fields: Vec<String>,
        #[arg(long)]
        template: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        /// Replace the whole definition instead of merging fields by name
        #[arg(long)]
        replace: bool,
        #[arg(long = "remove-field")]
        remove_fields: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ImportCmd {
    /// Claude Code auto-memory and plans
    Claude {
        #[arg(long)]
        claude_dir: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
        /// dirname=slug, repeatable ("global" as slug maps to global)
        #[arg(long = "project-map")]
        project_map: Vec<String>,
    },
    /// Markdown or text files from a file or directory
    Files {
        path: PathBuf,
        #[arg(long, short = 'p')]
        project: Option<String>,
        #[arg(long, short = 'k')]
        kind: Option<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ExportCmd {
    /// git bundle with full history, for moving PCs
    Bundle { file: PathBuf },
    /// One JSON object per note, kind and project
    Jsonl { file: PathBuf },
}

#[derive(Subcommand, Debug)]
pub enum RemoteCmd {
    Set {
        url: String,
    },
    /// Create a GitHub repo with gh (private by default) and push
    Github {
        name: String,
        #[arg(long)]
        public: bool,
    },
}

#[derive(Args, Debug)]
pub struct AddArgs {
    #[arg(long, short = 't')]
    pub title: String,
    #[arg(long, short = 'k')]
    pub kind: Option<String>,
    #[arg(long, short = 'p')]
    pub project: Option<String>,
    #[arg(long, conflicts_with = "project")]
    pub global: bool,
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    /// key=value, parsed per the kind's field type (repeatable)
    #[arg(long = "field", value_name = "K=V")]
    pub fields: Vec<String>,
    #[arg(long)]
    pub pin: bool,
    /// Body text, "-" reads stdin
    #[arg(long, allow_hyphen_values = true)]
    pub body: Option<String>,
    #[arg(long, conflicts_with = "body")]
    pub body_file: Option<PathBuf>,
    #[arg(long)]
    pub source: Option<String>,
}

#[derive(Args, Debug)]
pub struct UpdateArgs {
    pub id: String,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub kind: Option<String>,
    /// +tag adds, -tag removes (repeatable)
    #[arg(long = "tag", allow_hyphen_values = true)]
    pub tags: Vec<String>,
    #[arg(long = "field", value_name = "K=V")]
    pub fields: Vec<String>,
    #[arg(long)]
    pub unset: Vec<String>,
    #[arg(long, allow_hyphen_values = true)]
    pub body: Option<String>,
    #[arg(long, conflicts_with = "body")]
    pub body_file: Option<PathBuf>,
    #[arg(long, conflicts_with = "unpin")]
    pub pin: bool,
    #[arg(long)]
    pub unpin: bool,
    /// active or archived
    #[arg(long)]
    pub status: Option<String>,
    /// Actor recorded in backlog history
    #[arg(long)]
    pub by: Option<String>,
}

struct Out {
    json: bool,
}

impl Out {
    fn emit(&self, text: impl AsRef<str>, data: Json) {
        if self.json {
            println!("{}", serde_json::to_string_pretty(&data).unwrap_or_default());
        } else {
            let t = text.as_ref();
            if t.ends_with('\n') {
                print!("{t}");
            } else {
                println!("{t}");
            }
        }
    }
}

pub fn main_entry() -> i32 {
    let cli = Cli::parse();
    let json = cli.json;
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            if json {
                println!("{}", json!({ "error": format!("{e:#}") }));
            } else {
                eprintln!("error: {e:#}");
            }
            1
        }
    }
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn scope_label(s: &Scope) -> String {
    s.label()
}

fn notes_json(notes: &[crate::note::Note]) -> Json {
    Json::Array(notes.iter().map(|n| n.to_json()).collect())
}

pub fn run(cli: Cli) -> Result<i32> {
    let out = Out { json: cli.json };
    let root = resolve_vault_path(cli.vault.as_deref())?;
    let open = || Vault::open(&root);
    let cwd = cwd();
    match cli.cmd {
        Cmd::Init { remote } => {
            let (v, rep) = Vault::init(&root, remote.as_deref())?;
            let text = if rep.created_files.is_empty() {
                format!("vault at {} is already complete", path_str(&v.root))
            } else {
                format!("initialized vault at {} ({} files added{})", path_str(&v.root), rep.created_files.len(), if rep.committed { ", committed" } else { "" })
            };
            out.emit(text, json!({"vault": path_str(&v.root), "created_files": rep.created_files, "git_initialized": rep.git_initialized, "committed": rep.committed}));
        }
        Cmd::Where => {
            let w = export::where_info(&root, &cwd);
            let text = if w.exists {
                format!(
                    "vault:   {}\nproject: {}\ngit:     {}\nremote:  {}\ndirty:   {}\nnotes:   {}\ncommits: {}",
                    w.vault,
                    w.project.clone().unwrap_or_else(|| "(none, global)".into()),
                    w.git,
                    w.remote.clone().unwrap_or_else(|| "(none)".into()),
                    w.dirty,
                    w.notes,
                    w.commits
                )
            } else {
                format!("vault:   {} (does not exist, run `breadcrumb init`)", w.vault)
            };
            out.emit(text, serde_json::to_value(&w)?);
        }
        Cmd::Project(pc) => project_cmd(&out, &open()?, pc, &cwd)?,
        Cmd::Add(a) => {
            let v = open()?;
            let project = ops::target_project(&v, a.project.as_deref(), a.global, &cwd)?;
            let new_project = match &project {
                Some(p) => v.load_project(p)?.is_none(),
                None => false,
            };
            let n = ops::add(
                &v,
                AddReq {
                    title: a.title,
                    kind: a.kind,
                    project,
                    tags: a.tags,
                    fields: ops::parse_field_args(&a.fields)?,
                    pinned: a.pin,
                    body: read_body_arg(a.body, a.body_file)?,
                    source: a.source,
                    links: vec![],
                    require_existing_project: false,
                },
            )?;
            let created = if new_project { format!("\n(created new project {})", n.scope_label()) } else { String::new() };
            out.emit(format!("added {}  {}  {}  {}{created}", n.id, n.kind, n.scope_label(), n.title), n.to_json());
        }
        Cmd::Get { id } => {
            let n = open()?.get(&id)?;
            out.emit(format::note_full(&n), n.to_json());
        }
        Cmd::Update(u) => {
            let v = open()?;
            let pinned = if u.pin { Some(true) } else if u.unpin { Some(false) } else { None };
            let n = ops::update(
                &v,
                &u.id,
                UpdateReq {
                    title: u.title,
                    kind: u.kind,
                    tags: u.tags,
                    fields: ops::parse_field_args(&u.fields)?,
                    unset: u.unset,
                    body: read_body_arg(u.body, u.body_file)?,
                    pinned,
                    status: u.status,
                    links: None,
                    by: u.by,
                    ..Default::default()
                },
            )?;
            out.emit(format!("updated {}  {}", n.id, n.title), n.to_json());
        }
        Cmd::Append { id, text } => {
            let text = read_body_arg(Some(text), None)?.unwrap_or_default();
            let n = ops::append(&open()?, &id, &text)?;
            out.emit(format!("appended to {}  {}", n.id, n.title), n.to_json());
        }
        Cmd::Link { from, to, rel, remove } => {
            let n = ops::link(&open()?, &from, &to, rel.as_deref(), remove)?;
            let links: Vec<String> = n.links.iter().map(|l| format!("{} {}", l.rel, l.to)).collect();
            out.emit(format!("{} links: {}", n.id, if links.is_empty() { "(none)".into() } else { links.join(", ") }), n.to_json());
        }
        Cmd::Archive { id } => {
            let n = ops::set_status(&open()?, &id, "archived")?;
            out.emit(format!("archived {}  {}", n.id, n.title), n.to_json());
        }
        Cmd::Unarchive { id } => {
            let n = ops::set_status(&open()?, &id, "active")?;
            out.emit(format!("unarchived {}  {}", n.id, n.title), n.to_json());
        }
        Cmd::Delete { id, yes } => {
            let v = open()?;
            if !yes {
                let n = v.get(&id)?;
                bail!("refusing to delete {} '{}' without --yes", n.id, n.title);
            }
            let n = ops::delete(&v, &id)?;
            out.emit(format!("deleted {}  {} (still in git history)", n.id, n.title), json!({"deleted": n.id, "title": n.title}));
        }
        Cmd::List { scope, filter } => {
            let v = open()?;
            let sc = ops::resolve_scope(&v, &scope.sel(), &cwd)?;
            let notes = ops::list(&v, &sc, &filter.filter()?)?;
            out.emit(format::notes_text(&notes), notes_json(&notes));
        }
        Cmd::Search { query, scope, filter } => {
            let v = open()?;
            let sc = ops::resolve_scope(&v, &scope.sel(), &cwd)?;
            let q = query.join(" ");
            let r = ops::search(&v, &q, &sc, &filter.filter()?)?;
            let hits: Vec<Json> = r
                .hits
                .iter()
                .map(|h| json!({"id": h.note.id, "kind": h.note.kind, "project": h.note.project, "title": h.note.title, "score": h.score, "snippet": h.snippet}))
                .collect();
            out.emit(format::search_text(&r, &scope_label(&sc)), json!({"query": q, "scope": sc.label(), "fell_back_to_or": r.fell_back_to_or, "total": r.total, "hits": hits}));
        }
        Cmd::Context { project, global, limit } => {
            let v = open()?;
            let p = if global { None } else { ops::target_project(&v, project.as_deref(), false, &cwd)? };
            let text = ops::context(&v, p.as_deref(), limit)?;
            out.emit(&text, json!({"project": p, "markdown": text}));
        }
        Cmd::Kind(kc) => kind_cmd(&out, &open()?, kc)?,
        Cmd::Import(ic) => {
            let v = open()?;
            let rep = match ic {
                ImportCmd::Claude { claude_dir, dry_run, project_map } => {
                    let claude_dir = match claude_dir {
                        Some(d) => crate::util::absolutize(&d),
                        None => dirs::home_dir().context("cannot determine home directory, pass --claude-dir")?.join(".claude"),
                    };
                    importer::import_claude(&v, &importer::ClaudeOpts { claude_dir, dry_run, project_map: importer::parse_project_map(&project_map)? })?
                }
                ImportCmd::Files { path, project, kind, tags, dry_run } => importer::import_files(&v, &importer::FilesOpts { path, project, kind, tags, dry_run })?,
            };
            out.emit(rep.table(), rep.to_json());
        }
        Cmd::Export(ec) => {
            let v = open()?;
            match ec {
                ExportCmd::Bundle { file } => {
                    let f = export::bundle(&v, &file)?;
                    out.emit(format!("wrote bundle {}\nrestore on another PC with: breadcrumb restore {}", path_str(&f), path_str(&f)), json!({"bundle": path_str(&f)}));
                }
                ExportCmd::Jsonl { file } => {
                    let (f, n) = export::jsonl(&v, &file)?;
                    out.emit(format!("wrote {n} notes to {}", path_str(&f)), json!({"file": path_str(&f), "notes": n}));
                }
            }
        }
        Cmd::Restore { bundle, to } => {
            let target = match to {
                Some(t) => t,
                None => root.clone(),
            };
            let (dir, remote) = export::restore(&bundle, &target)?;
            let mut text = format!("restored vault to {}", path_str(&dir));
            match &remote {
                Some(r) => text.push_str(&format!("\norigin set to {r}")),
                None => text.push_str("\nno remote recorded, set one with `breadcrumb remote set <url>`"),
            }
            text.push_str("\nnext: run `breadcrumb project link <slug>` in each project directory on this PC");
            out.emit(text, json!({"vault": path_str(&dir), "remote": remote}));
        }
        Cmd::Remote(rc) => {
            let mut v = open()?;
            match rc {
                RemoteCmd::Set { url } => {
                    export::remote_set(&mut v, &url)?;
                    out.emit(format!("origin set to {url}"), json!({"remote": url}));
                }
                RemoteCmd::Github { name, public } => {
                    let url = export::remote_github(&mut v, &name, public)?;
                    out.emit(format!("created GitHub repo and pushed, origin {url}"), json!({"remote": url}));
                }
            }
        }
        Cmd::Sync { message } => {
            let r = export::sync(&open()?, message.as_deref())?;
            let text = format!("{}{}", if r.committed { "committed pending changes\n" } else { "" }, r.message);
            out.emit(text, serde_json::to_value(&r)?);
        }
        Cmd::Mcp => {
            crate::mcp::run_stdio(root, cwd)?;
        }
        Cmd::McpConfig => {
            let exe = std::env::current_exe().context("cannot determine the breadcrumb executable path")?;
            let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
            let exe = path_str(&exe);
            let quoted = if exe.contains(' ') { format!("\"{exe}\"") } else { exe.clone() };
            let cmd = format!("claude mcp add --scope user breadcrumb -- {quoted} mcp");
            out.emit(&cmd, json!({"command": cmd, "exe": exe}));
        }
        Cmd::Doctor => {
            let v = open()?;
            let problems = crate::doctor::check(&v);
            let n = v.note_files().len();
            let errors = problems.iter().filter(|p| p.severity == "error").count();
            out.emit(crate::doctor::text(&problems, n), json!({"notes": n, "errors": errors, "problems": problems}));
            return Ok(if errors > 0 { 1 } else { 0 });
        }
        Cmd::Guide => out.emit(crate::guide::AGENTS_MD, json!({"guide": crate::guide::AGENTS_MD})),
        Cmd::Idea { title, body, project, global, tags } => {
            let v = open()?;
            let project = ops::target_project(&v, project.as_deref(), global, &cwd)?;
            let body = read_body_arg(body, None)?;
            let n = backlog::capture_idea(&v, &title, body, project, tags, false)?;
            out.emit(format!("captured idea {}  {}", backlog::key(&n), n.title), n.to_json());
        }
        Cmd::Board { scope, kind, assignee, include_closed } => {
            let v = open()?;
            let sc = ops::resolve_scope(&v, &scope.sel(), &cwd)?;
            let cols = backlog::board(&v, &sc, &BoardFilter { kind, assignee, include_closed })?;
            out.emit(backlog::board_text(&cols, &sc.label()), json!({"scope": sc.label(), "columns": cols}));
        }
        Cmd::Queue { id, top, after, by } => {
            let pos = match (top, after) {
                (true, _) => QueuePos::Top,
                (_, Some(a)) => QueuePos::After(a),
                _ => QueuePos::End,
            };
            let n = backlog::queue(&open()?, &id, pos, by.as_deref())?;
            out.emit(format!("queued {}", backlog::item_summary(&n)), n.to_json());
        }
        Cmd::Next { scope, assignee, claim, by } => {
            let v = open()?;
            let sc = ops::resolve_scope(&v, &scope.sel(), &cwd)?;
            let claim_by = if claim { Some(by.unwrap_or_else(|| backlog::DEFAULT_ACTOR.to_string())) } else { None };
            match backlog::next(&v, &sc, assignee.as_deref(), claim_by.as_deref())? {
                Some(n) => {
                    let head = if claim_by.is_some() { "claimed" } else { "next" };
                    let mut text = format!("{head}: {}\n", backlog::item_summary(&n));
                    if !n.body.trim().is_empty() {
                        text.push('\n');
                        text.push_str(n.body.trim());
                        text.push('\n');
                    }
                    out.emit(text, n.to_json());
                }
                None => out.emit(format!("no unblocked queued items in {}", sc.label()), Json::Null),
            }
        }
        Cmd::Move { id, state, resolution, shipped_in, by } => {
            let r = backlog::move_item(&open()?, &id, MoveReq { state, resolution, shipped_in, by })?;
            out.emit(backlog::move_text(&r), json!({"from": r.from, "to": r.to, "unmet_acceptance": r.unmet, "note": r.note.to_json()}));
        }
        Cmd::Tree { id } => {
            let text = backlog::tree(&open()?, &id)?;
            out.emit(&text, json!({"tree": text}));
        }
        Cmd::Deps { id } => {
            let v = open()?;
            let d = backlog::deps(&v, &id)?;
            let title = v.get(&d.id)?.title;
            out.emit(backlog::deps_text(&d, &title), serde_json::to_value(&d)?);
        }
        Cmd::Promote { id, to, by } => {
            let n = backlog::promote(&open()?, &id, &to, by.as_deref())?;
            out.emit(format!("promoted {}", backlog::item_summary(&n)), n.to_json());
        }
        Cmd::Workflow(wc) => return workflow_cmd(&out, &open()?, wc, &cwd),
        Cmd::Serve { port, open: open_browser } => {
            open()?;
            crate::serve::run(&root, port, open_browser)?;
        }
    }
    Ok(0)
}

fn project_cmd(out: &Out, v: &Vault, pc: ProjectCmd, cwd: &Path) -> Result<()> {
    match pc {
        ProjectCmd::List => {
            let ps = v.load_projects();
            let detected = v.detect_project(cwd);
            let text = if ps.is_empty() {
                "no projects yet, run `breadcrumb project link <slug>` inside a project directory".to_string()
            } else {
                ps.iter()
                    .map(|p| {
                        let mark = if detected.as_deref() == Some(p.slug.as_str()) { "* " } else { "  " };
                        format!("{mark}{:<24} {}", p.slug, if p.description.is_empty() { &p.name } else { &p.description })
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            out.emit(text, json!({"detected": detected, "projects": ps.iter().map(format::project_json).collect::<Vec<_>>()}));
        }
        ProjectCmd::Show { slug } => {
            let p = v.load_project(&slug)?.with_context(|| format!("unknown project '{slug}'"))?;
            let count = v.load_notes().iter().filter(|n| n.project.as_deref() == Some(slug.as_str())).count();
            out.emit(format!("{}  notes: {count}", format::project_text(&p).trim_end()), format::project_json(&p));
        }
        ProjectCmd::Add { slug, name, description, paths } => {
            let (p, created) = ops::project_add(v, &slug, name.as_deref(), description.as_deref(), &paths)?;
            out.emit(format!("{} project {}", if created { "created" } else { "updated" }, p.slug), format::project_json(&p));
        }
        ProjectCmd::Link { slug, description, path } => {
            let dir = path.unwrap_or_else(|| cwd.to_path_buf());
            let r = ops::project_link(v, &slug, &dir, description.as_deref())?;
            let mut text = format!("{} project {}", if r.created { "created" } else { "linked" }, r.project.slug);
            if let Some(p) = &r.added_path {
                text.push_str(&format!("\n  added path {p}"));
            }
            for rm in &r.added_remotes {
                text.push_str(&format!("\n  added remote {rm}"));
            }
            if r.added_path.is_none() && r.added_remotes.is_empty() {
                text.push_str("\n  (path and remotes were already linked)");
            }
            out.emit(text, json!({"project": format::project_json(&r.project), "created": r.created, "added_path": r.added_path, "added_remotes": r.added_remotes}));
        }
    }
    Ok(())
}

fn workflow_cmd(out: &Out, v: &Vault, wc: WorkflowCmd, cwd: &Path) -> Result<i32> {
    match wc {
        WorkflowCmd::Save { files, project, global } => {
            let saved = workflow::save(v, &files, project.as_deref(), global, cwd)?;
            out.emit(workflow::save_text(&saved), json!(saved));
        }
        WorkflowCmd::Install { project, to, force } => {
            let slug = workflow::resolve_project(v, project.as_deref(), cwd)?;
            let dir = workflow::target_dir(v, &slug, to.as_deref(), cwd)?;
            let r = workflow::install(v, &slug, &dir, force)?;
            out.emit(workflow::install_text(&r), json!({"project": slug, "report": r}));
            if !r.differ.is_empty() {
                return Ok(1);
            }
        }
        WorkflowCmd::Status { project, to } => {
            let slug = workflow::resolve_project(v, project.as_deref(), cwd)?;
            let dir = workflow::target_dir(v, &slug, to.as_deref(), cwd)?;
            let st = workflow::status(v, &slug, &dir)?;
            out.emit(workflow::status_text(&dir, &st), json!({"project": slug, "dir": path_str(&dir), "workflows": st}));
        }
        WorkflowCmd::List { project, all } => {
            let sc = ops::resolve_scope(v, &ScopeSel { project, global: false, all }, cwd)?;
            let ws = workflow::list(v, &sc);
            out.emit(workflow::list_text(&ws, &sc.label()), json!({"scope": sc.label(), "workflows": ws}));
        }
    }
    Ok(0)
}

fn kind_cmd(out: &Out, v: &Vault, kc: KindCmd) -> Result<()> {
    match kc {
        KindCmd::List => {
            let ks = v.load_kinds();
            let text = ks.iter().map(|k| format!("{:<14} {}", k.name, k.description)).collect::<Vec<_>>().join("\n");
            out.emit(text, Json::Array(ks.iter().map(format::kind_json).collect()));
        }
        KindCmd::Show { name } => {
            let k = v.require_kind(&name)?;
            out.emit(format::kind_text(&k), format::kind_json(&k));
        }
        KindCmd::Define { name, description, fields, template, file, replace, remove_fields } => {
            let mut def = match &file {
                Some(f) => KindDef::from_toml(&crate::util::read_text(f)?).with_context(|| format!("parsing {}", f.display()))?,
                None => KindDef::new(name.as_deref().unwrap_or_default(), description.as_deref().unwrap_or_default()),
            };
            if let Some(n) = &name {
                def.name = n.trim().to_string();
            }
            if let Some(d) = &description {
                def.description = d.clone();
            }
            if let Some(t) = template {
                def.template = Some(crate::util::normalize_newlines(&t.replace("\\n", "\n")));
            }
            for spec in &fields {
                let f = kind::parse_field_spec(spec)?;
                def.fields.retain(|x| x.name != f.name);
                def.fields.push(f);
            }
            let (k, created) = ops::define_kind(v, def, replace, &remove_fields)?;
            out.emit(format!("{} kind {}\n{}", if created { "defined" } else { "updated" }, k.name, format::kind_text(&k)), format::kind_json(&k));
        }
    }
    Ok(())
}
