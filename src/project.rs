use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub remotes: Vec<String>,
    #[serde(default = "crate::util::now")]
    pub created: DateTime<Utc>,
}

impl Project {
    pub fn new(slug: &str, name: Option<&str>) -> Project {
        Project {
            name: name.unwrap_or(slug).to_string(),
            slug: slug.to_string(),
            description: String::new(),
            paths: vec![],
            remotes: vec![],
            created: crate::util::now(),
        }
    }

    /// Adds a path if no equivalent one is recorded yet. Returns true when added.
    pub fn add_path(&mut self, p: &str) -> bool {
        let n = normalize_path(p);
        if n.is_empty() || self.paths.iter().any(|x| normalize_path(x) == n) {
            return false;
        }
        self.paths.push(p.replace('\\', "/").trim_end_matches('/').to_string());
        true
    }

    pub fn add_remote(&mut self, url: &str) -> bool {
        let n = normalize_remote(url);
        if n.is_empty() || self.remotes.iter().any(|x| normalize_remote(x).eq_ignore_ascii_case(&n)) {
            return false;
        }
        self.remotes.push(n);
        true
    }
}

/// Normalizes git remote URLs so ssh and https forms compare equal: `host/owner/repo`.
pub fn normalize_remote(url: &str) -> String {
    let mut u = url.trim().trim_end_matches('/').to_string();
    if let Some(s) = u.strip_suffix(".git") {
        u = s.to_string();
    }
    let (host, path) = if let Some((_, rest)) = u.split_once("://") {
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
        (host.to_string(), path.to_string())
    } else if let Some((left, path)) = scp_split(&u) {
        let host = left.rsplit_once('@').map(|(_, h)| h).unwrap_or(left);
        (host.to_string(), path.to_string())
    } else {
        return u.replace('\\', "/");
    };
    let host = host.to_ascii_lowercase();
    let path = path.trim_matches('/');
    if path.is_empty() { host } else { format!("{host}/{path}") }
}

pub fn is_scp_url(u: &str) -> bool {
    scp_split(u).is_some()
}

/// Splits scp-like `user@host:path`, rejecting Windows drive paths such as `C:/x`.
fn scp_split(u: &str) -> Option<(&str, &str)> {
    let (left, right) = u.split_once(':')?;
    if left.is_empty() || left.contains('/') || left.contains('\\') || left.len() == 1 {
        return None;
    }
    Some((left, right))
}

pub fn normalize_path(p: &str) -> String {
    let mut s = crate::util::strip_verbatim(&p.trim().replace('\\', "/"));
    while s.len() > 1 && s.ends_with('/') && !s.ends_with(":/") {
        s.pop();
    }
    if cfg!(windows) { s.to_lowercase() } else { s }
}

pub fn path_has_prefix(path: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return false;
    }
    path == prefix || (path.starts_with(prefix) && (prefix.ends_with('/') || path[prefix.len()..].starts_with('/')))
}

/// Detects the project for `cwd`: git remote match first, then the longest path prefix.
pub fn detect(projects: &[Project], cwd_remotes: &[String], cwd: &Path) -> Option<String> {
    if !cwd_remotes.is_empty() {
        let wanted: Vec<String> = cwd_remotes.iter().map(|r| normalize_remote(r).to_ascii_lowercase()).collect();
        for p in projects {
            if p.remotes.iter().any(|r| wanted.contains(&normalize_remote(r).to_ascii_lowercase())) {
                return Some(p.slug.clone());
            }
        }
    }
    let cwd_n = normalize_path(&cwd.to_string_lossy());
    let mut best: Option<(usize, &Project)> = None;
    for p in projects {
        for path in &p.paths {
            let pn = normalize_path(path);
            if path_has_prefix(&cwd_n, &pn) && best.is_none_or(|(len, _)| pn.len() > len) {
                best = Some((pn.len(), p));
            }
        }
    }
    best.map(|(_, p)| p.slug.clone())
}
