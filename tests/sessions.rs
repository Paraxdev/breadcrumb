mod common;

use std::path::Path;
use std::process::Command;

const ME: &str = "a03e0d0a-951c-4892-a70a-6a4940f03ffd";
const OTHER: &str = "151cdeae-be99-4730-ba9d-daae66f59a6c";

fn session_file(claude: &Path, pid: u32, id: &str, name: &str) {
    std::fs::create_dir_all(claude.join("sessions")).unwrap();
    std::fs::write(claude.join("sessions").join(format!("{pid}.json")), format!(r#"{{"pid":{pid},"sessionId":"{id}","name":"{name}"}}"#)).unwrap();
}

fn crumb(vault: &Path, claude: &Path, session: &str, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_crumb"))
        .args(args)
        .env("BREADCRUMB_HOME", vault)
        .env("BREADCRUMB_CLAUDE_DIR", claude)
        .env("CLAUDE_CODE_SESSION_ID", session)
        .env("BREADCRUMB_SESSION", "")
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn added_id(out: &str) -> String {
    out.split_whitespace().nth(1).unwrap().to_string()
}

#[test]
fn add_append_and_move_record_the_session_and_show_its_current_name() {
    let (d, v) = common::vault();
    let claude = d.path().join("claude");
    session_file(&claude, std::process::id(), ME, "neocad-4b");
    let today = breadcrumb::util::fmt_date(&breadcrumb::util::now());

    let id = added_id(&crumb(&v.root, &claude, ME, &["add", "-k", "task", "-t", "Some task", "--body", "first"]));
    let n = v.get(&id).unwrap();
    assert_eq!(breadcrumb::session::created_by(&n).as_deref(), Some(ME));
    assert!(n.field("last_session").is_none(), "the only writer needs no second stamp");

    crumb(&v.root, &claude, ME, &["append", &id, "second"]);
    crumb(&v.root, &claude, ME, &["move", &id, "in-progress"]);
    crumb(&v.root, &claude, ME, &["move", &id, "review", "--by", "lead"]);
    let n = v.get(&id).unwrap();
    assert!(n.body.contains(&format!("{today} neocad-4b a03e0d0a:\nsecond")), "{}", n.body);
    assert!(n.body.contains("backlog -> in-progress (by neocad-4b a03e0d0a)"), "{}", n.body);
    assert!(n.body.contains("in-progress -> review (by lead a03e0d0a)"), "{}", n.body);

    crumb(&v.root, &claude, OTHER, &["append", &id, "third"]);
    let n = v.get(&id).unwrap();
    assert_eq!(breadcrumb::session::created_by(&n).as_deref(), Some(ME), "the creator stays");
    assert_eq!(breadcrumb::session::last_writer(&n).as_deref(), Some(OTHER));
    assert!(n.body.contains(&format!("{today} 151cdeae:\nthird")), "an unknown session shows its short id: {}", n.body);
    assert!(crumb(&v.root, &claude, ME, &["get", &id]).contains("written by neocad-4b a03e0d0a, last by 151cdeae"));
}

#[test]
fn a_renamed_session_shows_the_new_name_and_a_finished_one_its_short_id() {
    let (d, v) = common::vault();
    let claude = d.path().join("claude");
    session_file(&claude, std::process::id(), ME, "neocad-22");
    let id = added_id(&crumb(&v.root, &claude, ME, &["add", "-t", "A note"]));
    session_file(&claude, std::process::id(), ME, "neocad-33");
    assert!(crumb(&v.root, &claude, "", &["get", &id]).contains("written by neocad-33 a03e0d0a"));

    let mut child = Command::new(env!("CARGO_BIN_EXE_crumb")).arg("--version").spawn().unwrap();
    let dead = child.id();
    child.wait().unwrap();
    std::fs::remove_file(claude.join("sessions").join(format!("{}.json", std::process::id()))).unwrap();
    session_file(&claude, dead, ME, "neocad-33");
    let out = crumb(&v.root, &claude, "", &["get", &id]);
    assert!(out.contains("written by a03e0d0a") && !out.contains("neocad-33"), "{out}");
}

#[test]
fn outside_a_session_nothing_is_stamped_and_an_append_only_gets_its_date() {
    let (d, v) = common::vault();
    let claude = d.path().join("claude");
    let id = added_id(&crumb(&v.root, &claude, "", &["add", "-k", "task", "-t", "Some task"]));
    crumb(&v.root, &claude, "", &["append", &id, "text"]);
    crumb(&v.root, &claude, "", &["move", &id, "in-progress"]);
    let n = v.get(&id).unwrap();
    assert!(n.field("session").is_none() && n.field("last_session").is_none());
    assert!(n.body.contains(&format!("{}:\ntext", breadcrumb::util::fmt_date(&breadcrumb::util::now()))), "{}", n.body);
    assert!(n.body.contains("(by user)"), "{}", n.body);
    assert!(!crumb(&v.root, &claude, "", &["get", &id]).contains("written by"));
}

#[test]
fn a_note_without_a_stamp_gets_only_the_last_writer() {
    let (d, v) = common::vault();
    let claude = d.path().join("claude");
    let n = common::add(&v, "Old note", "note", None, &[]);
    assert!(n.field("session").is_none());
    crumb(&v.root, &claude, ME, &["append", &n.id, "later"]);
    let n = v.get(&n.id).unwrap();
    assert!(n.field("session").is_none());
    assert_eq!(n.field("last_session").and_then(|x| x.as_str()), Some(ME));
}

#[test]
fn the_stamps_cannot_be_set_or_unset_by_hand() {
    let (d, v) = common::vault();
    let claude = d.path().join("claude");
    let n = common::add(&v, "A note", "note", None, &[]);
    let run = |args: &[&str]| Command::new(env!("CARGO_BIN_EXE_crumb")).args(args).env("BREADCRUMB_HOME", &v.root).env("BREADCRUMB_CLAUDE_DIR", &claude).output().unwrap();
    assert!(!run(&["update", &n.id, "--field", "session=x"]).status.success());
    assert!(!run(&["update", &n.id, "--unset", "last_session"]).status.success());
}
