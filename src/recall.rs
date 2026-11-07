//! Just in time recall for hooks: notes scoped to file globs, first prompt search, staleness and session state.

use crate::note::Note;
use crate::ops::{Filter, Scope};
use crate::project::Project;
use crate::util::{atomic_write, now, path_str, read_text};
use crate::vault::Vault;
use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const FILE_NOTES_MAX: usize = 5;
pub const RECALL_MAX: usize = 3;
pub const RECALL_MIN_SCORE: f64 = 9.0;
pub const RECALL_MAX_TERMS: usize = 12;
pub const RECALL_MIN_WORDS: usize = 3;
pub const SESSION_MAX_AGE_SECS: u64 = 2 * 24 * 3600;
const ITEM_TITLE_MAX: usize = 70;
const STALE: &str = " (stale?)";

pub fn stale_exempt(n: &Note) -> bool {
    n.kind == "todo" || n.is_archived() || crate::backlog::is_item(n)
}

fn stale_since(fresh: DateTime<Utc>, days: u64) -> bool {
    days > 0 && (now() - fresh).num_days() >= days as i64
}

pub fn is_stale(n: &Note, days: u64) -> bool {
    !stale_exempt(n) && stale_since(n.fresh_at(), days)
}

pub fn stale_mark(n: &Note, days: u64) -> &'static str {
    if is_stale(n, days) { STALE } else { "" }
}

/// Forward slashes, no leading `./` or `/`, a trailing `/` means everything below. Lists keep their order.
pub fn normalize_globs(globs: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = vec![];
    for g in globs {
        let mut s = g.trim().replace('\\', "/");
        while let Some(r) = s.strip_prefix("./") {
            s = r.to_string();
        }
        let s = s.trim_start_matches('/');
        if s.is_empty() {
            continue;
        }
        let b = s.as_bytes();
        if s.starts_with('~') || (b.len() >= 2 && b[1] == b':') || s.split('/').any(|seg| seg == "..") {
            bail!("path glob '{g}' must be relative to the project root, like src/engine/** or *.hxx");
        }
        let mut s = s.to_string();
        if s.ends_with('/') {
            s.push_str("**");
        }
        if !out.contains(&s) {
            out.push(s);
        }
    }
    Ok(out)
}

fn has_glob_chars(s: &str) -> bool {
    s.contains(['*', '?'])
}

/// `*` and `?` stay within one segment, `**` spans any number of segments. A pattern without a slash
/// matches the file name at any depth, a literal directory matches everything below it. ASCII case is ignored.
pub fn glob_match(pattern: &str, rel: &str) -> bool {
    let p = pattern.trim().replace('\\', "/").to_ascii_lowercase();
    let p = p.trim_start_matches("./").trim_start_matches('/');
    let r = rel.replace('\\', "/").to_ascii_lowercase();
    let segs: Vec<&str> = r.split('/').filter(|s| !s.is_empty() && *s != ".").collect();
    if p.is_empty() || segs.is_empty() {
        return false;
    }
    let mut pat: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
    if !p.contains('/') {
        pat.insert(0, "**");
    }
    if match_segs(&pat, &segs) {
        return true;
    }
    if !has_glob_chars(p) {
        pat.push("**");
        return match_segs(&pat, &segs);
    }
    false
}

fn match_segs(p: &[&str], s: &[&str]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((&"**", rest)) => (0..=s.len()).any(|i| match_segs(rest, &s[i..])),
        Some((first, rest)) => !s.is_empty() && seg_match(first, s[0]) && match_segs(rest, &s[1..]),
    }
}

fn seg_match(p: &str, s: &str) -> bool {
    let p: Vec<char> = p.chars().collect();
    let s: Vec<char> = s.chars().collect();
    let (mut pi, mut si) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, si));
            pi += 1;
        } else if let Some((sp, ss)) = star {
            pi = sp + 1;
            si = ss + 1;
            star = Some((sp, ss + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

fn norm_path(p: &str) -> String {
    let s = crate::util::strip_verbatim(&p.trim().replace('\\', "/"));
    let t = s.trim_end_matches('/');
    if t.is_empty() || t.ends_with(':') { s } else { t.to_string() }
}

/// `file` relative to `root` with forward slashes, comparing ASCII case insensitively. None when outside.
pub fn rel_to_root(file: &str, root: &str) -> Option<String> {
    let f = norm_path(file);
    let r = norm_path(root);
    if r.is_empty() || f.len() <= r.len() || !f.is_char_boundary(r.len()) {
        return None;
    }
    let head = &f[..r.len()];
    let sep_ok = r.ends_with('/') || f.as_bytes()[r.len()] == b'/';
    if !head.eq_ignore_ascii_case(&r) || !sep_ok {
        return None;
    }
    let rest = f[r.len()..].trim_start_matches('/');
    (!rest.is_empty()).then(|| rest.to_string())
}

fn git_root(start: &Path) -> Option<PathBuf> {
    start.ancestors().find(|d| d.join(".git").exists()).map(Path::to_path_buf)
}

/// The project a file belongs to and its path relative to that project's root on this machine.
pub fn locate(projects: &[Project], cwd_project: Option<&str>, file: &Path) -> Option<(String, String)> {
    let fs = path_str(file);
    let mut best: Option<(usize, String, String)> = None;
    for p in projects {
        for root in &p.paths {
            if let Some(rel) = rel_to_root(&fs, root) {
                let len = norm_path(root).len();
                if best.as_ref().is_none_or(|(l, _, _)| len > *l) {
                    best = Some((len, p.slug.clone(), rel));
                }
            }
        }
    }
    if let Some((_, slug, rel)) = best {
        return Some((slug, rel));
    }
    let slug = cwd_project?;
    let root = git_root(file.parent()?)?;
    Some((slug.to_string(), rel_to_root(&fs, &path_str(&root))?))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub project: Option<String>,
    pub paths: Vec<String>,
    pub fresh: i64,
    pub exempt: bool,
    /// Superseded or expired when the index was built.
    #[serde(default)]
    pub lapsed: bool,
    /// Unix seconds an `expires` date or time runs out, checked on every read since time passes without file changes.
    #[serde(default)]
    pub expires: Option<i64>,
    /// The `summary` field or the title's first sentence, see digest::summary.
    #[serde(default)]
    pub summary: String,
    /// A backlog item that is done, shipped or rejected.
    #[serde(default)]
    pub closed: bool,
    /// A backlog item, whatever its state.
    #[serde(default)]
    pub item: bool,
}

impl IndexEntry {
    fn stale(&self, days: u64) -> bool {
        !self.exempt && DateTime::from_timestamp(self.fresh, 0).is_some_and(|t| stale_since(t, days))
    }

    pub fn holds(&self) -> bool {
        !self.lapsed && self.expires.is_none_or(|t| now().timestamp() < t)
    }
}

#[derive(Serialize, Deserialize, Default)]
struct PathIndex {
    sig: String,
    notes: Vec<IndexEntry>,
}

/// Names, sizes and mtimes of every note file. Directory listings carry those on all platforms we target,
/// so this costs no file reads.
fn notes_signature(v: &Vault) -> String {
    // The version rebuilds an index written by an older binary, which lacks newer entry fields.
    let mut parts: Vec<String> = vec!["v3".into()];
    for d in v.notes_dirs() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.ends_with(".md") {
                continue;
            }
            let Ok(m) = e.metadata() else { continue };
            let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
            parts.push(format!("{}/{name}:{}:{t}", path_str(&d), m.len()));
        }
    }
    parts.sort();
    crate::util::sha256_hex(parts.join("\n").as_bytes())
}

fn index_file(v: &Vault) -> PathBuf {
    v.root.join(".local").join("path-index.json")
}

/// Active notes that declare path globs, from a cache in .local that is rebuilt when any note file changes.
pub fn path_index(v: &Vault) -> Vec<IndexEntry> {
    let sig = notes_signature(v);
    let file = index_file(v);
    if let Ok(text) = read_text(&file)
        && let Ok(idx) = serde_json::from_str::<PathIndex>(&text)
        && idx.sig == sig
    {
        return idx.notes;
    }
    let all = v.load_notes();
    let lapsed = crate::lifecycle::Lapsed::of(&all);
    let notes: Vec<IndexEntry> = all
        .iter()
        .filter(|n| !n.is_archived())
        .filter_map(|n| {
            let paths = n.path_globs();
            (!paths.is_empty()).then(|| IndexEntry {
                id: n.id.clone(),
                kind: n.kind.clone(),
                title: n.title.clone(),
                project: n.project.clone(),
                paths,
                fresh: n.fresh_at().timestamp(),
                exempt: stale_exempt(n),
                lapsed: lapsed.hides(n),
                expires: match crate::lifecycle::expiry(n) {
                    Some(crate::lifecycle::Expiry::At(t)) => Some(t.timestamp()),
                    _ => None,
                },
                summary: crate::digest::summary(n),
                closed: crate::backlog::is_item(n) && crate::backlog::is_closed(&crate::backlog::state(n)),
                item: crate::backlog::is_item(n),
            })
        })
        .collect();
    if let Ok(text) = serde_json::to_string(&PathIndex { sig, notes: notes.clone() }) {
        let _ = atomic_write(&file, text.as_bytes());
    }
    notes
}

fn item(id: &str, kind: &str, title: &str, stale: bool) -> String {
    let t = crate::util::truncate_chars(&crate::util::one_line(title), ITEM_TITLE_MAX);
    format!("{id} {kind} {t}{}", if stale { STALE } else { "" })
}

const WHY_MAX: usize = 100;
const WHY_TITLE_MAX: usize = 60;

enum Place {
    /// Listed, lower ranks first: gotchas and decisions warn, then feedback, open items, other notes.
    Show(u8),
    /// Only counted, a code map is read with `brief`.
    Map,
    /// Only counted, a done, shipped or rejected item warns of nothing.
    Closed,
}

fn place(e: &IndexEntry) -> Place {
    if e.kind == crate::map::MAP {
        Place::Map
    } else if e.closed {
        Place::Closed
    } else {
        Place::Show(match e.kind.as_str() {
            "gotcha" | "decision" => 0,
            "feedback" => 1,
            _ if e.item => 2,
            _ => 3,
        })
    }
}

/// A gotcha or decision carries its why, the summary when it says more than the start of the title.
fn file_item(rank: u8, e: &IndexEntry, stale: bool) -> String {
    if rank > 0 {
        return item(&e.id, &e.kind, &e.title, stale);
    }
    let title = crate::util::one_line(&e.title);
    let why = e.summary.trim_end_matches("...");
    let text = if why.is_empty() || title.starts_with(why) {
        crate::util::truncate_chars(&title, WHY_MAX)
    } else {
        format!("{}: {}", crate::util::truncate_chars(&title, WHY_TITLE_MAX), crate::util::truncate_chars(&e.summary, WHY_MAX))
    };
    format!("{} {} {text}{}", e.id, e.kind, if stale { STALE } else { "" })
}

/// One line naming the notes whose globs match `file`, skipping ids in `exclude`. Returns the line and the ids.
/// Maps and closed items are left out behind a count, a map also points at `brief` for the file.
pub fn file_notes(v: &Vault, cwd: &Path, file: &Path, exclude: &HashSet<String>) -> Option<(String, Vec<String>)> {
    let abs = if file.is_absolute() { file.to_path_buf() } else { cwd.join(file) };
    let projects = v.load_projects();
    let cwd_project = crate::project::detect(&projects, &crate::git::config_remote_urls(cwd), cwd);
    let (slug, rel) = locate(&projects, cwd_project.as_deref(), &abs)?;
    let hits: Vec<IndexEntry> = path_index(v)
        .into_iter()
        .filter(|e| e.project.as_deref().is_none_or(|p| p == slug))
        .filter(|e| !exclude.contains(&e.id) && e.holds())
        .filter(|e| e.paths.iter().any(|g| glob_match(g, &rel)))
        .collect();
    let mut shown: Vec<(u8, IndexEntry)> = vec![];
    let (mut maps, mut closed): (Vec<String>, Vec<String>) = (vec![], vec![]);
    for e in hits {
        match place(&e) {
            Place::Show(rank) => shown.push((rank, e)),
            Place::Map => maps.push(e.id),
            Place::Closed => closed.push(e.id),
        }
    }
    if shown.is_empty() && maps.is_empty() {
        return None;
    }
    shown.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.fresh.cmp(&a.1.fresh)));
    shown.truncate(FILE_NOTES_MAX);
    let days = v.config.stale_days;
    let items: Vec<String> = shown.iter().map(|(rank, e)| file_item(*rank, e, e.stale(days))).collect();
    let mut counts: Vec<String> = vec![];
    if !maps.is_empty() {
        counts.push(format!("+{} map{}", maps.len(), if maps.len() == 1 { "" } else { "s" }));
    }
    if !closed.is_empty() {
        counts.push(format!("+{} closed", closed.len()));
    }
    let mut line = format!("breadcrumb notes for {rel}: {}", items.join("; "));
    if !counts.is_empty() {
        line.push_str(&if items.is_empty() { counts.join(", ") } else { format!(" ({})", counts.join(", ")) });
    }
    if !maps.is_empty() {
        line.push_str(&format!(". A code map covers it, `crumb brief {rel}` (or the map tool) lists its lines"));
    }
    let ids = shown.into_iter().map(|(_, e)| e.id).chain(maps).chain(closed).collect();
    Some((line, ids))
}

pub const STOPWORDS: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "your", "with", "this", "that", "these", "those", "from", "into", "onto", "have",
    "has", "had", "was", "were", "will", "would", "should", "could", "can", "cant", "does", "did", "doing", "done", "just", "also",
    "then", "than", "there", "their", "them", "they", "what", "when", "where", "which", "while", "who", "why", "how", "all", "any",
    "some", "more", "most", "other", "such", "only", "own", "same", "too", "very", "please", "pls", "lets", "let", "make", "want",
    "need", "like", "get", "got", "use", "using", "used", "now", "here", "about", "again", "out", "our", "ours", "its", "him",
    "her", "his", "she", "one", "two", "via", "per", "yes", "okay", "sure", "thanks", "thank", "hey", "hello", "able", "check",
    "look", "see", "try", "think", "know", "something", "anything", "everything", "thing", "things", "way", "work", "working",
    "over", "under", "after", "before", "because", "being", "been", "both", "each", "few", "whats", "dont", "don",
    "isnt", "doesnt", "im", "ive", "youre", "we", "me", "my", "is", "it", "to", "of", "in", "on", "at", "by", "or", "an", "as",
    "be", "do", "if", "so", "up", "no", "go", "a", "i",
];

/// Prompt words worth searching for: no stopwords or short words, deduplicated, capped.
pub fn prompt_terms(prompt: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for t in crate::search::tokenize(prompt) {
        if t.chars().count() < 3 || STOPWORDS.contains(&t.as_str()) || out.contains(&t) {
            continue;
        }
        out.push(t);
        if out.len() >= RECALL_MAX_TERMS {
            break;
        }
    }
    out
}

/// Best matches for a prompt in the project plus global, only those that clear a minimum score.
pub fn recall(v: &Vault, project: Option<&str>, prompt: &str, exclude: &HashSet<String>) -> Vec<Note> {
    let terms = prompt_terms(prompt);
    if terms.is_empty() {
        return vec![];
    }
    let scope = match project {
        Some(slug) => Scope::Project { slug: slug.to_string(), with_global: true },
        None => Scope::Global,
    };
    let all = v.load_notes();
    let lapsed = crate::lifecycle::Lapsed::of(&all);
    let Ok(notes) = crate::ops::filter_notes(all, &scope, &Filter::default()) else { return vec![] };
    let notes: Vec<Note> = notes.into_iter().filter(|n| !exclude.contains(&n.id) && !lapsed.hides(n)).collect();
    let need = terms.len().min(2);
    crate::search::search(notes, &terms.join(" "))
        .hits
        .into_iter()
        .filter(|h| h.score >= RECALL_MIN_SCORE && h.matched >= need)
        .take(RECALL_MAX)
        .map(|h| h.note)
        .collect()
}

pub fn recall_line(v: &Vault, notes: &[Note]) -> String {
    let days = v.config.stale_days;
    let items: Vec<String> = notes.iter().map(|n| item(&n.id, &n.kind, &n.title, is_stale(n, days))).collect();
    format!("breadcrumb may be relevant: {}", items.join("; "))
}

pub fn prompt_words(prompt: &str) -> usize {
    prompt.split_whitespace().count()
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub shown: Vec<String>,
    #[serde(default)]
    pub prompted: bool,
    /// Unix seconds of the first SessionStart, kept across compaction.
    #[serde(default)]
    pub started: i64,
    /// The Stop hook has decided about the handoff reminder, it never fires twice.
    #[serde(default)]
    pub handoff_done: bool,
}

impl Session {
    pub fn shown_set(&self) -> HashSet<String> {
        self.shown.iter().cloned().collect()
    }

    pub fn add_shown(&mut self, ids: impl IntoIterator<Item = String>) {
        for id in ids {
            if !self.shown.contains(&id) {
                self.shown.push(id);
            }
        }
    }
}

fn sessions_dir(root: &Path) -> PathBuf {
    root.join(".local").join("sessions")
}

/// None for ids that are not safe as a file name.
pub fn session_file(root: &Path, id: &str) -> Option<PathBuf> {
    let ok = !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then(|| sessions_dir(root).join(format!("{id}.json")))
}

pub fn load_session(root: &Path, id: &str) -> Session {
    session_file(root, id).and_then(|p| read_text(&p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn save_session(root: &Path, id: &str, s: &Session) -> Result<()> {
    let Some(p) = session_file(root, id) else { bail!("invalid session id") };
    atomic_write(&p, serde_json::to_string(s)?.as_bytes())
}

pub fn prune_sessions(root: &Path) {
    let Ok(rd) = std::fs::read_dir(sessions_dir(root)) else { return };
    for e in rd.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() > SESSION_MAX_AGE_SECS);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Note ids that appear in a digest text, so later hooks do not repeat them.
pub fn ids_in(text: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for t in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        let t = t.to_ascii_lowercase();
        if crate::id::is_valid_id(&t) && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}
