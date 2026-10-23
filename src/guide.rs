pub const AGENTS_MD: &str = r#"# breadcrumb vault

Git-backed memory and backlog shared by agent sessions. Use the breadcrumb MCP tools or CLI, not hand edits.

- The project digest is injected at session start, `context` reloads it.
- `search` before `add`, then `update` or `append` instead of near duplicates.
- One fact, decision or gotcha per note with a short specific title. Pin only what every session needs, a pin shows as one line (its `summary` or title), `get` the body.
- A permission or rule for one night or one batch gets `expires`: a date, 3d, or until:<id>.
- Kinds: note, decision, gotcha, todo, handoff, reference, plan, feedback, user. `kinds` lists or defines them.
- Backlog items (idea, epic, feature, task, bug) have a state and priority p0 to p3. `next` picks work, `claim` marks what this session works on, `move` changes state.
- Machine facts (where files and tools live on this PC) go in kind `env`, `breadcrumb env` lists them.
- End of session: add a `handoff` note, it supersedes the previous one.
- Notes belong to the project detected from cwd, or global. Never store secrets.
"#;
