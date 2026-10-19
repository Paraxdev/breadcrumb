//! Size of everything breadcrumb puts into an agent's context, in chars and approximate tokens.

use crate::mcp::Server;
use anyhow::Result;
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::path::Path;

pub struct Stat {
    pub name: &'static str,
    pub chars: usize,
    /// Paid on each trigger (a first prompt, a touched file) instead of once per session, so not in the total.
    pub per_trigger: bool,
}

impl Stat {
    pub fn tokens(&self) -> usize {
        self.chars.div_ceil(4)
    }
}

pub fn collect(vault_root: &Path, cwd: &Path, prompt: Option<&str>, file: Option<&Path>) -> Result<Vec<Stat>> {
    let tools = serde_json::to_string(&json!({"tools": crate::mcp::tool_defs()}))?;
    let hook: Value = serde_json::from_str(&crate::hooks::session_start(
        vault_root,
        &crate::hooks::HookInput { cwd: Some(cwd.to_path_buf()), source: Some("startup".into()), ..Default::default() },
        cwd,
    )?)?;
    let hook = hook["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string();
    let srv = Server { vault_root: vault_root.to_path_buf(), cwd: cwd.to_path_buf() };
    let context = srv.call_tool("context", &Map::new())?;
    let count = |s: &str| s.chars().count();
    let mut out = vec![
        Stat { name: "tools/list", chars: count(&tools), per_trigger: false },
        Stat { name: "instructions", chars: count(crate::mcp::INSTRUCTIONS), per_trigger: false },
        Stat { name: "hook", chars: count(&hook), per_trigger: false },
        Stat { name: "context", chars: count(&context), per_trigger: false },
    ];
    let v = crate::vault::Vault::open(vault_root)?;
    let none = HashSet::new();
    if let Some(p) = prompt {
        let hits = crate::recall::recall(&v, v.detect_project_fast(cwd).as_deref(), p, &none);
        let line = if hits.is_empty() { String::new() } else { crate::recall::recall_line(&v, &hits) };
        out.push(Stat { name: "hook prompt", chars: count(&line), per_trigger: true });
    }
    if let Some(f) = file {
        let line = crate::recall::file_notes(&v, cwd, f, &none).map(|(l, _)| l).unwrap_or_default();
        out.push(Stat { name: "hook file", chars: count(&line), per_trigger: true });
    }
    Ok(out)
}

pub fn text(stats: &[Stat]) -> String {
    let mut s = String::new();
    let line = |name: &str, chars: usize, suffix: &str| format!("{name:<13} {chars:>6} chars  ~{:>5} tokens{suffix}\n", chars.div_ceil(4));
    for x in stats.iter().filter(|x| !x.per_trigger) {
        s.push_str(&line(x.name, x.chars, ""));
    }
    let total: usize = stats.iter().filter(|x| !x.per_trigger).map(|x| x.chars).sum();
    s.push_str(&line("total", total, ""));
    for x in stats.iter().filter(|x| x.per_trigger) {
        s.push_str(&line(x.name, x.chars, "  per trigger"));
    }
    s
}

pub fn json(stats: &[Stat]) -> Value {
    Value::Object(stats.iter().map(|x| (x.name.to_string(), json!({"chars": x.chars, "tokens": x.tokens(), "per_trigger": x.per_trigger}))).collect())
}
