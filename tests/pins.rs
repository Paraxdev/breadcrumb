mod common;

use breadcrumb::pins::reads_finished;
use breadcrumb::vault::Vault;
use std::process::Command;

fn crumb(v: &Vault, args: &[&str]) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_crumb")).args(args).env("BREADCRUMB_HOME", &v.root).env_remove("CLAUDE_CODE_SESSION_ID").env_remove("BREADCRUMB_SESSION").output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

fn pin(v: &Vault, title: &str, kind: &str, body: &str) -> String {
    let id = common::add(v, title, kind, Some("app"), &[]).id;
    let (ok, _, err) = crumb(v, &["update", &id, "--pin", "--body", body]);
    assert!(ok, "{err}");
    id
}

/// Writes through the CLI always stamp today, so only the file can hold an old date.
fn backdate(v: &Vault, id: &str, day: &str) {
    let path = v.resolve_path(id).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<String> = text
        .lines()
        .map(|l| {
            if l.starts_with("verified:") {
                format!("verified: {day}")
            } else if l.starts_with("updated:") {
                format!("updated: {day}T00:00:00Z")
            } else if l.starts_with("created:") {
                format!("created: {day}T00:00:00Z")
            } else {
                l.to_string()
            }
        })
        .collect();
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
}

fn days_ago(n: i64) -> String {
    (breadcrumb::util::now() - chrono::Duration::days(n)).format("%Y-%m-%d").to_string()
}

fn counts(doctor: &str) -> String {
    doctor.lines().last().unwrap().split(": ").nth(1).unwrap().split(", ").take(2).collect::<Vec<_>>().join(", ")
}

#[test]
fn a_status_line_of_its_own_reads_as_finished() {
    for body in [
        "Plan.\n\n2026-10-21 12:30: done. The panel found no blockers.",
        "2026-10-21: finished.",
        "2026-10-21 done on 2026-10-22",
        "Done. All merged.",
        "Finished on 2026-10-21",
        "Long ago. This decision is obsolete.",
        "This one time decision finished on 2026-10-21 and kept loading.",
        "This no longer applies.",
    ] {
        assert!(reads_finished(body).is_some(), "{body}");
    }
}

#[test]
fn a_word_used_in_passing_does_not() {
    for body in [
        "- done: on dev, in the development build, accepted",
        "A board state: done means landed on dev.",
        "Finish doc-session first, then the rest.",
        "How to apply: when an item is done move it to review.",
        "This is done by CI on every merge.",
        "2026-10-29: asked about the review items already in beta, done items stay.",
        "The old rule no longer applies, use the new palette.",
        "Done means accepted by the user.",
        "2026-10-22: landed part one, part two is next.",
    ] {
        assert_eq!(reads_finished(body), None, "{body}");
    }
}

#[test]
fn doctor_reports_flagged_pins_as_info_and_keeps_its_counts() {
    let (_d, v) = common::vault();
    let live = pin(&v, "Live rule", "decision", "Keep doing this.");
    let (_, before, _) = crumb(&v, &["doctor"]);
    assert!(!before.contains(&live), "{before}");

    let done = pin(&v, "One time", "decision", "Promote once.\n\n2026-10-21: done. It went fine.");
    let old = pin(&v, "Old preference", "feedback", "Stays true.");
    backdate(&v, &old, &days_ago(31));
    let (ok, after, err) = crumb(&v, &["doctor"]);
    assert!(ok, "{err}");
    let line = |id: &str| after.lines().find(|l| l.contains(id)).unwrap_or_default().to_string();
    assert!(line(&done).starts_with("info:") && line(&done).contains("reads as finished") && line(&done).contains("2026-10-21: done."), "{after}");
    assert!(line(&old).starts_with("info:") && line(&old).contains("not verified for 31 days"), "{after}");
    assert!(!after.contains(&live), "{after}");
    assert_eq!(counts(&before), counts(&after));
}

#[test]
fn the_finished_check_is_for_pinned_decisions_only() {
    let (_d, v) = common::vault();
    let note = pin(&v, "A pinned note", "note", "2026-10-21: done.");
    let unpinned = common::add(&v, "Unpinned", "decision", Some("app"), &[]).id;
    crumb(&v, &["update", &unpinned, "--body", "2026-10-21: done."]);
    let (_, out, _) = crumb(&v, &["doctor"]);
    assert!(!out.contains(&note) && !out.contains(&unpinned), "{out}");
}

#[test]
fn pin_review_days_comes_from_the_config_and_zero_turns_it_off() {
    let (_d, v) = common::vault();
    let id = pin(&v, "Some rule", "decision", "Keep doing this.");
    backdate(&v, &id, &days_ago(10));
    let toml = v.root.join("breadcrumb.toml");
    let base = std::fs::read_to_string(&toml).unwrap();
    assert!(!crumb(&v, &["doctor"]).1.contains(&id));
    assert!(base.contains("pin_review_days = 30"), "{base}");
    std::fs::write(&toml, base.replace("pin_review_days = 30", "pin_review_days = 7")).unwrap();
    assert!(crumb(&v, &["doctor"]).1.contains("not verified for 10 days"));
    std::fs::write(&toml, base.replace("pin_review_days = 30", "pin_review_days = 0")).unwrap();
    assert!(!crumb(&v, &["doctor"]).1.contains(&id));
    std::fs::write(&toml, base.replace("pin_review_days = 30\n", "")).unwrap();
    assert!(!crumb(&v, &["doctor"]).1.contains(&id), "an old vault's toml has no such key, the default is 30");
}

#[test]
fn pins_lists_every_pin_flagged_first_with_the_commands_and_changes_nothing() {
    let (_d, v) = common::vault();
    let fresh = pin(&v, "Fresh rule", "decision", "Keep doing this.");
    let old = pin(&v, "Old rule", "decision", "Keep doing this too.");
    let done = pin(&v, "Finished rule", "decision", "2026-10-21: done.");
    backdate(&v, &old, &days_ago(40));
    common::add(&v, "Not a pin", "decision", Some("app"), &[]);
    let path = v.resolve_path(&done).unwrap();
    let file = std::fs::read(&path).unwrap();
    let head = common::git(&v.root, &["rev-parse", "HEAD"]);

    let (ok, out, err) = crumb(&v, &["pins", "--project", "app"]);
    assert!(ok, "{err}");
    assert!(out.contains("3 pins, 2 flagged") && !out.contains("Not a pin"), "{out}");
    let at = |s: &str| out.find(s).unwrap_or_else(|| panic!("{s} in {out}"));
    assert!(at(&format!("{old} decision 40d")) < at(&done), "oldest flagged first\n{out}");
    assert!(at(&done) < at(&fresh), "{out}");
    assert!(out.contains("why: not verified for 40 days") && out.contains("why: decision reads as finished"), "{out}");
    for c in [format!("breadcrumb verify {done}"), format!("breadcrumb update {done} --unpin"), format!("breadcrumb archive {done}")] {
        assert!(out.contains(&c), "{c}\n{out}");
    }
    assert_eq!(std::fs::read(&path).unwrap(), file);
    assert_eq!(common::git(&v.root, &["rev-parse", "HEAD"]), head);

    let (_, json, _) = crumb(&v, &["--json", "pins", "--project", "app"]);
    let j: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(j["pins"].as_array().unwrap().len(), 3);
    assert_eq!(j["pins"][2]["id"], fresh.as_str());
    assert_eq!(j["pins"][2]["reasons"].as_array().unwrap().len(), 0);
    assert!(j["pins"][2]["commands"][0].as_str().unwrap().contains(&fresh));
}

#[test]
fn a_pin_never_verified_ages_from_its_creation() {
    let (_d, v) = common::vault();
    let id = pin(&v, "Hand written", "decision", "Keep doing this.");
    backdate(&v, &id, &days_ago(35));
    let path = v.resolve_path(&id).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let kept: Vec<&str> = text.lines().filter(|l| !l.starts_with("verified:")).collect();
    std::fs::write(&path, kept.join("\n") + "\n").unwrap();
    assert!(crumb(&v, &["doctor"]).1.contains("not verified for 35 days"));
}

#[test]
fn verifying_clears_the_age_flag() {
    let (_d, v) = common::vault();
    let id = pin(&v, "Still true", "decision", "Keep doing this.");
    backdate(&v, &id, &days_ago(40));
    assert!(crumb(&v, &["pins", "--project", "app"]).1.contains("1 flagged"));
    crumb(&v, &["verify", &id]);
    assert!(crumb(&v, &["pins", "--project", "app"]).1.contains("0 flagged"));
}

#[test]
fn an_append_or_update_does_not_reset_the_age() {
    let (_d, v) = common::vault();
    let id = pin(&v, "Old rule", "decision", "Keep doing this.");
    backdate(&v, &id, &days_ago(40));
    assert!(crumb(&v, &["append", &id, "Another line."]).0);
    assert!(crumb(&v, &["update", &id, "--title", "Old rule still"]).0);
    assert!(crumb(&v, &["pins", "--project", "app"]).1.contains("not verified for 40 days"));
    assert!(v.get(&id).unwrap().field("pin_verified").is_none());
}

#[test]
fn only_verify_sets_the_date_and_an_append_keeps_it() {
    let (_d, v) = common::vault();
    let id = pin(&v, "Old rule", "decision", "Keep doing this.");
    backdate(&v, &id, &days_ago(40));
    crumb(&v, &["verify", &id]);
    let today = days_ago(0);
    assert_eq!(v.get(&id).unwrap().field("pin_verified").and_then(|d| d.as_str()), Some(today.as_str()));
    crumb(&v, &["append", &id, "Another line."]);
    assert_eq!(v.get(&id).unwrap().field("pin_verified").and_then(|d| d.as_str()), Some(today.as_str()));

    let (ok, _, err) = crumb(&v, &["update", &id, "--field", "pin_verified=2020-01-01"]);
    assert!(!ok && err.contains("verify"), "{err}");
    assert!(!crumb(&v, &["update", &id, "--unset", "pin_verified"]).0);
}

#[test]
fn verified_still_moves_with_every_write_for_the_stale_check() {
    let (_d, v) = common::vault();
    let id = common::add(&v, "Plain note", "note", Some("app"), &[]).id;
    backdate(&v, &id, "2020-01-01");
    assert!(crumb(&v, &["doctor"]).1.contains(&id));
    crumb(&v, &["append", &id, "Touched."]);
    assert!(!crumb(&v, &["doctor"]).1.contains(&id));
}
