mod common;

use breadcrumb::ops::{self, AddReq, Scope};
use std::process::{Command, Stdio};

fn add(v: &breadcrumb::vault::Vault, title: &str, kind: &str, tags: &[&str]) -> breadcrumb::note::Note {
    ops::add(v, AddReq { title: title.into(), kind: Some(kind.into()), project: Some("app".into()), tags: tags.iter().map(|t| t.to_string()).collect(), ..Default::default() }).unwrap()
}

fn similar(v: &breadcrumb::vault::Vault, n: &breadcrumb::note::Note) -> Vec<String> {
    ops::similar(v, n).into_iter().map(|x| x.id).collect()
}

#[test]
fn similar_needs_shared_title_words_and_tags_help() {
    let (_d, v) = common::vault();
    let limits = add(&v, "HDRI size limits on Linux", "gotcha", &["render"]);
    let other = add(&v, "Linux packaging needs a desktop file", "note", &[]);
    let n = add(&v, "HDRI limit for big files", "note", &[]);
    assert_eq!(similar(&v, &n), vec![limits.id.clone()], "limit matches limits, hdri is shared");
    let n = add(&v, "Size of HDRI previews in the render tab", "note", &["render"]);
    assert_eq!(similar(&v, &n), vec![limits.id.clone()], "a third of the words plus a shared tag");
    let n = add(&v, "Size of HDRI previews in the export tab", "note", &[]);
    assert!(!similar(&v, &n).contains(&limits.id), "the same words without a tag stay below half");
    let n = add(&v, "Linux release notes", "note", &[]);
    assert!(similar(&v, &n).is_empty(), "one shared word: {:?}", similar(&v, &n));
    let _ = other;
}

#[test]
fn similar_skips_handoffs_lapsed_and_archived_notes_and_keeps_two() {
    let (_d, v) = common::vault();
    let old = add(&v, "Release checklist for the beta", "note", &[]);
    ops::set_status(&v, &old.id, "archived").unwrap();
    let replaced = add(&v, "Release checklist for the beta channel", "decision", &[]);
    let newer = add(&v, "Release checklist", "decision", &[]);
    ops::link(&v, &newer.id, &replaced.id, Some("supersedes"), false).unwrap();
    for i in 0..3 {
        add(&v, &format!("Release checklist for the beta {i}"), "note", &[]);
    }
    let n = add(&v, "Release checklist for the beta", "note", &[]);
    let found = similar(&v, &n);
    assert_eq!(found.len(), 2);
    assert!(!found.contains(&old.id) && !found.contains(&replaced.id), "{found:?}");
    let h1 = add(&v, "Handoff 2026-10-22", "handoff", &[]);
    let h2 = add(&v, "Handoff 2026-10-23", "handoff", &[]);
    assert!(ops::similar(&v, &h2).is_empty() && !similar(&v, &add(&v, "Handoff notes for 2026-10-22", "note", &[])).contains(&h1.id));
}

#[test]
fn cli_add_names_possible_duplicates_and_search_is_a_short_list() {
    let (_d, v) = common::vault();
    let first = add(&v, "Shared cargo target lives on D", "note", &[]);
    let o = Command::new(env!("CARGO_BIN_EXE_breadcrumb"))
        .arg("--vault")
        .arg(&v.root)
        .args(["add", "-p", "app", "-t", "Shared cargo target on drive D"])
        .env_remove("BREADCRUMB_HOME")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success() && out.contains(&format!("possible duplicates, update one instead if it holds the same fact: {} \"Shared cargo target lives on D\"", first.id)), "{out}");

    for i in 0..8 {
        add(&v, &format!("Cargo note {i}"), "note", &[]);
    }
    let r = ops::search(&v, "cargo", &Scope::All, &ops::Filter::default()).unwrap();
    assert_eq!(r.hits.len(), ops::DEFAULT_SEARCH_LIMIT);
    assert!(breadcrumb::format::search_rows(&r, Some("app")).ends_with("5 more, refine the query or narrow with filters\n"));
    let r = ops::search(&v, "cargo target drive windows", &Scope::All, &ops::Filter::default()).unwrap();
    let rows = breadcrumb::format::search_rows(&r, Some("app"));
    assert!(rows.starts_with(&format!("{} note Shared cargo target on drive D (3/4 terms: cargo, target, drive)", r.hits[0].note.handle())), "{rows}");
    assert!(rows.contains(&format!("{} note Shared cargo target lives on D (2/4 terms: cargo, target)", first.handle())) && !rows.contains("Cargo note"), "{rows}");
}
