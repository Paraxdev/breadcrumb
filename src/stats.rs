//! Size of everything breadcrumb puts into an agent's context, in chars and approximate tokens.

use crate::mcp::Server;
use anyhow::Result;
use serde_json::{Map, Value, json};
use std::path::Path;

pub struct Stat {
    pub name: &'static str,
    pub chars: usize,
}

impl Stat {
    pub fn tokens(&self) -> usize {
        self.chars.div_ceil(4)
    }
}

pub fn collect(vault_root: &Path, cwd: &Path) -> Result<Vec<Stat>> {
    let tools = serde_json::to_string(&json!({"tools": crate::mcp::tool_defs()}))?;
    let hook: Value = serde_json::from_str(&crate::hooks::session_start(
        vault_root,
        &crate::hooks::HookInput { cwd: Some(cwd.to_path_buf()), source: Some("startup".into()) },
        cwd,
    )?)?;
    let hook = hook["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string();
    let srv = Server { vault_root: vault_root.to_path_buf(), cwd: cwd.to_path_buf() };
    let context = srv.call_tool("context", &Map::new())?;
    let count = |s: &str| s.chars().count();
    Ok(vec![
        Stat { name: "tools/list", chars: count(&tools) },
        Stat { name: "instructions", chars: count(crate::mcp::INSTRUCTIONS) },
        Stat { name: "hook", chars: count(&hook) },
        Stat { name: "context", chars: count(&context) },
    ])
}

pub fn text(stats: &[Stat]) -> String {
    let mut s = String::new();
    for x in stats {
        s.push_str(&format!("{:<13} {:>6} chars  ~{:>5} tokens\n", x.name, x.chars, x.tokens()));
    }
    let total: usize = stats.iter().map(|x| x.chars).sum();
    s.push_str(&format!("{:<13} {:>6} chars  ~{:>5} tokens\n", "total", total, total.div_ceil(4)));
    s
}

pub fn json(stats: &[Stat]) -> Value {
    Value::Object(stats.iter().map(|x| (x.name.to_string(), json!({"chars": x.chars, "tokens": x.tokens()}))).collect())
}
