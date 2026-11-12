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
/// What `is_valid` accepts.
const MAX_SLUG: usize = 60;
/// Even two such words and a clash number stay under MAX_SLUG.
const MAX_WORD: usize = 20;
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
    s.contains('-') && !s.contains("--") && s.len() <= MAX_SLUG && crate::util::is_valid_slug(s) && s.chars().any(|c| c.is_ascii_lowercase())
}

fn hexish(t: &str) -> bool {
    t.len() >= 7 && t.chars().any(|c| c.is_ascii_digit()) && t.chars().any(|c| c.is_ascii_lowercase())
}

/// The plain letters of an accented Latin one, itself otherwise. Combining accents drop.
fn plain_letter(c: char) -> &'static str {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ď' | 'đ' | 'ð' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'ĵ' => "j",
        'ķ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' | 'ț' => "t",
        'þ' => "th",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        '\u{300}'..='\u{36f}' | '\'' | '\u{2019}' => "",
        _ => " ",
    }
}

fn plain_text(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    for c in title.to_lowercase().chars() {
        if c.is_ascii() && c != '\'' {
            out.push(c);
        } else {
            out.push_str(plain_letter(c));
        }
    }
    out
}

/// `2026-10-30` splits into three number words, session ids and hashes read as noise.
fn title_words(title: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let cleaned = plain_text(title);
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
        out.push(t.chars().take(MAX_WORD).collect());
    }
    out
}

/// `task-1a2b`, for a title that gives no name of its own. Hand-written ids and kinds can hold anything.
fn kind_name(kind: &str, tail: &str) -> String {
    let kind = crate::util::slugify(kind, MAX_WORD);
    let kind = if kind.chars().any(|c| c.is_ascii_lowercase()) { kind } else { "note".to_string() };
    let tail: String = tail.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).take(MAX_WORD).collect();
    format!("{kind}-{}", if tail.is_empty() { "1" } else { &tail })
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
    let id4: String = id.chars().filter(char::is_ascii_alphanumeric).take(4).collect();
    if !name.is_empty() && !name.iter().any(|w| w.chars().any(|c| c.is_ascii_lowercase())) {
        return kind_name(kind, name[0]);
    }
    let word = crate::util::slugify(kind, MAX_WORD);
    let made = match name.as_slice() {
        [] => kind_name(kind, &id4),
        [w] if *w == word => kind_name(kind, &id4),
        [w] => format!("{w}-{word}"),
        _ => name.join("-"),
    };
    if is_valid(&made) { made } else { kind_name(kind, &id4) }
}

/// The board shows `PROJECT-id`, a slug must not read like that.
fn reads_like_key(s: &str, projects: &[String]) -> bool {
    s.rsplit_once('-').is_some_and(|(head, tail)| crate::id::is_valid_id(tail) && projects.iter().any(|p| p == head))
}

/// A trailing number is the clash number, not a word.
fn well_formed(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    let words = parts.len() - usize::from(parts.len() > 1 && parts[parts.len() - 1].chars().all(|c| c.is_ascii_digit()));
    is_valid(s) && words <= MAX_WORDS
}

/// A given name made valid the way title words are, cut to its first four words, and whether it was cut. None when no usable name is left.
fn repair(given: &str) -> Option<(String, bool)> {
    let plain = plain_text(given);
    let raw: Vec<&str> = plain.split(|c: char| !c.is_ascii_alphanumeric()).filter(|t| !t.is_empty()).collect();
    let long = raw.iter().take(MAX_WORDS).any(|t| t.chars().count() > MAX_WORD);
    let mut words: Vec<String> = raw.iter().take(MAX_WORDS).map(|t| t.chars().take(MAX_WORD).collect()).collect();
    while words.len() > 1 && words.join("-").len() > MAX_SLUG {
        words.pop();
    }
    let cut = long || raw.len() > words.len();
    let s = words.join("-");
    is_valid(&s).then_some((s, cut))
}

/// Normalizes a name given by hand, or says what is wrong with it.
pub fn check_given(given: &str, v: &Vault) -> Result<String> {
    let s = given.trim().to_ascii_lowercase();
    if !well_formed(&s) {
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
    (2..)
        .map(|i| {
            let tail = format!("-{i}");
            let room = MAX_SLUG.saturating_sub(tail.len()).min(base.len());
            format!("{}{tail}", base[..room].trim_end_matches('-'))
        })
        .find(|s| free(s))
        .unwrap_or_default()
}

/// The slug for a new note, from the given name or its title, never a refusal. A given name that breaks the rule is
/// repaired and the returned text says what happened. Call while holding the lock.
pub fn assign(v: &Vault, n: &mut Note, given: Option<&str>) -> Option<String> {
    let given = given.map(str::trim).filter(|g| !g.is_empty());
    let (base, how) = match given {
        Some(g) if well_formed(&g.to_ascii_lowercase()) => (g.to_ascii_lowercase(), None),
        Some(g) => match repair(g) {
            Some((s, cut)) => (s, Some(if cut { "shortened to" } else { "fixed to" })),
            None => (from_title(&n.title, &n.kind, &n.id), Some("made from the title:")),
        },
        None => (from_title(&n.title, &n.kind, &n.id), None),
    };
    let projects: Vec<String> = v.load_projects().into_iter().map(|p| p.slug).collect();
    let name = unique(&base, &taken(&v.load_notes()), &projects);
    n.set_field(SLUG_KEY, Value::String(name.clone()));
    let clash = (name != base).then(|| if reads_like_key(&base, &projects) { format!("{base} reads like an item's board key") } else { format!("{base} is taken") });
    match (how, clash) {
        (Some(h), Some(c)) => Some(format!("slug {h} {name}, {c}")),
        (Some(h), None) => Some(format!("slug {h} {name}")),
        (None, Some(c)) if given.is_some() => Some(format!("slug {name}, {c}")),
        _ => None,
    }
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
    /// Only global notes, `project` is then ignored
    pub global: bool,
    pub write: bool,
}

impl Backfill {
    pub fn covers(&self, n: &Note) -> bool {
        if self.global {
            return n.project.is_none();
        }
        self.project.as_ref().is_none_or(|p| n.project.as_ref() == Some(p))
    }
}

/// Names notes that have no slug or a stored one that is not valid, oldest open item first so that open items get the
/// shortest names. Never touches `updated`, a new name is not news. Files without frontmatter stay as they are, an id
/// keeps working for them.
pub fn backfill(v: &Vault, opts: &Backfill) -> Result<Vec<Note>> {
    let _g = if opts.write { Some(v.lock()?) } else { None };
    let all = v.load_notes();
    let mut names = taken(&all);
    let projects: Vec<String> = v.load_projects().into_iter().map(|p| p.slug).collect();
    let is_open = |n: &Note| crate::backlog::is_item(n) && !n.is_archived() && !crate::backlog::is_closed(&crate::backlog::state(n));
    let mut todo: Vec<Note> = all
        .into_iter()
        .filter(|n| n.slug().is_none() && n.had_frontmatter)
        .filter(|n| opts.covers(n))
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
