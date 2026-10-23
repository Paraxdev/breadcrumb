use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn breadcrumb(vault: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_breadcrumb"));
    c.arg("--vault").arg(vault).args(args).env_remove("BREADCRUMB_HOME").stdout(Stdio::piped()).stderr(Stdio::piped());
    c.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child = c.spawn().unwrap();
    if let Some(s) = stdin {
        child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn ok(vault: &Path, args: &[&str]) -> String {
    let o = breadcrumb(vault, args, None);
    assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}

fn json(vault: &Path, args: &[&str]) -> Value {
    let mut a = vec!["--json"];
    a.extend_from_slice(args);
    serde_json::from_str(&ok(vault, &a)).unwrap()
}

#[test]
fn cli_end_to_end() {
    let d = tempfile::tempdir().unwrap();
    let v = d.path().join("vault");
    assert!(ok(&v, &["init"]).contains("initialized"));
    assert!(ok(&v, &["init"]).contains("already complete"));

    let o = breadcrumb(&v, &["--json", "add", "--global", "--title", "From stdin", "--kind", "gotcha", "--tag", "x", "--tag", "y", "--body", "-"], Some("line one\r\nline two\r\n"));
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let added: Value = serde_json::from_slice(&o.stdout).unwrap();
    let id = added["id"].as_str().unwrap().to_string();
    assert_eq!(added["body"], "line one\nline two");

    let got = json(&v, &["get", &id[..4]]);
    assert_eq!(got["title"], "From stdin");
    assert!(ok(&v, &["get", &id]).starts_with("---\nid: "));

    ok(&v, &["update", &id, "--tag", "-x", "--tag", "+z", "--field", "severity=2", "--pin"]);
    let got = json(&v, &["get", &id]);
    assert_eq!(got["tags"], serde_json::json!(["y", "z"]));
    assert_eq!(got["severity"], 2);
    assert_eq!(got["pinned"], true);

    let list = json(&v, &["list", "--global", "--where", "severity=2"]);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let hits = json(&v, &["search", "--all", "stdin"]);
    assert_eq!(hits["hits"][0]["id"], id.as_str());
    assert!(ok(&v, &["context", "--global"]).contains(&format!("Pinned (get an id for its body):\n{id} gotcha From stdin\n")));

    let o = breadcrumb(&v, &["delete", &id], None);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--yes"));
    let o = breadcrumb(&v, &["--json", "get", "zzzz"], None);
    assert!(!o.status.success());
    assert!(serde_json::from_slice::<Value>(&o.stdout).unwrap()["error"].as_str().unwrap().contains("no note"));

    ok(&v, &["kind", "define", "--name", "runbook", "--description", "Ops", "--field", "service:string:required"]);
    assert!(ok(&v, &["kind", "show", "runbook"]).contains("service: string (required)"));
    let idea = json(&v, &["idea", "--global", "Try it"]);
    let iid = idea["id"].as_str().unwrap().to_string();
    ok(&v, &["queue", &iid]);
    assert!(ok(&v, &["next", "--global", "--claim", "--by", "cli"]).contains("claimed"));
    assert!(ok(&v, &["board", "--global"]).contains("in-progress (1)"));
    assert!(ok(&v, &["move", &iid, "rejected", "--resolution", "nah", "--ref", "run:wf_9"]).contains("-> rejected"));
    assert!(ok(&v, &["move", &iid, "rejected"]).contains("already in rejected"));
    assert_eq!(json(&v, &["list", "--global", "--ref", "run:wf_9"])[0]["id"], iid.as_str());
    ok(&v, &["update", &iid, "--ref", "-run:wf_9"]);
    assert!(json(&v, &["list", "--global", "--ref", "wf_9"]).as_array().unwrap().is_empty());
    assert!(ok(&v, &["delete", &id, "--yes"]).contains("deleted"));
    let guide = ok(&v, &["guide"]);
    assert!(guide.contains("context") && guide.chars().count() < 900);
    let st = json(&v, &["stats"]);
    for k in ["tools/list", "instructions", "hook", "context"] {
        assert!(st[k]["chars"].as_u64().unwrap() > 0 && st[k]["tokens"].as_u64().unwrap() > 0, "{st}");
    }
    assert!(ok(&v, &["mcp-config"]).starts_with("claude mcp add --scope user breadcrumb -- "));
    let w = json(&v, &["where"]);
    assert_eq!(w["exists"], true);
    assert_eq!(w["dirty"], false);
}
