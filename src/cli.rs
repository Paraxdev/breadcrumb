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
    /// Has this ref, type:value or just the value (repeatable)
    #[arg(long = "ref", value_name = "REF")]
    pub refs: Vec<String>,
}

impl FilterArgs {
    fn filter(&self) -> Result<Filter> {
        Ok(Filter {
            kind: self.kind.clone(),
            tags: self.tags.clone(),
            status: StatusFilter::parse(&self.status)?,
            wheres: self.wheres.iter().cloned().chain(self.refs.iter().map(|r| format!("ref={r}"))).collect(),
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
    /// Mark notes as still accurate, without changing them otherwise
    Verify {
        #[arg(required = true, num_args = 1..)]
        ids: Vec<String>,
    },
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
    /// Compact digest for agents to read at session start
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
    Doctor {
        /// Propose fixes: rewrite moved file paths, drop or mark gone ones, mark expired notes (dry run without --yes)
        #[arg(long)]
        fix: bool,
        /// Apply the --fix changes in one commit
        #[arg(long, requires = "fix")]
        yes: bool,
    },
    /// Print the agent guide (AGENTS.md)
    Guide,
    /// Size in chars and approximate tokens of what breadcrumb injects into an agent's context
    Stats {
        /// Sample first prompt, to measure the prompt recall hook
        #[arg(long)]
        prompt: Option<String>,
        /// Sample file, to measure the file notes hook
        #[arg(long)]
        file: Option<PathBuf>,
    },
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
    /// Machine facts (kind env) for this computer: where files, folders and tools live
    Env {
        #[command(flatten)]
        scope: ScopeArgs,
        /// Every machine, not only this one
        #[arg(long)]
        all_machines: bool,
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
        /// Session name (default: $BREADCRUMB_SESSION, else the Claude Code session id)
        #[arg(long)]
        by: Option<String>,
    },
    /// Claim an item for this session, `next` in other sessions skips it until the claim is a day without activity
    Claim {
        id: String,
        /// Session name (default: $BREADCRUMB_SESSION, else the Claude Code session id)
        #[arg(long)]
        by: Option<String>,
        /// Take over another session's fresh claim
        #[arg(long)]
        force: bool,
    },
    /// Release the claim on an item
    Release {
        id: String,
        #[arg(long)]
        by: Option<String>,
        /// Release another session's fresh claim
        #[arg(long)]
        force: bool,
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
        /// Also mark acceptance criteria met: all, or positions like 2,3
        #[arg(long, value_name = "WHICH")]
        check: Option<String>,
        /// Add a ref like commit:abc123, run:wf_1, branch:fix/x, pr:12, -type:value removes (repeatable)
        #[arg(long = "ref", value_name = "REF", allow_hyphen_values = true)]
        refs: Vec<String>,
    },
    /// Mark acceptance criteria of an item met ([x])
    Check {
        id: String,
        /// all (default), or 1 based positions like 2,3
        which: Option<String>,
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
    /// Code maps of a project's areas, lines of `file:line symbol, what it does` checked against the working tree
    #[command(subcommand)]
    Map(MapCmd),
    /// What to read before working on files, directories or map areas: their map lines with stale ones marked,
    /// and the gotchas, decisions and feedback whose path globs match, in about 2k tokens
    Brief {
        #[arg(required = true, num_args = 1..)]
        targets: Vec<String>,
        #[arg(long, short = 'p')]
        project: Option<String>,
        /// Git checkout to check freshness against (default: the one holding the cwd, else a recorded project path)
        #[arg(long, value_name = "DIR")]
        repo: Option<PathBuf>,
        /// Most chars to print
        #[arg(long, default_value_t = crate::map::BRIEF_BUDGET)]
        budget: usize,
    },
    /// Local web UI on 127.0.0.1
    Serve {
        #[arg(long, default_value_t = 7878)]
        port: u16,
        #[arg(long)]
        open: bool,
    },
    /// Entry points that Claude Code hooks call (reads the hook JSON on stdin, never fails)
    #[command(subcommand)]
    Hook(HookCmd),
    /// Register breadcrumb hooks in Claude Code settings
    #[command(subcommand)]
    Hooks(HooksCmd),
    /// Rebuild and reinstall breadcrumb with cargo, safe while other breadcrumb processes run
    Upgrade {
        /// breadcrumb source directory (default: the one this binary was built from)
        #[arg(long, value_name = "DIR")]
        from: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum HookCmd {
    /// Print the vault digest for the session's project as SessionStart additionalContext
    SessionStart,
    /// Start a background Claude memory import and sync when there is something to do
    SessionEnd {
        #[arg(long)]
        no_import: bool,
    },
    /// UserPromptSubmit: notes relevant to the first prompt of a session
    Prompt,
    /// PreToolUse on file tools: notes whose path globs match the file
    FileTouch,
    /// Stop: one handoff reminder when the session changed notes and wrote no handoff
    Stop,
    /// The detached work SessionEnd starts
    #[command(hide = true)]
    Background {
        #[arg(long)]
        no_import: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum HooksCmd {
    /// Add SessionStart (context) and SessionEnd (sync) hooks
    Install {
        /// Settings file (default: ~/.claude/settings.json)
        #[arg(long, value_name = "PATH")]
        settings: Option<PathBuf>,
        /// Skip the SessionEnd background sync hook
        #[arg(long)]
        no_sync: bool,
        /// Do not import Claude Code auto-memory at session end
        #[arg(long)]
        no_import: bool,
        /// Skip the UserPromptSubmit first prompt recall hook
        #[arg(long)]
        no_recall: bool,
        /// Skip the PreToolUse hook that shows notes scoped to the touched file
        #[arg(long)]
        no_file_notes: bool,
        /// Skip the Stop hook that asks once per session for a handoff note
        #[arg(long)]
        no_handoff: bool,
    },
    /// Remove breadcrumb hooks, leaving every other setting alone
    Uninstall {
        #[arg(long, value_name = "PATH")]
        settings: Option<PathBuf>,
    },
    /// Show which breadcrumb hooks are registered
    Status {
        #[arg(long, value_name = "PATH")]
        settings: Option<PathBuf>,
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
pub enum MapCmd {
    /// Create the area's map or merge lines into it, a line for the same file and symbol replaces the old one
    Put {
        area: String,
        /// `file:line symbol, what it does` (repeatable)
        #[arg(long = "line", value_name = "LINE")]
        lines: Vec<String>,
        /// File with one map line per line, - reads stdin
        #[arg(long, value_name = "FILE")]
        lines_file: Option<PathBuf>,
        /// Path glob the area covers (repeatable), default the files its lines name
        #[arg(long = "path-glob", value_name = "GLOB")]
        paths: Vec<String>,
        /// Remove the lines of a file, or one line by `file symbol` (repeatable)
        #[arg(long, value_name = "KEY")]
        drop: Vec<String>,
        #[arg(long, short = 'p')]
        project: Option<String>,
        #[arg(long, value_name = "DIR")]
        repo: Option<PathBuf>,
    },
    /// Show an area's map with stale lines marked
    Get {
        area: String,
        #[arg(long, short = 'p')]
        project: Option<String>,
        #[arg(long, value_name = "DIR")]
        repo: Option<PathBuf>,
    },
    /// One header line per map of the project
    List {
        #[arg(long, short = 'p')]
        project: Option<String>,
        #[arg(long, value_name = "DIR")]
        repo: Option<PathBuf>,
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
        /// Print nothing when nothing changed, one line otherwise
        #[arg(long)]
        quiet: bool,
        /// Replace each imported MEMORY.md with a one line pointer to breadcrumb, keeping MEMORY.md.breadcrumb.bak
        #[arg(long)]
        retire_index: bool,
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
    /// File glob relative to the project root the note applies to, like src/engine/** (repeatable)
    #[arg(long = "path-glob", value_name = "GLOB")]
    pub paths: Vec<String>,
    /// One line the session digest shows for a pinned note, instead of the title's first sentence
    #[arg(long)]
    pub summary: Option<String>,
    /// When it stops holding: a date (the last day), an RFC3339 time, 3d or 12h from now, or until:<id> is closed
    #[arg(long, value_name = "WHEN")]
    pub expires: Option<String>,
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
    /// +glob or glob adds a file glob, -glob removes it (repeatable)
    #[arg(long = "path-glob", value_name = "GLOB", allow_hyphen_values = true)]
    pub paths: Vec<String>,
    /// Add a ref like commit:abc123, run:wf_1, branch:fix/x, pr:12, -type:value removes (repeatable)
    #[arg(long = "ref", value_name = "REF", allow_hyphen_values = true)]
    pub refs: Vec<String>,
    /// One line the session digest shows for a pinned note
    #[arg(long)]
    pub summary: Option<String>,
    /// A date, an RFC3339 time, 3d or 12h from now, or until:<id>, `--unset expires` drops it
    #[arg(long, value_name = "WHEN")]
    pub expires: Option<String>,
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
    crate::upgrade::cleanup_old_exes();
    let cli = Cli::parse();
    if let Cmd::Hook(h) = &cli.cmd {
        run_hook(cli.vault.as_deref(), h);
        return 0;
    }
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

/// Hooks run inside every Claude Code session, so failures stay silent and the exit code is always 0.
fn run_hook(vault: Option<&Path>, h: &HookCmd) {
    let Ok(root) = resolve_vault_path(vault) else { return };
    let input = || {
        let mut stdin = String::new();
        let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut stdin);
        crate::hooks::parse_input(&stdin)
    };
    let print = |r: Result<Option<String>>| {
        if let Ok(Some(out)) = r {
            println!("{out}");
        }
    };
    match h {
        HookCmd::SessionStart => print(crate::hooks::session_start(&root, &input(), &cwd()).map(Some)),
        HookCmd::Prompt => print(crate::hooks::prompt(&root, &input(), &cwd())),
        HookCmd::FileTouch => print(crate::hooks::file_touch(&root, &input(), &cwd())),
        HookCmd::Stop => print(crate::hooks::stop(&root, &input(), &cwd())),
        HookCmd::SessionEnd { no_import } => {
            if let Ok(v) = Vault::open(&root)
                && (crate::hooks::needs_sync(&v) || (!no_import && crate::hooks::needs_import(&v.root)) || !crate::lifecycle::sweep_plan(&v).is_empty())
            {
                let _ = crate::hooks::spawn_background(&v.root, !no_import);
            }
        }
        HookCmd::Background { no_import } => {
            if let Err(e) = crate::hooks::background(&root, !no_import) {
                eprintln!("background: {e:#}");
            }
        }
    }
}

fn settings_path(p: Option<PathBuf>) -> Result<PathBuf> {
    match p {
        Some(p) => Ok(crate::util::absolutize(&p)),
        None => crate::hooks::default_settings_path(),
    }
}

fn hooks_text(r: &crate::hooks::HookChange) -> String {
    let line = |name: &str, c: &Option<String>| format!("  {name:<16} {}", c.as_deref().unwrap_or("(not installed)"));
    [
        r.settings.clone(),
        line("SessionStart", &r.session_start),
        line("SessionEnd", &r.session_end),
        line(crate::hooks::PROMPT_EVENT, &r.prompt),
        line(crate::hooks::FILE_EVENT, &r.file_touch),
        line(crate::hooks::STOP_EVENT, &r.stop),
    ]
    .join("\n")
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
                    fields: ops::with_field(ops::with_field(ops::parse_field_args(&a.fields)?, crate::digest::SUMMARY_KEY, a.summary), crate::note::EXPIRES_KEY, a.expires),
                    pinned: a.pin,
                    body: read_body_arg(a.body, a.body_file)?,
                    source: a.source,
                    links: vec![],
                    paths: a.paths,
                    require_existing_project: false,
                },
            )?;
            let mut created = if new_project { format!("\n(created new project {})", n.scope_label()) } else { String::new() };
            if ops::is_pref(&n) && !n.pinned {
                created.push_str(&format!("\n{}, `breadcrumb update {} --pin`", ops::PIN_HINT, n.id));
            }
            if let Some(h) = ops::similar_hint(&ops::similar(&v, &n)) {
                created.push_str(&format!("\n{h}"));
            }
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
                    fields: ops::with_field(ops::with_field(ops::parse_field_args(&u.fields)?, crate::digest::SUMMARY_KEY, u.summary), crate::note::EXPIRES_KEY, u.expires),
                    unset: u.unset,
                    body: read_body_arg(u.body, u.body_file)?,
                    pinned,
                    status: u.status,
                    links: None,
                    by: u.by,
                    paths: u.paths,
                    refs: u.refs,
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
        Cmd::Verify { ids } => {
            let ns = ops::verify(&open()?, &ids)?;
            let text: Vec<String> = ns.iter().map(|n| format::ack("verified", n)).collect();
            out.emit(text.join("\n"), Json::Array(ns.iter().map(|n| n.to_json()).collect()));
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
            let mut quiet = false;
            let mut tail = String::new();
            let rep = match ic {
                ImportCmd::Claude { claude_dir, dry_run, project_map, quiet: q, retire_index } => {
                    quiet = q;
                    let claude_dir = match claude_dir {
                        Some(d) => crate::util::absolutize(&d),
                        None => importer::default_claude_dir().context("cannot determine home directory, pass --claude-dir")?,
                    };
                    let rep = importer::import_claude(&v, &importer::ClaudeOpts { claude_dir: claude_dir.clone(), dry_run, project_map: importer::parse_project_map(&project_map)? })?;
                    let dirs = importer::memory_dirs(rep.items.iter().map(|i| i.source.as_str()));
                    if retire_index {
                        for p in importer::retire_indexes(&claude_dir, &dirs, dry_run)? {
                            tail.push_str(&format!("{} {}\n", if dry_run { "would retire" } else { "retired" }, path_str(&p)));
                        }
                    } else if !importer::live_indexes(&claude_dir, &dirs).is_empty() {
                        tail.push_str(importer::RETIRE_HINT);
                        tail.push('\n');
                    }
                    rep
                }
                ImportCmd::Files { path, project, kind, tags, dry_run } => importer::import_files(&v, &importer::FilesOpts { path, project, kind, tags, dry_run })?,
            };
            match (quiet, rep.quiet_line()) {
                (true, None) => {}
                (true, Some(line)) => out.emit(line, rep.to_json()),
                (false, _) => out.emit(format!("{}{tail}", rep.table()), rep.to_json()),
            }
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
        Cmd::Doctor { fix: true, yes } => {
            let v = open()?;
            let changes = crate::fix::plan(&v);
            let applied = if yes { Some(crate::fix::apply(&v, &changes)?) } else { None };
            let expired = if yes { crate::lifecycle::sweep(&v)? } else { crate::lifecycle::sweep_plan(&v) };
            let text = format!("{}{}", crate::fix::text(&changes, applied), crate::lifecycle::sweep_text(&expired, yes));
            out.emit(text, json!({"applied": applied, "changes": changes, "expired": expired}));
        }
        Cmd::Doctor { .. } => {
            let v = open()?;
            let problems = crate::doctor::check(&v);
            let n = v.note_files().len();
            let errors = problems.iter().filter(|p| p.severity == "error").count();
            out.emit(crate::doctor::text(&problems, n), json!({"notes": n, "errors": errors, "problems": problems}));
            return Ok(if errors > 0 { 1 } else { 0 });
        }
        Cmd::Stats { prompt, file } => {
            let st = crate::stats::collect(&open()?.root, &cwd, prompt.as_deref(), file.as_deref())?;
            out.emit(crate::stats::text(&st), crate::stats::json(&st));
        }
        Cmd::Guide => out.emit(crate::guide::AGENTS_MD, json!({"guide": crate::guide::AGENTS_MD})),
        Cmd::Idea { title, body, project, global, tags } => {
            let v = open()?;
            let project = ops::target_project(&v, project.as_deref(), global, &cwd)?;
            let body = read_body_arg(body, None)?;
            let n = backlog::capture_idea(&v, &title, body, project, tags, false)?;
            out.emit(format!("captured idea {}  {}", backlog::key(&n), n.title), n.to_json());
        }
        Cmd::Env { scope, all_machines } => {
            let v = open()?;
            let sc = ops::resolve_scope(&v, &scope.sel(), &cwd)?;
            let machine = (!all_machines).then(crate::util::machine_name);
            let notes = ops::env_facts(&v, &sc, machine.as_deref())?;
            out.emit(format::env_table(&notes, machine.as_deref()), notes_json(&notes));
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
            let me = backlog::session_name(by.as_deref());
            let claim_by = if claim { Some(me.clone().unwrap_or_else(|| backlog::DEFAULT_ACTOR.to_string())) } else { None };
            match backlog::next_as(&v, &sc, assignee.as_deref(), claim_by.as_deref(), me.as_deref())? {
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
        Cmd::Claim { id, by, force } => {
            let by = backlog::session_name(by.as_deref()).unwrap_or_else(|| backlog::DEFAULT_ACTOR.to_string());
            let n = backlog::claim(&open()?, &id, &by, force)?;
            out.emit(format!("claimed {}", backlog::item_summary(&n)), n.to_json());
        }
        Cmd::Release { id, by, force } => {
            let by = backlog::session_name(by.as_deref()).unwrap_or_else(|| backlog::DEFAULT_ACTOR.to_string());
            match backlog::release(&open()?, &id, &by, force)? {
                Some(n) => out.emit(format!("released {}", backlog::item_summary(&n)), n.to_json()),
                None => out.emit(format!("{id} has no claim to release"), Json::Null),
            }
        }
        Cmd::Move { id, state, resolution, shipped_in, by, check, refs } => {
            let check = check.as_deref().map(backlog::Check::parse).transpose()?;
            let r = backlog::move_item(&open()?, &id, MoveReq { state, resolution, shipped_in, by, check, refs })?;
            out.emit(backlog::move_text(&r), json!({"from": r.from, "to": r.to, "changed": r.changed, "unmet_acceptance": r.unmet, "note": r.note.to_json()}));
        }
        Cmd::Check { id, which } => {
            let (n, changed) = backlog::check(&open()?, &id, &backlog::Check::parse(which.as_deref().unwrap_or("all"))?)?;
            out.emit(backlog::check_text(&n, changed), json!({"ticked": changed, "unmet_acceptance": backlog::unmet_acceptance(&n), "note": n.to_json()}));
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
        Cmd::Map(mc) => map_cmd(&out, &open()?, mc, &cwd)?,
        Cmd::Brief { targets, project, repo, budget } => {
            let v = open()?;
            let slug = workflow::resolve_project(&v, project.as_deref(), &cwd)?;
            let repo = crate::map::repo_for(&v, &slug, repo.as_deref(), &cwd).ok();
            let text = crate::map::brief(&v, &slug, &targets, repo.as_deref(), budget)?;
            out.emit(&text, json!({"project": slug, "brief": text}));
        }
        Cmd::Serve { port, open: open_browser } => {
            open()?;
            crate::serve::run(&root, port, open_browser)?;
        }
        Cmd::Hook(_) => {}
        Cmd::Hooks(hc) => {
            let explicit_vault = cli.vault.as_deref().map(crate::util::absolutize);
            let (r, verb) = match hc {
                HooksCmd::Install { settings, no_sync, no_import, no_recall, no_file_notes, no_handoff } => {
                    let o = crate::hooks::InstallOpts { sync: !no_sync, import: !no_import, recall: !no_recall, file_notes: !no_file_notes, handoff: !no_handoff };
                    (crate::hooks::install(&settings_path(settings)?, explicit_vault.as_deref(), o)?, "installed")
                }
                HooksCmd::Uninstall { settings } => (crate::hooks::uninstall(&settings_path(settings)?)?, "removed"),
                HooksCmd::Status { settings } => (crate::hooks::status(&settings_path(settings)?)?, ""),
            };
            let head = match (verb, r.changed) {
                ("", _) => String::new(),
                (v, true) => format!("{v} breadcrumb hooks in "),
                (_, false) => "no change needed in ".into(),
            };
            out.emit(format!("{head}{}", hooks_text(&r)), serde_json::to_value(&r)?);
        }
        Cmd::Upgrade { from } => {
            let r = crate::upgrade::upgrade(from.as_deref())?;
            out.emit(
                format!("installed breadcrumb from {} to {}
running MCP and serve processes keep the old version until their client restarts them", r.source, r.exe),
                serde_json::to_value(&r)?,
            );
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

fn map_cmd(out: &Out, v: &Vault, mc: MapCmd, cwd: &Path) -> Result<()> {
    use crate::map;
    match mc {
        MapCmd::Put { area, mut lines, lines_file, paths, drop, project, repo } => {
            let slug = workflow::resolve_project(v, project.as_deref(), cwd)?;
            let repo = map::repo_for(v, &slug, repo.as_deref(), cwd)?;
            match lines_file {
                Some(f) if f.as_os_str() == "-" => lines.extend(read_body_arg(Some("-".into()), None)?),
                Some(f) => lines.extend(read_body_arg(None, Some(f))?),
                None => {}
            }
            let r = map::put(v, map::PutReq { area, project: slug, lines, paths, drop }, &repo)?;
            out.emit(r.text(), r.map.note.to_json());
        }
        MapCmd::Get { area, project, repo } => {
            let slug = workflow::resolve_project(v, project.as_deref(), cwd)?;
            let repo = map::repo_for(v, &slug, repo.as_deref(), cwd).ok();
            let text = map::get_text(v, &slug, &area, repo.as_deref())?;
            out.emit(&text, json!({"project": slug, "map": text}));
        }
        MapCmd::List { project, repo } => {
            let slug = workflow::resolve_project(v, project.as_deref(), cwd)?;
            let repo = map::repo_for(v, &slug, repo.as_deref(), cwd).ok();
            let text = map::list_text(v, &slug, repo.as_deref());
            out.emit(&text, json!({"project": slug, "maps": text}));
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
