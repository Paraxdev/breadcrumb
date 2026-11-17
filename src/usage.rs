//! The usage log: one line per CLI command and MCP tool call in the vault's ignored `.local` dir, never committed or
//! synced. A line holds the command path, the outcome, the project, the session and the chars printed, never note
//! text: a refused word is kept cut to 40 chars with the suggestion made for it. `usage` and `doctor` read it back, and
//! a miss that several sessions repeat is filed once as a judgment.

use crate::doctor::Problem;
use crate::vault::Vault;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

pub const FILE: &str = "usage.log";
const OLD: &str = "usage.log.1";
const FILED: &str = "usage-filed";
/// Past this size the log moves to usage.log.1, which replaces the one before.
const ROTATE_BYTES: u64 = 2 * 1024 * 1024;
const WORD_MAX: usize = 40;
const HINT_MAX: usize = 60;
pub const TAG: &str = "usage-miss";
pub const DEFAULT_DAYS: u64 = 7;
pub const DEFAULT_THRESHOLD: u64 = 3;
const DOCTOR_DAYS: u64 = 7;
const DOCTOR_CALLS: usize = 5;
const MISSES_SHOWN: usize = 10;

/// A value a command refused after clap took it, logged as a bad_value miss with the nearest value it takes.
#[derive(Debug)]
pub struct BadValue {
    pub word: String,
    pub hint: Option<String>,
    msg: String,
}

impl BadValue {
    pub fn error(word: &str, hint: Option<&str>, msg: String) -> anyhow::Error {
        anyhow::Error::new(BadValue { word: word.to_string(), hint: hint.map(str::to_string), msg })
    }
}

impl std::fmt::Display for BadValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for BadValue {}

/// The refused word and its nearest value, when the error is a refused value.
pub fn value_miss(e: &anyhow::Error) -> Option<(String, Option<String>)> {
    e.downcast_ref::<BadValue>().map(|b| (b.word.clone(), b.hint.clone()))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Line {
    pub time: String,
    pub cmd: String,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub project: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session: String,
    #[serde(default)]
    pub chars: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub miss: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<usize>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cut: bool,
}

thread_local! {
    static PRINTED: Cell<usize> = const { Cell::new(0) };
    static DIGEST: Cell<Option<(usize, usize, bool)>> = const { Cell::new(None) };
}

/// Counts what a command prints toward its line.
pub fn printed(text: &str) {
    let n = text.chars().count();
    PRINTED.with(|p| p.set(p.get() + n));
}

/// The digest a call built: its chars, its budget and whether lines were cut to fit, a later cut of the same digest
/// keeps it marked cut.
pub fn digest_built(size: usize, budget: usize, cut: bool) {
    DIGEST.with(|d| {
        let was = d.get().is_some_and(|x| x.2);
        d.set(Some((size, budget, cut || was)));
    });
}

/// Starts a call's count of chars printed and digest built afresh, for a server that handles many.
pub fn begin() {
    PRINTED.with(|p| p.set(0));
    DIGEST.with(|d| d.set(None));
}

fn stamp(t: chrono::DateTime<chrono::Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

impl Line {
    /// A line for this call with the chars printed and the digest built so far.
    pub fn new(cmd: impl Into<String>, outcome: impl Into<String>) -> Line {
        let (digest, budget, cut) = DIGEST.with(Cell::get).map_or((None, None, false), |(s, b, c)| (Some(s), Some(b), c));
        Line {
            time: stamp(crate::util::now()),
            cmd: cmd.into(),
            outcome: outcome.into(),
            session: crate::backlog::session_name(None).unwrap_or_default(),
            chars: PRINTED.with(Cell::get),
            digest,
            budget,
            cut,
            ..Default::default()
        }
    }

    pub fn with_project(mut self, p: Option<&str>) -> Line {
        self.project = p.map(str::trim).unwrap_or_default().to_string();
        self
    }

    /// The word that was not understood, and what was suggested for it.
    pub fn missed(mut self, word: &str, hint: Option<&str>) -> Line {
        self.miss = Some(clean(word, WORD_MAX, false));
        self.hint = hint.map(|h| clean(h, HINT_MAX, true));
        self
    }

    pub fn refused(&self) -> bool {
        !ok(&self.outcome)
    }
}

fn ok(outcome: &str) -> bool {
    outcome == "ok" || outcome.starts_with("exit")
}

/// A word with whitespace in it is text someone typed, not a mistyped command, so it is kept as `<text>`.
fn clean(w: &str, max: usize, spaces: bool) -> String {
    let w = w.trim();
    if w.is_empty() || w.chars().any(|c| c.is_control() || (c.is_whitespace() && !spaces)) {
        return "<text>".into();
    }
    w.chars().take(max).collect()
}

/// The outcome word for an error message.
pub fn kind_of(msg: &str) -> &'static str {
    let m = msg.to_ascii_lowercase();
    let has = |xs: &[&str]| xs.iter().any(|x| m.contains(x));
    if has(&["no breadcrumb vault"]) {
        "no_vault"
    } else if has(&["no note with", "not found", "no such", "does not exist"]) {
        "not_found"
    } else if has(&["ambiguous"]) {
        "ambiguous"
    } else if has(&["lock"]) {
        "locked"
    } else if has(&["missing required", "must be", "is required", "expects", "got '", "unknown ", "takes ", "needs ", "invalid"]) {
        "bad_input"
    } else if has(&["git "]) {
        "git"
    } else {
        "error"
    }
}

/// The outcome word for a command line clap refused.
pub fn clap_kind(k: clap::error::ErrorKind) -> &'static str {
    use clap::error::ErrorKind as K;
    match k {
        K::InvalidSubcommand => "unknown_command",
        K::UnknownArgument => "unknown_flag",
        K::InvalidValue | K::ValueValidation | K::InvalidUtf8 => "bad_value",
        K::MissingRequiredArgument => "missing_arg",
        K::MissingSubcommand => "missing_command",
        K::ArgumentConflict => "conflict",
        K::NoEquals | K::TooManyValues | K::TooFewValues | K::WrongNumberOfValues => "bad_count",
        _ => "refused",
    }
}

/// Note: writes only when the vault's .gitignore names .local, so the log can never be committed or synced.
fn ignored(root: &Path) -> bool {
    std::fs::read_to_string(root.join(".gitignore")).is_ok_and(|t| t.lines().any(|l| l.trim().trim_start_matches('/').trim_end_matches('/') == ".local"))
}

/// Appends the line in one write, the project detected from `cwd` when the call named none. Never fails the call.
pub fn record(root: &Path, mut line: Line, cwd: Option<&Path>) {
    if !ignored(root) {
        return;
    }
    if line.project.is_empty()
        && let Some(c) = cwd
        && let Ok(v) = Vault::open(root)
    {
        line.project = v.detect_project_fast(c).unwrap_or_default();
    }
    let _ = append(root, &line);
}

fn append(root: &Path, line: &Line) -> Result<()> {
    let dir = root.join(".local");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(FILE);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > ROTATE_BYTES) {
        let _ = std::fs::rename(&path, dir.join(OLD));
    }
    let mut text = serde_json::to_string(line)?;
    text.push('\n');
    std::fs::OpenOptions::new().create(true).append(true).open(&path)?.write_all(text.as_bytes())?;
    Ok(())
}

/// Every line kept, oldest first. A line that does not parse, like one cut short, is skipped.
pub fn read(root: &Path) -> Vec<Line> {
    [OLD, FILE]
        .iter()
        .flat_map(|f| {
            let text = std::fs::read_to_string(root.join(".local").join(f)).unwrap_or_default();
            text.lines().filter_map(|l| serde_json::from_str::<Line>(l).ok()).collect::<Vec<_>>()
        })
        .collect()
}

fn since(lines: Vec<Line>, days: u64) -> Vec<Line> {
    let from = stamp(crate::util::now() - chrono::Duration::days(days.min(36_500) as i64));
    lines.into_iter().filter(|l| l.time >= from).collect()
}

/// How a refused word reads to a person: `breadcrumb add --txt` or `MCP land gren`.
fn shown(cmd: &str, word: &str) -> String {
    match cmd.strip_prefix("mcp") {
        Some("") => format!("MCP tool {word}"),
        Some(t) => format!("MCP {} {word}", t.trim_start_matches(':').replace('.', " ")),
        None if cmd == "cli" => format!("breadcrumb {word}"),
        None => format!("breadcrumb {} {word}", cmd.replace('.', " ")),
    }
}

/// Files a refused miss once as a judgment when adapt_threshold hits or more from 2 or more sessions repeat it.
pub fn adapt(root: &Path, line: &Line) {
    let Some(word) = line.miss.as_deref() else { return };
    if !line.refused() || !ignored(root) {
        return;
    }
    let Ok(v) = Vault::open(root) else { return };
    let need = v.config.adapt_threshold;
    let key = format!("{} {word}", line.cmd);
    if need == 0 || filed_here(root, &key) {
        return;
    }
    let hits: Vec<Line> = read(root).into_iter().filter(|l| l.refused() && l.cmd == line.cmd && l.miss.as_deref() == Some(word)).collect();
    let sessions: BTreeSet<&str> = hits.iter().map(|l| l.session.as_str()).filter(|s| !s.is_empty()).collect();
    if (hits.len() as u64) < need || sessions.len() < 2 {
        return;
    }
    let _ = file(&v, &key, &line.cmd, word, &hits, sessions.len());
}

fn filed_here(root: &Path, key: &str) -> bool {
    std::fs::read_to_string(root.join(".local").join(FILED)).is_ok_and(|t| t.lines().any(|l| l == key))
}

fn mark_filed(root: &Path, key: &str) {
    let _ = std::fs::OpenOptions::new().create(true).append(true).open(root.join(".local").join(FILED)).and_then(|mut f| f.write_all(format!("{key}\n").as_bytes()));
}

fn file(v: &Vault, key: &str, cmd: &str, word: &str, hits: &[Line], sessions: usize) -> Result<()> {
    use crate::ops::{AddReq, FieldInput};
    let hints: BTreeSet<&str> = hits.iter().filter_map(|l| l.hint.as_deref()).collect();
    let typed = shown(cmd, word);
    let maps = match hints.iter().next() {
        Some(h) if hints.len() == 1 && hits.iter().all(|l| l.hint.is_some()) => {
            format!("Every hit got the same suggestion, `{h}`, so it maps cleanly to `{h}`, and an alias could take it.")
        }
        Some(_) => format!("The hits got different suggestions ({}) or none, so it does not map cleanly to one command, flag or value.", hints.iter().map(|h| format!("`{h}`")).collect::<Vec<_>>().join(", ")),
        None => "breadcrumb had no suggestion for it, so it maps to no command, flag or value.".into(),
    };
    let body = format!(
        "`{typed}` was refused {} times in {sessions} sessions. {maps} A person decides whether it becomes an alias or a clearer message, breadcrumb adds none by itself.\n\nFiled by the usage log, key `{key}`, `breadcrumb usage` shows the other misses.",
        hits.len()
    );
    let _g = v.lock()?;
    let marker = format!("key `{key}`");
    if v.load_notes().iter().any(|n| n.kind == crate::judge::JUDGMENT && n.tags.iter().any(|t| t == TAG) && n.body.contains(&marker)) {
        mark_filed(&v.root, key);
        return Ok(());
    }
    let req = AddReq {
        title: format!("`{typed}` was refused {} times in {sessions} sessions", hits.len()),
        kind: Some(crate::judge::JUDGMENT.into()),
        project: crate::judge::home(v),
        tags: vec![TAG.into()],
        fields: vec![("verdict".into(), FieldInput::Raw("confusing".into())), ("about".into(), FieldInput::Raw(cmd.into()))],
        body: Some(body),
        ..Default::default()
    };
    crate::ops::add_held(v, req)?;
    mark_filed(&v.root, key);
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct CmdRow {
    pub cmd: String,
    pub calls: usize,
    pub errors: usize,
    pub chars: usize,
}

#[derive(Debug, Serialize)]
pub struct MissRow {
    pub cmd: String,
    pub miss: String,
    pub hint: Option<String>,
    pub hits: usize,
    pub sessions: usize,
    pub refused: usize,
}

#[derive(Debug, Serialize)]
pub struct DigestRow {
    pub built: usize,
    pub mean: usize,
    pub largest: usize,
    pub budget: usize,
    pub cut: usize,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub days: u64,
    pub project: Option<String>,
    pub calls: usize,
    pub sessions: usize,
    pub commands: Vec<CmdRow>,
    pub misses: Vec<MissRow>,
    pub unused: Vec<String>,
    pub digest: Option<DigestRow>,
}

/// The command words a call can start with: the CLI's top level commands and `mcp:` and each MCP tool.
pub fn known() -> Vec<String> {
    use clap::CommandFactory;
    let cli = crate::cli::Cli::command();
    let mut out: Vec<String> = cli.get_subcommands().filter(|c| !c.is_hide_set() && !matches!(c.get_name(), "hook" | "help")).map(|c| c.get_name().to_string()).collect();
    out.extend(crate::mcp::tool_defs().iter().filter_map(|t| t["name"].as_str()).map(|t| format!("mcp:{t}")));
    out
}

pub fn report(root: &Path, days: u64, project: Option<&str>) -> Report {
    let lines: Vec<Line> = since(read(root), days).into_iter().filter(|l| project.is_none_or(|p| l.project == p)).collect();
    let mut cmds: BTreeMap<&str, CmdRow> = BTreeMap::new();
    for l in &lines {
        let r = cmds.entry(l.cmd.as_str()).or_insert_with(|| CmdRow { cmd: l.cmd.clone(), calls: 0, errors: 0, chars: 0 });
        r.calls += 1;
        r.errors += usize::from(l.refused());
        r.chars += l.chars;
    }
    let mut commands: Vec<CmdRow> = cmds.into_values().collect();
    commands.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.cmd.cmp(&b.cmd)));
    let mut misses: BTreeMap<(&str, &str), Vec<&Line>> = BTreeMap::new();
    for l in &lines {
        if let Some(w) = l.miss.as_deref() {
            misses.entry((l.cmd.as_str(), w)).or_default().push(l);
        }
    }
    let mut misses: Vec<MissRow> = misses
        .into_iter()
        .map(|((cmd, w), ls)| {
            let mut hints: BTreeMap<&str, usize> = BTreeMap::new();
            for h in ls.iter().filter_map(|l| l.hint.as_deref()) {
                *hints.entry(h).or_default() += 1;
            }
            let hint = hints.iter().max_by_key(|(_, n)| **n).map(|(h, _)| h.to_string());
            let sessions = ls.iter().map(|l| l.session.as_str()).filter(|s| !s.is_empty()).collect::<BTreeSet<_>>().len();
            MissRow { cmd: cmd.into(), miss: w.into(), hint, hits: ls.len(), sessions, refused: ls.iter().filter(|l| l.refused()).count() }
        })
        .collect();
    misses.sort_by(|a, b| b.hits.cmp(&a.hits).then_with(|| a.cmd.cmp(&b.cmd)).then_with(|| a.miss.cmp(&b.miss)));
    misses.truncate(MISSES_SHOWN);
    let used = |k: &str| lines.iter().any(|l| l.cmd == k || l.cmd.strip_prefix(k).is_some_and(|r| r.starts_with('.')));
    let unused = known().into_iter().filter(|k| !used(k)).collect();
    let built: Vec<&Line> = lines.iter().filter(|l| l.digest.is_some()).collect();
    let digest = (!built.is_empty()).then(|| {
        let sizes: Vec<usize> = built.iter().filter_map(|l| l.digest).collect();
        DigestRow {
            built: built.len(),
            mean: sizes.iter().sum::<usize>() / sizes.len(),
            largest: sizes.iter().copied().max().unwrap_or_default(),
            budget: built.iter().rev().find_map(|l| l.budget).unwrap_or_default(),
            cut: built.iter().filter(|l| l.cut).count(),
        }
    });
    let sessions = lines.iter().map(|l| l.session.as_str()).filter(|s| !s.is_empty()).collect::<BTreeSet<_>>().len();
    Report { days, project: project.map(String::from), calls: lines.len(), sessions, commands, misses, unused, digest }
}

fn pct(part: usize, whole: usize) -> usize {
    (part * 100 + whole / 2).checked_div(whole).unwrap_or(0)
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

impl Report {
    pub fn text(&self) -> String {
        use std::fmt::Write;
        let of = self.project.as_deref().map(|p| format!(" in {p}")).unwrap_or_default();
        if self.calls == 0 {
            return format!("no calls logged{of} in the last {}, the log is .local/{FILE} in the vault, one line per CLI command and MCP tool call on this machine\n", plural(self.days as usize, "day"));
        }
        let mut s = format!("usage{of} in the last {}: {} from {}\n", plural(self.days as usize, "day"), plural(self.calls, "call"), plural(self.sessions, "session"));
        let w = self.commands.iter().map(|c| c.cmd.chars().count()).max().unwrap_or(7).max(7);
        let _ = writeln!(s, "{:<w$}  {:>5}  {:>11}  {:>10}", "command", "calls", "errors", "chars/call");
        for c in &self.commands {
            let errors = if c.errors == 0 { "0".to_string() } else { format!("{} ({}%)", c.errors, pct(c.errors, c.calls)) };
            let _ = writeln!(s, "{:<w$}  {:>5}  {:>11}  {:>10}", c.cmd, c.calls, errors, c.chars / c.calls.max(1));
        }
        if !self.misses.is_empty() {
            s.push_str("misses, the commonest first:\n");
            for m in &self.misses {
                let how = if m.refused == m.hits { "refused" } else if m.refused == 0 { "worked" } else { "partly refused" };
                let hint = m.hint.as_deref().map(|h| format!(", suggested {h}")).unwrap_or_default();
                let _ = writeln!(s, "  {}  {} in {}, {how}{hint}", shown(&m.cmd, &m.miss), plural(m.hits, "hit"), plural(m.sessions, "session"));
            }
        }
        if !self.unused.is_empty() {
            let _ = writeln!(s, "never used: {}", self.unused.join(", "));
        }
        if let Some(d) = &self.digest {
            let _ = writeln!(
                s,
                "digest: built {}, {} chars on average, largest {} of digest_budget {}, cut {} ({}%)",
                plural(d.built, "time"),
                d.mean,
                d.largest,
                d.budget,
                plural(d.cut, "time"),
                pct(d.cut, d.built)
            );
        }
        s
    }
}

/// One line per command that failed more than a third of at least DOCTOR_CALLS calls in the last week.
pub fn doctor_problems(v: &Vault) -> Vec<Problem> {
    let r = report(&v.root, DOCTOR_DAYS, None);
    r.commands
        .iter()
        .filter(|c| c.calls >= DOCTOR_CALLS && c.errors * 3 > c.calls)
        .map(|c| Problem {
            path: format!(".local/{FILE}"),
            id: None,
            severity: "info",
            message: format!("{} failed {} of {} calls in the last {DOCTOR_DAYS} days ({}%), `breadcrumb usage` shows the misses", c.cmd, c.errors, c.calls, pct(c.errors, c.calls)),
        })
        .collect()
}
