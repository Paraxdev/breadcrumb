mod common;

use breadcrumb::backlog::{self, MoveReq};
use breadcrumb::vault::Vault;
use std::path::PathBuf;
use std::process::Command;

const A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const B: &str = "bbbbbbbb-0000-4000-8000-000000000002";

struct Env {
    _d: tempfile::TempDir,
    v: Vault,
    claude: PathBuf,
    work: PathBuf,
    _procs: Vec<common::Sleeper>,
}

/// Two live named sessions, and a work dir bound to project app.
fn setup(names: &[(&str, &str)]) -> Env {
    let (d, v) = common::vault();
    let claude = d.path().join("claude");
    std::fs::create_dir_all(claude.join("sessions")).unwrap();
    let work = d.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let mut e = Env { _d: d, v, claude, work, _procs: vec![] };
    for (id, name) in names {
        name_session(&mut e, id, name);
    }
    breadcrumb::ops::project_link(&e.v, "app", &e.work, None).unwrap();
    e
}

fn name_session(e: &mut Env, id: &str, name: &str) {
    let proc = common::Sleeper::start();
    let file = e.claude.join("sessions").join(format!("{}.json", &id[..8]));
    std::fs::write(file, format!(r#"{{"pid":{},"sessionId":"{id}","name":"{name}"}}"#, proc.pid())).unwrap();
    e._procs.push(proc);
}

fn crumb(e: &Env, session: &str, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_crumb"))
        .args(args)
        .current_dir(&e.work)
        .env("BREADCRUMB_HOME", &e.v.root)
        .env("BREADCRUMB_CLAUDE_DIR", &e.claude)
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

fn digest(e: &Env) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_crumb"))
        .args(["context", "-p", "app"])
        .current_dir(&e.work)
        .env("BREADCRUMB_HOME", &e.v.root)
        .env("BREADCRUMB_CLAUDE_DIR", &e.claude)
        .env("CLAUDE_CODE_SESSION_ID", "")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn tick() {
    std::thread::sleep(std::time::Duration::from_millis(1100));
}

fn mv(v: &Vault, id: &str, to: &str) {
    backlog::move_item(v, id, MoveReq { state: to.into(), resolution: Some("not needed".into()), ..Default::default() }).unwrap();
}

#[test]
fn append_drops_a_stamp_the_writer_typed_itself() {
    let mut e = setup(&[]);
    name_session(&mut e, A, "app-ee");
    let id = added_id(&crumb(&e, A, &["add", "-t", "Engine state", "-p", "app"]));
    let today = breadcrumb::util::fmt_date(&breadcrumb::util::now());
    crumb(&e, A, &["append", &id, &format!("{today} app-ee aaaaaaaa:\n{today} app-ee aaaaaaaa: Also running the bench.")]);
    crumb(&e, A, &["append", &id, &format!("{today} app-ee: fixed on main.")]);
    crumb(&e, A, &["append", &id, &format!("{today} build: green on dev.")]);
    let body = e.v.get(&id).unwrap().body;
    let stamp = format!("{today} app-ee aaaaaaaa:");
    assert_eq!(body.matches(&stamp).count(), 3, "{body}");
    assert!(body.contains(&format!("{stamp}\nAlso running the bench.")) && body.contains(&format!("{stamp}\nfixed on main.")), "{body}");
    assert!(body.contains(&format!("{stamp}\n{today} build: green on dev.")), "another word before the colon is the writer's text: {body}");
}

#[test]
fn moving_an_item_on_acknowledges_its_answer_and_old_answers_on_closed_items_leave_the_digest() {
    let e = setup(&[]);
    let item = common::add(&e.v, "Palette", "feature", Some("app"), &[]);
    crumb(&e, A, &["ask", &item.id, "Which palette?", "-o", "warm: reds", "-o", "cool: blues"]);
    crumb(&e, A, &["answer", &format!("{}.1", item.id), "2"]);
    let d = digest(&e);
    assert!(d.contains(&format!("answer arrived on {}.1 [backlog, 0m ago]", item.id)), "{d}");
    mv(&e.v, &item.id, "review");
    let d = digest(&e);
    assert!(!d.contains("answer arrived"), "{d}");
    assert!(e.v.get(&item.id).unwrap().body.contains("answer 1 acknowledged"));

    // What an old binary left: the item moved on after the answer and nothing acknowledged it.
    let old = common::add(&e.v, "Worktrees", "task", Some("app"), &[]);
    crumb(&e, A, &["ask", &old.id, "Remove the worktrees?", "-o", "yes: all", "-o", "no: keep"]);
    crumb(&e, A, &["answer", &format!("{}.1", old.id), "1"]);
    let mut n = e.v.get(&old.id).unwrap();
    n.set_field("state", serde_yaml::Value::String("done".into()));
    let later = (breadcrumb::util::now() + chrono::Duration::minutes(2)).format("%Y-%m-%dT%H:%MZ");
    n.body = backlog::append_history(&n.body, &format!("- {later}: state backlog -> done (by old)"));
    e.v.write_note(&mut n).unwrap();
    let d = digest(&e);
    assert!(!d.contains("answer arrived"), "{d}");
    assert!(backlog::next(&e.v, &breadcrumb::ops::Scope::Project { slug: "app".into(), with_global: false }, None, None).unwrap().is_none_or(|n| n.id != old.id));
}

#[test]
fn a_handoff_whose_items_landed_stops_leading_the_digest() {
    let e = setup(&[(A, "app-a"), (B, "app-b")]);
    let engine = common::add(&e.v, "Engine rewrite", "feature", Some("app"), &[]);
    let ui = common::add(&e.v, "Toolbar icons", "feature", Some("app"), &[]);
    let ui_slug = e.v.get(&ui.id).unwrap().slug().unwrap();
    let older = added_id(&crumb(&e, A, &["add", "-k", "handoff", "-t", "Engine state", "-p", "app", "--body", &format!("Continue {}.", engine.id)]));
    tick();
    let newer = added_id(&crumb(&e, B, &["add", "-k", "handoff", "-t", "UI state", "-p", "app", "--body", &format!("Resume {ui_slug} from the branch.")]));
    let d = digest(&e);
    assert!(d.contains(&format!("Latest handoff {} (", e.v.get(&newer).unwrap().handle())), "{d}");

    mv(&e.v, &ui.id, "review");
    let d = digest(&e);
    assert!(d.contains(&format!("Latest handoff {} (", e.v.get(&older).unwrap().handle())), "{d}");
    assert!(d.contains("UI state [settled, the 1 items it names landed]"), "{d}");

    mv(&e.v, &engine.id, "done");
    let d = digest(&e);
    assert!(!d.contains("Latest handoff"), "{d}");
    assert!(d.contains("Engine state [settled") && d.contains("UI state [settled"), "{d}");
}

#[test]
fn the_digest_shows_a_handoffs_summary_and_newest_update_first_and_says_when_it_went_quiet() {
    let e = setup(&[(A, "app-a")]);
    let id = added_id(&crumb(&e, A, &["add", "-k", "handoff", "-t", "Engine state", "-p", "app", "--body", "The copy path is half done.", "--field", "summary=Bench runs, then merge"]));
    crumb(&e, A, &["append", &id, "The copy path landed, the bench runs now."]);
    let d = digest(&e);
    let (s, upd, first) = (d.find("Summary: Bench runs, then merge").unwrap(), d.find("The copy path landed").unwrap(), d.find("First written:\nThe copy path is half done.").unwrap());
    assert!(s < upd && upd < first, "{d}");
    assert!(!d.contains("last written"), "{d}");

    let mut n = e.v.get(&id).unwrap();
    n.updated = breadcrumb::util::now() - chrono::Duration::hours(30);
    e.v.write_note(&mut n).unwrap();
    assert!(digest(&e).contains("last written 30h ago, check it still holds before resuming"));
}

#[test]
fn a_long_handoff_is_cut_after_a_sentence() {
    let e = setup(&[]);
    let long = "Short opener sentence here. ".repeat(200);
    common::add(&e.v, "Long state", "handoff", Some("app"), &[]);
    let id = e.v.load_notes().into_iter().find(|n| n.title == "Long state").unwrap().id;
    let mut n = e.v.get(&id).unwrap();
    n.body = long;
    e.v.write_note(&mut n).unwrap();
    let d = digest(&e);
    let line = d.lines().find(|l| l.contains("(get ")).unwrap();
    assert!(line.ends_with(&format!("here. ... (get {id} for more)")), "{line}");
}

#[test]
fn a_session_takes_a_track_and_the_digest_names_the_holder_apart_from_the_writer() {
    let mut e = setup(&[(B, "app-b")]);
    name_session(&mut e, A, "app-91");
    let id = added_id(&crumb(&e, A, &["add", "-k", "handoff", "-t", "Main worker state", "-p", "app", "--track", "main-worker"]));
    assert!(digest(&e).contains("app-91, track main-worker not taken"), "{}", digest(&e));

    let out = crumb(&e, B, &["claim", "track:main-worker"]);
    assert!(out.starts_with("took ") && out.contains("held by app-b"), "{out}");
    let n = e.v.get(&id).unwrap();
    assert_eq!(n.field("held_by").and_then(breadcrumb::note::yaml_scalar_string).as_deref(), Some(B));

    // The writer restarted under a new name, the digest keeps the name it wrote under.
    std::fs::remove_file(e.claude.join("sessions").join("aaaaaaaa.json")).unwrap();
    name_session(&mut e, A, "app-50");
    let d = digest(&e);
    assert!(d.contains("app-91 (now app-50), track main-worker held by app-b"), "{d}");

    let out = crumb(&e, B, &["release", &id]);
    assert!(out.starts_with("released "), "{out}");
    assert!(digest(&e).contains("track main-worker not taken"));
}

#[test]
fn only_the_holder_or_force_releases_a_handoff() {
    let e = setup(&[(A, "app-a"), (B, "app-b")]);
    let id = added_id(&crumb(&e, A, &["add", "-k", "handoff", "-t", "Main worker state", "-p", "app", "--track", "main-worker"]));
    assert!(crumb(&e, A, &["release", &id]).starts_with("released "), "nobody holds it, a quiet success");
    crumb(&e, B, &["claim", "track:main-worker"]);
    let out = Command::new(env!("CARGO_BIN_EXE_crumb"))
        .args(["release", &id])
        .current_dir(&e.work)
        .env("BREADCRUMB_HOME", &e.v.root)
        .env("BREADCRUMB_CLAUDE_DIR", &e.claude)
        .env("CLAUDE_CODE_SESSION_ID", A)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && err.contains("held by app-b, not ") && err.contains(&format!("breadcrumb release {id} --force")), "{err}");
    assert!(e.v.get(&id).unwrap().field("held_by").is_some());
    assert!(crumb(&e, A, &["release", &id, "--force"]).starts_with("released "));
    assert!(e.v.get(&id).unwrap().field("held_by").is_none());
    crumb(&e, B, &["claim", &id]);
    assert!(crumb(&e, B, &["release", "track:main-worker"]).starts_with("released "));
}

#[test]
fn kinds_say_which_to_use_and_add_hints_at_the_other_way() {
    let e = setup(&[]);
    assert!(crumb(&e, A, &["kinds"]).contains("Which kind: a raw idea nobody committed to is kind idea"));
    let out = crumb(&e, A, &["add", "-k", "feature", "-t", "Maybe dark mode", "-p", "app", "--field", "state=idea"]);
    assert!(out.contains("a raw idea is kind idea"), "{out}");
    let out = crumb(&e, A, &["add", "-k", "todo", "-t", "Fix the build", "-p", "app"]);
    assert!(out.contains("project work is kind task"), "{out}");
}

#[test]
fn claiming_a_handoff_takes_the_name_from_by_then_breadcrumb_session_then_the_session() {
    let e = setup(&[(B, "app-b")]);
    let id = added_id(&crumb(&e, A, &["add", "-k", "handoff", "-t", "Main worker state", "-p", "app", "--track", "main-worker"]));
    let held = |e: &Env| e.v.get(&id).unwrap().field("held_by").and_then(breadcrumb::note::yaml_scalar_string);
    let as_named = |named: &str, session: &str, args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_crumb"))
            .args(args)
            .current_dir(&e.work)
            .env("BREADCRUMB_HOME", &e.v.root)
            .env("BREADCRUMB_CLAUDE_DIR", &e.claude)
            .env("CLAUDE_CODE_SESSION_ID", session)
            .env("BREADCRUMB_SESSION", named)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };

    as_named("night-shift", "", &["claim", "track:main-worker"]);
    assert_eq!(held(&e).as_deref(), Some("night-shift"));
    as_named("night-shift", "", &["release", "track:main-worker"]);
    assert_eq!(held(&e), None);

    as_named("night-shift", B, &["claim", &id]);
    assert_eq!(held(&e).as_deref(), Some("night-shift"), "the name beats the session id");
    as_named("night-shift", B, &["claim", &id, "--by", "lead"]);
    assert_eq!(held(&e).as_deref(), Some("lead"), "--by beats both");

    as_named("", B, &["claim", &id]);
    assert_eq!(held(&e).as_deref(), Some(B), "with no name the live session holds it");
}

#[test]
fn a_handoff_a_newer_one_names_is_covered_and_never_leads() {
    let e = setup(&[(A, "app-a"), (B, "app-b")]);
    let ui = common::add(&e.v, "Toolbar icons", "feature", Some("app"), &[]);
    let palette = common::add(&e.v, "Palette", "feature", Some("app"), &[]);
    let older = added_id(&crumb(&e, A, &["add", "-k", "handoff", "-t", "Engine state", "-p", "app", "--body", &format!("Toolbar is {}.", ui.id)]));
    let older_slug = e.v.get(&older).unwrap().slug().unwrap();
    tick();
    let newer = added_id(&crumb(&e, B, &["add", "-k", "handoff", "-t", "UI state", "-p", "app", "--body", &format!("Carries on from {older_slug}, then {}.", palette.id)]));
    let newer_h = e.v.get(&newer).unwrap().handle();
    let d = digest(&e);
    assert!(d.contains(&format!("Latest handoff {newer_h} (")), "{d}");
    assert!(d.contains(&format!(": Engine state [covered by {newer_h}]\n")), "{d}");

    mv(&e.v, &ui.id, "review");
    let d = digest(&e);
    assert!(d.contains(&format!(": Engine state [covered by {newer_h}]\n")), "covered and settled show one mark: {d}");

    mv(&e.v, &palette.id, "done");
    let d = digest(&e);
    assert!(!d.contains("Latest handoff"), "a covered handoff never leads, like a settled one: {d}");
    assert!(d.contains(&format!(": Engine state [covered by {newer_h}]\n")) && d.contains(": UI state [settled, the 1 items it names landed]\n"), "{d}");
}

#[test]
fn the_digest_counts_commits_on_the_checkout_since_each_handoff() {
    let e = setup(&[(A, "app-a"), (B, "app-b")]);
    let commit = |m: &str| common::git(&e.work, &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "--allow-empty", "-m", m]);
    common::git(&e.work, &["init", "-q"]);
    added_id(&crumb(&e, B, &["add", "-k", "handoff", "-t", "UI state", "-p", "app"]));
    assert!(!digest(&e).contains("since"), "a repo with no commits yet counts nothing: {}", digest(&e));
    tick();
    commit("base");
    tick();
    let lead = added_id(&crumb(&e, A, &["add", "-k", "handoff", "-t", "Engine state", "-p", "app"]));
    let head = format!("Latest handoff {} ({}, app-a", e.v.get(&lead).unwrap().handle(), breadcrumb::util::fmt_date(&breadcrumb::util::now()));
    let d = digest(&e);
    assert!(d.contains(&format!("{head}): Engine state\n")), "0 commits shows nothing: {d}");
    assert!(d.contains(", 1 commit since: UI state\n"), "{d}");
    tick();
    commit("one");
    commit("two");
    let d = digest(&e);
    assert!(d.contains(&format!("{head}, 2 commits since): Engine state\n")) && d.contains(", 3 commits since: UI state\n"), "{d}");
    common::git(&e.work, &["checkout", "-q", "--detach"]);
    assert!(digest(&e).contains(", 3 commits since: UI state\n"), "a detached HEAD counts the same: {}", digest(&e));

    let mut n = e.v.get(&lead).unwrap();
    n.updated = breadcrumb::util::now() - chrono::Duration::hours(30);
    e.v.write_note(&mut n).unwrap();
    assert!(digest(&e).contains("last written 30h ago, 3 commits since, check it still holds before resuming"), "{}", digest(&e));

    // A second checkout of the project, the one holding the session's cwd is counted.
    let wt = e.work.parent().unwrap().join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    common::git(&wt, &["init", "-q"]);
    common::git(&wt, &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "--allow-empty", "-m", "wt"]);
    breadcrumb::ops::project_link(&e.v, "app", &wt, None).unwrap();
    let at = |cwd: &std::path::Path| breadcrumb::ops::context_with(&e.v, Some("app"), breadcrumb::ops::ContextOpts { cwd: Some(cwd.to_path_buf()), ..Default::default() }).unwrap();
    assert!(at(&wt.join("sub")).contains(", 1 commit since: UI state\n"), "{}", at(&wt));
    assert!(at(&e.work).contains(", 3 commits since: UI state\n") && at(e.work.parent().unwrap()).contains(", 3 commits since: UI state\n"));

    std::fs::rename(e.work.join(".git"), e.work.join("git-off")).unwrap();
    assert!(digest(&e).contains(", 1 commit since: UI state\n"), "a project path that is no checkout falls back to one that is: {}", digest(&e));
    std::fs::rename(wt.join(".git"), wt.join("git-off")).unwrap();
    assert!(!digest(&e).contains("since"), "no checkout, no count");
}

fn others_block(d: &str) -> Vec<String> {
    d.lines().skip_while(|l| *l != "Other live handoffs:").skip(1).take_while(|l| !l.starts_with(char::is_uppercase)).map(str::to_string).collect()
}

/// A handoff of project app, created and last written so many hours ago.
fn aged_handoff(v: &Vault, title: &str, body: &str, created_h: i64, written_h: i64) -> breadcrumb::note::Note {
    let n = breadcrumb::ops::add(v, breadcrumb::ops::AddReq { title: title.into(), kind: Some("handoff".into()), project: Some("app".into()), body: Some(body.into()), ..Default::default() }).unwrap();
    let mut n = v.get(&n.id).unwrap();
    n.created = breadcrumb::util::now() - chrono::Duration::hours(created_h);
    n.updated = breadcrumb::util::now() - chrono::Duration::hours(written_h);
    v.write_note(&mut n).unwrap();
    v.get(&n.id).unwrap()
}

#[test]
fn other_handoffs_show_the_age_of_their_last_write_and_open_ones_come_first() {
    let e = setup(&[]);
    let item = common::add(&e.v, "Engine rewrite", "feature", Some("app"), &[]);
    let open = aged_handoff(&e.v, "Open state", "Still half done.", 72, 2);
    let later = aged_handoff(&e.v, "Docs pass", "Halfway through.", 4, 4);
    for (i, t) in ["Settled one", "Settled two", "Settled three"].iter().enumerate() {
        aged_handoff(&e.v, t, &format!("Continue {}.", item.id), 5 - i as i64, 5 - i as i64);
    }
    let lead = aged_handoff(&e.v, "Current state", "Where we are.", 1, 1);
    mv(&e.v, &item.id, "review");
    let d = digest(&e);
    assert!(d.contains(&format!("Latest handoff {} (", lead.handle())), "{d}");
    let others = others_block(&d);
    assert_eq!(others.len(), 4, "{d}");
    assert_eq!(others[0], format!("{} 2h: Open state", open.handle()), "the open ones lead the list, by last write, the age shown: {d}");
    assert_eq!(others[1], format!("{} 4h: Docs pass", later.handle()), "{d}");
    assert!(others[2].ends_with(": Settled three [settled, the 1 items it names landed]"), "the marked ones after them: {d}");
    assert_eq!(others[3], "+2 more");

    let report = breadcrumb::lifecycle::handoff_report(&e.v, &lead, false);
    let listed: Vec<&str> = report.lines().filter(|l| l.starts_with("  ") && !l.starts_with("    ")).collect();
    assert!(listed[0].ends_with(": Open state") && listed[1].ends_with(": Docs pass") && listed[2].contains(": Settled three [settled"), "add lists open ones first too: {report}");
    assert!(listed[3].starts_with("  +2 more: "), "{report}");
}

#[test]
fn a_project_in_a_subdirectory_counts_only_commits_under_it() {
    let e = setup(&[]);
    let outer = e.work.join("outer");
    let app = outer.join("app");
    std::fs::create_dir_all(&app).unwrap();
    common::git(&outer, &["init", "-q"]);
    let commit = |file: &std::path::Path, m: &str| {
        std::fs::write(file, m).unwrap();
        common::git(&outer, &["add", "-A"]);
        common::git(&outer, &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "-m", m]);
    };
    commit(&outer.join("base.txt"), "base");
    breadcrumb::ops::project_link(&e.v, "sub", &app, None).unwrap();
    common::add(&e.v, "Sub state", "handoff", Some("sub"), &[]);
    tick();
    commit(&outer.join("elsewhere.txt"), "elsewhere");
    commit(&app.join("x.txt"), "inside");
    commit(&outer.join("elsewhere.txt"), "elsewhere again");
    let d = breadcrumb::ops::context_with(&e.v, Some("sub"), breadcrumb::ops::ContextOpts { cwd: Some(app.clone()), ..Default::default() }).unwrap();
    assert!(d.contains(", 1 commit since): Sub state\n"), "{d}");
}

/// A git.exe that answers every call from the real git but `log`, where it starts a grandchild and sleeps, like a
/// launcher style git that hangs. Windows only, that is where a grandchild keeps the hook's stdout open.
#[cfg(windows)]
#[test]
fn a_slow_git_and_what_it_started_are_ended_so_the_digest_returns() {
    let e = setup(&[]);
    common::git(&e.work, &["init", "-q"]);
    common::git(&e.work, &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "--allow-empty", "-m", "base"]);
    common::add(&e.v, "Engine state", "handoff", Some("app"), &[]);
    let path = std::env::var_os("PATH").unwrap();
    let real = std::env::split_paths(&path).map(|d| d.join("git.exe")).find(|p| p.is_file()).expect("git on PATH");
    let fake = e.work.parent().unwrap().join("fakegit");
    std::fs::create_dir_all(&fake).unwrap();
    let src = fake.join("git.rs");
    std::fs::write(
        &src,
        r#"use std::process::{Command, Stdio};
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.iter().any(|a| a == "log") {
        let s = Command::new(std::env::var("REAL_GIT").unwrap()).args(&args).status().unwrap();
        std::process::exit(s.code().unwrap_or(1));
    }
    Command::new("ping").args(["-n", "60", "127.0.0.1"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(60));
}
"#,
    )
    .unwrap();
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let built = Command::new(rustc).arg(&src).arg("-o").arg(fake.join("git.exe")).output().unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let mut paths = vec![fake.clone()];
    paths.extend(std::env::split_paths(&path));
    let start = std::time::Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_crumb"))
        .args(["context", "-p", "app"])
        .current_dir(&e.work)
        .env("BREADCRUMB_HOME", &e.v.root)
        .env("BREADCRUMB_CLAUDE_DIR", &e.claude)
        .env("REAL_GIT", &real)
        .env("PATH", std::env::join_paths(paths).unwrap())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let took = start.elapsed();
    let d = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success() && d.contains("Latest handoff") && !d.contains("since"), "{d}");
    assert!(took < std::time::Duration::from_secs(30), "the grandchild kept stdout open for {took:?}");
}
