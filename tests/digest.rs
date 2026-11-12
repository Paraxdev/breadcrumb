mod common;

use breadcrumb::digest::{self, ContextOpts};
use breadcrumb::note::Link;
use breadcrumb::ops::{self, AddReq, FieldInput, UpdateReq};
use serde_yaml::Value;
use std::time::Duration;

fn pin(v: &breadcrumb::vault::Vault, id: &str) {
    ops::update(v, id, UpdateReq { pinned: Some(true), ..Default::default() }).unwrap();
}

fn tick() {
    std::thread::sleep(Duration::from_millis(1100));
}

#[test]
fn summaries_come_from_the_field_or_the_titles_first_sentence() {
    let mut n = breadcrumb::note::Note::new("aaaa0001", "Use tabs. Spaces broke the parser, e.g. in tests.", "decision");
    assert_eq!(digest::summary(&n), "Use tabs.");
    n.title = "Prefer e.g. short names. Always".into();
    assert_eq!(digest::summary(&n), "Prefer e.g. short names.");
    n.title = "word ".repeat(40);
    assert_eq!(digest::summary(&n).chars().count(), digest::SUMMARY_MAX);
    n.set_field(digest::SUMMARY_KEY, Value::String("Own  summary\nline".into()));
    assert_eq!(digest::summary(&n), "Own summary line");
}

#[test]
fn summary_is_an_option_of_add_and_update() {
    let (d, v) = common::vault();
    let srv = breadcrumb::mcp::Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let mut a = serde_json::Map::new();
    a.insert("title".into(), "Release checklist".into());
    a.insert("project".into(), "global".into());
    a.insert("pinned".into(), true.into());
    a.insert("summary".into(), "Tag, build,\n then publish".into());
    srv.call_tool("add", &a).unwrap();
    let n = v.load_notes().into_iter().find(|n| n.title == "Release checklist").unwrap();
    assert_eq!(digest::summary(&n), "Tag, build, then publish");
    let mut u = serde_json::Map::new();
    u.insert("id".into(), n.id.clone().into());
    u.insert("summary".into(), "Tag and publish".into());
    srv.call_tool("update", &u).unwrap();
    assert!(ops::context(&v, None, 15).unwrap().contains(&format!("{} note Tag and publish\n", n.id)));
}

#[test]
fn pins_are_one_line_each_rules_first_and_lapsed_notes_never_show() {
    let (_d, v) = common::vault();
    let pinned = |title: &str, kind: &str, fields: &[(&str, &str)]| {
        ops::add(
            &v,
            AddReq {
                title: title.into(),
                kind: Some(kind.into()),
                project: Some("app".into()),
                pinned: true,
                body: Some("Body text that stays out of the digest.".into()),
                fields: fields.iter().map(|(k, x)| (k.to_string(), FieldInput::Raw(x.to_string()))).collect(),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let old_rule = pinned("Old rule", "decision", &[("summary", "Short own summary")]);
    tick();
    let gotcha = pinned("Pinned gotcha", "gotcha", &[]);
    tick();
    let new_rule = pinned("New rule", "feedback", &[]);
    let replaced = pinned("Replaced decision", "decision", &[("decision_status", "superseded")]);
    let linked = pinned("Superseded by a link", "note", &[]);
    let tagged = pinned("Tagged superseded", "note", &[]);
    let archived = pinned("Archived pin", "note", &[]);
    let newer = common::add(&v, "Newer fact", "note", Some("app"), &[]);
    ops::link(&v, &newer.id, &linked.id, Some("supersedes"), false).unwrap();
    ops::update(&v, &tagged.id, UpdateReq { tags: vec!["superseded".into()], ..Default::default() }).unwrap();
    ops::set_status(&v, &archived.id, "archived").unwrap();

    let ctx = ops::context(&v, Some("app"), 15).unwrap();
    let pins: Vec<&str> = ctx.lines().skip_while(|l| !l.starts_with("Pinned")).skip(1).take(3).collect();
    assert_eq!(
        pins,
        vec![
            format!("{} feedback (inferred) New rule", new_rule.id).as_str(),
            format!("{} decision Short own summary", old_rule.id).as_str(),
            format!("{} gotcha Pinned gotcha", gotcha.id).as_str()
        ],
        "{ctx}"
    );
    assert!(!ctx.contains("Body text"), "{ctx}");
    for gone in [&replaced, &linked, &tagged, &archived] {
        assert!(!ctx.contains(&gone.id), "{} must not appear: {ctx}", gone.title);
    }
    assert!(ctx.contains(&newer.id));
}

#[test]
fn a_new_handoff_supersedes_the_live_one_and_unpins_older_ones() {
    let (_d, v) = common::vault();
    let first = common::add(&v, "Handoff one", "handoff", Some("app"), &[("track", "main")]);
    pin(&v, &first.id);
    tick();
    let second = common::add(&v, "Handoff two", "handoff", Some("app"), &[("track", "main")]);
    assert_eq!(second.links, vec![Link { to: first.id.clone(), rel: "supersedes".into() }]);
    assert!(!v.get(&first.id).unwrap().pinned, "the superseded handoff is unpinned");
    pin(&v, &second.id);
    tick();
    // Touching an old handoff later must not make it the live one again.
    ops::append(&v, &first.id, "late remark").unwrap();
    let other = common::add(&v, "Other project handoff", "handoff", Some("web"), &[]);
    assert!(other.links.is_empty(), "handoffs of another project are not linked");
    let commits = breadcrumb::git::commit_count(&v.root);
    let third = ops::add(&v, AddReq { title: "Handoff three".into(), kind: Some("handoff".into()), project: Some("app".into()), track: Some("main".into()), body: Some("## Done\n\nParser.\n\n## Next\n\nLexer.".into()), ..Default::default() }).unwrap();
    assert_eq!(breadcrumb::git::commit_count(&v.root), commits + 1, "the unpin lands in the same commit");
    assert_eq!(third.links, vec![Link { to: second.id.clone(), rel: "supersedes".into() }]);
    assert!(!v.get(&second.id).unwrap().pinned);

    let ctx = ops::context(&v, Some("app"), 15).unwrap();
    assert!(ctx.contains(&format!("Latest handoff {} (", third.handle())) && ctx.contains(&format!("previous {}): Handoff three\nDone:\nParser.\nNext:\nLexer.\n", second.id)), "{ctx}");
    assert!(!ctx.contains(&first.id) && !ctx.contains(&other.id) && !ctx.contains('#'), "{ctx}");
}

#[test]
fn the_digest_fits_its_budget_with_every_pin_and_a_cut_handoff() {
    let (_d, v) = common::vault();
    for i in 0..20 {
        let n = common::add(&v, &format!("Standing rule number {i} with a title long enough to matter for the budget"), "decision", Some("app"), &[]);
        pin(&v, &n.id);
    }
    for i in 0..30 {
        common::add(&v, &format!("Fact {i}"), "note", Some("app"), &[]);
    }
    let body: String = (0..200).map(|i| format!("- step {i} of a long handoff that goes on\n")).collect();
    let h = ops::add(&v, AddReq { title: "Long handoff".into(), kind: Some("handoff".into()), project: Some("app".into()), body: Some(body), ..Default::default() }).unwrap();
    let ctx = ops::context(&v, Some("app"), 15).unwrap();
    assert!(ctx.chars().count() <= digest::DIGEST_BUDGET, "{} chars", ctx.chars().count());
    assert_eq!(ctx.matches(" decision Standing rule number").count(), 20, "{ctx}");
    assert!(ctx.contains(&format!("... (get {} for more)\n", h.id)) && ctx.contains("- step 0 of"), "{ctx}");
    assert!(ctx.contains("Recent (") && ctx.contains("Fact 29"), "recent notes still get room: {ctx}");

    let small = ops::context_with(&v, Some("app"), ContextOpts { budget: Some(2000), ..Default::default() }).unwrap();
    assert!(small.contains("more pinned: ") && small.contains(&h.id), "{small}");
}

#[test]
fn a_project_digest_counts_unscoped_globals_and_a_scope_brings_a_global_note_in() {
    let (d, v) = common::vault();
    let global = |title: &str, kind: &str, pinned: bool, fields: &[(&str, &str)]| {
        ops::add(&v, AddReq { title: title.into(), kind: Some(kind.into()), pinned, fields: fields.iter().map(|(k, x)| (k.to_string(), FieldInput::Raw(x.to_string()))).collect(), ..Default::default() }).unwrap()
    };
    let homelab = global("Homelab proxmox layout", "note", false, &[]);
    let pack = global("Biohazard pack VRAM crashes", "note", false, &[]);
    let cooking = global("Cookidoo dedupe recipes", "feedback", false, &[]);
    let rule = global("No AI attribution in commits", "feedback", true, &[]);
    let forge = global("Forge mappings need a rerun after an update", "gotcha", false, &[("scope", "create-*")]);
    let mod_pref = global("Mods ship with a changelog", "feedback", false, &[("scope", "web, create-laser")]);
    let srv = breadcrumb::mcp::Server { vault_root: v.root.clone(), cwd: d.path().to_path_buf() };
    let mut a = serde_json::Map::new();
    for (k, x) in [("title", serde_json::json!("Deploy with the staging key")), ("kind", "decision".into()), ("project", "global".into()), ("pinned", true.into()), ("fields", serde_json::json!({"scope": ["web", "api-*"]}))] {
        a.insert(k.into(), x);
    }
    srv.call_tool("add", &a).unwrap();
    let web_rule = v.load_notes().into_iter().find(|n| n.title == "Deploy with the staging key").unwrap();
    assert_eq!(breadcrumb::recall::scope_of(&web_rule), vec!["web", "api-*"]);
    for i in 0..3 {
        common::add(&v, &format!("Fact {i}"), "note", Some("create-laser"), &[]);
    }

    let ctx = ops::context(&v, Some("create-laser"), 15).unwrap();
    for gone in [&homelab, &pack, &cooking, &web_rule] {
        assert!(!ctx.contains(&gone.id), "{} must not show: {ctx}", gone.title);
    }
    assert!(ctx.contains(&format!("{} feedback (inferred) No AI attribution in commits (global)\n", rule.id)), "{ctx}");
    assert!(ctx.contains(&format!("Prefs (unpinned): {} Mods ship with a changelog, +1 global\n", mod_pref.id)), "{ctx}");
    assert!(ctx.contains(&format!("{} gotcha Forge mappings need a rerun after an update (global)\n", forge.id)), "{ctx}");
    assert!(ctx.ends_with("Global: 2 more unscoped notes, breadcrumb list --global lists all global notes\n"), "{ctx}");
    assert!(ctx.contains("Recent:\n"), "own notes and the scoped one all fit, so no count: {ctx}");

    let small = ops::context(&v, Some("create-laser"), 3).unwrap();
    assert!(small.contains("Recent (2 of 4):\n"), "of counts the project's own notes and scoped ones: {small}");

    let other = ops::context(&v, Some("app"), 15).unwrap();
    assert!(!other.contains(&forge.id) && !other.contains(&mod_pref.id) && other.contains("Prefs (unpinned): +1 global\n"), "{other}");
    assert!(ops::context(&v, Some("web"), 15).unwrap().contains(&format!("{} decision Deploy with the staging key (global)", web_rule.id)));

    let home = ops::context(&v, None, 15).unwrap();
    for shown in [&homelab, &pack, &cooking, &rule, &forge, &mod_pref, &web_rule] {
        assert!(home.contains(&shown.id), "{} shows in the global digest: {home}", shown.title);
    }
    assert!(!home.contains("Global:") && !home.contains("global\n"), "{home}");

    ops::update(&v, &forge.id, UpdateReq { unset: vec!["scope".into()], ..Default::default() }).unwrap();
    assert!(ops::context(&v, Some("create-laser"), 15).unwrap().contains("Global: 3 more unscoped notes, "));
}

#[test]
fn global_todos_and_backlog_items_follow_the_scope_rule_in_a_project_digest() {
    let (_d, v) = common::vault();
    let global = |title: &str, kind: &str, fields: &[(&str, &str)]| {
        ops::add(&v, AddReq { title: title.into(), kind: Some(kind.into()), fields: fields.iter().map(|(k, x)| (k.to_string(), FieldInput::Raw(x.to_string()))).collect(), ..Default::default() }).unwrap()
    };
    let todo = global("Renew the domain", "todo", &[]);
    let item = global("Vault wide cleanup", "task", &[("state", "queued")]);
    let scoped_todo = global("Bump the laser mod", "todo", &[("scope", "create-*")]);
    let scoped_item = global("Shared mod loader", "task", &[("state", "queued"), ("scope", "create-laser")]);
    common::add(&v, "Own task", "task", Some("create-laser"), &[("state", "queued")]);

    let ctx = ops::context(&v, Some("create-laser"), 15).unwrap();
    assert!(!ctx.contains(&todo.id) && !ctx.contains(&item.id), "{ctx}");
    assert!(ctx.contains(&format!("Todos:\nbump-laser-mod {} Bump the laser mod p2 (global)\nBacklog: queued 2\n", scoped_todo.id)), "{ctx}");
    assert!(ctx.contains(&scoped_item.id), "{ctx}");
    assert!(ctx.ends_with("Global: 2 more unscoped notes, breadcrumb list --global lists all global notes\n"), "the hidden ones are counted: {ctx}");

    let home = ops::context(&v, None, 15).unwrap();
    for shown in [&todo, &item, &scoped_todo, &scoped_item] {
        assert!(home.contains(&shown.id), "{home}");
    }
    assert!(!home.contains("Global:"), "{home}");
}

#[test]
fn a_global_item_in_review_stays_out_of_a_project_digests_review_line() {
    let (_d, mut v) = common::vault();
    breadcrumb::config::set(&mut v, "review_by", "agent").unwrap();
    let global = ops::add(&v, AddReq { title: "Vault wide cleanup".into(), kind: Some("task".into()), ..Default::default() }).unwrap();
    let own = common::add(&v, "Own task", "task", Some("create-laser"), &[]);
    let review = |id: &str, steps: &[&str]| {
        breadcrumb::backlog::move_item(&v, id, breadcrumb::backlog::MoveReq { state: "review".into(), steps: steps.iter().map(|s| s.to_string()).collect(), by: Some("builder".into()), ..Default::default() }).unwrap();
    };
    review(&global.id, &["Run it"]);
    review(&own.id, &["Run it"]);
    let ctx = ops::context(&v, Some("create-laser"), 15).unwrap();
    assert!(ctx.contains("Review: 1 waiting for an agent check (review_by = agent)"), "{ctx}");

    let user_only = common::add(&v, "Needs the user", "task", Some("create-laser"), &[]);
    review(&user_only.id, &["user: install it"]);
    breadcrumb::backlog::move_item(&v, &own.id, breadcrumb::backlog::MoveReq { state: "queued".into(), ..Default::default() }).unwrap();
    let ctx = ops::context(&v, Some("create-laser"), 15).unwrap();
    assert!(ctx.contains("Review: 0 waiting for an agent check, 1 waiting for the user"), "the global one is no agent check here: {ctx}");
}

#[test]
fn scope_matches_slugs_and_globs() {
    use breadcrumb::recall::scope_matches;
    let s = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert!(scope_matches(&s(&[]), "app"));
    assert!(scope_matches(&s(&["create-*"]), "create-laser") && scope_matches(&s(&["create-*"]), "Create-Laser"));
    assert!(!scope_matches(&s(&["create-*"]), "app") && !scope_matches(&s(&["web"]), "webapp"));
    assert!(scope_matches(&s(&["web", "*-mod"]), "neo-mod"));
}

#[test]
fn doctor_warns_once_about_a_global_scope_word_that_matches_no_project() {
    let (d, v) = common::vault();
    for p in ["app", "web"] {
        std::fs::create_dir_all(d.path().join(p)).unwrap();
        ops::project_link(&v, p, &d.path().join(p), None).unwrap();
    }
    let typo = common::add(&v, "Deploy with the staging key", "decision", None, &[("scope", "webb")]);
    let partly = common::add(&v, "Mods ship with a changelog", "feedback", None, &[("scope", "web, apx, zz-*")]);
    let fine = [common::add(&v, "Glob fits", "note", None, &[("scope", "a*")]), common::add(&v, "No scope", "note", None, &[]), common::add(&v, "Own note", "note", Some("app"), &[("scope", "nope")])];
    let mut gone = common::add(&v, "Archived typo", "note", None, &[("scope", "nope")]);
    gone.status = "archived".into();
    v.write_note(&mut gone).unwrap();

    let doc = breadcrumb::doctor::check(&v);
    let about = |id: &str| doc.iter().filter(|p| p.id.as_deref() == Some(id) && p.message.starts_with("scope ")).map(|p| p.message.clone()).collect::<Vec<_>>();
    assert_eq!(about(&typo.id), vec![format!("scope 'webb' matches no project, so only the global digest shows the note, projects: app, web, fix it with `breadcrumb update {} --field scope=<slugs>`", typo.id)]);
    assert_eq!(about(&partly.id), vec![format!("scope words 'apx', 'zz-*' match no project, projects: app, web, fix it with `breadcrumb update {} --field scope=<slugs>`", partly.id)]);
    for n in fine.iter().chain([&gone]) {
        assert!(about(&n.id).is_empty(), "{}: {doc:?}", n.title);
    }
}

#[test]
fn a_digest_over_budget_cuts_todo_and_board_rows_first_and_keeps_the_lines_that_ask_to_act() {
    use breadcrumb::question::{self, AskReq, Opt};
    let (_d, mut v) = common::vault();
    breadcrumb::config::set(&mut v, "review_by", "agent").unwrap();
    breadcrumb::config::set(&mut v, "judge_nudge", "session").unwrap();
    for i in 0..60 {
        common::add(&v, &format!("Todo {i:02} with a long title that runs on and on until the digest cuts it off"), "todo", Some("app"), &[]);
    }
    for i in 0..12 {
        common::add(&v, &format!("Feature {i:02} that someone is building, its title as long as they come"), "feature", Some("app"), &[("state", "in-progress")]);
    }
    let asked = common::add(&v, "Pick the palette", "task", Some("app"), &[("state", "queued")]);
    let options = ["red", "blue"].iter().map(|l| Opt { label: l.to_string(), desc: String::new() }).collect();
    let (n, q) = question::ask(&v, AskReq { id: asked.id.clone(), text: "Which colour?".into(), options, multi: false, free: false, by: None }).unwrap();
    question::answer(&v, &question::qid(&n, &q), &[2], None, "user").unwrap();
    let built = common::add(&v, "Thing to check", "feature", Some("app"), &[]);
    breadcrumb::backlog::move_item(&v, &built.id, breadcrumb::backlog::MoveReq { state: "review".into(), steps: vec!["Run it".into()], by: Some("builder".into()), ..Default::default() }).unwrap();
    let body = "Where it stands. ".repeat(150);
    ops::add(&v, AddReq { title: "Engine state".into(), kind: Some("handoff".into()), project: Some("app".into()), body: Some(body), ..Default::default() }).unwrap();

    for budget in [1000, 6000] {
        let d = digest::context_with(&v, Some("app"), ContextOpts { budget: Some(budget), ..Default::default() }).unwrap();
        assert!(d.chars().count() <= budget, "{budget}: {} chars\n{d}", d.chars().count());
        assert_eq!(breadcrumb::hooks::cap(d.trim_end(), budget), d.trim_end(), "the hook has nothing to cut");
        for line in [breadcrumb::judge::NUDGE, "Latest handoff", "Todos:\n", "Backlog: ", "answer arrived on", "Review: 1 waiting for an agent check"] {
            assert!(d.contains(line), "{budget}, {line}: {d}");
        }
        let more = |hint: &str| d.lines().find_map(|l| l.strip_suffix(hint).and_then(|l| l.strip_prefix('+')).and_then(|l| l.strip_suffix(" more, ")).map(|k| k.parse::<usize>().unwrap()));
        let todos = d.lines().filter(|l| l.starts_with("todo-")).count();
        let (hidden_todos, hidden_items) = (more("breadcrumb list -k todo").expect(&d), more("breadcrumb backlog").unwrap_or(0));
        assert!(hidden_items > 0 || budget == 6000, "{d}");
        assert_eq!(todos + hidden_todos, 60, "{d}");
        assert_eq!(d.lines().filter(|l| l.contains(" feature p2 Feature ") || l.contains(" task p2 Pick the palette")).count() + hidden_items, 13, "12 in progress and 1 queued: {d}");
        assert!(d.find("+").unwrap() < d.find("answer arrived on").unwrap() && d.find("answer arrived on").unwrap() < d.find("Review: 1 waiting").unwrap(), "{d}");
    }
    let wide = digest::context_with(&v, Some("app"), ContextOpts { budget: Some(20000), ..Default::default() }).unwrap();
    assert!(!wide.contains("more, breadcrumb list -k todo") && !wide.contains("more, breadcrumb backlog") && wide.contains("Todo 59"), "{wide}");
}

#[test]
fn a_digest_filled_by_pins_and_handoffs_still_fits_and_keeps_the_lines_that_ask_to_act() {
    let (_d, mut v) = common::vault();
    breadcrumb::config::set(&mut v, "review_by", "agent").unwrap();
    for i in 0..12 {
        let n = common::add(&v, &format!("Decision {i:02} that every session has to know about, written out at its full length here"), "decision", Some("app"), &[]);
        pin(&v, &n.id);
    }
    for t in 1..=4 {
        common::add(&v, &format!("State of track {t}, a handoff title that goes on for quite a while longer"), "handoff", Some("app"), &[("track", &format!("t{t}"))]);
    }
    for i in 0..20 {
        common::add(&v, &format!("Todo {i:02} with a title of some length"), "todo", Some("app"), &[]);
    }
    let built = common::add(&v, "Thing to check", "feature", Some("app"), &[]);
    breadcrumb::backlog::move_item(&v, &built.id, breadcrumb::backlog::MoveReq { state: "review".into(), steps: vec!["Run it".into()], by: Some("builder".into()), ..Default::default() }).unwrap();

    for budget in [700, 1000, 1500] {
        let d = digest::context_with(&v, Some("app"), ContextOpts { budget: Some(budget), ..Default::default() }).unwrap();
        assert!(d.chars().count() <= budget, "{budget}: {} chars\n{d}", d.chars().count());
        assert_eq!(breadcrumb::hooks::cap(d.trim_end(), budget), d.trim_end(), "the hook has nothing to cut");
        assert!(d.contains("Review: 1 waiting for an agent check") && d.starts_with("breadcrumb context: app"), "{budget}: {d}");
        assert!(budget == 1500 || d.ends_with(&format!("[cut to fit digest_budget = {budget}, lines are missing, `breadcrumb config set digest_budget <chars>` gives the digest more room]\n")), "{budget}: {d}");
    }
    let wide = digest::context_with(&v, Some("app"), ContextOpts { budget: Some(20000), ..Default::default() }).unwrap();
    assert!(!wide.contains("[cut to fit") && wide.contains("Decision 11") && wide.contains("Todo 19"), "{wide}");
}

#[test]
fn the_cut_to_fit_never_takes_an_answer_that_arrived_or_leaves_a_heading_alone() {
    use breadcrumb::question::{self, AskReq, Opt};
    let (_d, mut v) = common::vault();
    breadcrumb::config::set(&mut v, "review_by", "agent").unwrap();
    for i in 0..6 {
        let n = common::add(&v, &format!("Decision {i:02} that every session has to know about, written out at its full length here"), "decision", Some("app"), &[]);
        pin(&v, &n.id);
    }
    for t in 1..=3 {
        common::add(&v, &format!("State of track {t}, a handoff title that goes on for quite a while longer"), "handoff", Some("app"), &[("track", &format!("o{t}"))]);
    }
    ops::add(&v, AddReq { title: "Main state".into(), kind: Some("handoff".into()), project: Some("app".into()), body: Some("Where it stands. ".repeat(60)), ..Default::default() }).unwrap();
    for i in 0..4 {
        let t = common::add(&v, &format!("Task {i} with a question"), "task", Some("app"), &[("state", "queued")]);
        let options = ["this way", "that way"].iter().map(|l| Opt { label: l.to_string(), desc: String::new() }).collect();
        let text = format!("Question {i}: which of the two ways should this be built, given what the last session found out about it?");
        let (n, q) = question::ask(&v, AskReq { id: t.id.clone(), text, options, multi: false, free: false, by: None }).unwrap();
        if i < 3 {
            question::answer(&v, &question::qid(&n, &q), &[2], None, "user").unwrap();
        }
    }
    let built = common::add(&v, "Thing to check", "feature", Some("app"), &[]);
    breadcrumb::backlog::move_item(&v, &built.id, breadcrumb::backlog::MoveReq { state: "review".into(), steps: vec!["Run it".into()], by: Some("builder".into()), ..Default::default() }).unwrap();

    for budget in [1000, 1200, 1500] {
        let d = digest::context_with(&v, Some("app"), ContextOpts { budget: Some(budget), ..Default::default() }).unwrap();
        assert!(d.chars().count() <= budget, "{budget}: {} chars\n{d}", d.chars().count());
        assert_eq!(d.matches("answer arrived on ").count(), 3, "{budget}: {d}");
        assert!(budget != 1000 || d.contains("[cut to fit digest_budget = 1000"), "the tight budget is one that cuts: {d}");
        assert!(d.contains("Review: 1 waiting for an agent check"), "{budget}: {d}");
        for (i, l) in d.lines().enumerate() {
            let next = d.lines().nth(i + 1).unwrap_or_default();
            assert!(!["Other live handoffs:", "Todos:", "Pinned (get an id for its body):"].contains(&l) || !(next.is_empty() || next.starts_with("Backlog:") || next.starts_with("Latest handoff") || next.starts_with("[cut to fit")), "{budget}, a heading without rows: {d}");
        }
    }
}
