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
