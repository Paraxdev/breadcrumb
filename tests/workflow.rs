mod common;

use breadcrumb::note::yaml_scalar_string;
use breadcrumb::ops::{self, Scope};
use breadcrumb::vault::Vault;
use breadcrumb::workflow::{self, State};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const SCRIPT: &str = r#"// Reviews a change set.
export const meta = {
  name: 'review-changes',
  description: "Review the diff, then fix what it finds",
  whenToUse: `after a feature branch is ready`,
  phases: [
    { title: 'Scan', detail: "it's quick" },
    { title: "Fix" },
  ],
};

export default async function run(ctx) {
  return { ok: true };
}
"#;

fn commits(v: &Vault) -> usize {
    common::git(&v.root, &["rev-list", "--count", "HEAD"]).trim().parse().unwrap()
}

fn setup() -> (tempfile::TempDir, Vault, PathBuf, PathBuf) {
    let (d, v) = common::vault();
    let proj = d.path().join("demo-app");
    let wf_dir = proj.join(".claude").join("workflows");
    fs::create_dir_all(&wf_dir).unwrap();
    ops::project_link(&v, "demo-app", &proj, None).unwrap();
    let file = wf_dir.join("review.js");
    fs::write(&file, SCRIPT).unwrap();
    (d, v, proj, file)
}

#[test]
fn meta_parsing_is_tolerant() {
    let m = workflow::parse_meta(SCRIPT, "review");
    assert_eq!(m.name, "review-changes");
    assert_eq!(m.description, "Review the diff, then fix what it finds");
    assert_eq!(m.when_to_use, "after a feature branch is ready");
    assert_eq!(m.phases, vec!["Scan", "Fix"]);

    let m = workflow::parse_meta("export const meta = { \"description\": 'Say \\'hi\\'' }", "greet");
    assert_eq!(m.name, "greet");
    assert_eq!(m.description, "Say 'hi'");
    assert!(m.phases.is_empty());

    let m = workflow::parse_meta("export default function () {}", "bare");
    assert_eq!(m, workflow::Meta { name: "bare".into(), ..Default::default() });

    let m = workflow::parse_meta("export const meta = { name: 'unclosed', description: 'still read'", "x");
    assert_eq!((m.name.as_str(), m.description.as_str()), ("unclosed", "still read"));

    assert_eq!(workflow::note_title(&workflow::parse_meta(SCRIPT, "review")), "review-changes: Review the diff, then fix what it finds");
}

#[test]
fn save_is_idempotent_and_updates_one_note() {
    let (_d, v, proj, file) = setup();
    let before = commits(&v);
    let saved = workflow::save(&v, std::slice::from_ref(&file), None, false, &proj).unwrap();
    assert_eq!(saved[0].action, "added");
    assert_eq!(saved[0].project.as_deref(), Some("demo-app"));
    assert_eq!(saved[0].script, "projects/demo-app/workflows/review.js");
    assert_eq!(commits(&v), before + 1);
    assert_eq!(fs::read_to_string(v.root.join(&saved[0].script)).unwrap(), SCRIPT);

    let n = v.get(&saved[0].note_id).unwrap();
    assert_eq!(n.kind, "workflow");
    assert!(n.has_tag("workflow"));
    assert_eq!(n.field("workflow_name").and_then(yaml_scalar_string).as_deref(), Some("review"));
    assert_eq!(n.field("when_to_use").and_then(yaml_scalar_string).as_deref(), Some("after a feature branch is ready"));
    assert!(n.body.contains(".claude/workflows/review.js"));
    assert!(n.body.contains("- Scan"));

    let other_cwd = tempfile::tempdir().unwrap();
    let again = workflow::save(&v, std::slice::from_ref(&file), Some("demo-app"), false, other_cwd.path()).unwrap();
    assert_eq!(again[0].action, "unchanged");
    assert_eq!(commits(&v), before + 1);

    let detected = workflow::save(&v, std::slice::from_ref(&file), None, false, other_cwd.path());
    assert_eq!(detected.unwrap()[0].action, "unchanged", "the project is detected from the file location");

    fs::write(&file, SCRIPT.replace("Review the diff", "Review every diff")).unwrap();
    let upd = workflow::save(&v, &[file], None, false, &proj).unwrap();
    assert_eq!(upd[0].action, "updated");
    assert_eq!(upd[0].note_id, saved[0].note_id);
    assert_eq!(commits(&v), before + 2);
    let notes: Vec<_> = v.load_notes().into_iter().filter(|n| n.kind == "workflow").collect();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].title.contains("Review every diff"));

    let ctx = ops::context(&v, Some("demo-app"), 10).unwrap();
    assert!(ctx.contains("Workflows: review (Review every diff"), "{ctx}");

    let lone = tempfile::tempdir().unwrap();
    let stray = lone.path().join("stray.js");
    fs::write(&stray, "export default 1").unwrap();
    assert!(workflow::save(&v, std::slice::from_ref(&stray), None, false, lone.path()).unwrap_err().to_string().contains("no project detected"));
    let g = workflow::save(&v, &[stray], None, true, lone.path()).unwrap();
    assert_eq!(g[0].script, "global/workflows/stray.js");
    assert_eq!(workflow::list(&v, &Scope::All).len(), 2);
    assert_eq!(workflow::list(&v, &Scope::Global).len(), 1);
}

#[test]
fn install_refuses_to_overwrite_differing_files() {
    let (d, v, proj, file) = setup();
    workflow::save(&v, &[file], None, false, &proj).unwrap();

    let clone = d.path().join("demo-app-clone");
    fs::create_dir_all(&clone).unwrap();
    let r = workflow::install(&v, "demo-app", &clone, false).unwrap();
    assert_eq!(r.installed, vec!["review"]);
    let local = clone.join(".claude/workflows/review.js");
    assert_eq!(fs::read_to_string(&local).unwrap(), SCRIPT);

    let r = workflow::install(&v, "demo-app", &clone, false).unwrap();
    assert_eq!(r.unchanged, vec!["review"]);

    fs::write(&local, "local edit").unwrap();
    let r = workflow::install(&v, "demo-app", &clone, false).unwrap();
    assert_eq!(r.differ, vec!["review"]);
    assert!(r.installed.is_empty());
    assert_eq!(fs::read_to_string(&local).unwrap(), "local edit");
    assert!(workflow::install_text(&r).contains("--force"));

    let r = workflow::install(&v, "demo-app", &clone, true).unwrap();
    assert_eq!(r.installed, vec!["review"]);
    assert_eq!(fs::read_to_string(&local).unwrap(), SCRIPT);

    assert_eq!(workflow::target_dir(&v, "demo-app", None, &proj.join(".claude")).unwrap(), PathBuf::from(breadcrumb::util::path_str(&proj)));
    let elsewhere = tempfile::tempdir().unwrap();
    assert_eq!(workflow::target_dir(&v, "demo-app", None, elsewhere.path()).unwrap(), PathBuf::from(breadcrumb::util::path_str(&proj)));
    assert_eq!(workflow::target_dir(&v, "demo-app", Some(Path::new("sub")), elsewhere.path()).unwrap(), elsewhere.path().join("sub"));
}

#[test]
fn status_reports_every_state() {
    let (_d, v, proj, file) = setup();
    let wf = proj.join(".claude/workflows");
    for name in ["older", "gone"] {
        fs::write(wf.join(format!("{name}.js")), format!("export const meta = {{ name: '{name}' }}")).unwrap();
    }
    let all: Vec<PathBuf> = ["review", "older", "gone"].iter().map(|n| wf.join(format!("{n}.js"))).collect();
    workflow::save(&v, &all, None, false, &proj).unwrap();

    fs::write(&file, "edited after save").unwrap();
    fs::write(wf.join("older.js"), "stale copy").unwrap();
    let older = fs::File::options().write(true).open(wf.join("older.js")).unwrap();
    older.set_modified(SystemTime::now() - Duration::from_secs(86_400 * 30)).unwrap();
    drop(older);
    fs::remove_file(wf.join("gone.js")).unwrap();
    fs::write(wf.join("draft.js"), "export default 0").unwrap();
    fs::write(wf.join("same.js"), "export default 1").unwrap();
    workflow::save(&v, &[wf.join("same.js")], None, false, &proj).unwrap();

    let st = workflow::status(&v, "demo-app", &proj).unwrap();
    let state = |n: &str| st.iter().find(|s| s.name == n).map(|s| s.state);
    assert_eq!(state("review"), Some(State::LocalNewer));
    assert_eq!(state("older"), Some(State::VaultNewer));
    assert_eq!(state("gone"), Some(State::MissingLocally));
    assert_eq!(state("draft"), Some(State::OnlyLocal));
    assert_eq!(state("same"), Some(State::Same));
    assert_eq!(st.len(), 5);
}

#[test]
fn init_adds_the_workflow_kind_to_existing_vaults() {
    let (_d, v) = common::vault();
    let k = v.root.join("kinds/workflow.toml");
    assert!(k.exists());
    fs::remove_file(&k).unwrap();
    let (_, rep) = Vault::init(&v.root, None).unwrap();
    assert_eq!(rep.created_files, vec!["kinds/workflow.toml"]);
    let kd = v.require_kind("workflow").unwrap();
    let names: Vec<&str> = kd.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["script", "workflow_name", "when_to_use"]);
    let (_, rep) = Vault::init(&v.root, None).unwrap();
    assert!(rep.created_files.is_empty());
}
