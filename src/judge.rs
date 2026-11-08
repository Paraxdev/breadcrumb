//! Agents judging breadcrumb itself: one line on what helped, got in the way, confused them or was missing, filed as
//! kind judgment in `judge_project` wherever the agent worked, so the tool's own sessions can turn them into items.

use crate::note::{Note, yaml_scalar_string};
use crate::ops::{self, AddReq, FieldInput};
use crate::vault::Vault;
use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write;

pub const JUDGMENT: &str = "judgment";
pub const VERDICTS: [&str; 4] = ["hindered", "confusing", "missing", "helped"];
/// The link a judgment gets once an item covers it.
pub const FILED_AS: &str = "filed-as";
const TITLE_MAX: usize = 100;

fn field(n: &Note, key: &str) -> Option<String> {
    n.field(key).and_then(yaml_scalar_string).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn verdict(n: &Note) -> String {
    field(n, "verdict").unwrap_or_else(|| "hindered".into())
}

pub fn about(n: &Note) -> String {
    field(n, "about").unwrap_or_else(|| "general".into())
}

/// Covered by an item (any outgoing link) or archived, so nothing is left to triage.
pub fn triaged(n: &Note) -> bool {
    n.is_archived() || !n.links.is_empty()
}

fn check_verdict(v: &str) -> Result<String> {
    let v = v.trim().to_ascii_lowercase();
    if !VERDICTS.contains(&v.as_str()) {
        bail!("verdict is one of {}, got '{v}'", VERDICTS.join(", "));
    }
    Ok(v)
}

/// The project judgments go to, whatever the caller asked for.
pub fn home(v: &Vault) -> Option<String> {
    Some(v.config.judge_project.trim().to_string()).filter(|p| !p.is_empty())
}

pub fn judge(v: &Vault, text: &str, verdict: &str, about: Option<&str>, seen_in: Option<&str>) -> Result<Note> {
    let text = crate::util::normalize_newlines(text.trim());
    if text.is_empty() {
        bail!("say what helped, got in your way, confused you or was missing");
    }
    let mut fields = vec![("verdict".to_string(), FieldInput::Raw(check_verdict(verdict)?))];
    if let Some(a) = about.map(str::trim).filter(|a| !a.is_empty()) {
        fields.push(("about".into(), FieldInput::Raw(a.to_string())));
    }
    let home = home(v);
    if let Some(p) = seen_in.map(str::trim).filter(|p| !p.is_empty() && home.as_deref() != Some(*p)) {
        fields.push(("seen_in".into(), FieldInput::Raw(p.to_string())));
    }
    let title = crate::util::truncate_chars(&crate::util::one_line(&text), TITLE_MAX);
    ops::add(v, AddReq { title, kind: Some(JUDGMENT.into()), project: home, fields, body: Some(text), ..Default::default() })
}

#[derive(Debug, Serialize)]
pub struct Row {
    pub id: String,
    pub verdict: String,
    pub about: String,
    pub title: String,
    pub seen_in: Option<String>,
    pub created: String,
    pub triaged: bool,
}

pub fn list(v: &Vault, since: Option<DateTime<Utc>>, all: bool) -> Vec<Row> {
    let mut notes: Vec<Note> = v.load_notes().into_iter().filter(|n| n.kind == JUDGMENT && since.is_none_or(|s| n.created >= s) && (all || !triaged(n))).collect();
    notes.sort_by_key(|n| std::cmp::Reverse(n.created));
    notes
        .iter()
        .map(|n| Row {
            id: n.id.clone(),
            verdict: verdict(n),
            about: about(n),
            title: n.title.clone(),
            seen_in: field(n, "seen_in"),
            created: crate::util::fmt_time(&n.created),
            triaged: triaged(n),
        })
        .collect()
}

pub fn list_text(rows: &[Row]) -> String {
    if rows.is_empty() {
        return "no judgments to triage, --all shows the triaged ones too\n".into();
    }
    let mut by_about: BTreeMap<&str, BTreeMap<&str, usize>> = BTreeMap::new();
    for r in rows {
        *by_about.entry(&r.about).or_default().entry(&r.verdict).or_default() += 1;
    }
    let mut groups: Vec<(&str, usize, String)> = by_about
        .iter()
        .map(|(a, vs)| {
            let total = vs.values().sum();
            let parts: Vec<String> = VERDICTS.iter().filter_map(|v| vs.get(v).map(|c| format!("{c} {v}"))).collect();
            (*a, total, parts.join(", "))
        })
        .collect();
    groups.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let mut s = format!("{} judgments of breadcrumb, by what they are about:\n", rows.len());
    for (a, _, parts) in &groups {
        let _ = writeln!(s, "  {a}: {parts}");
    }
    s.push('\n');
    for r in rows {
        let seen = r.seen_in.as_ref().map(|p| format!(" (in {p})")).unwrap_or_default();
        let done = if r.triaged { " [triaged]" } else { "" };
        let _ = writeln!(s, "{} {} {}: {}{seen}{done}", r.id, r.verdict, r.about, r.title);
    }
    s.push_str("\nTurn one into an item (or update one), then `breadcrumb link <judgment> <item> --rel filed-as` so it leaves this list.\n");
    s
}

fn nudge_file(v: &Vault) -> std::path::PathBuf {
    v.root.join(".local").join("judge-nudge")
}

/// `daily` shows the ask in one digest a day on this machine, the date it last showed is kept in .local.
fn nudge_due(v: &Vault) -> bool {
    match v.config.judge_nudge.as_str() {
        "session" => true,
        "daily" => {
            let today = crate::util::fmt_date(&crate::util::now());
            let file = nudge_file(v);
            if std::fs::read_to_string(&file).is_ok_and(|s| s.trim() == today) {
                return false;
            }
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&file, &today);
            true
        }
        _ => false,
    }
}

/// The ask to judge breadcrumb, and in its own project the count left to triage.
pub fn digest_lines(v: &Vault, project: Option<&str>, all: &[Note]) -> Vec<String> {
    let mut out = vec![];
    if nudge_due(v) {
        out.push("Judge breadcrumb: before you finish, file one line on what in breadcrumb got in your way, confused you, was missing or helped, `breadcrumb judge \"...\" --verdict hindered --about <command>` (MCP add kind judgment)".to_string());
    }
    if project.is_some() && project == home(v).as_deref() {
        let open = all.iter().filter(|n| n.kind == JUDGMENT && !triaged(n)).count();
        if open > 0 {
            out.push(format!("Judgments: {open} from agents to triage, `breadcrumb judge --list`"));
        }
    }
    out
}
