mod common;

use breadcrumb::vault::Vault;
use std::path::Path;
use std::process::{Command, Output};

fn crumb_in(v: &Vault, cwd: Option<&Path>, session: Option<&str>, args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_crumb"));
    c.args(args).env("BREADCRUMB_HOME", &v.root).env_remove("CLAUDE_CODE_SESSION_ID").env_remove("BREADCRUMB_SESSION");
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    if let Some(s) = session {
        c.env("CLAUDE_CODE_SESSION_ID", s);
    }
    c.output().unwrap()
}

fn crumb(v: &Vault, args: &[&str]) -> (i32, String, String) {
    let o = crumb_in(v, None, None, args);
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

#[test]
fn a_batch_move_where_every_move_fails_says_all_failed() {
    let (_d, v) = common::vault();
    let (code, _, err) = crumb(&v, &["move", "zzzzzzzz", "yyyyyyyy", "done"]);
    assert!(code == 1 && err.contains("error: all 2 moves failed"), "{err}");
}

#[test]
fn unsetting_the_state_says_where_the_item_went() {
    let (_d, v) = common::vault();
    let id = common::add(&v, "Item one", "task", Some("app"), &[]).id;
    let (code, _, err) = crumb(&v, &["move", &id, "review", "--step", "Look"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = crumb(&v, &["update", &id, "--unset", "state"]);
    assert_eq!(code, 0, "{err}");
    assert!(format!("{out}{err}").contains(&format!("state unset, {id} reads as backlog now (was review)")), "{out}{err}");
}

#[test]
fn project_add_with_a_missing_path_notes_it() {
    let (d, v) = common::vault();
    let missing = d.path().join("no-such-dir");
    let (code, out, err) = crumb(&v, &["project", "add", "web", "--path", missing.to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    assert!(format!("{out}{err}").contains("is no directory on this machine, kept for a checkout elsewhere, `breadcrumb project link web --path <dir>`"), "{out}{err}");
}

#[test]
fn the_config_listing_names_a_default_kind_that_names_no_kind_as_ignored() {
    let (d, v) = common::vault();
    let work = d.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    breadcrumb::ops::project_link(&v, "app", &work, None).unwrap();
    std::fs::write(v.root.join("projects/app/settings.toml"), "default_kind = \"ghost\"\n").unwrap();
    let o = crumb_in(&v, Some(&work), None, &["config"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("Project app (detected here) overrides nothing and ignores default_kind = ghost (it names no kind)"), "{out}");
}

#[test]
fn a_mistyped_flag_exits_2_past_a_literal_dash_dash_and_as_a_short_flag_before_a_title() {
    let (_d, v) = common::vault();
    let (code, _, err) = crumb(&v, &["add", "--body", "--", "--bogus", "t", "--global"]);
    assert!(code == 2 && err.contains("unknown flag --bogus"), "{code} {err}");
    let (code, _, err) = crumb(&v, &["add", "-x", "t", "--global"]);
    assert!(code == 2 && err.contains("unknown flag -x"), "{code} {err}");
    assert!(v.load_notes().iter().all(|n| n.title != "t"), "nothing is added");
}

#[test]
fn a_tick_on_a_note_that_is_no_item_points_at_update() {
    let (_d, v) = common::vault();
    let id = common::add(&v, "A gotcha", "gotcha", Some("app"), &[]).id;
    let (code, _, err) = crumb(&v, &["check", &id, "1"]);
    assert!(code == 1 && err.contains("not a backlog item") && err.contains("update with kind"), "{err}");
}

#[test]
fn a_refused_release_names_the_holder_not_the_whole_session_id() {
    let (_d, v) = common::vault();
    let id = common::add(&v, "Item one", "task", Some("app"), &[]).id;
    let a = "1e78255a-1111-2222-3333-444455556666";
    let o = crumb_in(&v, None, Some(a), &["claim", &id]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = crumb_in(&v, None, Some("2e78255a-1111-2222-3333-444455556666"), &["release", &id]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && err.contains("is claimed by 1e78255a, not 2e78255a") && !err.contains("1111-2222"), "{err}");
}

#[test]
fn a_given_slug_with_a_word_over_20_letters_is_cut() {
    let (_d, v) = common::vault();
    let (code, out, err) = crumb(&v, &["add", "Long slug", "-k", "task", "-p", "app", "--slug", "abcdefghijklmnopqrstuvwxyz-two"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("[abcdefghijklmnopqrst-two]") && out.contains("slug shortened to abcdefghijklmnopqrst-two"), "{out}");
}
