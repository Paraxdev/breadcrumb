# breadcrumb

Git-backed memory and backlog for AI coding agents, a CLI and an MCP server. Personal tool, shared as is.

Install or update, the same line each time ([UPGRADE.md](UPGRADE.md) has a prompt to paste into Claude Code):

```
irm https://raw.githubusercontent.com/Paraxdev/breadcrumb/main/install.ps1 | iex         # Windows
curl -fsSL https://raw.githubusercontent.com/Paraxdev/breadcrumb/main/install.sh | sh    # macOS, Linux
```

From source: `cargo install --path .`, `breadcrumb init`, `claude mcp add --scope user breadcrumb -- breadcrumb mcp`, `breadcrumb hooks install`. Later, `breadcrumb upgrade` pulls and rebuilds the checkout, or fetches the latest release.

Shared repos: `breadcrumb share init` in a repo writes the project's backlog, decisions and gotchas to a committed `.breadcrumb/` folder. Everyone with breadcrumb gets them loaded into their own vault when a session starts there, and what changed is written back when it ends.

Phone: install Tailscale on the PC and the phone, run `tailscale serve --bg 7878` on the PC, then `crumb serve --remote --allow-host <the PC's tailnet name>` and open the login link it prints.

Every CLI command and MCP call is counted in the vault's ignored `.local/usage.log`, without note text, `crumb usage` shows what is used, what fails and what is never used, and a miss several sessions repeat is filed as a judgment.

Agents: read [AGENTS.md](AGENTS.md). MIT licensed.
