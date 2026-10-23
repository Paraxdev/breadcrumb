# breadcrumb

A small personal tool: a git-backed notes, memory and backlog vault for AI coding agents, with a CLI, an MCP server and a local kanban page. Shared as is, no support, no roadmap.

```
cargo install --path .
breadcrumb init                      # vault at ~/breadcrumb (or BREADCRUMB_HOME)
breadcrumb import claude             # optional, pulls in Claude Code auto-memory
breadcrumb import claude --retire-index  # optional, swaps each MEMORY.md for a pointer to breadcrumb
claude mcp add --scope user breadcrumb -- /path/to/breadcrumb mcp
breadcrumb --help
```

Notes are plain markdown files with YAML frontmatter, every write is a git commit, and `breadcrumb sync` pushes to whatever remote you set. Run `breadcrumb guide` for the agent conventions.

`breadcrumb workflow save .claude/workflows/*.js` keeps Claude Code workflow scripts in the vault, `breadcrumb workflow install` puts them back on another PC.

`breadcrumb hooks install` adds Claude Code hooks to ~/.claude/settings.json: SessionStart injects a short vault digest for the current project, SessionEnd imports new Claude Code auto-memory and starts a background `breadcrumb sync` when the vault has unsynced changes. `breadcrumb hooks uninstall` removes them again. Two more hooks inject one line each, and only when something matches: UserPromptSubmit names up to 3 notes relevant to the first prompt of a session, PreToolUse on Read, Edit, Write, MultiEdit and NotebookEdit names notes whose `paths` globs match the file (`breadcrumb add --path-glob 'src/engine/**'`), never the same note twice per session. A Stop hook asks once per session for a handoff note when the session changed the project's notes and wrote none. Skip them with `--no-recall`, `--no-file-notes`, `--no-handoff` or `--no-import`.

The digest (`breadcrumb context`, the `context` tool and the SessionStart hook) fits in about 6000 chars, so nothing important is cut off at the end. Every pinned note gets one line, `id kind summary`, from its `summary` field (`breadcrumb add --summary ...`) or else its title's first sentence, decisions and preferences first and newest first, and agents `get` the body when they need it. Then the project's latest handoff in full, the unpinned preferences on one line, the backlog and recent notes. One handoff per project is live: a new one supersedes the current one (a `supersedes` link) and unpins older ones. Superseded notes, by a `supersedes` link, a `superseded` tag or `decision_status: superseded`, never show in the digest.

A permission or rule meant for one night or one batch gets `--expires` (`expires` in the MCP fields): a date, the last day it holds, an RFC3339 time, `3d` or `12h` from now, or `until:<id>`, until that item is done, shipped, rejected or archived. An expired note leaves the digest and the hooks and ranks last in search, marked `[expired]`. Closing the item it waits for, the SessionEnd background job and `breadcrumb doctor --fix --yes` unpin it and set a decision's `decision_status` to superseded, and `breadcrumb doctor` names what expired in the last two weeks.

`add` names up to two existing notes that look like the same fact, by shared title words with shared tags helping, so an agent can update one of them instead. `search` lists notes matching more of the query first, then by score weighted by kind and recency, drops notes that only share a word or two with a long query, shows 5 by default and marks partial matches like `(2/3 terms)`.

Sessions that share a project claim what they work on: `breadcrumb claim <id>` (the `claim` tool) sets the item's `assignee` to the session name and records when, `breadcrumb release <id>` clears it. The name is `--by`, else `BREADCRUMB_SESSION`, else the start of the Claude Code session id. `next` skips items another session holds, the board and the digest show the holder, and a claim with no write to its item for a day is stale: it shows as such and no longer keeps anyone away. Taking over or releasing someone else's fresh claim needs `--force`.

Machine facts, like where screen recordings, logs or build caches live on this PC or which tools are missing, go in notes of kind `env`: `breadcrumb add -k env -t "Screen recordings" --field location=C:/Users/me/Videos --field access=read-only`. Their `machine` field defaults to this computer (`BREADCRUMB_MACHINE` overrides the name), `breadcrumb env` lists this machine's facts as a table (`--all-machines` for every one), and the digest only says how many there are. A built-in kind that is newer than the vault is written into it the first time a note uses it.

Notes carry a `verified` date, set on every write. `breadcrumb verify <id>` confirms a note is still accurate without changing it, notes older than `stale_days` (breadcrumb.toml, default 60) get a `(stale?)` marker, and `breadcrumb doctor` lists them along with backticked repo paths that no longer exist in the project checkout. `breadcrumb stats --prompt "..." --file src/x.rs` shows what the hooks would inject.

`breadcrumb upgrade` rebuilds from the source checkout and reinstalls even while an MCP server or `breadcrumb serve` holds the exe open on Windows, running processes keep the old version until their client restarts them. Killing `breadcrumb serve` between requests is safe, writes happen under the vault lock and Ctrl+C lets in-flight requests finish.

MIT licensed.
