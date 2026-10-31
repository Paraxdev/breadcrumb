use crate::kind::FieldType;
use crate::note::yaml_scalar_string;
use crate::vault::Vault;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize)]
pub struct Problem {
    pub path: String,
    pub id: Option<String>,
    pub severity: &'static str,
    pub message: String,
}

/// Backticked tokens that look like repo relative file paths: a slash and a file extension.
pub fn file_refs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for (_, _, t) in ref_spans(text) {
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// Marks a backticked path that is known to be gone, so doctor stops reporting it.
pub const GONE: &str = "(gone)";

/// Byte span of each backticked path mention, with the path normalized like `file_refs` reports it.
pub fn ref_spans(text: &str) -> Vec<(usize, usize, String)> {
    let re = regex::Regex::new(r"`([^`\s]+)`").expect("valid regex");
    let ext = regex::Regex::new(r"^[^/]*\.[A-Za-z0-9]{1,8}$").expect("valid regex");
    let line_no = regex::Regex::new(r"(:\d+)+$").expect("valid regex");
    let placeholder = regex::Regex::new(r"(^|[/._-])(v?X\.Y(\.Z)?|YYYY|NNN+)([/._-]|$)").expect("valid regex");
    let mut out = vec![];
    for c in re.captures_iter(text) {
        let whole = c.get(0).expect("match");
        if text[whole.end()..].starts_with(&format!(" {GONE}")) {
            continue;
        }
        let t = line_no.replace(&c[1], "").to_string();
        let t = t.strip_prefix("./").unwrap_or(&t).to_string();
        let b = t.as_bytes();
        let skip = !t.contains('/')
            || t.contains("://")
            || t.contains("..")
            || t.starts_with(['/', '~', '$', '-', '@'])
            || t.contains(['*', '?', '{', '}', '[', ']', '<', '>', '$', '=', '%', '\\'])
            || (b.len() >= 2 && b[1] == b':')
            || placeholder.is_match(&t)
            || !t.rsplit('/').next().is_some_and(|last| ext.is_match(last));
        if !skip {
            out.push((whole.start(), whole.end(), t));
        }
    }
    out
}

/// Paths a note mentions that exist in none of its project's checkouts on this machine.
pub fn missing_refs(n: &crate::note::Note, roots: &[std::path::PathBuf]) -> Vec<String> {
    if roots.is_empty() {
        return vec![];
    }
    let exists = |rel: &str| roots.iter().any(|r| r.join(rel).exists());
    let mut out: Vec<String> = file_refs(&n.body).into_iter().filter(|t| !exists(t)).collect();
    for g in n.path_globs() {
        let lit: Vec<&str> = g.split('/').take_while(|seg| !seg.contains(['*', '?'])).collect();
        // A glob without a slash matches the file name at any depth, so it has no fixed location to go missing.
        if !g.contains('/') {
            continue;
        }
        let lit = lit.join("/");
        if !lit.is_empty() && !exists(&lit) && !out.contains(&lit) {
            out.push(lit);
        }
    }
    out
}

fn ops_pref_unpinned(n: &crate::note::Note) -> bool {
    crate::ops::is_pref(n) && !n.pinned && !n.is_archived()
}

pub fn check(v: &Vault) -> Vec<Problem> {
    check_with(v, crate::importer::default_claude_dir().as_deref())
}

/// `claude_dir` is where imported Claude Code memory came from, for the duplicate index check.
pub fn check_with(v: &Vault, claude_dir: Option<&std::path::Path>) -> Vec<Problem> {
    let mut out = vec![];
    let mut err = |path: String, id: Option<String>, severity: &'static str, message: String| {
        out.push(Problem { path, id, severity, message });
    };
    let (kinds, kind_errors) = v.load_kinds_report();
    for (p, e) in kind_errors {
        err(v.rel(&p), None, "error", format!("invalid kind file: {e}"));
    }
    let (projects, project_errors) = v.load_projects_report();
    for (p, e) in project_errors {
        err(v.rel(&p), None, "error", format!("invalid project file: {e}"));
    }
    let slugs: HashSet<String> = projects.iter().map(|p| p.slug.clone()).collect();
    let roots: HashMap<String, Vec<std::path::PathBuf>> = projects
        .iter()
        .map(|p| (p.slug.clone(), p.paths.iter().map(std::path::PathBuf::from).filter(|d| d.is_dir()).collect()))
        .collect();
    let days = v.config.stale_days;
    let kinds: HashMap<String, crate::kind::KindDef> = kinds.into_iter().map(|k| (k.name.clone(), k)).collect();
    let (notes, note_errors) = v.load_notes_report();
    for (p, e) in note_errors {
        err(v.rel(&p), None, "error", e);
    }
    let lapsed = crate::lifecycle::Lapsed::of(&notes);
    let mut ids: HashMap<String, Vec<String>> = HashMap::new();
    for n in &notes {
        ids.entry(n.id.clone()).or_default().push(n.path.as_ref().map(|p| v.rel(p)).unwrap_or_default());
    }
    for n in &notes {
        let path = n.path.as_ref().map(|p| v.rel(p)).unwrap_or_default();
        let id = Some(n.id.clone());
        let file_id = n.path.as_ref().and_then(|p| crate::id::id_from_file_name(&p.file_name()?.to_string_lossy()));
        if let Some(fid) = &file_id
            && fid != &n.id {
                err(path.clone(), id.clone(), "error", format!("frontmatter id '{}' differs from file name id '{fid}'", n.id));
            }
        if !crate::id::is_valid_id(&n.id) {
            err(path.clone(), id.clone(), "warning", format!("id '{}' is not an 8 character base32 id", n.id));
        }
        if let Some(s) = n.slug()
            && notes.iter().any(|o| o.id != n.id && o.slug().as_deref() == Some(s.as_str())) {
                err(path.clone(), id.clone(), "warning", format!("slug '{s}' is used by several notes, rename one with `breadcrumb update --slug`"));
            }
        if let Some(paths) = ids.get(&n.id)
            && paths.len() > 1 && paths.first() == Some(&path) {
                err(path.clone(), id.clone(), "error", format!("duplicate id {} in: {}", n.id, paths.join(", ")));
            }
        for w in &n.warnings {
            let sev = if w.starts_with(crate::note::INVALID_YAML) { "error" } else { "warning" };
            err(path.clone(), id.clone(), sev, w.clone());
        }
        if n.status != "active" && n.status != "archived" {
            err(path.clone(), id.clone(), "warning", format!("unknown status '{}'", n.status));
        }
        let dir_project = v.project_of_path(n.path.as_deref().unwrap_or(std::path::Path::new("")));
        if dir_project.is_some() && dir_project != n.project {
            err(path.clone(), id.clone(), "warning", format!("project field {:?} does not match directory {:?}", n.project, dir_project));
        }
        if let Some(p) = &n.project
            && !slugs.contains(p) {
                err(path.clone(), id.clone(), "warning", format!("project '{p}' has no project.toml"));
            }
        for l in &n.links {
            if !ids.contains_key(&l.to) {
                err(path.clone(), id.clone(), "error", format!("broken link to '{}' (rel {})", l.to, l.rel));
            }
        }
        if crate::recall::is_stale(n, days) {
            let age = (crate::util::now() - n.fresh_at()).num_days();
            err(path.clone(), id.clone(), "warning", format!("not verified for {age} days, check it and run `breadcrumb verify {}`", n.id));
        }
        if !n.is_archived()
            && let Some(rs) = n.project.as_ref().and_then(|p| roots.get(p))
        {
            for m in missing_refs(n, rs) {
                err(path.clone(), id.clone(), "warning", format!("mentions `{m}`, which does not exist in the project checkout"));
            }
        }
        if let Some(crate::lifecycle::Expiry::Invalid(raw)) = crate::lifecycle::expiry(n) {
            err(path.clone(), id.clone(), "warning", format!("expires '{raw}' is not a date, an RFC3339 time or until:<id>, so it never expires"));
        }
        if let Some(e) = lapsed.expired(n).filter(|_| !n.is_archived()) {
            if crate::lifecycle::needs_mark(n) {
                err(path.clone(), id.clone(), "warning", format!("{} but still pinned or an accepted decision, `breadcrumb doctor --fix --yes` marks it", e.why));
            } else if e.when.is_none_or(|t| (crate::util::now() - t).num_days() < crate::lifecycle::RECENTLY_DAYS) {
                err(path.clone(), id.clone(), "info", format!("{}, hidden from the digest and ranked last in search", e.why));
            }
        }
        if ops_pref_unpinned(n) {
            err(path.clone(), id.clone(), "info", format!("unpinned {}, a new session sees only its title, `breadcrumb update {} --pin` if every session needs it", n.kind, n.id));
        }
        match kinds.get(&n.kind) {
            None => err(path.clone(), id.clone(), "error", format!("unknown kind '{}', define it with `breadcrumb kind define`", n.kind)),
            Some(k) => {
                for p in k.problems(&n.extra) {
                    err(path.clone(), id.clone(), "error", p);
                }
                for f in k.fields.iter().filter(|f| f.ty == FieldType::Ref) {
                    if let Some(r) = n.field(&f.name).and_then(yaml_scalar_string)
                        && !ids.contains_key(&r) {
                            err(path.clone(), id.clone(), "error", format!("field '{}' refers to missing note '{r}'", f.name));
                        }
                }
            }
        }
    }
    if let Some(cdir) = claude_dir {
        let dirs = crate::importer::memory_dirs(notes.iter().filter(|n| !n.is_archived()).filter_map(|n| n.source.as_deref()));
        for p in crate::importer::live_indexes(cdir, &dirs) {
            let lines = crate::util::read_text(&p).map(|t| t.lines().filter(|l| !l.trim().is_empty()).count()).unwrap_or(0);
            if lines > crate::importer::INDEX_MAX_LINES {
                out.push(Problem {
                    path: crate::util::path_str(&p),
                    id: None,
                    severity: "warning",
                    message: format!("{lines} line Claude memory index beside imported notes loads the same facts twice, run `breadcrumb import claude --retire-index`"),
                });
            }
        }
    }
    out
}

pub fn text(problems: &[Problem], note_count: usize) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    for p in problems {
        let _ = writeln!(s, "{}: {}: {}", p.severity, p.path, p.message);
    }
    let count = |sev: &str| problems.iter().filter(|p| p.severity == sev).count();
    let _ = write!(s, "checked {note_count} notes: {} errors, {} warnings", count("error"), count("warning"));
    let info = count("info");
    if info > 0 {
        let _ = write!(s, ", {info} info");
    }
    s.push('\n');
    s
}
