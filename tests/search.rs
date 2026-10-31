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

#[test]
fn more_matched_terms_rank_first_and_one_word_matches_drop_out_of_long_queries() {
    let notes = vec![
        note("ffff0001", "Drop handling", &[], "an hdr file dropped on the window is ignored on linux"),
        note("ffff0002", "HDRI size limit", &[], ""),
        note("ffff0003", "Linux packaging", &[], ""),
        note("ffff0004", "Window title", &[], ""),
    ];
    let r = search(notes, "hdr dropped window ignored linux size");
    assert!(r.fell_back_to_or && r.terms == 6);
    let ids: Vec<&str> = r.hits.iter().map(|h| h.note.id.as_str()).collect();
    assert_eq!(ids, vec!["ffff0001"], "5 of 6 terms keeps 4 of 6 and up, one word matches are noise: {ids:?}");

    let r = search(vec![note("gggg0001", "Parser cache", &[], ""), note("gggg0002", "Parser", &[], "")], "parser cache");
    assert_eq!(r.hits.len(), 1, "a note matching every term hides partial ones");
}

#[test]
fn kind_and_recency_break_ties_between_equal_matches() {
    let mut log = note("hhhh0001", "Shader cache", &[], "");
    log.kind = "handoff".into();
    let mut rule = note("hhhh0002", "Shader cache", &[], "");
    rule.kind = "decision".into();
    let mut old = note("hhhh0003", "Shader cache", &[], "");
    old.updated = breadcrumb::util::now() - chrono::Duration::days(400);
    let new = note("hhhh0004", "Shader cache", &[], "");
    let ids: Vec<String> = search(vec![log, old, new, rule], "shader cache").hits.into_iter().map(|h| h.note.id).collect();
    assert_eq!(ids, vec!["hhhh0002", "hhhh0004", "hhhh0003", "hhhh0001"]);
}

#[test]
fn an_identifier_ranks_whole_above_its_split_terms() {
    let mut rich = note("kkkk0001", "Import loose faces", &["import", "loose", "faces"], "import loose faces import loose faces");
    rich.updated = breadcrumb::util::now();
    let literal = note("kkkk0002", "Some task", &[], "first line\nimport_loose_faces can crash on rerun\nlast");
    let r = search(vec![rich, literal], "import_loose_faces");
    assert_eq!(r.hits[0].note.id, "kkkk0002");
    assert!(r.hits[0].exact_whole && !r.hits[1].exact_whole);
    assert_eq!(r.hits[1].matched, 3, "it still matches split");
    assert_eq!(r.hits[0].snippet, "import_loose_faces can crash on rerun", "the snippet shows the literal hit");
}

#[test]
fn a_literal_word_needs_its_edges_free() {
    let r = search(vec![note("llll0001", "import_loose_faces_v2", &[], ""), note("llll0002", "import_loose_faces.", &[], "")], "import_loose_faces");
    assert_eq!(r.hits[0].note.id, "llll0002");
}

#[test]
fn an_exact_token_ranks_above_a_prefix_whatever_the_score() {
    let mut prefix = note("mmmm0001", "Blending modes", &["blending"], "blending blending blending");
    prefix.kind = "decision".into();
    let exact = note("mmmm0002", "Other", &[], "one blend somewhere");
    let r = search(vec![prefix, exact], "blend");
    assert_eq!(r.hits[0].note.id, "mmmm0002");
}

#[test]
fn an_error_string_ranks_the_note_holding_it_first_across_line_breaks() {
    let scattered = note("nnnn0001", "Cannot borrow, mutable as", &["borrow", "mutable", "cannot"], "as cannot mutable borrow");
    let literal = note("nnnn0002", "Other", &[], "the build said\nerror: cannot borrow\nas mutable, twice");
    let r = search(vec![scattered, literal], "cannot borrow as mutable:");
    assert_eq!(r.hits[0].note.id, "nnnn0002");
    assert!(r.hits[0].exact_whole);
}

#[test]
fn a_literal_identifier_survives_the_partial_match_floor() {
    let notes = vec![
        note("oooo0001", "Crash when sectioning a blend at the fallback", &[], "crash sectioning blend fallback"),
        note("oooo0002", "Task", &[], "import_loose_faces"),
    ];
    let r = search(notes, "import_loose_faces crash sectioning blend fallback");
    assert!(r.hits.iter().any(|h| h.note.id == "oooo0002"), "{:?}", r.hits.iter().map(|h| &h.note.id).collect::<Vec<_>>());
    assert_eq!(r.hits[0].note.id, "oooo0001", "four literal words beat one");
}

#[test]
fn an_id_search_puts_the_note_itself_first() {
    let target = note("vb39tsfk", "Round of reviews", &[], "");
    let mention = note("pppp0001", "From the round vb39tsfk", &["vb39tsfk"], "vb39tsfk vb39tsfk vb39tsfk");
    let r = search(vec![mention, target], "vb39tsfk");
    assert_eq!(r.hits[0].note.id, "vb39tsfk");
}

#[test]
fn hits_name_the_terms_they_matched() {
    let notes = vec![note("qqqq0001", "Parser cache", &[], ""), note("qqqq0002", "Parser", &[], "with parser_cache in it"), note("qqqq0003", "Parser only", &[], "")];
    let r = search(notes, "parser golang");
    assert_eq!(r.hits[0].matched_terms, vec!["parser"]);
    let text = breadcrumb::format::search_rows(&r, None);
    assert!(text.contains("(1/2 terms: parser)"), "{text}");
    let r = search(vec![note("rrrr0001", "Other", &[], "the parser cache is stale"), note("ssss0001", "Other", &[], "the cache of the parser")], "parser cache");
    assert_eq!(breadcrumb::format::search_rows(&r, None).matches("(exact)").count(), 1, "only the literal one says so");
    let r = search(vec![note("rrrr0001", "Other", &[], "the parser cache is stale")], "parser cache");
    assert!(!breadcrumb::format::search_rows(&r, None).contains("(exact)"), "nothing to tell it from");
    let r = search(vec![note("tttt0001", "Other", &[], "the parser")], "parser");
    assert!(!breadcrumb::format::search_text(&r, "x").contains("(exact)"), "one word needs no label");
}

#[test]
fn the_display_form_of_an_id_finds_the_note_itself() {
    let target = note("wc579n42", "Session stamps", &[], "");
    let mention = note("uuuu0001", "Epic", &["breadcrumb"], "see BREADCRUMB-wc579n42");
    let r = search(vec![mention, target], "BREADCRUMB-wc579n42");
    assert_eq!(r.hits[0].note.id, "wc579n42");
    assert_eq!(r.hits.len(), 2);
}
