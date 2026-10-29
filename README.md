# breadcrumb

Git-backed memory and backlog for AI coding agents, a CLI and an MCP server. Personal tool, shared as is.

```
cargo install --path .
breadcrumb init
claude mcp add --scope user breadcrumb -- breadcrumb mcp
breadcrumb hooks install
```

Phone: install Tailscale on the PC and the phone, run `tailscale serve --bg 7878` on the PC, then `crumb serve --remote --allow-host <the PC's tailnet name>` and open the login link it prints.

Agents: read [AGENTS.md](AGENTS.md). MIT licensed.
