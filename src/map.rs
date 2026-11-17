//! Code maps: one note per area of a project, lines of `file:line symbol, what it does`, each checked
//! against the working tree by the git blob hash its file had when the line was saved.

use crate::note::{Note, yaml_scalar_string};
use crate::recall::{glob_match, normalize_globs, rel_to_root};
use crate::util::{now, one_line, path_str, truncate_chars};
use crate::vault::Vault;
use anyhow::{Context, Result, bail};
use serde_yaml::{Mapping, Value};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

pub const MAP: &str = "map";
pub const AREA_KEY: &str = "area";
pub const BLOBS_KEY: &str = "blobs";
/// Stands in for the blob hash of a directory, no git hash equals it.
const DIR: &str = "dir";
const HASH_LEN: usize = 12;
/// About 2k tokens at 4 chars per token.
pub const BRIEF_BUDGET: usize = 8000;
const NOTE_MAX: usize = 360;
const HEADER_FILES: usize = 3;
/// Lines of each map for the targets that rank above the gotchas.
const FIRST_LINES: usize = 5;
const NOTES_HEAD: usize = 24;
const CUT_RESERVE: usize = 240;

#[derive(Clone, Debug, PartialEq)]
pub struct MapLine {
    pub file: String,
    pub line: Option<String>,
    pub symbol: String,
    pub text: String,
    /// A directory, written with a trailing slash and only checked for being there.
    pub dir: bool,
}

fn looks_like_path(s: &str) -> bool {
    !s.is_empty() && (s.contains('/') || s.contains('.')) && !s.contains(['*', '?', ' ']) && !s.contains("://")
}

fn is_line_no(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_digit()) && s.chars().all(|c| c.is_ascii_digit() || c == '-')
}

fn is_abs(s: &str) -> bool {
    let b = s.as_bytes();
    s.starts_with('/') || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'/'))
}

/// The first word of a map line without list mark, backticks and trailing colon, and the rest.
fn split_loc(raw: &str) -> (String, &str) {
    let s = raw.trim();
    let s = s.strip_prefix("- ").or_else(|| s.strip_prefix("* ")).unwrap_or(s).trim();
    let (loc, rest) = s.split_once(char::is_whitespace).unwrap_or((s, ""));
    (loc.trim_matches('`').trim_end_matches([':', ',']).replace('\\', "/"), rest)
}

/// The words of a note without the mark it was written or stored with, for comparing notes.
fn note_words(raw: &str) -> String {
    let t = raw.trim();
    let t = t.strip_prefix('>').or_else(|| t.strip_prefix("- ")).or_else(|| t.strip_prefix("* ")).unwrap_or(t);
    one_line(t.trim())
}

impl MapLine {
    /// Reads `file:line symbol, what it does`, the line number and the description are optional. An absolute
    /// file inside `repo` becomes relative to it.
    pub fn parse(raw: &str, repo: Option<&Path>) -> Option<MapLine> {
        Self::read(raw, repo).map(|x| x.0)
    }

    /// `parse`, also saying whether a symbol followed the file after a colon, like `src/a.rs:put(` or `src/a.rs:4:put`.
    fn read(raw: &str, repo: Option<&Path>) -> Option<(MapLine, bool)> {
        let (loc, rest) = split_loc(raw);
        if loc.contains("://") {
            return None;
        }
        let skip = if is_abs(&loc) && !loc.starts_with('/') { 2 } else { 0 };
        let (file, tail) = match loc[skip..].find(':') {
            Some(i) => (&loc[..skip + i], &loc[skip + i + 1..]),
            None => (loc.as_str(), ""),
        };
        let (line, sym) = match tail.split_once(':') {
            Some((n, s)) if is_line_no(n) => (Some(n), s),
            _ if is_line_no(tail) => (Some(tail), ""),
            _ => (None, tail),
        };
        let sym = sym.split('(').next().unwrap_or("");
        let mut file = file.trim_start_matches("./").to_string();
        let dir = file.len() > 1 && file.ends_with('/');
        file = file.trim_end_matches('/').to_string();
        if is_abs(&file) {
            file = rel_to_root(&file, &path_str(repo?))?;
        }
        let slash = if dir { "/" } else { "" };
        if !looks_like_path(&format!("{file}{slash}")) || file.split('/').any(|x| x == "..") {
            return None;
        }
        let mut rest = one_line(rest.trim().trim_start_matches(['-', ':']).trim());
        if !sym.is_empty() {
            rest = if rest.is_empty() { sym.to_string() } else { format!("{sym}, {rest}") };
        }
        let symbol = rest.split_once(',').map(|x| x.0).unwrap_or(&rest).trim().to_string();
        let at = format!("{file}{slash}{}", line.map(|n| format!(":{n}")).unwrap_or_default());
        let text = if rest.is_empty() { at } else { format!("{at} {rest}") };
        Some((MapLine { file, line: line.map(str::to_string), symbol, text, dir }, !sym.is_empty()))
    }

    fn into_dir(mut self) -> MapLine {
        self.text = format!("{}/{}", self.file, &self.text[self.file.len()..]);
        self.dir = true;
        self
    }

    /// Lines with the same key describe the same thing, a put replaces them.
    pub fn key(&self) -> String {
        if self.symbol.is_empty() {
            format!("{}:{}", self.file, self.line.as_deref().unwrap_or(""))
        } else {
            format!("{} {}", self.file, self.symbol)
        }
    }

    fn first_line_no(&self) -> u64 {
        self.line.as_deref().and_then(|n| n.split('-').next()).and_then(|n| n.parse().ok()).unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fresh {
    Fresh,
    /// The symbol is now on this line.
    Moved(u64),
    Changed,
    Gone,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct CodeMap {
    pub note: Note,
    pub lines: Vec<MapLine>,
    /// The area's notes: body lines that are not map lines, kept as written and never checked. A put stores a new
    /// one as `> text`, which every binary reads as no map line.
    pub notes: Vec<String>,
    pub blobs: HashMap<String, String>,
}

impl CodeMap {
    pub fn of(note: Note) -> CodeMap {
        let mut lines = vec![];
        let mut notes = vec![];
        for l in note.body.lines().filter(|l| !l.trim().is_empty()) {
            match MapLine::parse(l, None) {
                Some(m) if !l.trim_start().starts_with('>') => lines.push(m),
                _ => notes.push(l.trim_end().to_string()),
            }
        }
        let blobs = match note.field(BLOBS_KEY) {
            Some(Value::Mapping(m)) => m.iter().filter_map(|(k, v)| Some((yaml_scalar_string(k)?, yaml_scalar_string(v)?))).collect(),
            _ => HashMap::new(),
        };
        CodeMap { note, lines, notes, blobs }
    }

    pub fn area(&self) -> String {
        area_of(&self.note)
    }

    pub fn files(&self) -> Vec<String> {
        self.lines.iter().map(|l| l.file.clone()).collect::<BTreeSet<_>>().into_iter().collect()
    }

    /// Files with a line whose saved hash differs from the current one, the only ones worth reading.
    pub fn changed_files(&self, cur: &HashMap<String, Option<String>>) -> BTreeSet<String> {
        let differs = |l: &&MapLine| matches!(cur.get(&l.file), Some(Some(h)) if h != DIR && self.blobs.get(&l.key()) != Some(h));
        self.lines.iter().filter(differs).map(|l| l.file.clone()).collect()
    }

    /// `texts` holds the current text of the files `changed_files` named, a changed line is fresh while its
    /// symbol is still in the file and moved when it sits on another line.
    pub fn states(&self, current: Option<&HashMap<String, Option<String>>>, texts: &HashMap<String, String>) -> Vec<Fresh> {
        self.lines
            .iter()
            .map(|l| match current {
                None => Fresh::Unknown,
                Some(cur) => match cur.get(&l.file) {
                    Some(Some(h)) if h == DIR || self.blobs.get(&l.key()) == Some(h) => Fresh::Fresh,
                    Some(Some(_)) => texts.get(&l.file).map_or(Fresh::Changed, |t| locate(l, t)),
                    _ => Fresh::Gone,
                },
            })
            .collect()
    }

    /// One line: area, id, line count and which files went stale.
    pub fn header(&self, states: &[Fresh]) -> String {
        let n = self.lines.len();
        let k = self.notes.len();
        let notes = if k == 0 { String::new() } else { format!(", {k} note{}", if k == 1 { "" } else { "s" }) };
        let head = format!("map {} {}: {n} line{}{notes}", self.area(), self.note.id, if n == 1 { "" } else { "s" });
        if states.contains(&Fresh::Unknown) {
            return format!("{head}, freshness unknown, no checkout of the repo found here (pass repo)");
        }
        let mut stale: Vec<(String, &str)> = vec![];
        let (mut count, mut moved) = (0, 0);
        for (l, s) in self.lines.iter().zip(states) {
            let why = match s {
                Fresh::Moved(_) => {
                    moved += 1;
                    continue;
                }
                Fresh::Changed => "changed",
                Fresh::Gone => "gone",
                _ => continue,
            };
            count += 1;
            if !stale.iter().any(|(f, _)| *f == l.file) {
                stale.push((l.file.clone(), why));
            }
        }
        if count == 0 && moved == 0 {
            return format!("{head}, all fresh");
        }
        let mut out = format!("{head}, {} fresh", n - count - moved);
        if moved > 0 {
            out.push_str(&format!(", {moved} moved"));
        }
        if count > 0 {
            let mut named: Vec<String> = stale.iter().take(HEADER_FILES).map(|(f, w)| format!("{f} {w}")).collect();
            if stale.len() > HEADER_FILES {
                named.push(format!("+{} files", stale.len() - HEADER_FILES));
            }
            out.push_str(&format!(", {count} stale ({}), recheck them and map put the corrected lines", named.join(", ")));
        }
        out
    }

    pub fn render(&self, states: &[Fresh]) -> String {
        let mut out = self.header(states);
        out.push('\n');
        for p in &self.notes {
            out.push_str(p);
            out.push('\n');
        }
        for (l, s) in self.lines.iter().zip(states) {
            out.push_str(&line_text(l, *s));
            out.push('\n');
        }
        out
    }
}

fn line_text(l: &MapLine, s: Fresh) -> String {
    match s {
        Fresh::Changed | Fresh::Gone => format!("[stale] {}", l.text),
        Fresh::Moved(to) => {
            let old = l.line.as_deref().unwrap_or("");
            let at = format!("{}:{old}", l.file);
            let shifted = match old.split_once('-') {
                Some((_, end)) => format!("{to}-{}", end.parse::<u64>().map_or(to, |e| (e + to).saturating_sub(l.first_line_no()))),
                None => to.to_string(),
            };
            let rest = l.text.strip_prefix(&at).unwrap_or("");
            format!("{}:{shifted}{rest} [moved from {old}]", l.file)
        }
        _ => l.text.clone(),
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// First token of the line's description, the name to look for in the file.
fn symbol_token(l: &MapLine) -> &str {
    l.symbol.split_whitespace().next().unwrap_or("").trim_matches(|c: char| !is_word(c))
}

fn has_word(row: &str, sym: &str) -> bool {
    row.match_indices(sym).any(|(at, m)| !row[..at].chars().next_back().is_some_and(is_word) && !row[at + m.len()..].chars().next().is_some_and(is_word))
}

/// Looks the line's symbol up as a whole word in the file's current text, the occurrence nearest the old line wins.
fn locate(l: &MapLine, text: &str) -> Fresh {
    let sym = symbol_token(l);
    if sym.is_empty() {
        return Fresh::Changed;
    }
    let old = l.first_line_no();
    let mut best: Option<u64> = None;
    for (i, row) in text.lines().enumerate() {
        let no = i as u64 + 1;
        if has_word(row, sym) && best.is_none_or(|b| no.abs_diff(old) < b.abs_diff(old)) {
            best = Some(no);
        }
    }
    match best {
        None => Fresh::Changed,
        Some(_) if l.line.is_none() => Fresh::Fresh,
        Some(n) if n == old => Fresh::Fresh,
        Some(n) => Fresh::Moved(n),
    }
}

/// How many rows either side of the given number the symbol may sit.
const LINE_WINDOW: u64 = 3;

/// One short warning per incoming line that does not match the checkout, each file read once. A miss is advice, the put still saves.
fn line_warnings(repo: &Path, lines: &[MapLine], blobs: &HashMap<String, Option<String>>) -> Vec<String> {
    let mut texts: HashMap<&str, Option<String>> = HashMap::new();
    let mut out = vec![];
    for l in lines {
        let at = l.text.split_whitespace().next().unwrap_or_default();
        match blobs.get(&l.file) {
            Some(Some(h)) if h == DIR => continue,
            _ if l.dir => {
                out.push(format!("{at} directory not found"));
                continue;
            }
            _ => {}
        }
        let present = blobs.get(&l.file).is_none_or(Option::is_some);
        let text = texts.entry(&l.file).or_insert_with(|| present.then(|| std::fs::read(repo.join(&l.file)).ok()).flatten().map(|b| String::from_utf8_lossy(&b).into_owned()));
        let Some(text) = text else {
            out.push(format!("{at} file not found"));
            continue;
        };
        let Some(range) = l.line.as_deref() else { continue };
        let total = text.lines().count() as u64;
        let last = range.rsplit('-').next().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
        let first = l.first_line_no();
        if first == 0 || last > total {
            out.push(format!("{at} is past the end, the file ends at line {total}"));
            continue;
        }
        let sym = symbol_token(l);
        if sym.is_empty() {
            continue;
        }
        let hits: Vec<u64> = text.lines().enumerate().filter(|(_, row)| has_word(row, sym)).map(|(i, _)| i as u64 + 1).collect();
        if hits.iter().any(|&h| h + LINE_WINDOW >= first && h <= last.max(first) + LINE_WINDOW) {
            continue;
        }
        match hits.iter().min_by_key(|h| h.abs_diff(first)) {
            Some(n) => out.push(format!("{at} {sym} is at line {n}")),
            None => out.push(format!("{at} {sym} is not in the file")),
        }
    }
    out
}

pub fn area_of(n: &Note) -> String {
    n.field(AREA_KEY).and_then(yaml_scalar_string).map(|s| one_line(&s)).filter(|s| !s.is_empty()).unwrap_or_else(|| n.title.clone())
}

fn same_area(a: &str, b: &str) -> bool {
    let (x, y) = (crate::util::slugify(a, 80), crate::util::slugify(b, 80));
    if x.is_empty() { a.trim().eq_ignore_ascii_case(b.trim()) } else { x == y }
}

/// Active maps of a project, by area.
pub fn maps(v: &Vault, project: &str) -> Vec<CodeMap> {
    let mut out: Vec<CodeMap> = v
        .load_notes()
        .into_iter()
        .filter(|n| n.kind == MAP && n.project.as_deref() == Some(project) && !n.is_archived())
        .map(CodeMap::of)
        .collect();
    out.sort_by_key(|m| m.area().to_lowercase());
    out
}

pub fn find(v: &Vault, project: &str, area: &str) -> Option<CodeMap> {
    maps(v, project).into_iter().find(|m| same_area(&m.area(), area))
}

/// Refuses a second map for an area, `ops::add` calls it for kind map.
pub fn check_new(v: &Vault, n: &Note) -> Result<()> {
    let area = n.field(AREA_KEY).and_then(yaml_scalar_string).unwrap_or_default();
    if area.trim().is_empty() {
        bail!("a map needs an area, use `breadcrumb map put <area>`");
    }
    let Some(p) = n.project.as_deref() else { bail!("a map belongs to a project, pass --project") };
    if let Some(m) = find(v, p, &area) {
        bail!("project {p} already has a map for area '{}' ({}), add lines to it with `breadcrumb map put`", m.area(), m.note.id);
    }
    Ok(())
}

/// The git checkout maps of `project` are checked against: `explicit`, else the linked worktree of the project
/// holding cwd, else the checkout holding cwd, else a recorded project path on this PC.
pub fn repo_for(v: &Vault, project: &str, explicit: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    let dir = match explicit {
        Some(p) => crate::util::absolutize(p),
        // A linked worktree holds its branch's files, which the checkout at the bound path may lack.
        None if crate::git::main_checkout(cwd).is_some() && v.detect_project(cwd).as_deref() == Some(project) => cwd.to_path_buf(),
        None => crate::workflow::target_dir(v, project, None, cwd)?,
    };
    let top = crate::git::toplevel(&dir).with_context(|| format!("{} is not inside a git checkout, pass repo", path_str(&dir)))?;
    Ok(PathBuf::from(top))
}

/// Blob hashes of the files as they are in the working tree, `DIR` for a directory, None for a path that is not there.
pub fn current_blobs(repo: &Path, files: &[String]) -> Result<HashMap<String, Option<String>>> {
    let mut out: HashMap<String, Option<String>> = HashMap::new();
    let present: Vec<&String> = files.iter().filter(|f| repo.join(f).is_file()).collect();
    for f in files {
        out.insert(f.clone(), repo.join(f).is_dir().then(|| DIR.to_string()));
    }
    for chunk in present.chunks(200) {
        let mut args = vec!["hash-object", "--"];
        args.extend(chunk.iter().map(|f| f.as_str()));
        let text = crate::git::ok(repo, &args)?;
        for (f, h) in chunk.iter().zip(text.lines()) {
            out.insert((*f).clone(), Some(h.trim().chars().take(HASH_LEN).collect()));
        }
    }
    Ok(out)
}

#[derive(Clone, Debug, Default)]
pub struct PutReq {
    pub area: String,
    pub project: String,
    pub lines: Vec<String>,
    pub paths: Vec<String>,
    /// Line keys (`file symbol`), whole files or note texts to remove.
    pub drop: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PutReport {
    pub map: CodeMap,
    pub created: bool,
    pub added: usize,
    pub replaced: usize,
    pub refreshed: usize,
    pub unchanged: usize,
    pub dropped: usize,
    pub notes_added: usize,
    /// How each line that is not a plain file line was taken: as a note, a directory or a file and symbol.
    pub taken: Vec<String>,
    /// Lines that do not match the checkout, saved all the same.
    pub warnings: Vec<String>,
}

impl PutReport {
    pub fn text(&self) -> String {
        let m = &self.map;
        let k = m.notes.len();
        let mut notes = if k == 0 { String::new() } else { format!(", {k} note{}", if k == 1 { "" } else { "s" }) };
        if self.notes_added > 0 {
            notes.push_str(&format!(" ({} new)", self.notes_added));
        }
        let warned: String = self.taken.iter().chain(&self.warnings).map(|w| format!("
{w}")).collect();
        format!(
            "{} map {} {}: {} added, {} replaced, {} refreshed, {} unchanged, {} dropped, {} lines{notes}{warned}",
            if self.created { "created" } else { "updated" },
            m.area(),
            m.note.id,
            self.added,
            self.replaced,
            self.refreshed,
            self.unchanged,
            self.dropped,
            m.lines.len()
        )
    }
}

/// Creates the area's map or merges lines into it: a line with the key of an existing one replaces it and records
/// the file's current hash, every other existing line keeps the hash it was saved with.
pub fn put(v: &Vault, req: PutReq, repo: &Path) -> Result<PutReport> {
    let area = one_line(&req.area);
    if area.is_empty() {
        bail!("area is required");
    }
    let project = crate::ops::check_slug(&req.project)?;
    let mut incoming: Vec<MapLine> = vec![];
    let mut notes: Vec<String> = vec![];
    let mut taken: Vec<String> = vec![];
    for raw in req.lines.iter().flat_map(|l| l.lines()) {
        let t = raw.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let read = if t.starts_with('>') { None } else { MapLine::read(t, Some(repo)) };
        let why = match read {
            None if is_abs(&split_loc(t).0) && !t.starts_with('>') => {
                let file = split_loc(t).0;
                let hint = Path::new(&file).parent().and_then(crate::git::toplevel).map(|r| format!(" ({r})")).unwrap_or_default();
                bail!("map line '{t}' names a file outside the checkout {}, write it relative to that checkout or pass repo (--repo) with the checkout it lies in{hint}", path_str(repo));
            }
            None => "not checked".to_string(),
            Some((l, sym)) => {
                let at = repo.join(&l.file);
                let l = if !l.dir && at.is_dir() {
                    taken.push(format!("taken as a directory: {t}"));
                    l.into_dir()
                } else {
                    l
                };
                if !l.dir && !sym && l.line.is_none() && !l.file.contains('/') && !at.exists() {
                    format!("no file {} here", l.file)
                } else {
                    if sym {
                        taken.push(format!("taken as {}: {t}", l.text.split(',').next().unwrap_or_default()));
                    }
                    incoming.retain(|x| x.key() != l.key());
                    incoming.push(l);
                    continue;
                }
            }
        };
        let words = note_words(t);
        if !words.is_empty() {
            taken.push(format!("taken as a note, {why}: {}", truncate_chars(&words, 100)));
            notes.push(words);
        }
    }
    let files: Vec<String> = incoming.iter().map(|l| l.file.clone()).collect::<BTreeSet<_>>().into_iter().collect();
    let hashes = current_blobs(repo, &files)?;
    let warnings = line_warnings(repo, &incoming, &hashes);
    let globs = normalize_globs(&req.paths)?;

    let _g = v.lock()?;
    let existing = find(v, &project, &area);
    let created = existing.is_none();
    let mut m = match existing {
        Some(m) => m,
        None => {
            let mut n = Note::new(&v.fresh_id(), &area, MAP);
            n.project = Some(project.clone());
            n.set_field(AREA_KEY, Value::String(area.clone()));
            CodeMap { note: n, lines: vec![], notes: vec![], blobs: HashMap::new() }
        }
    };
    let mut r = PutReport { map: m.clone(), created, added: 0, replaced: 0, refreshed: 0, unchanged: 0, dropped: 0, notes_added: 0, taken, warnings };
    let before = m.lines.len() + m.notes.len();
    m.lines.retain(|l| !req.drop.iter().any(|d| d.trim() == l.key() || d.trim().trim_end_matches('/') == l.file));
    m.notes.retain(|n| !req.drop.iter().any(|d| note_words(d).eq_ignore_ascii_case(&note_words(n))));
    r.dropped = before - m.lines.len() - m.notes.len();
    for n in notes {
        if !m.notes.iter().any(|x| note_words(x).eq_ignore_ascii_case(&n)) {
            m.notes.push(format!("> {n}"));
            r.notes_added += 1;
        }
    }
    for l in incoming {
        let hash = hashes[&l.file].clone().unwrap_or_default();
        let key = l.key();
        match m.lines.iter_mut().find(|x| x.key() == key) {
            Some(old) if old.text == l.text && m.blobs.get(&key) == Some(&hash) => r.unchanged += 1,
            Some(old) if old.text == l.text => {
                r.refreshed += 1;
                *old = l;
            }
            Some(old) => {
                r.replaced += 1;
                *old = l;
            }
            None => {
                r.added += 1;
                m.lines.push(l);
            }
        }
        m.blobs.insert(key, hash);
    }
    m.lines.sort_by(|a, b| a.file.cmp(&b.file).then(a.first_line_no().cmp(&b.first_line_no())));

    let mut paths = m.note.path_globs();
    for g in globs {
        if !paths.contains(&g) {
            paths.push(g);
        }
    }
    if paths.is_empty() {
        paths = m.files();
    }
    let mut body: Vec<String> = m.notes.clone();
    if !body.is_empty() {
        body.push(String::new());
    }
    body.extend(m.lines.iter().map(|l| format!("- {}", l.text)));
    m.note.body = body.join("\n");
    m.note.set_field(crate::note::PATHS_KEY, Value::Sequence(paths.into_iter().map(Value::String).collect()));
    let mut blobs = Mapping::new();
    for l in &m.lines {
        if let Some(h) = m.blobs.get(&l.key()) {
            blobs.insert(Value::String(l.key()), Value::String(h.clone()));
        }
    }
    m.blobs.retain(|k, _| m.lines.iter().any(|l| l.key() == *k));
    m.note.set_field(BLOBS_KEY, Value::Mapping(blobs));
    let k = v.require_kind(MAP)?;
    k.validate(&mut m.note.extra)?;
    m.note.mark_verified();
    crate::session::stamp_write(&mut m.note);
    m.note.updated = now();
    let mut touched: Vec<PathBuf> = crate::ops::ensure_project(v, &project)?.into_iter().collect();
    touched.extend(v.ensure_kind_file(MAP)?);
    touched.push(v.write_note(&mut m.note)?);
    v.commit(&touched, &crate::ops::commit_msg(if created { "map add" } else { "map put" }, &m.note))?;
    r.map = m;
    Ok(r)
}

fn check_all(maps: &[CodeMap], repo: Option<&Path>) -> Vec<Vec<Fresh>> {
    let files: Vec<String> = maps.iter().flat_map(|m| m.files()).collect::<BTreeSet<_>>().into_iter().collect();
    let cur = repo.and_then(|r| current_blobs(r, &files).ok());
    let mut texts: HashMap<String, String> = HashMap::new();
    if let (Some(r), Some(c)) = (repo, &cur) {
        for f in maps.iter().flat_map(|m| m.changed_files(c)) {
            if let Ok(b) = std::fs::read(r.join(&f)) {
                texts.insert(f, String::from_utf8_lossy(&b).into_owned());
            }
        }
    }
    maps.iter().map(|m| m.states(cur.as_ref(), &texts)).collect()
}

pub fn get_text(v: &Vault, project: &str, area: &str, repo: Option<&Path>) -> Result<String> {
    let Some(m) = find(v, project, area) else {
        let names: Vec<String> = maps(v, project).iter().map(CodeMap::area).collect();
        bail!("no map for area '{area}' in {project}, maps: {}", if names.is_empty() { "none yet".into() } else { names.join(", ") });
    };
    let states = check_all(std::slice::from_ref(&m), repo).remove(0);
    Ok(m.render(&states))
}

pub fn list_text(v: &Vault, project: &str, repo: Option<&Path>) -> String {
    let all = maps(v, project);
    if all.is_empty() {
        return format!("no maps in {project} yet, `breadcrumb map put <area>` saves one");
    }
    let states = check_all(&all, repo);
    all.iter().zip(&states).map(|(m, s)| m.header(s) + "\n").collect()
}

fn path_target(t: &str, repo: Option<&Path>) -> String {
    let s = t.trim().replace('\\', "/");
    let b = s.as_bytes();
    let s = if s.starts_with('/') || (b.len() >= 2 && b[1] == b':') {
        repo.and_then(|r| rel_to_root(&s, &path_str(r))).unwrap_or(s)
    } else {
        s
    };
    s.trim_start_matches("./").trim_end_matches('/').to_string()
}

fn covers(target: &str, file: &str) -> bool {
    let (t, f) = (target.to_ascii_lowercase(), file.to_ascii_lowercase());
    f == t || f.starts_with(&format!("{t}/")) || (t.contains(['*', '?']) && glob_match(target, file))
}

/// A directory line also covers the files inside it.
fn line_covers(target: &str, l: &MapLine) -> bool {
    covers(target, &l.file) || (l.dir && covers(&l.file, target))
}

fn kind_rank(kind: &str) -> Option<usize> {
    ["gotcha", "decision", "feedback"].iter().position(|k| *k == kind)
}

fn note_text(n: &Note, mark: &str) -> String {
    let body: Vec<&str> = n.body.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    let body = one_line(&body.join(" "));
    let mark = if mark.is_empty() { String::new() } else { format!(" {mark}") };
    let s = if body.is_empty() { format!("{} {} {}{mark}", n.kind, n.id, n.title) } else { format!("{} {} {}{mark}: {body}", n.kind, n.id, n.title) };
    truncate_chars(&s, NOTE_MAX)
}

struct Item {
    class: u8,
    /// Position within its class and map, so maps take turns.
    rank: usize,
    section: usize,
    order: usize,
    text: String,
    /// Map area for a map line, note id for a note.
    of: String,
    is_note: bool,
}

/// One text for an agent about to work on `targets` (repo relative files, directories, globs or map areas): the
/// matching maps with stale lines marked, then the gotchas, decisions and feedback whose path globs match, cut to `budget` chars.
pub fn brief(v: &Vault, project: &str, targets: &[String], repo: Option<&Path>, budget: usize) -> Result<String> {
    if targets.iter().all(|t| t.trim().is_empty()) {
        bail!("brief needs files, directories or map areas");
    }
    let all = maps(v, project);
    let mut areas: Vec<usize> = vec![];
    let mut paths: Vec<String> = vec![];
    for t in targets.iter().filter(|t| !t.trim().is_empty()) {
        match all.iter().position(|m| same_area(&m.area(), t)) {
            Some(i) => areas.push(i),
            None => paths.push(path_target(t, repo)),
        }
    }
    let picked: Vec<usize> = (0..all.len())
        .filter(|i| {
            let m = &all[*i];
            areas.contains(i)
                || paths.iter().any(|p| m.lines.iter().any(|l| line_covers(p, l)) || m.note.path_globs().iter().any(|g| glob_match(g, p)))
        })
        .collect();
    let chosen: Vec<CodeMap> = picked.iter().map(|i| all[*i].clone()).collect();
    let states = check_all(&chosen, repo);

    let mut files: Vec<String> = paths.clone();
    for i in &areas {
        files.extend(all[*i].files());
    }
    let mut hits: Vec<(usize, usize, i64, String)> = crate::recall::path_index(v)
        .into_iter()
        .filter(|e| e.project.as_deref().is_none_or(|p| p == project) && e.holds())
        .filter_map(|e| {
            let rank = kind_rank(&e.kind)?;
            let n = files.iter().filter(|f| e.paths.iter().any(|g| glob_match(g, f))).count();
            (n > 0).then_some((rank, n, e.fresh, e.id))
        })
        .collect();
    hits.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(b.2.cmp(&a.2)));

    let mut items: Vec<Item> = vec![];
    for (si, (m, st)) in chosen.iter().zip(&states).enumerate() {
        let by_area = areas.iter().any(|i| all[*i].note.id == m.note.id);
        items.push(Item { class: 0, rank: 0, section: si, order: 0, text: m.header(st), of: m.area(), is_note: false });
        for (ni, t) in m.notes.iter().enumerate() {
            let class = if ni < FIRST_LINES { 1 } else { 3 };
            items.push(Item { class, rank: ni, section: si, order: ni + 1, text: t.clone(), of: m.area(), is_note: false });
        }
        let mut seen = [0usize; 8];
        for (li, (l, s)) in m.lines.iter().zip(st).enumerate() {
            let named = paths.iter().any(|p| line_covers(p, l));
            let stale = matches!(s, Fresh::Changed | Fresh::Gone);
            let class = match (named, by_area, stale) {
                (true, _, _) if seen[1] < FIRST_LINES => 1,
                (true, _, _) | (false, true, _) => 3,
                (false, false, false) => 5,
                (false, false, true) => 7,
            };
            let rank = seen[class as usize];
            seen[class as usize] += 1;
            items.push(Item { class, rank, section: si, order: m.notes.len() + li + 1, text: line_text(l, *s), of: m.area(), is_note: false });
        }
    }
    let notes_section = chosen.len();
    let mut found: HashMap<String, Note> = HashMap::new();
    for (o, (rank, _, _, id)) in hits.iter().enumerate() {
        let Ok(n) = v.get(id) else { continue };
        let class = [2, 4, 6][*rank];
        items.push(Item { class, rank: o, section: notes_section, order: o, text: note_text(&n, ""), of: n.id.clone(), is_note: true });
        found.insert(n.id.clone(), n);
    }

    let shown_targets = targets.iter().map(|t| one_line(t)).collect::<Vec<_>>().join(", ");
    let mut head = format!("brief for {shown_targets}: {} map{}, {} note{}", chosen.len(), if chosen.len() == 1 { "" } else { "s" }, hits.len(), if hits.len() == 1 { "" } else { "s" });
    if chosen.is_empty() {
        head.push_str("\nno map covers these yet, after you learn the area save it with `breadcrumb map put <area>` (map tool, action put)");
    }
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|i| (items[*i].class, items[*i].rank, items[*i].section, items[*i].order));
    let select = |items: &[Item], limit: usize| -> Vec<bool> {
        let mut used = head.len() + 1;
        let mut keep = vec![false; items.len()];
        for i in &order {
            let len = items[*i].text.len() + 1;
            if items[*i].class == 0 || used + len <= limit {
                keep[*i] = true;
                used += len;
            }
        }
        keep
    };
    let fit = |items: &[Item]| {
        let keep = select(items, budget.saturating_sub(NOTES_HEAD));
        if keep.contains(&false) { select(items, budget.saturating_sub(NOTES_HEAD + CUT_RESERVE)) } else { keep }
    };
    let mut keep = fit(&items);
    // Only notes that made the cut cost a git call, then the marks count against the budget too.
    let mut marked = false;
    for (it, _) in items.iter_mut().zip(&keep).filter(|(it, k)| it.is_note && **k) {
        if let (Some(dir), Some(n)) = (repo, found.get(&it.of))
            && let Some(d) = crate::drift::check(dir, n, crate::digest::GIT_LIMIT)
        {
            it.text = note_text(n, &d.mark());
            marked = true;
        }
    }
    if marked {
        keep = fit(&items);
    }
    let mut out = head;
    out.push('\n');
    let mut section = usize::MAX;
    for (i, it) in items.iter().enumerate() {
        if !keep[i] {
            continue;
        }
        if it.is_note && section != it.section {
            out.push_str("notes for these paths:\n");
        }
        section = it.section;
        out.push_str(&it.text);
        out.push('\n');
    }
    let cut_lines: Vec<&Item> = items.iter().zip(&keep).filter(|(it, k)| !**k && !it.is_note).map(|x| x.0).collect();
    let cut_notes: Vec<&str> = items.iter().zip(&keep).filter(|(it, k)| !**k && it.is_note).map(|x| x.0.of.as_str()).collect();
    if !cut_lines.is_empty() || !cut_notes.is_empty() {
        let mut parts = vec![];
        if !cut_lines.is_empty() {
            let mut per: Vec<(String, usize)> = vec![];
            for it in &cut_lines {
                match per.iter_mut().find(|(a, _)| *a == it.of) {
                    Some(x) => x.1 += 1,
                    None => per.push((it.of.clone(), 1)),
                }
            }
            let names: Vec<String> = per.iter().map(|(a, n)| format!("{n} of {a}")).collect();
            parts.push(format!("{} map lines ({}), map get shows them", cut_lines.len(), names.join(", ")));
        }
        if !cut_notes.is_empty() {
            let ids: Vec<&str> = cut_notes.iter().take(8).copied().collect();
            parts.push(format!("{} note{} ({}), get shows them", cut_notes.len(), if cut_notes.len() == 1 { "" } else { "s" }, ids.join(" ")));
        }
        out.push_str(&format!("cut to fit {budget} chars: {}\n", parts.join("; ")));
    }
    Ok(out)
}
