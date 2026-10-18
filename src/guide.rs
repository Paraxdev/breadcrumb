pub const AGENTS_MD: &str = r#"# breadcrumb vault

Git-backed memory and backlog shared by agent sessions. Use the breadcrumb MCP tools or CLI, not hand edits.

- The project digest is injected at session start, `context` reloads it.
- `search` before `add`, then `update` or `append` instead of near duplicates.
- One fact, decision or gotcha per note with a short specific title. Pin only what every session needs.
- Kinds: note, decision, gotcha, todo, handoff, reference, plan, feedback, user. `kinds` lists or defines them.
- Backlog items (idea, epic, feature, task, bug) have a state and priority p0 to p3. `next` picks work, `move` changes state.
- End of session: add a `handoff` note.
- Notes belong to the project detected from cwd, or global. Never store secrets.
"#;
