use breadcrumb::note::Note;
use serde_yaml::Value;

const SAMPLE: &str = "---
id: k3m9x2qa
title: Short title
kind: note
project: webapp
tags: [deploy, gotcha]
links: [{to: ab12cd34, rel: relates}, {to: zz99yy88}]
status: active
pinned: true
created: 2026-10-18T12:00:00Z
updated: 2026-10-18T13:00:00Z
source: chat 42
custom_number: 42
custom_nested:
  a: [1, 2]
  b: {c: hello}
zebra: last
---
# Heading

Body line one.
";

#[test]
fn round_trip_keeps_known_and_extra_fields() {
    let n = Note::parse(SAMPLE, Some("k3m9x2qa-short-title.md")).unwrap();
    assert_eq!(n.id, "k3m9x2qa");
    assert_eq!(n.title, "Short title");
    assert_eq!(n.project.as_deref(), Some("webapp"));
    assert_eq!(n.tags, vec!["deploy", "gotcha"]);
    assert_eq!(n.links.len(), 2);
    assert_eq!(n.links[1].rel, "relates");
    assert!(n.pinned);
    assert_eq!(n.source.as_deref(), Some("chat 42"));
    assert_eq!(n.field("custom_number"), Some(&Value::Number(42.into())));
    assert!(matches!(n.field("custom_nested"), Some(Value::Mapping(_))));
    let keys: Vec<String> = n.extra.keys().filter_map(|k| k.as_str().map(str::to_string)).collect();
    assert_eq!(keys, vec!["custom_number", "custom_nested", "zebra"]);

    let md = n.to_markdown();
    assert!(!md.contains('\r'));
    let again = Note::parse(&md, Some("k3m9x2qa-short-title.md")).unwrap();
    assert_eq!(again.extra, n.extra);
    assert_eq!(again.links, n.links);
    assert_eq!(again.created, n.created);
    assert_eq!(again.updated, n.updated);
    assert_eq!(again.body, n.body);
    assert_eq!(again.to_markdown(), md, "serialization must be stable");
}

#[test]
fn crlf_and_bom_input_parse_like_lf() {
    let crlf = format!("\u{feff}{}", SAMPLE.replace('\n', "\r\n"));
    let a = Note::parse(&crlf, None).unwrap();
    let b = Note::parse(SAMPLE, None).unwrap();
    assert_eq!(a.title, b.title);
    assert_eq!(a.extra, b.extra);
    assert_eq!(a.body, b.body);
    assert!(!a.body.contains('\r'));
    assert_eq!(a.to_markdown(), b.to_markdown());
}

#[test]
fn missing_frontmatter_uses_heading_or_file_name() {
    let n = Note::parse("# Deploy steps\r\n\r\nrun it\r\n", Some("whatever.md")).unwrap();
    assert!(!n.had_frontmatter);
    assert_eq!(n.title, "Deploy steps");
    assert_eq!(n.body, "# Deploy steps\n\nrun it");

    let n = Note::parse("just text", Some("k3m9x2qa-my-cool_note.md")).unwrap();
    assert_eq!(n.id, "k3m9x2qa");
    assert_eq!(n.title, "my cool note");
    assert_eq!(n.kind, "note");
}

#[test]
fn sloppy_yaml_is_read_leniently_and_dashes_in_body_are_kept() {
    let n = Note::parse("---\nname: x\ndescription: \"Body, then face\" click policy, more\ntags: [a, b]\nmetadata:\n  type: project\n---\nbody", None).unwrap();
    assert_eq!(n.field("description").and_then(|v| v.as_str()), Some("\"Body, then face\" click policy, more"));
    assert_eq!(n.tags, vec!["a", "b"], "lines that parse on their own keep their types");
    assert!(matches!(n.field("metadata"), Some(Value::Mapping(_))));
    assert!(n.warnings.iter().any(|w| w.starts_with(breadcrumb::note::INVALID_YAML)));
    assert!(Note::parse("---\n\t- [\n  : : {\n---\nbody", None).is_err());
    let n = Note::parse("---\ntitle: t\n---\nabove\n\n---\n\nbelow", None).unwrap();
    assert_eq!(n.body, "above\n\n---\n\nbelow");
}

#[test]
fn json_output_contains_frontmatter_and_body() {
    let n = Note::parse(SAMPLE, None).unwrap();
    let j = n.to_json();
    assert_eq!(j["id"], "k3m9x2qa");
    assert_eq!(j["custom_number"], 42);
    assert_eq!(j["zebra"], "last");
    assert!(j["body"].as_str().unwrap().contains("Body line one."));
}
