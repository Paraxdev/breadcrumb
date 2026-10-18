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

pub fn check(v: &Vault) -> Vec<Problem> {
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
    let kinds: HashMap<String, crate::kind::KindDef> = kinds.into_iter().map(|k| (k.name.clone(), k)).collect();
    let (notes, note_errors) = v.load_notes_report();
    for (p, e) in note_errors {
        err(v.rel(&p), None, "error", e);
    }
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
    out
}

pub fn text(problems: &[Problem], note_count: usize) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    for p in problems {
        let _ = writeln!(s, "{}: {}: {}", p.severity, p.path, p.message);
    }
    let errors = problems.iter().filter(|p| p.severity == "error").count();
    let warnings = problems.len() - errors;
    let _ = writeln!(s, "checked {note_count} notes: {errors} errors, {warnings} warnings");
    s
}
