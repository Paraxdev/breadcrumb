//! Notes that no longer hold: superseded by a newer note, or a decision marked superseded.

use crate::note::{Note, yaml_scalar_string};
use std::collections::HashSet;

pub const SUPERSEDES: &str = "supersedes";
pub const SUPERSEDED: &str = "superseded";
pub const HANDOFF: &str = "handoff";
pub const DECISION_STATUS: &str = "decision_status";

pub fn decision_superseded(n: &Note) -> bool {
    n.kind == "decision" && n.field(DECISION_STATUS).and_then(yaml_scalar_string).is_some_and(|s| s.trim().eq_ignore_ascii_case(SUPERSEDED))
}

/// Which notes of a vault no longer hold. Built from every note, so links from other projects count too.
#[derive(Debug, Default, Clone)]
pub struct Lapsed {
    superseded: HashSet<String>,
}

impl Lapsed {
    pub fn of(all: &[Note]) -> Lapsed {
        let mut superseded: HashSet<String> = HashSet::new();
        for n in all {
            if n.has_tag(SUPERSEDED) || decision_superseded(n) {
                superseded.insert(n.id.clone());
            }
            if n.is_archived() {
                continue;
            }
            for l in n.links.iter().filter(|l| l.rel == SUPERSEDES && l.to != n.id) {
                superseded.insert(l.to.clone());
            }
        }
        Lapsed { superseded }
    }

    pub fn is_superseded(&self, n: &Note) -> bool {
        self.superseded.contains(&n.id)
    }

    /// Why the note no longer holds, None while it does.
    pub fn label(&self, n: &Note) -> Option<&'static str> {
        self.is_superseded(n).then_some(SUPERSEDED)
    }

    pub fn hides(&self, n: &Note) -> bool {
        self.label(n).is_some()
    }
}

/// The one live handoff of a project (None is global): the newest created active handoff that nothing supersedes.
/// Created, not updated, so tagging or appending to an old handoff never makes it current again.
pub fn latest_handoff<'a>(notes: &'a [Note], project: Option<&str>, lapsed: &Lapsed) -> Option<&'a Note> {
    notes
        .iter()
        .filter(|n| n.kind == HANDOFF && n.project.as_deref() == project && !n.is_archived() && !lapsed.hides(n))
        .max_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)))
}
