#![allow(dead_code)]

use breadcrumb::note::Note;
use breadcrumb::ops::{self, AddReq, FieldInput};
use breadcrumb::vault::Vault;
use std::path::Path;

pub fn vault() -> (tempfile::TempDir, Vault) {
    let dir = tempfile::tempdir().unwrap();
    let (v, _) = Vault::init(&dir.path().join("vault"), None).unwrap();
    (dir, v)
}

pub fn add(v: &Vault, title: &str, kind: &str, project: Option<&str>, fields: &[(&str, &str)]) -> Note {
    ops::add(
        v,
        AddReq {
            title: title.into(),
            kind: Some(kind.into()),
            project: project.map(Into::into),
            fields: fields.iter().map(|(k, val)| (k.to_string(), FieldInput::Raw(val.to_string()))).collect(),
            ..Default::default()
        },
    )
    .unwrap()
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    breadcrumb::git::ok(dir, args).unwrap()
}
