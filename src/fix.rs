//! `doctor --fix`: rewrites file paths in notes that moved in the project checkout, drops or marks the rest.

use crate::doctor::{GONE, ref_spans};
use crate::note::Note;
use crate::vault::Vault;
use anyhow::Result;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Change {
    pub id: String,
    pub path: String,
    /// Path glob entry when true, a backticked mention in the body otherwise.
    pub glob: bool,
    pub from: String,
    /// New path, None drops the glob or marks the mention as gone.
    pub to: Option<String>,
}

impl Change {
    pub fn line(&self) -> String {
        match (&self.to, self.glob) {
            (Some(t), true) => format!("{}: path glob {} -> {t}", self.id, self.from),
            (Some(t), false) => format!("{}: `{}` -> `{t}`", self.id, self.from),
            (None, true) => format!("{}: drop path glob {}", self.id, self.from),
            (None, false) => format!("{}: mark `{}` {GONE}", self.id, self.from),
        }
    }
}

const SKIP_DIRS: [&str; 6] = [".git", "target", "node_modules", "dist", "build", ".venv"];
const MAX_FILES: usize = 200_000;

/// What one checkout knows about moved files: git renames (old -> new, newest first) and files by name.
struct Checkout {
    root: PathBuf,
    renames: Vec<(String, String)>,
    by_name: HashMap<String, Vec<String>>,
    dirs_by_name: HashMap<String, Vec<String>>,
}

impl Checkout {
    fn load(root: &Path) -> Checkout {
        let mut renames = vec![];
        if crate::git::toplevel(root).is_some()
            && let Ok(log) = crate::git::ok(root, &["log", "-M", "--diff-filter=R", "--name-status", "--format=", "--relative"])
        {
            for l in log.lines() {
                let parts: Vec<&str> = l.split('\t').collect();
                if parts.len() == 3 && parts[0].starts_with('R') {
                    renames.push((parts[1].to_string(), parts[2].to_string()));
                }
            }
        }
        let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
        let mut dirs_by_name: HashMap<String, Vec<String>> = HashMap::new();
        let walk = walkdir::WalkDir::new(root)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !SKIP_DIRS.contains(&e.file_name().to_string_lossy().as_ref()))
            .flatten()
            .take(MAX_FILES);
        for e in walk {
            let Ok(rel) = e.path().strip_prefix(root) else { continue };
            let rel = crate::util::path_str(rel);
            if rel.is_empty() {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_lowercase();
            let map = if e.file_type().is_dir() { &mut dirs_by_name } else { &mut by_name };
            map.entry(name).or_default().push(rel);
        }
        Checkout { root: root.to_path_buf(), renames, by_name, dirs_by_name }
    }

    fn exists(&self, rel: &str) -> bool {
        self.root.join(rel).exists()
    }

    /// Follows renames of `old` forward until a path that exists, for files and for whole directories.
    fn renamed(&self, old: &str) -> Option<String> {
        let mut cur = old.to_string();
        for _ in 0..10 {
            let next = self.renames.iter().find_map(|(from, to)| {
                if *from == cur {
                    return Some(to.clone());
                }
                let rest = from.strip_prefix(&format!("{cur}/"))?;
                to.strip_suffix(&format!("/{rest}")).map(str::to_string)
            })?;
            if self.exists(&next) {
                return Some(next);
            }
            cur = next;
        }
        None
    }

    fn unique_named(&self, old: &str, dir: bool) -> Option<String> {
        let name = old.rsplit('/').next()?.to_lowercase();
        let map = if dir { &self.dirs_by_name } else { &self.by_name };
        match map.get(&name).map(Vec::as_slice) {
            Some([one]) => Some(one.clone()),
            _ => None,
        }
    }

    fn candidate(&self, old: &str) -> Option<String> {
        self.renamed(old).or_else(|| self.unique_named(old, !old.rsplit('/').next().is_some_and(|n| n.contains('.'))))
    }
}

/// Proposed changes for every active note whose project has a checkout on this machine.
pub fn plan(v: &Vault) -> Vec<Change> {
    let roots: HashMap<String, Vec<PathBuf>> = v
        .load_projects()
        .into_iter()
        .map(|p| (p.slug.clone(), p.paths.iter().map(PathBuf::from).filter(|d| d.is_dir()).collect::<Vec<_>>()))
        .filter(|(_, rs)| !rs.is_empty())
        .collect();
    let mut checkouts: HashMap<String, Checkout> = HashMap::new();
    let mut out = vec![];
    for n in v.load_notes() {
        if n.is_archived() {
            continue;
        }
        let Some(rs) = n.project.as_ref().and_then(|p| roots.get(p)) else { continue };
        let missing = crate::doctor::missing_refs(&n, rs);
        if missing.is_empty() {
            continue;
        }
        let co = checkouts.entry(n.project.clone().unwrap_or_default()).or_insert_with(|| Checkout::load(&rs[0]));
        let path = n.path.as_ref().map(|p| v.rel(p)).unwrap_or_default();
        let globs = n.path_globs();
        for m in missing {
            let to = co.candidate(&m);
            let glob = globs.iter().find(|g| **g == m || g.starts_with(&format!("{m}/")));
            if let Some(g) = glob {
                out.push(Change { id: n.id.clone(), path: path.clone(), glob: true, from: g.clone(), to: to.as_ref().map(|t| format!("{t}{}", &g[m.len()..])) });
            }
            if glob.is_none() || ref_spans(&n.body).iter().any(|(_, _, r)| *r == m) {
                out.push(Change { id: n.id.clone(), path: path.clone(), glob: false, from: m, to });
            }
        }
    }
    out
}

fn apply_to(n: &mut Note, changes: &[&Change]) {
    let mut globs = n.path_globs();
    let mut body = n.body.clone();
    for c in changes {
        if c.glob {
            match &c.to {
                Some(t) => globs.iter_mut().filter(|g| **g == c.from).for_each(|g| *g = t.clone()),
                None => globs.retain(|g| *g != c.from),
            }
            continue;
        }
        let mut next = String::new();
        let mut last = 0;
        for (start, end, norm) in ref_spans(&body) {
            if norm != c.from {
                continue;
            }
            next.push_str(&body[last..start]);
            match &c.to {
                Some(t) => next.push_str(&body[start..end].replacen(&c.from, t, 1)),
                None => {
                    next.push_str(&body[start..end]);
                    next.push(' ');
                    next.push_str(GONE);
                }
            }
            last = end;
        }
        next.push_str(&body[last..]);
        body = next;
    }
    n.body = body;
    let globs: Vec<serde_yaml::Value> = globs.into_iter().map(serde_yaml::Value::String).collect();
    if globs.is_empty() {
        n.unset_field(crate::note::PATHS_KEY);
    } else {
        n.set_field(crate::note::PATHS_KEY, serde_yaml::Value::Sequence(globs));
    }
}

/// Applies the changes under the vault lock in one commit. Returns how many notes changed.
pub fn apply(v: &Vault, changes: &[Change]) -> Result<usize> {
    if changes.is_empty() {
        return Ok(0);
    }
    let _g = v.lock()?;
    let mut ids: Vec<&str> = changes.iter().map(|c| c.id.as_str()).collect();
    ids.dedup();
    let mut touched = vec![];
    for id in ids {
        let mut n = v.get(id)?;
        let mine: Vec<&Change> = changes.iter().filter(|c| c.id == id).collect();
        let before = n.to_markdown();
        apply_to(&mut n, &mine);
        if n.to_markdown() != before {
            n.updated = crate::util::now();
            touched.push(v.write_note(&mut n)?);
        }
    }
    if !touched.is_empty() {
        v.commit(&touched, &format!("breadcrumb: doctor fix stale paths in {} notes", touched.len()))?;
    }
    Ok(touched.len())
}

pub fn text(changes: &[Change], applied: Option<usize>) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    for c in changes {
        let _ = writeln!(s, "{}", c.line());
    }
    let moved = changes.iter().filter(|c| c.to.is_some()).count();
    let summary = format!("{moved} rewritten, {} dropped or marked gone", changes.len() - moved);
    match applied {
        _ if changes.is_empty() => s.push_str("no stale paths to fix\n"),
        Some(n) => {
            let _ = writeln!(s, "fixed {n} notes: {summary}");
        }
        None => {
            let _ = writeln!(s, "dry run: {summary}, apply with --yes");
        }
    }
    s
}
