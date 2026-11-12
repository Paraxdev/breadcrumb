//! Agents judging breadcrumb itself: one line on what got in the way, confused them, was missing or helped, filed as
//! kind judgment in `judge_project` wherever the agent worked, so the tool's own sessions can turn them into items.

use crate::kind::{FieldDef, KindDef};
use crate::note::{Link, Note, yaml_scalar_string};
use crate::ops::{self, AddReq, FieldInput};
use crate::vault::Vault;
use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_yaml::Value;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::PathBuf;

pub const JUDGMENT: &str = "judgment";
pub const VERDICTS: [&str; 4] = ["hindered", "confusing", "missing", "helped"];
pub const HELPED: &str = "helped";
/// Only the tool assigns it, to a judgment filed without a verdict.
pub const UNRATED: &str = "unrated";
/// What the kind accepts, the verdicts plus unrated.
pub const STORED: [&str; 5] = ["hindered", "confusing", "missing", "helped", UNRATED];
/// The verdicts that wait for triage, in the order they are counted.
const TRIAGE: [&str; 4] = ["hindered", "confusing", "missing", UNRATED];
/// How many times a helped judgment was given, absent means once.
pub const COUNT_KEY: &str = "count";
/// The link a judgment gets once an item covers it.
pub const FILED_AS: &str = "filed-as";
pub const FILE_KINDS: [&str; 4] = ["bug", "idea", "feature", "task"];
const TITLE_MAX: usize = 100;

fn field(n: &Note, key: &str) -> Option<String> {
    n.field(key).and_then(yaml_scalar_string).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn verdict(n: &Note) -> String {
    field(n, "verdict").unwrap_or_else(|| UNRATED.into())
}

pub fn about(n: &Note) -> String {
    field(n, "about").unwrap_or_else(|| "general".into())
}

pub fn count(n: &Note) -> u64 {
    field(n, COUNT_KEY).and_then(|c| c.parse().ok()).filter(|c| *c > 0).unwrap_or(1)
}

/// Covered by an item (any outgoing link) or archived, so nothing is left to triage.
pub fn triaged(n: &Note) -> bool {
    n.is_archived() || !n.links.is_empty()
}

/// Praise is never a finding, so a helped judgment never waits for triage.
pub fn to_triage(n: &Note) -> bool {
    n.kind == JUDGMENT && verdict(n) != HELPED && !triaged(n)
}

/// `hindered: search`, or `helped: brief, x3` for praise given more than once. None for other kinds.
pub fn mark(n: &Note) -> Option<String> {
    if n.kind != JUDGMENT {
        return None;
    }
    let c = count(n);
    Some(format!("{}: {}{}", verdict(n), about(n), if c > 1 { format!(", x{c}") } else { String::new() }))
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

fn verdict_field() -> Option<FieldDef> {
    crate::kind::seed_kinds().into_iter().find(|k| k.name == JUDGMENT).and_then(|k| k.field("verdict").cloned())
}

/// The kind with this binary's verdict field, since a vault's kind file may predate unrated.
pub fn current_kind(mut k: KindDef) -> KindDef {
    if let Some(f) = verdict_field() {
        k.fields.retain(|x| x.name != f.name);
        k.fields.insert(0, f);
    }
    k
}

/// Rewrites a judgment kind file that predates unrated. Call while holding the lock.
pub fn upgrade_kind_file(v: &Vault) -> Result<Option<PathBuf>> {
    let Some(k) = v.load_kind(JUDGMENT)? else { return Ok(None) };
    let fresh = k.field("verdict").is_some_and(|f| f.values.iter().any(|x| x == UNRATED) && f.default.as_ref().and_then(|d| d.as_str()) == Some(UNRATED));
    if fresh {
        return Ok(None);
    }
    Ok(Some(v.write_kind(&current_kind(k))?))
}

/// A judgment as saved: a new note, or `counted` on an existing helped one with the same about.
pub struct Judged {
    pub note: Note,
    pub counted: bool,
}

impl Judged {
    pub fn text(&self) -> String {
        if self.counted {
            format!("counted, no new note: {} \"{}\" now holds {} helped judgments about {}", self.note.handle(), crate::format::short_title(&self.note.title), count(&self.note), about(&self.note))
        } else {
            crate::format::ack("judged", &self.note)
        }
    }
}

/// The line an unrated judgment's reply ends with, `mcp` names the tool's way to set it instead of the CLI's.
pub fn unrated_hint(n: &Note, mcp: bool) -> Option<String> {
    (n.kind == JUDGMENT && verdict(n) == UNRATED).then(|| {
        let how = if mcp { format!("update {} with fields {{\"verdict\": \"...\"}}", n.id) } else { format!("`breadcrumb update {} --field verdict=...`", n.id) };
        format!("no verdict, saved as unrated, set one of {} with {how}", VERDICTS.join(", "))
    })
}

fn input_text(f: &FieldInput) -> Option<String> {
    match f {
        FieldInput::Raw(s) => Some(s.clone()),
        FieldInput::Value(x) => yaml_scalar_string(x),
    }
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

fn given(req: &AddReq, key: &str) -> Option<String> {
    req.fields.iter().rev().find(|(k, _)| k.trim() == key).and_then(|(_, f)| input_text(f))
}

/// Every add of kind judgment comes here: the verdict is checked, a missing one saved as unrated, and a helped one
/// whose about already has a helped judgment is counted on that note instead of filed again.
pub fn add(v: &Vault, mut req: AddReq) -> Result<Judged> {
    let verdict = match given(&req, "verdict") {
        Some(g) => check_verdict(&g)?,
        None => UNRATED.to_string(),
    };
    req.fields.retain(|(k, _)| k.trim() != "verdict");
    req.fields.push(("verdict".into(), FieldInput::Raw(verdict.clone())));
    // The project asked for only becomes seen_in, the note goes to the configured one, so a vault without it yet is fine.
    if home(v).is_some() {
        req.require_existing_project = false;
    }
    if verdict == HELPED {
        let about = given(&req, "about").unwrap_or_else(|| "general".into());
        let project = home(v).or_else(|| req.project.clone());
        let text = req.body.as_deref().map(str::trim).filter(|b| !b.is_empty()).unwrap_or(&req.title).to_string();
        if let Some(note) = count_on(v, project.as_deref(), &about, &text)? {
            return Ok(Judged { note, counted: true });
        }
    }
    Ok(Judged { note: ops::add(v, req)?, counted: false })
}

fn count_on(v: &Vault, project: Option<&str>, about_: &str, text: &str) -> Result<Option<Note>> {
    let _g = v.lock()?;
    let mut same: Vec<Note> = v
        .load_notes()
        .into_iter()
        .filter(|n| n.kind == JUDGMENT && !n.is_archived() && n.project.as_deref() == project && verdict(n) == HELPED && about(n).eq_ignore_ascii_case(about_))
        .collect();
    same.sort_by_key(|n| (std::cmp::Reverse(count(n)), n.created));
    let Some(mut n) = same.into_iter().next() else { return Ok(None) };
    n.set_field(COUNT_KEY, Value::Number((count(&n) + 1).into()));
    append_line(&mut n, &crate::util::one_line(text));
    let p = v.write_note(&mut n)?;
    v.commit(&[p], &ops::commit_msg("count", &n))?;
    Ok(Some(n))
}

/// What `ops::append` does to a note, for writes that change more than one note in one commit.
fn append_line(n: &mut Note, text: &str) {
    let text = format!("{}\n{text}", crate::session::append_head());
    n.body = if crate::backlog::is_item(n) {
        crate::backlog::insert_before_history(&n.body, &text)
    } else if n.body.trim().is_empty() {
        text
    } else {
        format!("{}\n\n{text}", n.body.trim_end())
    };
    n.mark_verified();
    crate::session::stamp_write(n);
    n.updated = crate::util::now();
}

pub fn judge(v: &Vault, text: &str, verdict: &str, about: Option<&str>, seen_in: Option<&str>) -> Result<Judged> {
    let text = crate::util::normalize_newlines(text.trim());
    if text.is_empty() {
        bail!("say what got in your way, confused you, was missing or helped");
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
    add(v, AddReq { title, kind: Some(JUDGMENT.into()), project: home, fields, body: Some(text), ..Default::default() })
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
    pub count: u64,
}

/// Judgments to triage and every helped one not archived, `all` adds the triaged and archived ones.
pub fn list(v: &Vault, since: Option<DateTime<Utc>>, all: bool) -> Vec<Row> {
    let open = |n: &Note| if verdict(n) == HELPED { !n.is_archived() } else { !triaged(n) };
    let mut notes: Vec<Note> = v.load_notes().into_iter().filter(|n| n.kind == JUDGMENT && since.is_none_or(|s| n.created >= s) && (all || open(n))).collect();
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
            count: count(n),
        })
        .collect()
}

fn row_line(s: &mut String, r: &Row) {
    let times = if r.count > 1 { format!(", x{}", r.count) } else { String::new() };
    let seen = r.seen_in.as_ref().map(|p| format!(" (in {p})")).unwrap_or_default();
    let done = if r.triaged && r.verdict != HELPED { " [triaged]" } else { "" };
    let _ = writeln!(s, "{} {} {}{times}: {}{seen}{done}", r.id, r.verdict, r.about, r.title);
}

/// The ones to triage in full, grouped counts first, and helped as one line of counts per about, or with
/// `helped` the helped ones in full.
pub fn list_text(rows: &[Row], helped: bool) -> String {
    let (praise, triage): (Vec<&Row>, Vec<&Row>) = rows.iter().partition(|r| r.verdict == HELPED);
    if helped {
        if praise.is_empty() {
            return "no helped judgments\n".into();
        }
        let mut s = format!("{}, given {} times:\n", plural(praise.len(), "helped judgment"), praise.iter().map(|r| r.count).sum::<u64>());
        for r in &praise {
            row_line(&mut s, r);
        }
        return s;
    }
    let mut s = String::new();
    if triage.is_empty() {
        s.push_str("no judgments to triage, --all shows the triaged ones too\n");
    } else {
        let mut by_about: BTreeMap<&str, BTreeMap<&str, usize>> = BTreeMap::new();
        for r in &triage {
            *by_about.entry(&r.about).or_default().entry(&r.verdict).or_default() += 1;
        }
        let mut groups: Vec<(&str, usize, String)> = by_about
            .iter()
            .map(|(a, vs)| {
                let total = vs.values().sum();
                let parts: Vec<String> = TRIAGE.iter().filter_map(|v| vs.get(v).map(|c| format!("{c} {v}"))).collect();
                (*a, total, parts.join(", "))
            })
            .collect();
        groups.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let _ = writeln!(s, "{} of breadcrumb to triage, by what they are about:", plural(triage.len(), "judgment"));
        for (a, _, parts) in &groups {
            let _ = writeln!(s, "  {a}: {parts}");
        }
        s.push('\n');
        for r in &triage {
            row_line(&mut s, r);
        }
    }
    if !praise.is_empty() {
        let mut by_about: BTreeMap<&str, u64> = BTreeMap::new();
        for r in &praise {
            *by_about.entry(&r.about).or_default() += r.count;
        }
        let mut counts: Vec<(&str, u64)> = by_about.into_iter().collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let parts: Vec<String> = counts.iter().map(|(a, c)| format!("{a} {c}")).collect();
        let _ = writeln!(s, "\nhelped: {}, `breadcrumb judge --helped` lists them", parts.join(", "));
    }
    if !triage.is_empty() {
        s.push_str("\nTriage: `breadcrumb judge --file <id>... [--as idea] [--title \"...\"]` makes one item of them, `--to <item> <id>...` files them under an existing one, `--dismiss <id>... --why \"...\"` archives them.\n");
    }
    s
}

pub fn plural(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

fn judgments(v: &Vault, ids: &[String]) -> Result<Vec<Note>> {
    if ids.is_empty() {
        bail!("name the judgments by id, `breadcrumb judge --list` shows them");
    }
    let mut out: Vec<Note> = vec![];
    for id in ids {
        let n = v.get(id)?;
        if n.kind != JUDGMENT {
            bail!("{} is a {}, not a judgment", n.handle(), n.kind);
        }
        crate::merge::refuse_merged(&n)?;
        if !out.iter().any(|x| x.id == n.id) {
            out.push(n);
        }
    }
    Ok(out)
}

/// The item title from a judgment's, without the `judge:` some agents start them with.
fn item_title(t: &str) -> String {
    let t = crate::util::one_line(t);
    let t = match t.split_once(':') {
        Some((p, rest)) if ["judge", "judgment", "judgement"].contains(&p.trim().to_ascii_lowercase().as_str()) && !rest.trim().is_empty() => rest.trim().to_string(),
        _ => t,
    };
    let mut c = t.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn quoted(n: &Note) -> String {
    let text = Some(crate::util::one_line(&n.body)).filter(|b| !b.is_empty()).unwrap_or_else(|| crate::util::one_line(&n.title));
    let seen = field(n, "seen_in").map(|p| format!(", seen in {p}")).unwrap_or_default();
    format!("{} {} about {}{seen}: {text}", n.id, verdict(n), about(n))
}

/// Links each judgment `filed-as` the item, and with `name` appends one line per judgment to it, in one commit.
fn file_under(v: &Vault, js: &[Note], item: &str, name: bool) -> Result<(Note, Vec<Note>)> {
    let _g = v.lock()?;
    let mut item = v.get(item)?;
    crate::merge::refuse_merged(&item)?;
    let mut touched = vec![];
    let mut linked = vec![];
    for j in js {
        let mut j = v.get(&j.id)?;
        if j.id == item.id {
            bail!("{} cannot be filed under itself", j.handle());
        }
        if !j.links.iter().any(|l| l.to == item.id && l.rel == FILED_AS) {
            j.links.push(Link { to: item.id.clone(), rel: FILED_AS.into() });
            crate::session::stamp_write(&mut j);
            j.updated = crate::util::now();
            touched.push(v.write_note(&mut j)?);
        }
        linked.push(j);
    }
    if name {
        let lines: Vec<String> = linked.iter().map(|j| format!("filed from judgment {}", quoted(j))).collect();
        append_line(&mut item, &lines.join("\n"));
        touched.push(v.write_note(&mut item)?);
    }
    v.commit(&touched, &format!("breadcrumb: file {} judgments as {} {}", linked.len(), item.id, crate::util::one_line(&item.title)))?;
    Ok((item, linked))
}

/// One new backlog item in the judge project from the judgments, each linked `filed-as` to it.
pub fn file(v: &Vault, ids: &[String], kind: Option<&str>, title: Option<&str>, priority: Option<&str>) -> Result<(Note, Vec<Note>)> {
    let js = judgments(v, ids)?;
    let kind = kind.map(|k| k.trim().to_ascii_lowercase()).unwrap_or_else(|| "bug".into());
    if !FILE_KINDS.contains(&kind.as_str()) {
        bail!("--as is one of {}, got '{kind}'", FILE_KINDS.join(", "));
    }
    let title = title.map(crate::util::one_line).filter(|t| !t.is_empty()).unwrap_or_else(|| item_title(&js[0].title));
    let mut body = String::from("Filed from judgments of breadcrumb:\n");
    for j in &js {
        let _ = writeln!(body, "- {}", quoted(j));
    }
    let fields = priority.map(|p| vec![("priority".to_string(), FieldInput::Raw(p.trim().to_string()))]).unwrap_or_default();
    let project = home(v).or_else(|| js[0].project.clone());
    let item = ops::add(v, AddReq { title, kind: Some(kind), project, fields, body: Some(body), ..Default::default() })?;
    let (_, linked) = file_under(v, &js, &item.id, false)?;
    Ok((v.get(&item.id)?, linked))
}

/// Links the judgments `filed-as` an existing item and names each on it.
pub fn file_to(v: &Vault, item: &str, ids: &[String]) -> Result<(Note, Vec<Note>)> {
    let js = judgments(v, ids)?;
    file_under(v, &js, item, true)
}

/// Appends the reason to each judgment and archives it, in one commit.
pub fn dismiss(v: &Vault, ids: &[String], why: &str) -> Result<Vec<Note>> {
    let why = crate::util::one_line(why);
    if why.is_empty() {
        bail!("say why with --why \"...\", it stays on each judgment");
    }
    let js = judgments(v, ids)?;
    let _g = v.lock()?;
    let mut touched = vec![];
    let mut out = vec![];
    for j in &js {
        let mut j = v.get(&j.id)?;
        append_line(&mut j, &format!("dismissed: {why}"));
        j.status = "archived".into();
        j.clear_kept("status");
        touched.push(v.write_note(&mut j)?);
        out.push(j);
    }
    v.commit(&touched, &format!("breadcrumb: dismiss {} judgments", out.len()))?;
    Ok(out)
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

pub const NUDGE: &str = "Judge breadcrumb: if it hindered you, confused you or lacked something, `crumb judge \"...\" -v hindered|confusing|missing -a <cmd>` (MCP add kind judgment). Nothing to say is fine, -v helped only if it stood out";

/// The ask to judge breadcrumb, and in its own project what waits for triage.
pub fn digest_lines(v: &Vault, project: Option<&str>, all: &[Note]) -> Vec<String> {
    let mut out = vec![];
    if nudge_due(v) {
        out.push(NUDGE.to_string());
    }
    if project.is_some() && project == home(v).as_deref() {
        let mut by: BTreeMap<String, usize> = BTreeMap::new();
        for n in all.iter().filter(|n| to_triage(n)) {
            *by.entry(verdict(n)).or_default() += 1;
        }
        let open: usize = by.values().sum();
        if open > 0 {
            let parts: Vec<String> = TRIAGE.iter().filter_map(|t| by.get(*t).map(|c| format!("{c} {t}"))).collect();
            out.push(format!("Judgments: {open} to triage ({}), `breadcrumb judge --list`", parts.join(", ")));
        }
    }
    out
}
