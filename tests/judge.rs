mod common;

use breadcrumb::judge;
use breadcrumb::mcp::Server;
use breadcrumb::note::Note;
use breadcrumb::ops::{self, AddReq, FieldInput};
use breadcrumb::vault::Vault;
use serde_json::{Map, Value, json};
use std::process::Command;

fn crumb_in(v: &Vault, cwd: &std::path::Path, args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_crumb"))
        .args(args)
        .current_dir(cwd)
        .env("BREADCRUMB_HOME", &v.root)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("BREADCRUMB_SESSION")
        .output()
        .unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

fn args(x: Value) -> Map<String, Value> {
    x.as_object().unwrap().clone()
}

fn judgments(v: &Vault) -> Vec<Note> {
    v.load_notes().into_iter().filter(|n| n.kind == "judgment").collect()
}

#[test]
fn a_judgment_lands_in_the_judge_project_with_where_it_was_seen() {
    let (_d, v) = common::vault();
    let n = judge::judge(&v, "search ranked the gotcha below an old note", "hindered", Some("search"), Some("neocad")).unwrap().note;
    assert_eq!(n.kind, "judgment");
    assert_eq!(n.project.as_deref(), Some("breadcrumb"));
    assert_eq!(judge::verdict(&n), "hindered");
    assert_eq!(judge::about(&n), "search");
    assert_eq!(n.field("seen_in").and_then(|x| x.as_str()), Some("neocad"));

    let n = judge::judge(&v, "merge was easy", "helped", None, Some("breadcrumb")).unwrap().note;
    assert!(n.field("seen_in").is_none(), "no seen_in when it was seen at home");
    assert_eq!(judge::about(&n), "general");

    assert!(judge::judge(&v, "x", "meh", None, None).err().unwrap().to_string().contains("hindered, confusing, missing, helped"));
    assert!(judge::judge(&v, "x", "unrated", None, None).is_err(), "unrated is the tool's word, not the agent's");
    assert!(judge::judge(&v, "  ", "helped", None, None).is_err());
}

#[test]
fn add_with_kind_judgment_is_routed_too_so_mcp_agents_need_nothing_new() {
    let (_d, v) = common::vault();
    let n = ops::add(
        &v,
        AddReq { title: "the digest cut my handoff".into(), kind: Some("judgment".into()), project: Some("app".into()), fields: vec![("verdict".into(), FieldInput::Raw("confusing".into()))], ..Default::default() },
    )
    .unwrap();
    assert_eq!(n.project.as_deref(), Some("breadcrumb"));
    assert_eq!(n.field("seen_in").and_then(|x| x.as_str()), Some("app"));
}

#[test]
fn no_verdict_is_unrated_never_hindered() {
    let (d, v) = common::vault();
    assert_eq!(judge::verdict(&Note::new("abcd1234", "old judgment", "judgment")), "unrated", "a stored judgment without the field");

    let proj = d.path().join("app");
    std::fs::create_dir_all(&proj).unwrap();
    let (ok, _, err) = crumb_in(&v, &proj, &["judge", "search was slow"]);
    assert!(!ok && err.contains("--verdict hindered|confusing|missing|helped"), "{err}");
    assert!(judgments(&v).is_empty());

    // A vault whose kind file predates unrated, as old binaries wrote it.
    std::fs::write(
        v.root.join("kinds/judgment.toml"),
        "name = \"judgment\"\ndescription = \"old\"\n\n[[fields]]\nname = \"verdict\"\ntype = \"enum\"\ndefault = \"hindered\"\nvalues = [\"hindered\", \"confusing\", \"missing\", \"helped\"]\n",
    )
    .unwrap();
    let srv = Server { vault_root: v.root.clone(), cwd: proj.clone() };
    let out = srv.call_tool("add", &args(json!({"title": "the digest cut my handoff", "kind": "judgment", "fields": {"about": "digest"}}))).unwrap();
    let n = judgments(&v).pop().unwrap();
    assert!(
        out.contains(&format!("no verdict, saved as unrated, to rate it call update with id {} and fields {{\"verdict\": V}}, V one of \"hindered\", \"confusing\", \"missing\", \"helped\"", n.id)),
        "{out}"
    );
    assert_eq!(n.field("verdict").and_then(|x| x.as_str()), Some("unrated"));
    assert!(std::fs::read_to_string(v.root.join("kinds/judgment.toml")).unwrap().contains("unrated"), "the kind file learns unrated");
    let doctor = breadcrumb::doctor::check(&v);
    assert!(!format!("{doctor:?}").contains("must be one of"), "{doctor:?}");

    let e = srv.call_tool("add", &args(json!({"title": "x", "kind": "judgment", "fields": {"verdict": "unrated"}}))).unwrap_err().to_string();
    assert!(e.contains("hindered, confusing, missing, helped"), "{e}");
    let (ok, out, _) = crumb_in(&v, &proj, &["add", "-k", "judgment", "-t", "list cut the title"]);
    assert!(ok && out.contains("--field verdict=..."), "{out}");
}

#[test]
fn praise_with_the_same_about_counts_on_one_note() {
    let (d, v) = common::vault();
    let first = judge::judge(&v, "handoff append helped a resume", "helped", Some("Handoff append"), None).unwrap();
    assert!(!first.counted);
    let again = judge::judge(&v, "handoff append kept the thread", "helped", Some("  handoff APPEND "), Some("app")).unwrap();
    assert!(again.counted && again.note.id == first.note.id);
    assert_eq!(judge::count(&again.note), 2);
    assert!(again.text().starts_with(&format!("counted, no new note: {}", again.note.handle())) && again.text().contains("now holds 2 helped judgments about Handoff append"), "{}", again.text());
    let n = v.get(&first.note.id).unwrap();
    assert!(n.body.ends_with(":\nhandoff append kept the thread"), "stamped like an append: {}", n.body);
    assert_eq!(judgments(&v).len(), 1);

    assert!(!judge::judge(&v, "handoff append was slow", "hindered", Some("handoff append"), None).unwrap().counted, "only praise counts");
    assert!(!judge::judge(&v, "general praise", "helped", None, None).unwrap().counted);
    assert!(judge::judge(&v, "more general praise", "helped", Some("general"), None).unwrap().counted, "no about counts under general");

    let other = judge::judge(&v, "brief was what I needed", "helped", Some("brief"), None).unwrap();
    assert!(!other.counted);
    ops::set_status(&v, &other.note.id, "archived").unwrap();
    assert!(!judge::judge(&v, "brief again", "helped", Some("brief"), None).unwrap().counted, "an archived one is not counted on");

    let srv = Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let out = srv.call_tool("add", &args(json!({"title": "handoff append, third time", "kind": "judgment", "fields": {"verdict": "helped", "about": "handoff append"}}))).unwrap();
    assert!(out.starts_with("counted, no new note") && out.contains("now holds 3"), "{out}");
    let proj = d.path().join("app");
    std::fs::create_dir_all(&proj).unwrap();
    let (ok, out, err) = crumb_in(&v, &proj, &["judge", "handoff append once more", "-v", "helped", "-a", "handoff append"]);
    assert!(ok && out.contains("now holds 4"), "{out}{err}");
    let (ok, out, _) = crumb_in(&v, &proj, &["add", "-k", "judgment", "-t", "and again", "--field", "verdict=helped", "--field", "about=handoff append"]);
    assert!(ok && out.contains("now holds 5"), "{out}");
}

#[test]
fn rows_show_verdict_about_and_count_and_filter_by_verdict() {
    let (d, v) = common::vault();
    judge::judge(&v, "search missed an exact id", "hindered", Some("search"), None).unwrap();
    judge::judge(&v, "brief was what I needed", "helped", Some("brief"), None).unwrap();
    judge::judge(&v, "brief again", "helped", Some("brief"), None).unwrap();
    let (ok, out, _) = crumb_in(&v, d.path(), &["list", "-k", "judgment", "-p", "breadcrumb"]);
    assert!(ok && out.contains("[hindered: search] search missed") && out.contains("[helped: brief, x2] brief was"), "{out}");
    let (_, out, _) = crumb_in(&v, d.path(), &["list", "-k", "judgment", "-p", "breadcrumb", "--where", "verdict=hindered"]);
    assert!(out.contains("search missed") && !out.contains("brief"), "{out}");
    let (_, out, _) = crumb_in(&v, d.path(), &["search", "exact id", "-p", "breadcrumb"]);
    assert!(out.contains("[hindered: search] search missed"), "{out}");

    let srv = Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let out = srv.call_tool("list", &args(json!({"project": "breadcrumb", "kind": "judgment", "where": {"verdict": "helped"}}))).unwrap();
    assert!(out.contains("[helped: brief, x2]") && !out.contains("search missed"), "{out}");
    let out = srv.call_tool("search", &args(json!({"query": "exact id", "project": "breadcrumb"}))).unwrap();
    assert!(out.contains("[hindered: search]"), "{out}");

    let mut old = Note::new("abcd1234", "an old judgment", "judgment");
    old.project = Some("breadcrumb".into());
    assert!(ops::where_matches(&old, "verdict=unrated").unwrap(), "a judgment without the field filters as unrated");
}

#[test]
fn list_shows_what_to_triage_in_full_and_praise_as_counts() {
    let (_d, v) = common::vault();
    let a = judge::judge(&v, "search missed an exact id", "hindered", Some("search"), None).unwrap().note;
    judge::judge(&v, "search found it at once", "helped", Some("search"), None).unwrap();
    judge::judge(&v, "and again", "helped", Some("search"), None).unwrap();
    judge::judge(&v, "handoff kept the plan", "helped", Some("handoff"), None).unwrap();
    judge::judge(&v, "no way to see who holds a lease", "missing", Some("lease"), None).unwrap();
    let rows = judge::list(&v, None, false);
    let text = judge::list_text(&rows, false);
    assert!(text.starts_with("2 judgments of breadcrumb to triage"), "{text}");
    assert!(text.contains("  search: 1 hindered\n") && text.contains("  lease: 1 missing"), "{text}");
    assert!(text.contains(&format!("{} hindered search: search missed an exact id", a.id)), "{text}");
    assert!(text.contains("helped: search 2, handoff 1, `breadcrumb judge --helped` lists them"), "{text}");
    assert!(!text.contains("found it at once") && text.contains("--file <id>...") && text.contains("--to <item>") && text.contains("--dismiss <id>... --why"), "{text}");
    let helped = judge::list_text(&rows, true);
    assert!(helped.starts_with("2 helped judgments, given 3 times") && helped.contains("helped search, x2: search found it at once"), "{helped}");

    let item = common::add(&v, "Search ranks exact ids first", "bug", Some("breadcrumb"), &[]);
    ops::link(&v, &a.id, &item.id, Some(judge::FILED_AS), false).unwrap();
    let text = judge::list_text(&judge::list(&v, None, false), false);
    assert!(text.starts_with("1 judgment of") && !text.contains("search missed"), "a filed judgment leaves the list: {text}");
    assert!(judge::list_text(&judge::list(&v, None, true), false).contains("[triaged]"));
}

#[test]
fn the_digest_asks_by_the_setting_and_counts_only_what_needs_triage() {
    let (_d, mut v) = common::vault();
    let d = breadcrumb::digest::context(&v, Some("app"), 15).unwrap();
    assert!(!d.contains("Judge breadcrumb"), "off by default: {d}");

    breadcrumb::config::set(&mut v, "judge_nudge", "session").unwrap();
    for _ in 0..2 {
        let d = breadcrumb::digest::context(&v, Some("app"), 15).unwrap();
        assert!(d.contains(judge::NUDGE), "every digest: {d}");
    }
    assert!(judge::NUDGE.contains("hindered|confusing|missing") && judge::NUDGE.contains("Nothing to say is fine") && judge::NUDGE.len() < 212);

    breadcrumb::config::set(&mut v, "judge_nudge", "daily").unwrap();
    assert!(breadcrumb::digest::context(&v, Some("app"), 15).unwrap().contains("Judge breadcrumb"));
    assert!(!breadcrumb::digest::context(&v, Some("app"), 15).unwrap().contains("Judge breadcrumb"), "once a day");

    judge::judge(&v, "praise", "helped", None, Some("app")).unwrap();
    assert!(!breadcrumb::digest::context(&v, Some("breadcrumb"), 15).unwrap().contains("Judgments:"), "praise needs no triage");
    for (t, verdict) in [("a", "hindered"), ("b", "hindered"), ("c", "missing")] {
        judge::judge(&v, t, verdict, None, Some("app")).unwrap();
    }
    ops::add(&v, AddReq { title: "no verdict".into(), kind: Some("judgment".into()), ..Default::default() }).unwrap();
    let d = breadcrumb::digest::context(&v, Some("breadcrumb"), 15).unwrap();
    assert!(d.contains("Judgments: 4 to triage (2 hindered, 1 missing, 1 unrated), `breadcrumb judge --list`"), "{d}");
    assert!(!breadcrumb::digest::context(&v, Some("app"), 15).unwrap().contains("Judgments:"), "only at home");

    let e = breadcrumb::config::set(&mut v, "judge_nudge", "hourly").unwrap_err().to_string();
    assert!(e.contains("off, session, daily"), "{e}");
}

#[test]
fn cli_judge_files_from_the_cwd_project_and_lists() {
    let (d, v) = common::vault();
    let proj = d.path().join("app");
    std::fs::create_dir_all(&proj).unwrap();
    assert!(crumb_in(&v, &proj, &["project", "link", "app"]).0);
    let (ok, out, err) = crumb_in(&v, &proj, &["judge", "brief was what I needed", "--verdict", "helped", "--about", "brief"]);
    assert!(ok, "{err}");
    assert!(out.contains("judged"), "{out}");
    let n = judgments(&v).pop().unwrap();
    assert_eq!(n.project.as_deref(), Some("breadcrumb"));
    assert_eq!(n.field("seen_in").and_then(|x| x.as_str()), Some("app"));
    let (_, out, _) = crumb_in(&v, &proj, &["judge", "--list"]);
    assert!(out.contains("no judgments to triage") && out.contains("helped: brief 1"), "{out}");
    let (_, out, _) = crumb_in(&v, &proj, &["judge"]);
    assert!(out.contains("helped: brief 1"), "a bare judge lists: {out}");
    let (_, out, _) = crumb_in(&v, &proj, &["judge", "--helped"]);
    assert!(out.contains("helped brief: brief was what I needed (in app)"), "{out}");
    let (_, out, _) = crumb_in(&v, &proj, &["judge", "--list", "--since", "1d", "--all"]);
    assert!(out.contains("helped: brief 1"), "{out}");
}

#[test]
fn file_makes_one_item_from_judgments_and_links_them() {
    let (d, v) = common::vault();
    let a = judge::judge(&v, "judge: search ranked an old note first", "hindered", Some("search"), Some("neocad")).unwrap().note;
    let b = judge::judge(&v, "search hid the gotcha", "confusing", Some("search"), None).unwrap().note;
    let (ok, out, err) = crumb_in(&v, d.path(), &["judge", "--file", &a.id, &b.id]);
    assert!(ok, "{err}");
    let item = v.load_notes().into_iter().find(|n| n.kind == "bug").unwrap();
    assert_eq!(item.title, "Search ranked an old note first");
    assert_eq!(item.project.as_deref(), Some("breadcrumb"));
    assert!(out.starts_with(&format!("filed bug {} \"Search ranked", item.handle())) && out.contains("from 2 judgments"), "{out}");
    assert!(item.body.contains(&format!("{} hindered about search, seen in neocad: judge: search ranked", a.id)) && item.body.contains(&format!("{} confusing about search: search hid", b.id)), "{}", item.body);
    assert_eq!(item.body.matches(&a.id).count(), 1, "each judgment is quoted once: {}", item.body);
    for j in [&a, &b] {
        assert!(v.get(&j.id).unwrap().links.iter().any(|l| l.to == item.id && l.rel == "filed-as"));
    }
    assert!(judge::list(&v, None, false).is_empty());

    let c = judge::judge(&v, "no lease owner shown", "missing", Some("lease"), None).unwrap().note;
    let (ok, _, err) = crumb_in(&v, d.path(), &["judge", "--file", &c.id, "--as", "idea", "--title", "Show lease owners", "--priority", "p1"]);
    assert!(ok, "{err}");
    let idea = v.load_notes().into_iter().find(|n| n.kind == "idea").unwrap();
    assert_eq!(idea.title, "Show lease owners");
    assert_eq!(idea.field("priority").and_then(|x| x.as_str()), Some("p1"));
    let (ok, _, err) = crumb_in(&v, d.path(), &["judge", "--file", &c.id, "--as", "note"]);
    assert!(!ok && err.contains("bug, idea, feature, task"), "{err}");
    let (ok, _, err) = crumb_in(&v, d.path(), &["judge", "--file", &item.id]);
    assert!(!ok && err.contains("is a bug, not a judgment"), "{err}");
}

#[test]
fn to_files_judgments_under_an_existing_item() {
    let (d, v) = common::vault();
    let item = common::add(&v, "Search ranks exact ids first", "bug", Some("breadcrumb"), &[]);
    let a = judge::judge(&v, "search missed an exact id", "hindered", Some("search"), None).unwrap().note;
    let b = judge::judge(&v, "exact id came third", "hindered", Some("search"), Some("app")).unwrap().note;
    let commits = |v: &Vault| common::git(&v.root, &["rev-list", "--count", "HEAD"]).trim().parse::<u32>().unwrap();
    let before = commits(&v);
    let (ok, out, err) = crumb_in(&v, d.path(), &["judge", "--to", item.slug().as_deref().unwrap(), &a.id, &b.id]);
    assert!(ok, "{err}");
    assert!(out.contains(&format!("linked {}, {} filed-as {}", a.id, b.id, item.handle())), "{out}");
    assert_eq!(commits(&v), before + 1, "one vault commit");
    let item = v.get(&item.id).unwrap();
    assert!(item.body.contains(&format!("filed from judgment {} hindered about search: search missed", a.id)) && item.body.contains(&format!("{} hindered about search, seen in app", b.id)), "{}", item.body);
    assert!(judge::list(&v, None, false).is_empty());
}

#[test]
fn dismiss_archives_with_the_reason() {
    let (d, v) = common::vault();
    let a = judge::judge(&v, "search felt slow", "hindered", Some("search"), None).unwrap().note;
    let (ok, _, err) = crumb_in(&v, d.path(), &["judge", "--dismiss", &a.id]);
    assert!(!ok && err.contains("--why"), "{err}");
    let (ok, out, err) = crumb_in(&v, d.path(), &["judge", "--dismiss", &a.id, "--why", "it was a cold cache"]);
    assert!(ok && out.contains(&format!("dismissed and archived {}", a.id)), "{out}{err}");
    let n = v.get(&a.id).unwrap();
    assert!(n.is_archived() && n.body.ends_with(":\ndismissed: it was a cold cache"), "{}", n.body);
    assert!(judge::list(&v, None, false).is_empty());
}

const OLD_KIND: &str = "name = \"judgment\"\ndescription = \"old\"\n\n[[fields]]\nname = \"verdict\"\ntype = \"enum\"\ndefault = \"hindered\"\nvalues = [\"hindered\", \"confusing\", \"missing\", \"helped\"]\n";

fn note_file(v: &Vault, id: &str) -> std::path::PathBuf {
    v.note_files().into_iter().find(|p| std::fs::read_to_string(p).unwrap().contains(&format!("id: {id}"))).unwrap()
}

#[test]
fn an_old_judgment_kind_file_never_stamps_hindered_or_refuses_unrated_on_update_or_doctor() {
    let (_d, v) = common::vault();
    let rated = judge::judge(&v, "search was slow", "confusing", Some("search"), None).unwrap().note;
    let unrated = ops::add(&v, AddReq { title: "list cut the title".into(), kind: Some("judgment".into()), ..Default::default() }).unwrap();
    let kind_file = v.root.join("kinds/judgment.toml");
    std::fs::write(&kind_file, OLD_KIND).unwrap();
    let p = note_file(&v, &rated.id);
    let text = std::fs::read_to_string(&p).unwrap();
    assert!(text.contains("verdict: confusing"), "{text}");
    std::fs::write(&p, text.replace("verdict: confusing\n", "")).unwrap();
    common::git(&v.root, &["commit", "-qam", "old vault"]);

    let doctor = format!("{:?}", breadcrumb::doctor::check(&v));
    assert!(!doctor.contains("must be one of") && !doctor.contains("unrated"), "{doctor}");
    assert!(!std::fs::read_to_string(&kind_file).unwrap().contains("unrated"), "doctor writes nothing");

    let n = ops::update(&v, &unrated.id, ops::UpdateReq { title: Some("list cut the long title".into()), ..Default::default() }).unwrap();
    assert_eq!(judge::verdict(&n), "unrated");
    let n = ops::update(&v, &rated.id, ops::UpdateReq { title: Some("search was slow twice".into()), ..Default::default() }).unwrap();
    assert_eq!(n.field("verdict").and_then(|x| x.as_str()), Some("unrated"), "never the old default hindered");
    assert!(std::fs::read_to_string(&kind_file).unwrap().contains("unrated"), "an update of a judgment upgrades the kind file");
    assert_eq!(common::git(&v.root, &["status", "--porcelain"]), "", "the upgrade is committed with the update");
}

#[test]
fn to_refuses_a_judgment_as_the_item() {
    let (d, v) = common::vault();
    let a = judge::judge(&v, "search missed an exact id", "hindered", Some("search"), None).unwrap().note;
    let b = judge::judge(&v, "exact id came third", "hindered", Some("search"), None).unwrap().note;
    let (ok, _, err) = crumb_in(&v, d.path(), &["judge", "--to", &a.id, &b.id]);
    assert!(!ok && err.contains(&format!("{} is a judgment, and judgments are filed under a work item or note, name one with --to", a.handle())), "{err}");
    assert!(v.get(&b.id).unwrap().links.is_empty());
    assert_eq!(judge::list(&v, None, false).len(), 2);
}

#[test]
fn json_says_whether_a_judgment_was_counted() {
    let (d, v) = common::vault();
    let json_of = |out: &str| serde_json::from_str::<Value>(out).unwrap();
    let (ok, out, err) = crumb_in(&v, d.path(), &["--json", "judge", "brief was what I needed", "--verdict", "helped", "--about", "brief"]);
    assert!(ok, "{err}");
    let first = json_of(&out);
    assert_eq!(first["counted"], json!(false), "{out}");
    let (ok, out, err) = crumb_in(&v, d.path(), &["--json", "judge", "brief again", "--verdict", "helped", "--about", "brief"]);
    assert!(ok, "{err}");
    let again = json_of(&out);
    assert_eq!((&again["counted"], &again["count"], &again["id"]), (&json!(true), &json!(2), &first["id"]), "{out}");
    let (ok, out, err) = crumb_in(&v, d.path(), &["--json", "add", "-k", "judgment", "-t", "brief a third time", "--field", "verdict=helped", "--field", "about=brief"]);
    assert!(ok, "{err}");
    assert_eq!((&json_of(&out)["counted"], &json_of(&out)["count"]), (&json!(true), &json!(3)), "{out}");
    let (ok, out, err) = crumb_in(&v, d.path(), &["--json", "add", "-k", "judgment", "-t", "list cut the title", "--field", "verdict=hindered"]);
    assert!(ok, "{err}");
    assert_eq!(json_of(&out)["counted"], json!(false), "{out}");
    assert!(json_of(&out).get("count").is_none(), "{out}");
}

#[test]
fn a_judgment_reply_says_when_its_slug_was_repaired() {
    let (d, v) = common::vault();
    let srv = Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let out = srv.call_tool("add", &args(json!({"title": "search was slow", "kind": "judgment", "slug": "Slow Search!", "fields": {"verdict": "hindered"}}))).unwrap();
    assert!(out.contains("slug fixed to slow-search"), "{out}");
    let (ok, out, err) = crumb_in(&v, d.path(), &["add", "-k", "judgment", "-t", "list cut the title", "--slug", "Cut Title!", "--field", "verdict=hindered"]);
    assert!(ok && out.contains("slug fixed to cut-title"), "{out}{err}");
}
