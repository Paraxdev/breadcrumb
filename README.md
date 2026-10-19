# breadcrumb

A small personal tool: a git-backed notes, memory and backlog vault for AI coding agents, with a CLI, an MCP server and a local kanban page. Shared as is, no support, no roadmap.

```
cargo install --path .
breadcrumb init                      # vault at ~/breadcrumb (or BREADCRUMB_HOME)
breadcrumb import claude             # optional, pulls in Claude Code auto-memory
claude mcp add --scope user breadcrumb -- /path/to/breadcrumb mcp
breadcrumb --help
```

Notes are plain markdown files with YAML frontmatter, every write is a git commit, and `breadcrumb sync` pushes to whatever remote you set. Run `breadcrumb guide` for the agent conventions.

`breadcrumb workflow save .claude/workflows/*.js` keeps Claude Code workflow scripts in the vault, `breadcrumb workflow install` puts them back on another PC.

`breadcrumb hooks install` adds Claude Code hooks to ~/.claude/settings.json: SessionStart injects a short vault digest for the current project, SessionEnd imports new Claude Code auto-memory and starts a background `breadcrumb sync` when the vault has unsynced changes. `breadcrumb hooks uninstall` removes them again. Two more hooks inject one line each, and only when something matches: UserPromptSubmit names up to 3 notes relevant to the first prompt of a session, PreToolUse on Read, Edit, Write, MultiEdit and NotebookEdit names notes whose `paths` globs match the file (`breadcrumb add --path-glob 'src/engine/**'`), never the same note twice per session. Skip them with `--no-recall`, `--no-file-notes` or `--no-import`.

Notes carry a `verified` date, set on every write. `breadcrumb verify <id>` confirms a note is still accurate without changing it, notes older than `stale_days` (breadcrumb.toml, default 60) get a `(stale?)` marker, and `breadcrumb doctor` lists them along with backticked repo paths that no longer exist in the project checkout. `breadcrumb stats --prompt "..." --file src/x.rs` shows what the hooks would inject.

`breadcrumb upgrade` rebuilds from the source checkout and reinstalls even while an MCP server or `breadcrumb serve` holds the exe open on Windows, running processes keep the old version until their client restarts them. Killing `breadcrumb serve` between requests is safe, writes happen under the vault lock and Ctrl+C lets in-flight requests finish.

MIT licensed.
