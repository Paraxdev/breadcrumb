mod common;

use breadcrumb::cli::Cli;
use breadcrumb::vault::Vault;
use clap::CommandFactory;
use std::process::Command;

fn crumb(v: &Vault, args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_crumb")).args(args).env("BREADCRUMB_HOME", &v.root).env_remove("CLAUDE_CODE_SESSION_ID").env_remove("BREADCRUMB_SESSION").output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

fn ok(v: &Vault, args: &[&str]) -> String {
    let (ok, out, err) = crumb(v, args);
    assert!(ok, "{args:?}: {err}");
    out
}

fn walk(c: &clap::Command, path: &str, missing: &mut Vec<String>) {
    for a in c.get_arguments() {
        let id = a.get_id().as_str();
        if id != "help" && id != "version" && a.get_help().is_none() {
            missing.push(format!("{path} {id}"));
        }
    }
    for s in c.get_subcommands() {
        let sub = format!("{path} {}", s.get_name());
        if s.get_name() != "help" && s.get_about().is_none() {
            missing.push(format!("{sub} (no about)"));
        }
        walk(s, &sub, missing);
    }
}

#[test]
fn every_argument_and_command_has_a_line_of_help() {
    let mut missing = vec![];
    walk(&Cli::command(), "crumb", &mut missing);
    assert!(missing.is_empty(), "no help on:\n{}", missing.join("\n"));
}

#[test]
fn every_mcp_tool_name_is_a_cli_command() {
    let cmd = Cli::command();
    for tool in breadcrumb::mcp::tool_defs() {
        let name = tool["name"].as_str().unwrap();
        assert!(cmd.get_subcommands().any(|s| s.get_name() == name || s.get_all_aliases().any(|a| a == name)), "{name}");
    }
}

#[test]
fn the_mcp_words_work_on_the_cli() {
    let (_d, v) = common::vault();
    let item = common::add(&v, "An item", "task", Some("app"), &[]).id;
    assert_eq!(ok(&v, &["backlog", "-p", "app"]), ok(&v, &["board", "-p", "app"]));
    assert!(ok(&v, &["kinds"]).contains("decision"));
    assert_eq!(ok(&v, &["kinds"]), ok(&v, &["kind", "list"]));
    assert_eq!(ok(&v, &["kind"]), ok(&v, &["kind", "list"]));
    assert!(ok(&v, &["kinds", "show", "decision"]).contains("decision"));
    assert!(ok(&v, &["question", &item, "Which one?", "-o", "a: first", "-o", "b: second"]).contains(&item));
    assert!(ok(&v, &["questions", "-p", "app"]).contains("Which one?"));
    ok(&v, &["withdraw", &format!("{item}.1")]);
    assert!(!ok(&v, &["questions", "-p", "app"]).contains("Which one?"));
    ok(&v, &["update", &item, "--pinned", "--tags", "+x", "--refs", "commit:abcdef1"]);
    ok(&v, &["move", &item, "review", "--steps", "Try it"]);
    ok(&v, &["claim", &item, "--by", "me"]);
    assert!(ok(&v, &["claim", &item, "--by", "me", "--release"]).contains("released"));
    ok(&v, &["doctor", "--fix", "--apply"]);
    let gone = common::add(&v, "Gone", "note", Some("app"), &[]).id;
    ok(&v, &["delete", &gone, "--confirm"]);
}

#[test]
fn move_takes_a_queue_position() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "First", "task", Some("app"), &[]).id;
    let b = common::add(&v, "Second", "task", Some("app"), &[]).id;
    ok(&v, &["queue", &a]);
    ok(&v, &["move", &b, "queued", "--position", "top"]);
    assert!(ok(&v, &["next", "-p", "app"]).contains(&b));
    assert!(!crumb(&v, &["move", &b, "review", "--position", "top"]).0);
}

#[test]
fn unlink_is_link_remove() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "A", "note", None, &[]).id;
    let b = common::add(&v, "B", "note", None, &[]).id;
    ok(&v, &["link", &a, &b, "--rel", "blocks"]);
    assert!(ok(&v, &["unlink", &a, &b]).contains("(none)"));
    assert!(!crumb(&v, &["unlink", &a, &b]).0);
    ok(&v, &["link", &a, &b]);
    assert!(ok(&v, &["link", &a, &b, "--remove"]).contains("(none)"));
}

#[test]
fn a_bare_sha_reads_as_a_commit_ref_on_move_and_update() {
    let (_d, v) = common::vault();
    let id = common::add(&v, "Item", "task", Some("app"), &[]).id;
    ok(&v, &["move", &id, "review", "--ref", "ABC1234"]);
    let long = "0123456789abcdef0123456789abcdef01234567";
    ok(&v, &["update", &id, "--ref", long]);
    let got = v.get(&id).unwrap().refs();
    assert_eq!(got, vec!["commit:abc1234".to_string(), format!("commit:{long}")]);
    assert!(ok(&v, &["list", "-p", "app", "--ref", "abc1234"]).contains(&id));
    ok(&v, &["update", &id, "--ref", "-abc1234"]);
    assert_eq!(v.get(&id).unwrap().refs(), vec![format!("commit:{long}")]);
    for bad in ["abc123", "nothexx", &"a".repeat(41)] {
        let (fine, _, err) = crumb(&v, &["update", &id, "--ref", bad]);
        assert!(!fine && err.contains("must be type:value"), "{bad}: {err}");
    }
}

fn tree(v: &Vault, id: &str) -> String {
    ok(v, &["tree", id])
}

#[test]
fn tree_honours_parent_and_part_of_links_with_no_data_change() {
    let (_d, v) = common::vault();
    let epic = common::add(&v, "The epic", "epic", Some("app"), &[]).id;
    let by_parent = common::add(&v, "By a parent link", "task", Some("app"), &[]).id;
    let by_part = common::add(&v, "By a part-of link", "task", Some("app"), &[]).id;
    ok(&v, &["link", &by_parent, &epic, "--rel", "parent"]);
    ok(&v, &["link", &by_part, &epic, "--rel", "part-of"]);
    let file = std::fs::read(v.resolve_path(&by_parent).unwrap()).unwrap();
    let t = tree(&v, &epic);
    assert!(t.contains(&by_parent) && t.contains(&by_part), "{t}");
    assert!(tree(&v, &by_parent).starts_with("parents: "), "{}", tree(&v, &by_parent));
    assert_eq!(std::fs::read(v.resolve_path(&by_parent).unwrap()).unwrap(), file);
    assert!(v.get(&by_parent).unwrap().field("parent").is_none());
}

#[test]
fn the_parent_field_wins_and_a_note_has_one_place() {
    let (_d, v) = common::vault();
    let field = common::add(&v, "Field epic", "epic", Some("app"), &[]).id;
    let linked = common::add(&v, "Linked epic", "epic", Some("app"), &[]).id;
    let part = common::add(&v, "Part-of epic", "epic", Some("app"), &[]).id;
    let child = common::add(&v, "The child", "task", Some("app"), &[("parent", &field)]).id;
    let said = ok(&v, &["link", &child, &linked, "--rel", "parent"]);
    assert!(said.contains(&format!("the parent field {field} wins")), "{said}");
    assert!(tree(&v, &field).contains(&child));
    assert!(!tree(&v, &linked).contains(&child));

    let two = common::add(&v, "Two links", "task", Some("app"), &[]).id;
    ok(&v, &["link", &two, &part, "--rel", "part-of"]);
    let said = ok(&v, &["link", &two, &linked, "--rel", "parent"]);
    assert!(said.contains("as a child of"), "{said}");
    assert!(tree(&v, &linked).contains(&two));
    assert!(!tree(&v, &part).contains(&two), "a parent link beats a part-of link, so it never shows twice");
}

#[test]
fn a_link_cycle_ends_and_says_so() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "A", "epic", Some("app"), &[]).id;
    let b = common::add(&v, "B", "feature", Some("app"), &[]).id;
    ok(&v, &["link", &a, &b, "--rel", "parent"]);
    ok(&v, &["link", &b, &a, "--rel", "parent"]);
    assert!(tree(&v, &a).contains("(cycle)"), "{}", tree(&v, &a));
}

#[test]
fn only_backlog_items_take_a_place_from_a_link() {
    let (_d, v) = common::vault();
    let epic = common::add(&v, "The epic", "epic", Some("app"), &[]).id;
    let note = common::add(&v, "A plain note", "decision", Some("app"), &[]).id;
    let said = ok(&v, &["link", &note, &epic, "--rel", "part-of"]);
    assert!(said.contains("only places backlog items"), "{said}");
    assert!(!tree(&v, &epic).contains(&note));
}

#[test]
fn other_rels_print_only_the_links() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "A", "task", Some("app"), &[]).id;
    let b = common::add(&v, "B", "task", Some("app"), &[]).id;
    let out = ok(&v, &["link", &a, &b, "--rel", "relates"]);
    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(!tree(&v, &b).contains(&a));
}

#[test]
fn progress_and_next_still_read_only_the_field() {
    let (_d, v) = common::vault();
    let epic = common::add(&v, "The epic", "epic", Some("app"), &[]).id;
    let linked = common::add(&v, "Linked", "task", Some("app"), &[]).id;
    ok(&v, &["link", &linked, &epic, "--rel", "parent"]);
    assert!(!ok(&v, &["board", "-p", "app"]).contains("/1 done"), "{}", ok(&v, &["board", "-p", "app"]));
    let idx_notes = v.load_notes();
    assert!(breadcrumb::backlog::Index::new(&idx_notes).children(&epic).is_empty());
}
