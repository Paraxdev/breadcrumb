pub const AGENTS_MD: &str = r#"# breadcrumb vault

Git-backed memory and backlog shared by agent sessions. Use the breadcrumb MCP tools or CLI, not hand edits.

- The digest comes at session start, `context` reloads it.
- `search` before `add`, then `update` or `append` instead of near duplicates.
- One fact, decision or gotcha per note with a short specific title. Pin only what every session needs, a pin shows one line, `get` the body.
- A permission or rule for one night or one batch gets `expires`: a date, 3d, or until:<id>.
- Kinds: note, decision, gotcha, todo, handoff, reference, plan, feedback, user, map (read with `brief`). `kinds` lists or defines them.
- Backlog items (idea, epic, feature, task, bug) have a state and priority p0 to p3. `next` picks work, `claim` marks what this session works on, `move` changes state.
- Need the user's call on an item? `ask` locks it until answered. Items with an unseen answer come first: built means adjust, not built means build with it. `claim` or `ack` clears it.
- Machine facts (where things live on this PC) go in kind `env`.
- Writes record session and date, do not type them.
- End of session: add a `handoff`, it replaces only this session's or its `track`'s, `supersedes` names another.
- Notes have a short `slug` beside the id, commands take either. Name notes to the user by slug, with plain words when it matters.
- Notes belong to the cwd's project, or global. Never store secrets.
"#;
