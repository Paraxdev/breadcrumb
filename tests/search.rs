use breadcrumb::note::Note;
use breadcrumb::search::{parse_query, search, tokenize};

fn note(id: &str, title: &str, tags: &[&str], body: &str) -> Note {
    let mut n = Note::new(id, title, "note");
    n.tags = tags.iter().map(|s| s.to_string()).collect();
    n.body = body.to_string();
    n
}

fn ids(notes: Vec<Note>, q: &str) -> Vec<String> {
    search(notes, q).hits.into_iter().map(|h| h.note.id).collect()
}

#[test]
fn tokenizer_and_query_parsing() {
    assert_eq!(tokenize("Hello, World! foo_bar 42"), vec!["hello", "world", "foo", "bar", "42"]);
    let q = parse_query("deploy \"Exact Phrase\" deploy KEY");
    assert_eq!(q.terms, vec!["deploy", "key"]);
    assert_eq!(q.phrases, vec!["exact phrase"]);
}

#[test]
fn title_beats_tag_beats_body() {
    let notes = vec![
        note("aaaa0001", "Unrelated", &[], "deploy mentioned once"),
        note("aaaa0002", "Deploy pipeline", &[], ""),
        note("aaaa0003", "Other", &["deploy"], ""),
    ];
    assert_eq!(ids(notes, "deploy"), vec!["aaaa0002", "aaaa0003", "aaaa0001"]);
}

#[test]
fn body_hits_are_capped_and_prefix_counts_half() {
    let notes = vec![
        note("bbbb0001", "x", &[], &"cache ".repeat(50)),
        note("bbbb0002", "Caches", &[], ""),
    ];
    let r = search(notes, "cache");
    let score = |id: &str| r.hits.iter().find(|h| h.note.id == id).unwrap().score;
    assert_eq!(score("bbbb0002"), 3.0, "prefix title hit is half of 6");
    assert_eq!(score("bbbb0001"), 3.0, "body hits cap at 3");
}

#[test]
fn id_prefix_scores_high() {
    let notes = vec![note("k3m9x2qa", "Something", &[], ""), note("zzzz0000", "k3m9 in title", &[], "")];
    assert_eq!(ids(notes, "k3m9")[0], "k3m9x2qa");
}

#[test]
fn field_values_are_searched() {
    let mut n = note("cccc0001", "Server", &[], "");
    n.set_field("hostname", serde_yaml::Value::String("omv.local".into()));
    let r = search(vec![n, note("cccc0002", "Other", &[], "")], "omv");
    assert_eq!(r.hits.len(), 1);
    assert_eq!(r.hits[0].score, 2.0);
}

#[test]
fn and_semantics_with_or_fallback() {
    let notes = || vec![note("dddd0001", "Rust build", &[], ""), note("dddd0002", "Rust tests", &[], ""), note("dddd0003", "Python build", &[], "")];
    let r = search(notes(), "rust build");
    assert!(!r.fell_back_to_or);
    assert_eq!(r.hits.len(), 1);
    assert_eq!(r.hits[0].note.id, "dddd0001");
    let r = search(notes(), "rust golang");
    assert!(r.fell_back_to_or);
    assert_eq!(r.hits.len(), 2);
    assert!(search(notes(), "nothing here").hits.is_empty());
}

#[test]
fn phrases_must_appear_and_snippet_is_best_line() {
    let notes = vec![
        note("eeee0001", "Notes", &[], "first line\nthe build cache is stale here\nlast"),
        note("eeee0002", "Notes", &[], "cache and build but apart"),
    ];
    let r = search(notes, "\"build cache\"");
    assert_eq!(r.hits.len(), 1);
    assert_eq!(r.hits[0].note.id, "eeee0001");
    assert_eq!(r.hits[0].snippet, "the build cache is stale here");
}
