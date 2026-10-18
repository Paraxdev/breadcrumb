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

MIT licensed.
