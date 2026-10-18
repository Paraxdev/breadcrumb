pub const AGENTS_MD: &str = r#"# breadcrumb vault: notes for agents

This folder is a breadcrumb vault, a git-backed memory shared by every coding agent session.
Use the `breadcrumb` CLI or the breadcrumb MCP tools, do not hand-edit files unless you must.

## Workflow

1. Session start: call `context` (MCP) or `breadcrumb context`. It shows the current project,
   pinned notes, open todos, recent notes and the available kinds.
2. Before adding, `search` for the topic. If a note already covers it, `update` or `append`
   to it instead of creating a near-duplicate.
3. One fact, decision or gotcha per note, with a short specific title.
4. Connect related notes with `link` (rel is free-form: relates, supersedes, depends-on, ...).
5. `pin` only what every session in the project needs (build commands, hard constraints).
6. End of session: add a `handoff` note (what was done, what is next, open questions).

## Kinds and fields

- Kinds live in `kinds/<name>.toml`. Prefer existing kinds: note, decision, gotcha, todo,
  handoff, reference, plan, chat-summary, feedback (user corrections about how to work), user.
- Need a new kind or typed field? Define it first with `define_kind` or
  `breadcrumb kind define --name x --description "..." --field name:type[:required][:a|b|c]`.
  Field types: string, int, float, bool, date, enum, list, ref, url.
- Undeclared fields are allowed (`--field key=value`) and kept as-is.

## Backlog

- Ideas, epics, features, tasks and bugs are ordinary notes with a `state` field
  (idea, backlog, queued, in-progress, review, done, shipped, rejected) and a `priority` (p0 to p3).
- Capture with `capture_idea` / `breadcrumb idea "..."`, see work with `board`, take work with
  `next` (claim: true, by: your name), finish with `move <id> done --by <name>`.
- Express order with `depends-on` / `blocks` links and hierarchy with the `parent` field.

## Scope

- Notes belong to the project detected from the working directory (git remote or path),
  or to `global` when none matches. Bind a directory with `breadcrumb project link <slug>`.
- Use `--global` for knowledge that applies everywhere (user preferences, tools).

## Rules

- Never store secrets: no tokens, passwords, private keys or connection strings with credentials.
- Note ids are 8 characters, any unique prefix of 4+ characters works.
- Every change is committed to git automatically, `breadcrumb sync` pushes to the remote.
"#;
