mod common;

use breadcrumb::backlog::{self, BoardFilter, QueuePos};
use breadcrumb::ops::Scope;
use serde_yaml::Value;
use std::process::{Command, Stdio};

fn err<T: std::fmt::Debug>(r: anyhow::Result<T>) -> String {
    format!("{:#}", r.unwrap_err())
}

/// Backdates the last write and the claim, as if the session went quiet two days ago.
fn go_quiet(v: &breadcrumb::vault::Vault, id: &str) {
    let mut n = v.get(id).unwrap();
    let old = breadcrumb::util::now() - chrono::Duration::days(2);
    n.updated = old;
    n.set_field("claimed", Value::String(breadcrumb::util::fmt_time(&old)));
    v.write_note(&mut n).unwrap();
}

#[test]
fn claim_and_release_guard_fresh_claims_of_other_sessions() {
    let (_d, v) = common::vault();
    let t = common::add(&v, "Scale fix", "task", Some("app"), &[]);
    let n = backlog::claim(&v, &t.id, "neocad-fc", false).unwrap();
    assert_eq!(backlog::assignee(&n).as_deref(), Some("neocad-fc"));
    assert!(backlog::claimed_at(&n).is_some() && n.body.contains("claimed (by neocad-fc)"), "{}", n.body);
    assert_eq!(backlog::state(&n), "backlog", "a claim leaves the state alone");

    let e = err(backlog::claim(&v, &t.id, "neocad-49", false));
    assert!(e.contains("claimed by neocad-fc") && e.contains("force"), "{e}");
    assert!(err(backlog::release(&v, &t.id, "neocad-49", false)).contains("not neocad-49"));
    assert!(backlog::claim(&v, &t.id, "NEOCAD-FC", false).unwrap().body.contains("claim renewed"));

    let n = backlog::claim(&v, &t.id, "neocad-49", true).unwrap();
    assert!(n.body.contains("claim taken over from neocad-fc (by neocad-49)"));
    let n = backlog::release(&v, &t.id, "neocad-49", false).unwrap().unwrap();
    assert!(backlog::assignee(&n).is_none() && n.field("claimed").is_none());
    assert!(backlog::release(&v, &t.id, "neocad-49", false).unwrap().is_none(), "nothing left to release");

    backlog::claim(&v, &t.id, "neocad-fc", false).unwrap();
    go_quiet(&v, &t.id);
    let n = backlog::claim(&v, &t.id, "neocad-49", false).unwrap();
    assert!(n.body.contains("claim taken over from neocad-fc"), "a stale claim can be taken without force");

    backlog::move_item(&v, &t.id, backlog::MoveReq { state: "done".into(), ..Default::default() }).unwrap();
    assert!(err(backlog::claim(&v, &t.id, "x", false)).contains("open items"));
}

#[test]
fn next_skips_items_other_sessions_hold_and_the_board_shows_holders() {
    let (_d, v) = common::vault();
    let first = common::add(&v, "First", "task", Some("app"), &[("priority", "p1")]);
    let second = common::add(&v, "Second", "task", Some("app"), &[("priority", "p2")]);
    for n in [&first, &second] {
        backlog::queue(&v, &n.id, QueuePos::End, None).unwrap();
    }
    backlog::claim(&v, &first.id, "s1", false).unwrap();
    let scope = Scope::Project { slug: "app".into(), with_global: false };
    let next = |me: Option<&str>| backlog::next_as(&v, &scope, None, None, me).unwrap().map(|n| n.id);
    assert_eq!(next(Some("s1")), Some(first.id.clone()), "the holder still gets it");
    assert_eq!(next(Some("s2")), Some(second.id.clone()));
    assert_eq!(next(None), Some(second.id.clone()), "an unnamed caller does not take claimed work");

    let claimed = backlog::next_as(&v, &scope, None, Some("s2"), Some("s2")).unwrap().unwrap();
    assert_eq!(claimed.id, second.id);
    assert!(backlog::claimed_at(&v.get(&second.id).unwrap()).is_some(), "next --claim records the claim time");

    let cols = backlog::board(&v, &scope, &BoardFilter::default()).unwrap();
    let item = |id: &str| cols.iter().flat_map(|c| &c.items).find(|i| i.id == id).unwrap().clone();
    assert!(item(&first.id).short().ends_with(" @s1"), "{}", item(&first.id).short());
    go_quiet(&v, &first.id);
    let cols = backlog::board(&v, &scope, &BoardFilter::default()).unwrap();
    let it = cols.iter().flat_map(|c| &c.items).find(|i| i.id == first.id).unwrap();
    assert!(it.claim_stale && it.short().ends_with(" @s1 (stale)") && it.line().contains("@s1 (stale claim)"), "{}", it.line());
    assert_eq!(next(Some("s3")), Some(first.id.clone()), "a stale claim no longer keeps others away");
    let ctx = breadcrumb::ops::context(&v, Some("app"), 15).unwrap();
    assert!(ctx.contains(&format!("{} task p2 Second @s2\n", second.id)), "{ctx}");
}

#[test]
fn cli_and_mcp_take_the_session_name_from_the_environment() {
    let (d, v) = common::vault();
    let t = common::add(&v, "Perf edit", "task", None, &[]);
    let run = |args: &[&str], session: &str| {
        let o = Command::new(env!("CARGO_BIN_EXE_breadcrumb"))
            .arg("--vault")
            .arg(&v.root)
            .args(args)
            .env_remove("BREADCRUMB_HOME")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env("BREADCRUMB_SESSION", session)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr))
    };
    let (ok, out) = run(&["claim", &t.id], "neocad-fc");
    assert!(ok && out.contains("@neocad-fc"), "{out}");
    let (ok, out) = run(&["release", &t.id], "neocad-49");
    assert!(!ok && out.contains("claimed by neocad-fc"), "{out}");
    let (ok, out) = run(&["board", "--global"], "x");
    assert!(ok && out.contains("@neocad-fc"), "{out}");

    let srv = breadcrumb::mcp::Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let mut a = serde_json::Map::new();
    a.insert("id".into(), t.id.clone().into());
    a.insert("by".into(), "neocad-49".into());
    assert!(format!("{:#}", srv.call_tool("claim", &a).unwrap_err()).contains("force"));
    a.insert("force".into(), true.into());
    assert!(srv.call_tool("claim", &a).unwrap().ends_with("for neocad-49"));
    a.insert("release".into(), true.into());
    assert!(srv.call_tool("claim", &a).unwrap().starts_with("released "));
}
