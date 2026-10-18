mod common;

use breadcrumb::id;
use std::collections::HashSet;

#[test]
fn ids_are_8_chars_of_lowercase_crockford() {
    let mut seen = HashSet::new();
    for _ in 0..2000 {
        let i = id::new_id();
        assert_eq!(i.len(), 8);
        assert!(i.chars().all(|c| "0123456789abcdefghjkmnpqrstvwxyz".contains(c)), "{i}");
        seen.insert(i);
    }
    assert!(seen.len() > 1990);
}

#[test]
fn id_from_file_name_requires_a_valid_id() {
    assert_eq!(id::id_from_file_name("k3m9x2qa-some-title.md").as_deref(), Some("k3m9x2qa"));
    assert_eq!(id::id_from_file_name("k3m9x2qa.md").as_deref(), Some("k3m9x2qa"));
    assert_eq!(id::id_from_file_name("readme.md"), None);
    assert_eq!(id::id_from_file_name("illegal1-x.md"), None);
}

#[test]
fn prefix_resolution() {
    let c = |ids: &[&str]| ids.iter().map(|i| (i.to_string(), format!("label {i}"))).collect::<Vec<_>>();
    let cands = c(&["abcd1234", "abcd5678", "zzzz0000"]);
    assert_eq!(id::resolve_prefix("zzzz", &cands).unwrap(), "zzzz0000");
    assert_eq!(id::resolve_prefix("ABCD1", &cands).unwrap(), "abcd1234");
    assert_eq!(id::resolve_prefix("abcd5678", &cands).unwrap(), "abcd5678");
    let err = id::resolve_prefix("abcd", &cands).unwrap_err().to_string();
    assert!(err.contains("ambiguous") && err.contains("abcd1234") && err.contains("abcd5678"), "{err}");
    assert!(id::resolve_prefix("abc", &cands).unwrap_err().to_string().contains("too short"));
    assert!(id::resolve_prefix("yyyy", &cands).unwrap_err().to_string().contains("no note"));
}

#[test]
fn vault_lookup_by_prefix_and_stable_file_names() {
    let (_d, v) = common::vault();
    let n = common::add(&v, "Original title", "note", None, &[]);
    let path = n.path.clone().unwrap();
    assert_eq!(v.get(&n.id[..4]).unwrap().id, n.id);
    let u = breadcrumb::ops::update(&v, &n.id[..5], breadcrumb::ops::UpdateReq { title: Some("Renamed".into()), ..Default::default() }).unwrap();
    assert_eq!(u.title, "Renamed");
    assert_eq!(u.path.unwrap(), path, "file name must not change on title edit");
    assert!(path.file_name().unwrap().to_string_lossy().starts_with(&format!("{}-original-title", n.id)));
}
