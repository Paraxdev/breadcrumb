mod common;

use breadcrumb::backlog::{self, MoveReq};
use breadcrumb::vault::Vault;
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

struct Repo {
    _d: tempfile::TempDir,
    root: std::path::PathBuf,
    /// A commit on the feature branch, the merge bringing it in, a second merge and a commit outside the range.
    side: String,
    merge: String,
    lonely_merge: String,
    before: String,
}

fn repo() -> Repo {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().to_path_buf();
    git(&root, &["init", "-q", "-b", "main"]);
    let commit = |msg: &str| {
        git(&root, &["commit", "-q", "--allow-empty", "-m", msg]);
        git(&root, &["rev-parse", "HEAD"])
    };
    let before = commit("base");
    git(&root, &["branch", "base"]);
    git(&root, &["checkout", "-q", "-b", "side"]);
    let side = commit("side work");
    git(&root, &["checkout", "-q", "main"]);
    commit("on main");
    git(&root, &["merge", "-q", "--no-ff", "-m", "merge: Side work", "side"]);
    let merge = git(&root, &["rev-parse", "HEAD"]);
    git(&root, &["checkout", "-q", "-b", "other", "base"]);
    commit("other work");
    git(&root, &["checkout", "-q", "main"]);
    git(&root, &["merge", "-q", "--no-ff", "-m", "merge: Nobody recorded this", "other"]);
    let lonely_merge = git(&root, &["rev-parse", "HEAD"]);
    Repo { _d: d, root, side, merge, lonely_merge, before }
}

fn crumb(v: &Vault, args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_crumb")).args(args).env("BREADCRUMB_HOME", &v.root).env_remove("CLAUDE_CODE_SESSION_ID").env_remove("BREADCRUMB_SESSION").output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

fn ship(v: &Vault, r: &Repo, extra: &[&str]) -> (bool, String, String) {
    let repo = r.root.to_str().unwrap();
    let mut args = vec!["ship", "base..main", "--repo", repo, "--project", "app"];
    args.extend(extra);
    crumb(v, &args)
}

fn item(v: &Vault, title: &str, state: &str, refs: &[String], shipped_in: Option<&str>) -> String {
    let id = common::add(v, title, "task", Some("app"), &[]).id;
    backlog::move_item(v, &id, MoveReq { state: state.into(), refs: refs.to_vec(), resolution: (state == "rejected").then(|| "no".to_string()), ..Default::default() }).unwrap();
    if let Some(s) = shipped_in {
        let (ok, _, err) = crumb(v, &["update", &id, "--field", &format!("shipped_in={s}")]);
        assert!(ok, "{err}");
    }
    id
}

fn commit_ref(sha: &str, len: usize) -> Vec<String> {
    vec![format!("commit:{}", &sha[..len])]
}

fn state_of(v: &Vault, id: &str) -> String {
    backlog::state(&v.get(id).unwrap())
}

fn vault_commits(v: &Vault) -> usize {
    git(&v.root, &["rev-list", "--count", "HEAD"]).parse().unwrap()
}

#[test]
fn a_release_note_is_one_line_set_by_update_and_move() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "Noted", "task", Some("app"), &[]).id;
    let (ok, _, err) = crumb(&v, &["update", &a, "--release-note", "Saving is faster\nnow"]);
    assert!(ok, "{err}");
    assert_eq!(v.get(&a).unwrap().field("release_note").and_then(|x| x.as_str()), Some("Saving is faster now"));

    let (ok, out, _) = crumb(&v, &["move", &a, "review", "--release-note", "Saving is much faster"]);
    assert!(ok && !out.contains("release note"), "{out}");
    assert_eq!(v.get(&a).unwrap().field("release_note").and_then(|x| x.as_str()), Some("Saving is much faster"));
    let (ok, _, _) = crumb(&v, &["move", &a, "review", "--release-note", "Same state, new line"]);
    assert!(ok);
    assert_eq!(v.get(&a).unwrap().field("release_note").and_then(|x| x.as_str()), Some("Same state, new line"), "a move within a state still writes it");
    let (ok, _, _) = crumb(&v, &["update", &a, "--unset", "release_note"]);
    assert!(ok);
    assert!(v.get(&a).unwrap().field("release_note").is_none());
}

#[test]
fn moving_to_review_hints_and_warns_but_never_refuses() {
    let (_d, v) = common::vault();
    let a = common::add(&v, "Bare", "task", Some("app"), &[]).id;
    let (ok, out, _) = crumb(&v, &["move", &a, "review", "--step", "Try"]);
    assert!(ok, "{out}");
    assert_eq!(out.lines().filter(|l| l.starts_with("hint:") && l.contains("no release note")).count(), 1, "{out}");
    assert_eq!(out.lines().filter(|l| l.starts_with("warning:") && l.contains("no commit: ref or shipped_in")).count(), 1, "{out}");

    let b = common::add(&v, "With refs", "task", Some("app"), &[]).id;
    let (ok, out, _) = crumb(&v, &["move", &b, "review", "--step", "Try", "--ref", "commit:abc1234", "--release-note", "Line"]);
    assert!(ok && !out.contains("hint:") && !out.contains("warning:"), "{out}");

    let c = common::add(&v, "With shipped_in", "task", Some("app"), &[]).id;
    crumb(&v, &["update", &c, "--field", "shipped_in=beta 1.0.386 (main 19fcf4af)"]);
    assert!(!crumb(&v, &["move", &c, "review", "--step", "Try"]).1.contains("no commit: ref"));

    let d = common::add(&v, "Elsewhere", "task", Some("app"), &[]).id;
    let (ok, out, _) = crumb(&v, &["move", &d, "in-progress"]);
    assert!(ok && !out.contains("hint:") && !out.contains("no commit: ref"), "{out}");
}

#[test]
fn a_range_lists_notes_then_items_then_shipped_then_merges_and_a_dry_run_writes_nothing() {
    let (_d, v) = common::vault();
    let r = repo();
    let noted = item(&v, "Noted item", "review", &commit_ref(&r.merge, 9), None);
    crumb(&v, &["update", &noted, "--release-note", "Side work now works"]);
    item(&v, "Plain item", "done", &commit_ref(&r.side, 7), None);
    item(&v, "Gone item", "shipped", &commit_ref(&r.merge, 40), None);
    item(&v, "Rejected item", "rejected", &commit_ref(&r.merge, 8), None);
    let outside = item(&v, "Outside item", "review", &commit_ref(&r.before, 9), None);
    let short = item(&v, "Short ref item", "review", &commit_ref(&r.merge, 6), None);
    let unknown = item(&v, "Unknown ref", "review", &["commit:1234567".to_string()], None);
    let foreign = common::add(&v, "Other project", "task", Some("elsewhere"), &[]).id;
    crumb(&v, &["update", &foreign, "--ref", &commit_ref(&r.merge, 9)[0]]);

    let commits = vault_commits(&v);
    let head = git(&r.root, &["rev-parse", "HEAD"]);
    let (ok, out, err) = ship(&v, &r, &[]);
    assert!(ok, "{err}");

    let at = |s: &str| out.find(s).unwrap_or_else(|| panic!("no {s} in {out}"));
    assert!(at("Release notes (1)") < at("Side work now works") && at("Side work now works") < at("No release note (1)"), "{out}");
    assert!(at("No release note (1)") < at("Plain item") && at("Plain item") < at("Already shipped or rejected, skipped (2)"), "{out}");
    assert!(at("Gone item") > at("skipped (2)") && at("Rejected item") > at("skipped (2)"), "{out}");
    assert!(at("Merges no item points at (1)") < at("merge: Nobody recorded this"), "{out}");
    assert!(!out.contains("merge: Side work"), "an item points at it: {out}");
    assert!(out.contains("[done]") && out.contains(&noted) && out.contains(&format!("{noted}, review")), "{out}");
    for absent in [&outside, &short, &unknown, &foreign] {
        assert!(!out.contains(absent.as_str()), "{absent}: {out}");
    }
    assert!(out.contains("Dry run"), "{out}");
    assert_eq!(vault_commits(&v), commits, "a dry run writes nothing");
    assert_eq!(state_of(&v, &noted), "review");
    assert_eq!(git(&r.root, &["rev-parse", "HEAD"]), head);
    assert!(!git(&r.root, &["log", "-1", "--format=%H", &r.lonely_merge]).is_empty());
}

#[test]
fn every_hex_word_of_shipped_in_counts() {
    let (_d, v) = common::vault();
    let r = repo();
    let sha = &r.merge;
    let a = item(&v, "Plain sha", "done", &[], Some(&sha[..8]));
    let b = item(&v, "Beta line", "done", &[], Some(&format!("beta 1.0.386 (main {})", &sha[..8])));
    let c = item(&v, "Sha then word", "done", &[], Some(&format!("{} beta", &sha[..8])));
    let d = item(&v, "Only a version", "done", &[], Some("beta 1.0.386"));
    let e = item(&v, "Six hex", "done", &[], Some(&sha[..6]));
    let (_, out, _) = ship(&v, &r, &[]);
    for hit in [&a, &b, &c] {
        assert!(out.contains(hit.as_str()), "{hit}: {out}");
    }
    assert!(!out.contains(&d) && !out.contains(&e), "{out}");
}

#[test]
fn yes_moves_review_and_done_in_one_vault_commit_and_records_who() {
    let (_d, v) = common::vault();
    let r = repo();
    let sha = commit_ref(&r.merge, 9);
    let rev = item(&v, "In review", "review", &sha, None);
    let done = item(&v, "Done", "done", &sha, None);
    let gone = item(&v, "Already shipped", "shipped", &sha, Some("beta 1"));
    let no = item(&v, "Rejected", "rejected", &sha, None);
    let rule = common::add(&v, "Holds until review ships", "note", Some("app"), &[]).id;
    crumb(&v, &["update", &rule, "--pin", "--expires", &format!("until:{rev}")]);

    let before = vault_commits(&v);
    let (ok, out, err) = ship(&v, &r, &["--yes", "--in", "beta 1.0.400", "--by", "lead"]);
    assert!(ok, "{err}");
    assert!(out.contains("Moved 2 items"), "{out}");
    assert_eq!(vault_commits(&v), before + 1, "one vault commit");
    for id in [&rev, &done] {
        let n = v.get(id).unwrap();
        assert_eq!(backlog::state(&n), "shipped");
        assert_eq!(n.field("shipped_in").and_then(|x| x.as_str()), Some("beta 1.0.400"));
        assert!(n.body.contains("-> shipped (by lead)"), "{}", n.body);
    }
    assert_eq!(v.get(&gone).unwrap().field("shipped_in").and_then(|x| x.as_str()), Some("beta 1"), "a shipped item stays as it is");
    assert_eq!(state_of(&v, &no), "rejected");
    assert!(!v.get(&rule).unwrap().pinned, "a rule until the item ended with it");

    let (ok, out, _) = ship(&v, &r, &["--yes"]);
    assert!(ok && out.contains("Moved 0 items"), "{out}");
    assert_eq!(vault_commits(&v), before + 1, "nothing to move, no commit");
}

#[test]
fn a_review_item_with_open_steps_says_so() {
    let (_d, v) = common::vault();
    let r = repo();
    let id = common::add(&v, "Unchecked", "task", Some("app"), &[]).id;
    crumb(&v, &["move", &id, "review", "--step", "One", "--step", "Two", "--ref", &commit_ref(&r.merge, 9)[0]]);
    let (_, out, _) = ship(&v, &r, &[]);
    assert!(out.contains("[review, 2 steps unchecked]"), "{out}");
}

#[test]
fn an_unknown_range_fails_and_one_ref_is_refused() {
    let (_d, v) = common::vault();
    let r = repo();
    let repo = r.root.to_str().unwrap();
    let id = item(&v, "Stays", "review", &commit_ref(&r.merge, 9), None);
    let (ok, _, err) = crumb(&v, &["ship", "nosuch..main", "--repo", repo, "--project", "app", "--yes"]);
    assert!(!ok && err.contains("could not read nosuch..main") && err.contains("nothing moved"), "{err}");
    assert_eq!(state_of(&v, &id), "review");
    let (ok, _, err) = crumb(&v, &["ship", "main", "--repo", repo, "--project", "app", "--yes"]);
    assert!(!ok && err.contains("origin/main..dev"), "{err}");
    assert_eq!(state_of(&v, &id), "review");
}

#[test]
fn an_unknown_ref_on_an_item_never_fails_the_range() {
    let (_d, v) = common::vault();
    let r = repo();
    let id = item(&v, "Points nowhere", "review", &["commit:deadbeef".to_string()], None);
    let (ok, _, err) = ship(&v, &r, &["--yes"]);
    assert!(ok, "{err}");
    assert_eq!(state_of(&v, &id), "review");
}

#[test]
fn a_dry_run_names_done_items_no_range_can_find_and_leaves_them() {
    let (_d, v) = common::vault();
    let r = repo();
    let lost = item(&v, "Done with no pointer", "done", &[], None);
    let refd = item(&v, "Done with a commit elsewhere", "done", &["commit:deadbeef".to_string()], None);
    let named = item(&v, "Done with a shipped_in word", "done", &[], Some("beta"));
    let open = item(&v, "Still in review", "review", &[], None);
    let (ok, out, err) = ship(&v, &r, &[]);
    assert!(ok, "{err}");
    let (head, tail) = out.split_once("Done with no commit ref or shipped_in").expect(&out);
    assert!(head.contains("Merges no item points at") && tail.contains(&lost) && tail.contains("Dry run"), "{out}");
    for other in [&refd, &named, &open] {
        assert!(!tail.contains(other.as_str()), "{out}");
    }
    assert_eq!(state_of(&v, &lost), "done");
    let (_, json, _) = crumb(&v, &["--json", "ship", "base..main", "--repo", r.root.to_str().unwrap(), "--project", "app"]);
    let j: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(j["unrecorded_done"][0]["id"], lost.as_str());
    let (_, wet, _) = ship(&v, &r, &["--yes"]);
    assert!(!wet.contains("Done with no commit ref"), "{wet}");
    assert_eq!(state_of(&v, &lost), "done");
}

#[test]
fn the_done_section_stays_away_when_nothing_is_unrecorded() {
    let (_d, v) = common::vault();
    let r = repo();
    item(&v, "Done and recorded", "done", &commit_ref(&r.merge, 9), None);
    let (_, out, _) = ship(&v, &r, &[]);
    assert!(!out.contains("Done with no commit ref"), "{out}");
}

#[test]
fn only_landed_items_move_and_the_rest_say_why() {
    let (_d, v) = common::vault();
    let r = repo();
    let sha = commit_ref(&r.merge, 9);
    let landed = item(&v, "Landed", "review", &sha, None);
    crumb(&v, &["move", &landed, "review", "--step", "Look at it"]);
    let failed = item(&v, "Failed review", "review", &sha, None);
    let (ok, _, err) = crumb(&v, &["review", "fail", &failed, "--reason", "broken"]);
    assert!(ok, "{err}");
    let held = item(&v, "Held elsewhere", "in-progress", &sha, None);
    crumb(&v, &["claim", &held, "--by", "lead-b"]);
    let idea = item(&v, "An idea", "idea", &sha, None);
    let asks = item(&v, "Asks the user", "review", &sha, None);
    crumb(&v, &["ask", &asks, "Which?", "-o", "a: one", "-o", "b: two"]);
    let epic = common::add(&v, "Parent epic", "epic", Some("app"), &[]).id;
    backlog::move_item(&v, &epic, MoveReq { state: "done".into(), refs: sha.clone(), ..Default::default() }).unwrap();
    let child = common::add(&v, "Open child", "task", Some("app"), &[]).id;
    crumb(&v, &["update", &child, "--field", &format!("parent={epic}")]);

    let (ok, out, err) = ship(&v, &r, &["--yes"]);
    assert!(ok, "{err}");
    assert!(out.contains("Moved 1 items"), "{out}");
    assert!(out.contains("1 of these in review still have unchecked steps, they ship anyway"), "{out}");
    assert!(out.contains("[review, 1 step unchecked]"), "{out}");
    assert_eq!(state_of(&v, &landed), "shipped");
    let head = out.find("Matched but not landed, not moved (5)").unwrap_or_else(|| panic!("{out}"));
    for (id, state, why) in [(&failed, "queued", "not in review or done"), (&held, "in-progress", "held by lead-b"), (&idea, "idea", "not in review or done"), (&asks, "review", "open question"), (&epic, "done", "epic with 1 open children")] {
        assert_eq!(state_of(&v, id), state);
        let line = out[head..].lines().find(|l| l.contains(id.as_str())).unwrap_or_else(|| panic!("{id}: {out}"));
        assert!(line.contains(&format!("[{state}")) && line.contains(why), "{line}");
    }
    let (ok, json, _) = ship(&v, &r, &["--json"]);
    assert!(ok && json.contains("not_moved") && json.contains("reason"), "{json}");

    crumb(&v, &["move", &child, "rejected", "--resolution", "no"]);
    let (_, out, _) = ship(&v, &r, &["--yes"]);
    assert!(out.contains("Moved 1 items"), "an epic moves once its children closed: {out}");
    assert_eq!(state_of(&v, &epic), "shipped");
}
