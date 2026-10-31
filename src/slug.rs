//! Short readable names beside note ids. A slug always holds a hyphen and ids never do, so the two cannot be confused.

use crate::note::Note;
use crate::vault::Vault;
use anyhow::{Result, bail};
use serde_yaml::Value;
use std::collections::HashSet;
use std::path::PathBuf;

pub const SLUG_KEY: &str = "slug";
const MAX_WORDS: usize = 4;
const MAX_LEN: usize = 34;
/// A full vault backfill names too many files for one git command line on Windows.
const COMMIT_CHUNK: usize = 100;

/// Words that carry little in a name, on top of the recall stopwords.
const FILLER: &[&str] = &[
    "gets", "getting", "goes", "going", "keeps", "keep", "kept", "makes", "made", "takes", "take", "took", "puts", "put", "says", "said", "still", "never",
    "always", "ever", "instead", "without", "within", "across", "through", "between", "already", "yet", "much", "many", "new", "real", "really", "actually", "their",
    "his", "hers", "itself", "themselves", "whose", "whom", "may", "might", "must", "shall", "cannot", "wont", "not", "non", "per", "etc", "vs", "am", "are", "when", "once",
    "should", "every", "everywhere", "anywhere", "somewhere", "nothing", "none", "first", "last", "next", "than", "them", "works", "worked", "well", "better", "good", "fine", "right", "wrong",
    "uses", "shows", "show", "shown", "tells", "offers", "gives", "lets", "sometimes", "silently", "often", "again", "more", "less", "both", "either", "else", "even", "really",
];

pub fn is_valid(s: &str) -> bool {
    s.contains('-') && !s.contains("--") && s.len() <= 60 && crate::util::is_valid_slug(s) && s.chars().any(|c| c.is_ascii_lowercase())
}

fn hexish(t: &str) -> bool {
    t.len() >= 7 && t.chars().any(|c| c.is_ascii_digit()) && t.chars().any(|c| c.is_ascii_lowercase())
}

/// `2026-10-30` splits into three number words, session ids and hashes read as noise.
fn title_words(title: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let cleaned = title.to_ascii_lowercase().replace(['\'', '\u{2019}'], "");
    // 0.56 GB would leave a lone 0 in the name.
    let cleaned = cleaned.split_whitespace().filter(|w| !(w.contains('.') && w.starts_with(|c: char| c.is_ascii_digit()))).collect::<Vec<_>>().join(" ");
    let tokens: Vec<&str> = cleaned.split(|c: char| !c.is_ascii_alphanumeric()).filter(|t| !t.is_empty()).collect();
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        i += 1;
        let is_year = t.len() == 4 && t.starts_with("20") && t.chars().all(|c| c.is_ascii_digit());
        if is_year {
            while i < tokens.len() && tokens[i].len() <= 2 && tokens[i].chars().all(|c| c.is_ascii_digit()) {
                i += 1;
            }
            continue;
        }
        let filler = crate::recall::STOPWORDS.contains(&t) || FILLER.contains(&t);
        if filler || hexish(t) || out.iter().any(|w| w == t) || (t.len() == 1 && !t.chars().all(|c| c.is_ascii_digit())) {
            continue;
        }
        out.push(t.to_string());
    }
    out
}

/// Two to four words of the title, cut before a word that would make the name long.
pub fn from_title(title: &str, kind: &str, id: &str) -> String {
    let words = title_words(title);
    let mut name: Vec<&str> = vec![];
    for w in &words {
        let len: usize = name.iter().map(|x| x.len() + 1).sum::<usize>() + w.len();
        if name.len() == MAX_WORDS || (name.len() >= 2 && len > MAX_LEN) {
            break;
        }
        name.push(w);
    }
    // A bare number at the end would read as the number a clash adds.
    if name.len() > 2 && name.last().is_some_and(|w| w.chars().all(|c| c.is_ascii_digit())) {
        name.pop();
    }
    let kind = crate::util::slugify(kind, 20);
    match name.as_slice() {
        [] => format!("{kind}-{}", id.chars().take(4).collect::<String>()),
        [w] if *w == kind => format!("{kind}-{}", id.chars().take(4).collect::<String>()),
        [w] => format!("{w}-{kind}"),
        _ => name.join("-"),
    }
}

/// The board shows `PROJECT-id`, a slug must not read like that.
fn reads_like_key(s: &str, projects: &[String]) -> bool {
    s.rsplit_once('-').is_some_and(|(head, tail)| crate::id::is_valid_id(tail) && projects.iter().any(|p| p == head))
}

/// Normalizes a name given by hand, or says what is wrong with it.
pub fn check_given(given: &str, v: &Vault) -> Result<String> {
    let s = given.trim().to_ascii_lowercase();
    let parts: Vec<&str> = s.split('-').collect();
    let words = parts.len() - usize::from(parts.len() > 1 && parts[parts.len() - 1].chars().all(|c| c.is_ascii_digit()));
    if !is_valid(&s) || words > MAX_WORDS {
        bail!("slug '{given}' must be two to four lowercase words joined by hyphens, like stress-auto-mesh");
    }
    let projects: Vec<String> = v.load_projects().into_iter().map(|p| p.slug).collect();
    if reads_like_key(&s, &projects) {
        bail!("slug '{given}' reads like the board key of an item, pick another name");
    }
    Ok(s)
}

/// Every name in use: slugs and ids with a hyphen in them, which hand-written notes can have.
pub fn taken(notes: &[Note]) -> HashSet<String> {
    notes.iter().flat_map(|n| [n.slug(), n.id.contains('-').then(|| n.id.clone())]).flatten().collect()
}

/// `base` when free, else `base-2`, `base-3`.
pub fn unique(base: &str, taken: &HashSet<String>, projects: &[String]) -> String {
    let free = |s: &str| !taken.contains(s) && !reads_like_key(s, projects);
    if free(base) {
        return base.to_string();
    }
    (2..).map(|i| format!("{base}-{i}")).find(|s| free(s)).unwrap_or_default()
}

/// The slug for a new note, from the given name or its title. Call while holding the lock.
pub fn assign(v: &Vault, n: &mut Note, given: Option<&str>) -> Result<()> {
    let base = match given.map(str::trim).filter(|g| !g.is_empty()) {
        Some(g) => check_given(g, v)?,
        None => from_title(&n.title, &n.kind, &n.id),
    };
    let projects: Vec<String> = v.load_projects().into_iter().map(|p| p.slug).collect();
    let name = unique(&base, &taken(&v.load_notes()), &projects);
    n.set_field(SLUG_KEY, Value::String(name));
    Ok(())
}

/// An explicit rename, the one thing that changes a slug. A clash is an error here since the note already has a name.
pub fn rename(v: &Vault, n: &mut Note, given: &str) -> Result<()> {
    let s = check_given(given, v)?;
    if v.load_notes().iter().any(|o| o.id != n.id && (o.slug().as_deref() == Some(s.as_str()) || o.id == s)) {
        bail!("slug '{s}' is already taken");
    }
    n.set_field(SLUG_KEY, Value::String(s));
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct Backfill {
    pub open_only: bool,
    pub project: Option<String>,
    pub write: bool,
}

/// Names notes that have no slug, oldest open item first so that open items get the shortest names. Never touches
/// `updated`, a new name is not news. Files without frontmatter stay as they are, an id keeps working for them.
pub fn backfill(v: &Vault, opts: &Backfill) -> Result<Vec<Note>> {
    let _g = if opts.write { Some(v.lock()?) } else { None };
    let all = v.load_notes();
    let mut names = taken(&all);
    let projects: Vec<String> = v.load_projects().into_iter().map(|p| p.slug).collect();
    let is_open = |n: &Note| crate::backlog::is_item(n) && !n.is_archived() && !crate::backlog::is_closed(&crate::backlog::state(n));
    let mut todo: Vec<Note> = all
        .into_iter()
        .filter(|n| n.slug().is_none() && n.had_frontmatter)
        .filter(|n| opts.project.as_ref().is_none_or(|p| n.project.as_ref() == Some(p)))
        .filter(|n| !opts.open_only || is_open(n))
        .collect();
    todo.sort_by_key(|n| (!is_open(n), n.created, n.id.clone()));
    let mut paths: Vec<PathBuf> = vec![];
    for n in &mut todo {
        let name = unique(&from_title(&n.title, &n.kind, &n.id), &names, &projects);
        names.insert(name.clone());
        n.set_field(SLUG_KEY, Value::String(name));
        if opts.write {
            paths.push(v.write_note(n)?);
        }
    }
    for chunk in paths.chunks(COMMIT_CHUNK) {
        v.commit(chunk, &format!("breadcrumb: name {} notes", chunk.len()))?;
    }
    Ok(todo)
}

pub fn backfill_text(named: &[Note], write: bool) -> String {
    if named.is_empty() {
        return "every note in scope has a slug\n".into();
    }
    let width = named.iter().filter_map(|n| n.slug()).map(|s| s.len()).max().unwrap_or(0);
    let mut s = String::new();
    for n in named {
        s.push_str(&format!("{:<width$}  {}  {}\n", n.slug().unwrap_or_default(), n.id, crate::format::short_title(&n.title)));
    }
    s.push_str(&if write { format!("named {} notes\n", named.len()) } else { format!("would name {} notes, nothing was written, pass --yes to write\n", named.len()) });
    s
}
