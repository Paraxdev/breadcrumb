mod common;

use breadcrumb::importer::{self, ClaudeOpts, Decoded, FilesOpts, decode_claude_dir_name, encode_claude_path};
use serde_yaml::Value;
use std::fs;
use std::path::{Path, PathBuf};

fn slug(d: &Decoded) -> Option<String> {
    match d {
        Decoded::Global => None,
        Decoded::Project { slug, .. } => Some(slug.clone()),
    }
}

#[test]
fn decode_claude_directory_names() {
    let home = Path::new(r"C:\Users\alice");
    let claude = Path::new(r"C:\Users\alice\.claude");
    assert_eq!(decode_claude_dir_name("C--Users-alice", Some(home), Some(claude)), Decoded::Global);
    assert_eq!(decode_claude_dir_name("c--Users-alice", Some(home), Some(claude)), Decoded::Global);
    assert_eq!(decode_claude_dir_name("C--Users-alice--claude-plugins-x", Some(home), Some(claude)), Decoded::Global);
    assert_eq!(slug(&decode_claude_dir_name("D--dev-webapp", Some(home), Some(claude))).as_deref(), Some("webapp"));
    assert_eq!(slug(&decode_claude_dir_name("c--Users-alice-Downloads-Mods-ModA", Some(home), Some(claude))).as_deref(), Some("moda"));
    assert_eq!(slug(&decode_claude_dir_name("--nas-share-Profiles-GameSrv", Some(home), Some(claude))).as_deref(), Some("gamesrv"));
}

#[test]
fn decode_reconstructs_existing_paths_with_dashes_and_dots() {
    let d = tempfile::tempdir().unwrap();
    let real = d.path().join("my-proj").join("Sub.Dir");
    fs::create_dir_all(&real).unwrap();
    fs::create_dir_all(d.path().join("my")).unwrap();
    let canon = fs::canonicalize(&real).unwrap();
    let s = breadcrumb::util::path_str(&canon);
    let encoded = encode_claude_path(&s);
    match decode_claude_dir_name(&encoded, None, None) {
        Decoded::Project { slug, path: Some(p), .. } => {
            assert_eq!(slug, "sub-dir");
            assert_eq!(fs::canonicalize(p).unwrap(), canon);
        }
        other => panic!("expected reconstructed path for {encoded}, got {other:?}"),
    }
}

fn write(p: &Path, s: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, s).unwrap();
}

fn fake_claude(root: &Path) -> PathBuf {
    let c = root.join("dot-claude");
    let mem = c.join("projects").join("D--zz-nonexistent-webapp").join("memory");
    write(&mem.join("MEMORY.md"), "- index file, must be skipped\n");
    write(
        &mem.join("feedback_style.md"),
        "---\r\nname: No em dashes\r\ndescription: User wants commas instead of dashes\r\nmetadata:\r\n  type: feedback\r\n  extra: kept\r\n---\r\nUse commas.\r\n",
    );
    write(&mem.join("user_role.md"), "---\nname: Role\ndescription: Senior dev\ntype: user\n---\nWorks on Windows.\n");
    write(&mem.join("ref.md"), "---\ndescription: Dashboards live here\nmetadata: {type: reference}\n---\nhttp://grafana\n");
    write(&mem.join("plain.md"), "# Just a heading\n\nno frontmatter\n");
    write(&mem.join("sloppy.md"), "---\nname: sloppy\ndescription: \"Quoted\" then more: text\nmetadata:\n  type: project\n---\nstill imported\n");
    let home = dirs::home_dir().unwrap();
    let home_dir_name = encode_claude_path(&home.to_string_lossy());
    write(&c.join("projects").join(home_dir_name).join("memory").join("global_fact.md"), "---\nname: Global fact\ntype: project\n---\nhi\n");
    write(&c.join("plans").join("big-plan.md"), "# The plan\n\n1. do it\n");
    c
}

#[test]
fn claude_import_is_idempotent_and_updates_changed_sources() {
    let (d, v) = common::vault();
    let claude = fake_claude(d.path());
    let opts = ClaudeOpts { claude_dir: claude.clone(), dry_run: true, project_map: vec![] };
    let dry = importer::import_claude(&v, &opts).unwrap();
    assert_eq!(dry.items.len(), 7);
    assert!(v.load_notes().is_empty(), "dry run must not write");

    let opts = ClaudeOpts { dry_run: false, ..opts };
    let first = importer::import_claude(&v, &opts).unwrap();
    let c = first.counts();
    assert_eq!(c["webapp"].created, 5);
    assert_eq!(c["global"].created, 2);
    let notes = v.load_notes();
    assert_eq!(notes.len(), 7);
    let fb = notes.iter().find(|n| n.title == "No em dashes").unwrap();
    assert_eq!(fb.kind, "feedback");
    assert_eq!(fb.project.as_deref(), Some("webapp"));
    assert!(fb.has_tag("imported") && fb.has_tag("claude-memory"));
    assert_eq!(fb.field("summary"), Some(&Value::String("User wants commas instead of dashes".into())));
    assert_eq!(fb.field("source_hash").and_then(|h| h.as_str()).map(str::len), Some(64));
    assert!(fb.source.as_deref().unwrap().ends_with("feedback_style.md"));
    assert_eq!(fb.body, "Use commas.");
    assert_eq!(notes.iter().find(|n| n.title == "Role").unwrap().kind, "user");
    assert_eq!(notes.iter().find(|n| n.title == "Dashboards live here").unwrap().kind, "reference");
    assert_eq!(notes.iter().find(|n| n.title == "Just a heading").unwrap().kind, "note");
    let g = notes.iter().find(|n| n.title == "Global fact").unwrap();
    assert!(g.project.is_none());
    let plan = notes.iter().find(|n| n.kind == "plan").unwrap();
    assert!(plan.project.is_none());
    assert!(v.load_project("webapp").unwrap().is_some());

    let second = importer::import_claude(&v, &opts).unwrap();
    assert!(second.items.iter().all(|i| i.action == importer::Action::Skipped));
    assert_eq!(v.load_notes().len(), 7);

    let f = claude.join("projects/D--zz-nonexistent-webapp/memory/user_role.md");
    fs::write(&f, "---\nname: Role\ndescription: Staff dev now\ntype: user\n---\nWorks on Windows and Linux.\n").unwrap();
    let third = importer::import_claude(&v, &opts).unwrap();
    let c = third.counts();
    assert_eq!(c["webapp"].updated, 1);
    assert_eq!(c["webapp"].skipped, 4);
    let notes = v.load_notes();
    assert_eq!(notes.len(), 7);
    let role = notes.iter().find(|n| n.title == "Role").unwrap();
    assert_eq!(role.body, "Works on Windows and Linux.");
    assert!(third.table().contains("webapp"));
}

#[test]
fn project_map_overrides_decoding() {
    let (d, v) = common::vault();
    let claude = fake_claude(d.path());
    let map = importer::parse_project_map(&["D--zz-nonexistent-webapp=screen".into()]).unwrap();
    let rep = importer::import_claude(&v, &ClaudeOpts { claude_dir: claude, dry_run: false, project_map: map }).unwrap();
    assert_eq!(rep.counts()["screen"].created, 5);
}

#[test]
fn file_import_is_idempotent() {
    let (d, v) = common::vault();
    let src = d.path().join("docs");
    write(&src.join("a.md"), "---\ntitle: From frontmatter\nkind: gotcha\ntags: [x]\n---\nbody a\n");
    write(&src.join("sub").join("b.txt"), "plain text b\n");
    write(&src.join("ignored.rs"), "fn main() {}\n");
    let opts = FilesOpts { path: src.clone(), project: Some("docs".into()), kind: None, tags: vec!["batch".into()], dry_run: false };
    let r1 = importer::import_files(&v, &opts).unwrap();
    assert_eq!(r1.counts()["docs"].created, 2);
    let notes = v.load_notes();
    let a = notes.iter().find(|n| n.title == "From frontmatter").unwrap();
    assert_eq!(a.kind, "gotcha");
    assert!(a.has_tag("x") && a.has_tag("batch") && a.has_tag("imported"));
    assert!(notes.iter().any(|n| n.title == "b"));
    let r2 = importer::import_files(&v, &opts).unwrap();
    assert_eq!(r2.counts()["docs"].skipped, 2);
    assert_eq!(v.load_notes().len(), 2);
}

fn cli(vault: &Path, claude: &Path, args: &[&str]) -> String {
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_breadcrumb"))
        .arg("--vault")
        .arg(vault)
        .args(args)
        .env_remove("BREADCRUMB_HOME")
        .env("BREADCRUMB_CLAUDE_DIR", claude)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn retire_index_is_opt_in_backs_up_and_doctor_warns_before() {
    let (d, v) = common::vault();
    let claude = fake_claude(d.path());
    let index = claude.join("projects/D--zz-nonexistent-webapp/memory/MEMORY.md");
    let long: String = (0..12).map(|i| format!("- [Fact {i}](fact_{i}.md) one line hook\n")).collect();
    fs::write(&index, &long).unwrap();

    let out = cli(&v.root, &claude, &["import", "claude"]);
    assert!(out.trim_end().ends_with(importer::RETIRE_HINT), "{out}");
    assert_eq!(fs::read_to_string(&index).unwrap(), long, "never automatic");
    let warn = |v: &breadcrumb::vault::Vault| breadcrumb::doctor::check_with(v, Some(&claude)).into_iter().filter(|p| p.message.contains("--retire-index")).count();
    assert_eq!(warn(&v), 1);

    let out = cli(&v.root, &claude, &["import", "claude", "--retire-index", "--dry-run"]);
    assert!(out.contains("would retire") && fs::read_to_string(&index).unwrap() == long, "{out}");
    let out = cli(&v.root, &claude, &["import", "claude", "--retire-index"]);
    assert!(out.contains("retired "), "{out}");
    assert_eq!(fs::read_to_string(&index).unwrap().trim(), importer::INDEX_POINTER);
    assert_eq!(fs::read_to_string(index.with_file_name(importer::INDEX_BAK)).unwrap(), long);
    assert_eq!(warn(&v), 0);

    let out = cli(&v.root, &claude, &["import", "claude", "--retire-index"]);
    assert!(!out.contains("retired") && !out.contains("hint:"), "{out}");
    let baks = fs::read_dir(index.parent().unwrap()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains("breadcrumb")).count();
    assert_eq!(baks, 1);
}
