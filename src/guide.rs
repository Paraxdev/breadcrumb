pub const AGENTS_MD: &str = r#"# breadcrumb vault

Memory and backlog for agent sessions. Use MCP or CLI, not hand edits.

- The digest comes at session start, `context` reloads it.
- `search` before `add`, `update` or `append` over near duplicates.
- One fact per note, short title. Pin only what every session needs, `pins` reviews them.
- A rule for one batch gets `expires`: a date, 3d, or until:<id>.
- Kinds: note, decision, gotcha, todo, handoff, reference, plan, feedback, user, map (read with `brief`), env (machine facts).
- Backlog items (idea, epic, feature, task, bug) have a state and priority p0 to p3. `next` picks work, `move` changes state, `claim` when work starts.
- `move` to review with `--step "text"` (MCP `steps`), `--ref <merge sha>`, `--release-note`. Review others' work with `review pass --evidence`.
- Need the user's call on an item? `ask` locks it until answered. Unseen answers come first: built means adjust, else build with it. `claim` or `ack` clears it.
- End of session: add a `handoff`, it replaces only this session's or its `track`'s, `supersedes` names another.
- Handoffs and feedback keep the user's words in `said`, apart from conclusions.
- Notes have a `slug` beside the id, either works, name notes to the user by it.
- `-` reads stdin on `add`, `append`, `update --body`.
- `crumb lease <name>` holds a build dir or runner, `leases` lists.
- `crumb judge "..." -v <verdict>` rates crumb, `crumb config` tunes it.
- Notes belong to the cwd's project, or global. Never store secrets.
"#;
