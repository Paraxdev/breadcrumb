//! Which pins may have outlived their use. Nothing here writes, a person or lead decides.

use crate::note::Note;
use regex::Regex;
use serde::Serialize;
use std::fmt::Write as _;
use std::sync::LazyLock;

// Conservative on purpose: a dated or bare status line of its own, or a sentence about the note itself. A list entry
// like `- done: on dev` defines the word, and `this is done by CI` describes something else.
static FINISHED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?im-u)^[ \t]*\d{4}-\d{2}-\d{2}(?:[ T]~?\d{1,2}:\d{2}Z?)?[ \t]*[:,]?[ \t]*(?:done|finished|completed?|shipped|landed)(?:[ \t]*[.:]|[ \t]+on[ \t]+\d{4}|[ \t]*$)",
        r"|^[ \t]*(?:done|finished|completed)(?:\.|[ \t]+on[ \t]+\d{4})",
        r"|\bthis no longer (?:applies|holds)\b",
        r"|\bthis (?:one[ -]time )?(?:decision|rule|plan) (?:finished|no longer (?:applies|holds)|(?:is|was) (?:now )?(?:done|finished|completed?|obsolete|retired))\b",
    ))
    .expect("valid regex")
});

pub fn reads_finished(body: &str) -> Option<String> {
    FINISHED.find(body).map(|m| m.as_str().trim().to_string())
}

pub fn age_days(n: &Note) -> i64 {
    (crate::util::now() - n.pin_checked_at()).num_days()
}

/// Why a pin deserves a look, empty when it does not. `review_days` 0 turns the age check off.
pub fn reasons(n: &Note, review_days: u64) -> Vec<String> {
    let mut out = vec![];
    if n.kind == "decision"
        && let Some(phrase) = reads_finished(&n.body)
    {
        out.push(format!("decision reads as finished: \"{phrase}\""));
    }
    let age = age_days(n);
    if review_days > 0 && age >= review_days as i64 {
        out.push(format!("not verified for {age} days"));
    }
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct Pin {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub project: Option<String>,
    pub age_days: i64,
    pub reasons: Vec<String>,
    pub commands: Vec<String>,
}

/// Flagged first, then oldest first.
pub fn review(notes: &[Note], review_days: u64) -> Vec<Pin> {
    let mut pins: Vec<Pin> = notes
        .iter()
        .filter(|n| n.pinned && !n.is_archived())
        .map(|n| Pin {
            id: n.id.clone(),
            kind: n.kind.clone(),
            title: n.title.clone(),
            project: n.project.clone(),
            age_days: age_days(n),
            reasons: reasons(n, review_days),
            commands: vec![format!("breadcrumb verify {}", n.id), format!("breadcrumb update {} --unpin", n.id), format!("breadcrumb archive {}", n.id)],
        })
        .collect();
    pins.sort_by_key(|p| (p.reasons.is_empty(), -p.age_days));
    pins
}

pub fn text(pins: &[Pin], scope: &str, review_days: u64) -> String {
    let flagged = pins.iter().filter(|p| !p.reasons.is_empty()).count();
    let mut s = format!("{scope}: {} pins, {flagged} flagged, age is since verified, review after {review_days} days\n", pins.len());
    let line = |p: &Pin| format!("{} {} {}d {}", p.id, p.kind, p.age_days, crate::format::fit_title(&p.title, None));
    for p in pins.iter().filter(|p| !p.reasons.is_empty()) {
        let _ = writeln!(s, "\n{}\n  why: {}", line(p), p.reasons.join(", "));
        for c in &p.commands {
            let _ = writeln!(s, "  {c}");
        }
    }
    if flagged < pins.len() {
        s.push_str("\nNot flagged:\n");
        for p in pins.iter().filter(|p| p.reasons.is_empty()) {
            let _ = writeln!(s, "  {}", line(p));
        }
        s.push_str("  To act on one: breadcrumb verify <id>, breadcrumb update <id> --unpin, breadcrumb archive <id>\n");
    }
    s.push_str("\nNothing changed, a person or lead decides.\n");
    s
}
