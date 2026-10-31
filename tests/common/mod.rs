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

/// A process to stand behind a test session, each session needs its own pid since that is what identifies it.
pub struct Sleeper(std::process::Child);

impl Sleeper {
    pub fn start() -> Sleeper {
        let mut c = if cfg!(windows) {
            let mut c = std::process::Command::new("ping");
            c.args(["-n", "300", "127.0.0.1"]);
            c
        } else {
            let mut c = std::process::Command::new("sleep");
            c.arg("300");
            c
        };
        c.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        Sleeper(c.spawn().unwrap())
    }

    pub fn pid(&self) -> u32 {
        self.0.id()
    }

    pub fn stop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        self.stop();
    }
}
