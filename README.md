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

`breadcrumb hooks install` adds Claude Code hooks to ~/.claude/settings.json: SessionStart injects a short vault digest for the current project, SessionEnd starts a background `breadcrumb sync` when the vault has unsynced changes. `breadcrumb hooks uninstall` removes them again.

`breadcrumb upgrade` rebuilds from the source checkout and reinstalls even while an MCP server or `breadcrumb serve` holds the exe open on Windows, running processes keep the old version until their client restarts them. Killing `breadcrumb serve` between requests is safe, writes happen under the vault lock and Ctrl+C lets in-flight requests finish.

MIT licensed.
