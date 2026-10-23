mod common;

use breadcrumb::ops::{self, AddReq, FieldInput, Scope};
use std::process::{Command, Stdio};

fn fact(v: &breadcrumb::vault::Vault, title: &str, fields: &[(&str, &str)]) -> breadcrumb::note::Note {
    ops::add(
        v,
        AddReq {
            title: title.into(),
            kind: Some("env".into()),
            project: Some("app".into()),
            fields: fields.iter().map(|(k, x)| (k.to_string(), FieldInput::Raw(x.to_string()))).collect(),
            ..Default::default()
        },
    )
    .unwrap()
}

#[test]
fn an_older_vault_gets_the_env_kind_on_first_use() {
    let (_d, v) = common::vault();
    let kind_file = v.root.join("kinds").join("env.toml");
    std::fs::remove_file(&kind_file).unwrap();
    common::git(&v.root, &["commit", "-qam", "vault from before env"]);
    let commits = breadcrumb::git::commit_count(&v.root);
    let n = fact(&v, "Screen recordings", &[("location", "C:/Users/me/Videos"), ("access", "read-only")]);
    assert!(kind_file.exists());
    assert_eq!(breadcrumb::git::commit_count(&v.root), commits + 1);
    assert_eq!(common::git(&v.root, &["status", "--porcelain"]), "", "the kind file is committed with the note");
    assert_eq!(n.field("machine").and_then(|x| x.as_str()), Some(breadcrumb::util::machine_name().as_str()));
    assert!(breadcrumb::doctor::check(&v).iter().all(|p| p.severity != "error"), "{:?}", breadcrumb::doctor::check(&v));
    assert!(ops::add(&v, AddReq { title: "x".into(), kind: Some("env".into()), fields: vec![("access".into(), FieldInput::Raw("sometimes".into()))], ..Default::default() }).is_err());
}

#[test]
fn env_facts_are_per_machine_sorted_by_path_and_stay_out_of_the_digest() {
    let (_d, v) = common::vault();
    let here = breadcrumb::util::machine_name();
    let logs = fact(&v, "App log", &[("location", "C:/Users/me/AppData/Local/app/logs")]);
    let vids = fact(&v, "Screen recordings", &[("location", "C:/Users/me/Videos"), ("access", "read-only")]);
    let other = fact(&v, "NAS share", &[("location", "//nas/share"), ("machine", "mini-pc")]);
    let anywhere = fact(&v, "No ffmpeg", &[("location", "ffmpeg")]);
    let mut n = v.get(&anywhere.id).unwrap();
    n.unset_field("machine");
    v.write_note(&mut n).unwrap();

    let scope = Scope::Project { slug: "app".into(), with_global: true };
    let ids = |m: Option<&str>| ops::env_facts(&v, &scope, m).unwrap().into_iter().map(|n| n.id).collect::<Vec<_>>();
    assert_eq!(ids(Some(&here)), vec![logs.id.clone(), vids.id.clone(), anywhere.id.clone()]);
    assert_eq!(ids(None).len(), 4);
    let table = breadcrumb::format::env_table(&ops::env_facts(&v, &scope, Some(&here)).unwrap(), Some(&here));
    assert!(table.contains(&format!("{}  C:/Users/me/Videos", vids.id)) && table.contains("Screen recordings  [read-only]"), "{table}");
    assert!(breadcrumb::format::env_table(&ops::env_facts(&v, &scope, None).unwrap(), None).contains("NAS share  (mini-pc)"));
    assert!(breadcrumb::format::row(&vids, Some("app"), false).ends_with("Screen recordings: C:/Users/me/Videos"));

    let ctx = ops::context(&v, Some("app"), 15).unwrap();
    assert!(!ctx.contains(&vids.id) && !ctx.contains(&other.id), "{ctx}");
    assert!(ctx.contains(&format!("Machine facts: 3 for {here}, `breadcrumb env` or list kind env\n")), "{ctx}");
}

#[test]
fn breadcrumb_env_lists_this_machine_unless_asked_for_all() {
    let (_d, v) = common::vault();
    fact(&v, "Build cache", &[("location", "D:/cache"), ("machine", "box1")]);
    fact(&v, "Other cache", &[("location", "E:/cache"), ("machine", "box2")]);
    let run = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_breadcrumb"))
            .arg("--vault")
            .arg(&v.root)
            .args(args)
            .env_remove("BREADCRUMB_HOME")
            .env("BREADCRUMB_MACHINE", "BOX1")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    let out = run(&["env", "-p", "app"]);
    assert!(out.contains("D:/cache  Build cache") && !out.contains("E:/cache"), "{out}");
    let out = run(&["env", "-p", "app", "--all-machines"]);
    assert!(out.contains("(box1)") && out.contains("(box2)"), "{out}");
    assert!(run(&["env", "--global"]).starts_with("no machine facts for box1 yet"));
}

#[test]
fn kinds_list_built_in_kinds_the_vault_has_no_file_for() {
    let (_d, v) = common::vault();
    std::fs::remove_file(v.root.join("kinds").join("env.toml")).unwrap();
    assert!(v.load_kinds().iter().any(|k| k.name == "env"));
    assert!(breadcrumb::format::kinds_compact(&v.load_kinds()).contains("env: A fact about one machine"));
}
