//! Questions an agent asks the user about a backlog item. They live in the item's `questions`
//! frontmatter list. An open one locks the item, an answered one stays flagged unseen until an
//! agent acknowledges it, so nothing depends on a live session.
use crate::backlog::{self, event_line, get_item, save, state};
use crate::note::Note;
use crate::util::{fmt_time, now, one_line, truncate_chars};
use crate::vault::Vault;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::fmt::Write;

pub const KEY: &str = "questions";
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 4;
const DIGEST_LINES: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Opt {
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub desc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Answer {
    #[serde(default)]
    pub picked: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub at: String,
    pub by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Question {
    pub q: u32,
    pub text: String,
    #[serde(default)]
    pub options: Vec<Opt>,
    #[serde(default)]
    pub multi: bool,
    #[serde(default)]
    pub free: bool,
    pub asked: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Answer>,
    /// False until an agent acknowledges the answer.
    #[serde(default)]
    pub seen: bool,
}

impl Question {
    pub fn open(&self) -> bool {
        self.answer.is_none()
    }

    pub fn unseen(&self) -> bool {
        self.answer.is_some() && !self.seen
    }

    pub fn answer_text(&self) -> String {
        let Some(a) = &self.answer else { return String::new() };
        let mut parts = a.picked.clone();
        if let Some(t) = &a.text {
            parts.push(t.clone());
        }
        parts.join(", ")
    }
}

pub fn questions(n: &Note) -> Vec<Question> {
    match n.field(KEY) {
        Some(Value::Sequence(s)) => s.iter().filter_map(|v| serde_yaml::from_value(v.clone()).ok()).collect(),
        _ => vec![],
    }
}

fn store(n: &mut Note, qs: &[Question]) {
    if qs.is_empty() {
        n.unset_field(KEY);
    } else {
        n.set_field(KEY, serde_yaml::to_value(qs).unwrap_or(Value::Null));
    }
}

pub fn qid(n: &Note, q: &Question) -> String {
    format!("{}.{}", n.id, q.q)
}

pub fn open_questions(n: &Note) -> Vec<Question> {
    questions(n).into_iter().filter(Question::open).collect()
}

/// An open question keeps the item out of `next` and claims.
pub fn locked(n: &Note) -> bool {
    backlog::is_item(n) && questions(n).iter().any(Question::open)
}

/// Answered and not yet acknowledged by an agent.
pub fn answered_unseen(n: &Note) -> bool {
    backlog::is_item(n) && questions(n).iter().any(Question::unseen)
}

fn built(n: &Note) -> bool {
    matches!(state(n).as_str(), "review" | "done" | "shipped")
}

pub fn hint(n: &Note) -> &'static str {
    if built(n) { "already built, adjust it to the answer" } else { "not built yet, build it with the answer" }
}

/// Splits `<item>.<n>`, a bare item id leaves the number open.
fn parse_qid(v: &Vault, target: &str) -> Result<(Note, Option<u32>)> {
    let (id, q) = match target.rsplit_once('.') {
        Some((id, q)) if !q.is_empty() && q.chars().all(|c| c.is_ascii_digit()) => (id, q.parse().ok()),
        _ => (target, None),
    };
    Ok((get_item(v, id)?, q))
}

pub struct AskReq {
    pub id: String,
    pub text: String,
    pub options: Vec<Opt>,
    pub multi: bool,
    pub free: bool,
    pub by: Option<String>,
}

/// Parses `label: description` option arguments.
pub fn parse_options(raw: &[String]) -> Vec<Opt> {
    raw.iter()
        .filter(|s| !s.trim().is_empty())
        .map(|s| match s.split_once(':') {
            Some((l, d)) => Opt { label: l.trim().to_string(), desc: d.trim().to_string() },
            None => Opt { label: s.trim().to_string(), desc: String::new() },
        })
        .collect()
}

pub fn ask(v: &Vault, req: AskReq) -> Result<(Note, Question)> {
    let text = one_line(&req.text);
    if text.is_empty() {
        bail!("a question needs text");
    }
    let n_opts = req.options.len();
    if n_opts == 0 && !req.free {
        bail!("give {MIN_OPTIONS} to {MAX_OPTIONS} options, or set free for an open question");
    }
    if n_opts > MAX_OPTIONS || (n_opts > 0 && n_opts < MIN_OPTIONS) {
        bail!("a question takes {MIN_OPTIONS} to {MAX_OPTIONS} options, got {n_opts}");
    }
    if req.options.iter().any(|o| o.label.is_empty()) {
        bail!("every option needs a label");
    }
    let _g = v.lock()?;
    let mut n = get_item(v, &req.id)?;
    let mut qs = questions(&n);
    let by = backlog::session_name(req.by.as_deref()).unwrap_or_else(|| backlog::DEFAULT_ACTOR.to_string());
    let q = Question {
        q: qs.iter().map(|q| q.q).max().unwrap_or(0) + 1,
        text,
        options: req.options,
        multi: req.multi,
        free: req.free || n_opts == 0,
        asked: fmt_time(&now()),
        by: by.clone(),
        answer: None,
        seen: false,
    };
    qs.push(q.clone());
    store(&mut n, &qs);
    n.body = backlog::append_history(&n.body, &event_line(&format!("asked question {}", q.q), &by));
    save(v, &mut n, "ask")?;
    Ok((n, q))
}

/// Picks 1-based option numbers from tokens like `2`, `1,3` or `1 3`.
pub fn parse_choices(tokens: &[String]) -> Result<Vec<usize>> {
    let mut out = vec![];
    for t in tokens.iter().flat_map(|t| t.split([',', ' '])).filter(|t| !t.is_empty()) {
        match t.parse::<usize>() {
            Ok(i) if i > 0 => {
                if !out.contains(&i) {
                    out.push(i);
                }
            }
            _ => bail!("'{t}' is not an option number"),
        }
    }
    Ok(out)
}

/// Records an answer by option numbers and/or free text. `target` may be a bare item id when it has one open question.
pub fn answer(v: &Vault, target: &str, choices: &[usize], text: Option<&str>, by: &str) -> Result<(Note, Question)> {
    let _g = v.lock()?;
    let (mut n, num) = parse_qid(v, target)?;
    let mut qs = questions(&n);
    let open: Vec<usize> = qs.iter().enumerate().filter(|(_, q)| q.open() && num.is_none_or(|x| q.q == x)).map(|(i, _)| i).collect();
    let i = match open.as_slice() {
        [i] => *i,
        [] => match num {
            Some(x) if qs.iter().any(|q| q.q == x) => bail!("{} question {x} is already answered", n.id),
            Some(x) => bail!("{} has no question {x}", n.id),
            None => bail!("{} has no open question", n.id),
        },
        many => {
            let ids: Vec<String> = many.iter().map(|&i| qid(&n, &qs[i])).collect();
            bail!("{} has several open questions, name one: {}", n.id, ids.join(", "));
        }
    };
    let q = &mut qs[i];
    let text = text.map(one_line).filter(|t| !t.is_empty());
    if choices.is_empty() && text.is_none() {
        bail!("pick an option number or give a free text answer");
    }
    if text.is_some() && !q.free {
        bail!("question {} takes no free text, pick one of its options", q.q);
    }
    if choices.len() > 1 && !q.multi {
        bail!("question {} takes a single option", q.q);
    }
    let mut picked = vec![];
    for &c in choices {
        match q.options.get(c.wrapping_sub(1)) {
            Some(o) => picked.push(o.label.clone()),
            None => bail!("question {} has {} options, not {c}", q.q, q.options.len()),
        }
    }
    let by = if by.trim().is_empty() { backlog::DEFAULT_ACTOR } else { by.trim() };
    q.answer = Some(Answer { picked, text, at: fmt_time(&now()), by: by.to_string() });
    q.seen = false;
    let q = q.clone();
    store(&mut n, &qs);
    n.body = backlog::append_history(&n.body, &event_line(&format!("question {} answered", q.q), by));
    save(v, &mut n, "answer")?;
    Ok((n, q))
}

/// Drops an open question nobody should answer any more, which unlocks the item.
pub fn withdraw(v: &Vault, target: &str, by: &str) -> Result<Note> {
    let _g = v.lock()?;
    let (mut n, num) = parse_qid(v, target)?;
    let mut qs = questions(&n);
    let Some(i) = qs.iter().position(|q| q.open() && num.is_none_or(|x| q.q == x)) else {
        bail!("{} has no open question{}", n.id, num.map(|x| format!(" {x}")).unwrap_or_default());
    };
    let q = qs.remove(i);
    store(&mut n, &qs);
    n.body = backlog::append_history(&n.body, &event_line(&format!("question {} withdrawn", q.q), by));
    save(v, &mut n, "withdraw")?;
    Ok(n)
}

/// Marks every unseen answer seen in memory, false when there was none.
pub fn ack_in(n: &mut Note, by: &str) -> bool {
    let mut qs = questions(n);
    let mut hit = vec![];
    for q in qs.iter_mut().filter(|q| q.unseen()) {
        q.seen = true;
        hit.push(q.q.to_string());
    }
    if hit.is_empty() {
        return false;
    }
    store(n, &qs);
    n.body = backlog::append_history(&n.body, &event_line(&format!("answer {} acknowledged", hit.join(", ")), by));
    true
}

pub fn ack(v: &Vault, id: &str, by: &str) -> Result<Option<Note>> {
    let _g = v.lock()?;
    let mut n = get_item(v, id)?;
    if !ack_in(&mut n, by) {
        return Ok(None);
    }
    save(v, &mut n, "ack")?;
    Ok(Some(n))
}

/// What an agent needs to know about an item's questions: the open ones and the unseen answers with the hint.
pub fn notice(n: &Note) -> String {
    let mut s = String::new();
    for q in open_questions(n) {
        let _ = writeln!(s, "waiting on the user, question {}: {}", qid(n, &q), q.text);
    }
    let unseen: Vec<Question> = questions(n).into_iter().filter(Question::unseen).collect();
    for q in &unseen {
        let _ = writeln!(s, "answer arrived on {}: {} -> {}", qid(n, q), q.text, q.answer_text());
    }
    if !unseen.is_empty() {
        let _ = writeln!(s, "item is [{}], {}. Claim the item or `ack` it to clear the flag.", state(n), hint(n));
    }
    s
}

/// One open question with numbered options, for `questions` and the interactive prompt.
pub fn render(n: &Note, q: &Question) -> String {
    let mut s = format!("{} {}\n  {}\n", qid(n, q), crate::format::short_title(&n.title), q.text);
    for (i, o) in q.options.iter().enumerate() {
        let _ = writeln!(s, "  {}. {}{}", i + 1, o.label, if o.desc.is_empty() { String::new() } else { format!(": {}", o.desc) });
    }
    let how = match (q.multi, q.free) {
        (true, true) => "pick several (1,3) and/or type text",
        (true, false) => "pick several (1,3)",
        (false, true) => "pick one or type text",
        (false, false) => "pick one",
    };
    let _ = writeln!(s, "  ({how})");
    s
}

/// Open questions in the given notes, oldest first, with their items.
pub fn open_in(notes: &[Note]) -> Vec<(Note, Question)> {
    let mut out: Vec<(Note, Question)> = notes
        .iter()
        .filter(|n| backlog::is_item(n) && !n.is_archived())
        .flat_map(|n| open_questions(n).into_iter().map(move |q| (n.clone(), q)))
        .collect();
    out.sort_by(|a, b| a.1.asked.cmp(&b.1.asked));
    out
}

/// Digest lines: unseen answers first, then open questions, each capped at a few lines.
pub fn digest_lines(items: &[&Note]) -> String {
    let mut ans = vec![];
    let mut open = vec![];
    for n in items.iter().filter(|n| !n.is_archived()) {
        for q in questions(n) {
            if q.unseen() {
                let hint = if built(n) { "built, adjust" } else { "not built, build with it" };
                let t = truncate_chars(&format!("{} -> {}", one_line(&q.text), one_line(&q.answer_text())), 150);
                ans.push(format!("answer arrived on {}: {t} ({hint})", qid(n, &q)));
            } else if q.open() {
                open.push(format!("waiting on user {}: {}", qid(n, &q), truncate_chars(&one_line(&q.text), 80)));
            }
        }
    }
    let mut s = String::new();
    for list in [ans, open] {
        for l in list.iter().take(DIGEST_LINES) {
            let _ = writeln!(s, "{l}");
        }
        if list.len() > DIGEST_LINES {
            let _ = writeln!(s, "+{} more, see `breadcrumb questions`", list.len() - DIGEST_LINES);
        }
    }
    s
}
