//! Command lines clap refuses, read the way an agent meant them (MCP tool and action words, MCP parameter names,
//! snake_case flags), or answered with the nearest valid command line.

use clap::Command;
use clap::error::{ContextKind, ContextValue, ErrorKind};

/// MCP tool and action pairs whose CLI form is one top-level command.
const MCP_PAIRS: &[(&str, &str, &str)] = &[
    ("question", "ask", "ask"),
    ("question", "list", "questions"),
    ("question", "answer", "answer"),
    ("question", "ack", "ack"),
    ("question", "withdraw", "unask"),
    ("backlog", "board", "board"),
    ("backlog", "review", "review"),
    ("backlog", "tree", "tree"),
    ("backlog", "deps", "deps"),
    ("board", "review", "review"),
    ("board", "tree", "tree"),
    ("board", "deps", "deps"),
    ("project", "config", "config"),
];

/// Words agents reach for that are another command here, the replacement goes in place of the word.
const VERBS: &[(&str, &str)] = &[
    ("green", "land green"),
    ("red", "land red"),
    ("approve", "land approve"),
    ("pushed", "land pushed"),
    ("pass", "review pass"),
    ("fail", "review fail"),
    ("note", "add"),
    ("save", "add"),
    ("remember", "add"),
    ("take", "claim"),
    ("start", "claim"),
    ("unclaim", "release"),
    ("rename", "update"),
    ("retitle", "update"),
    ("tag", "update"),
    ("done", "move"),
    ("close", "move"),
    ("reject", "move"),
];

/// Flag names agents reach for, and the flag that does it here.
const FLAGS: &[(&str, &str)] = &[
    ("type", "kind"),
    ("text", "body"),
    ("content", "body"),
    ("message", "body"),
    ("description", "body"),
    ("label", "tag"),
    ("labels", "tag"),
    ("reason", "resolution"),
    ("sha", "shipped-in"),
    ("commit", "ref"),
    ("pinned", "pin"),
];

/// A field of the backlog kinds, given as a flag to a command that takes --field.
const FIELD_FLAGS: &[&str] = &["priority", "state", "parent", "review_by", "review-by"];

/// The command line with every rewrite that applies, or None when nothing did.
pub fn rewrite(root: &Command, args: &[String]) -> Option<Vec<String>> {
    let mut out: Vec<String> = args.to_vec();
    for _ in 0..8 {
        let next = snake_flags(&out)
            .or_else(|| action_flag(root, &out))
            .or_else(|| mcp_pair(root, &out))
            .or_else(|| verdict_move(root, &out))
            .or_else(|| flag_to_positional(root, &out));
        match next {
            Some(n) if n != out => out = n,
            _ => break,
        }
    }
    (out != args).then_some(out)
}

/// What clap refused, for `suggest`.
pub enum Bad {
    Subcommand(String),
    Flag(String),
    Value(String, Vec<String>),
}

impl Bad {
    pub fn from_error(e: &clap::Error) -> Option<Bad> {
        let s = |k| match e.get(k) {
            Some(ContextValue::String(s)) => Some(s.clone()),
            _ => None,
        };
        match e.kind() {
            ErrorKind::InvalidSubcommand => s(ContextKind::InvalidSubcommand).map(Bad::Subcommand),
            ErrorKind::UnknownArgument => s(ContextKind::InvalidArg).map(Bad::Flag),
            ErrorKind::InvalidValue => {
                let valid = match e.get(ContextKind::ValidValue) {
                    Some(ContextValue::Strings(v)) => v.clone(),
                    _ => vec![],
                };
                s(ContextKind::InvalidValue).map(|v| Bad::Value(v, valid))
            }
            _ => None,
        }
    }
}

/// The nearest valid command line for what clap refused, as `breadcrumb ...`, or a hint naming the command that does it.
pub fn suggest(root: &Command, args: &[String], bad: &Bad) -> Option<String> {
    match bad {
        Bad::Subcommand(w) => {
            let i = args.iter().position(|a| a == w)?;
            let (cmds, _) = chain(root, &args[..i]);
            let parent = cmds.last()?;
            let lw = w.to_ascii_lowercase();
            let with = |repl: &str| line(&splice(args, i, 1, repl.split(' ')));
            if cmds.len() == 1
                && let Some((_, r)) = VERBS.iter().find(|(v, _)| *v == lw)
            {
                return Some(match lw.as_str() {
                    "done" | "close" | "reject" => {
                        let state = if lw == "reject" { "rejected" } else { "done" };
                        let extra = if lw == "reject" { " --resolution \"why\"" } else { "" };
                        let mut out = splice(args, i, 1, ["move"]);
                        out.push(state.into());
                        format!("{}{extra}", line(&out))
                    }
                    _ => with(r),
                });
            }
            let names: Vec<&str> = parent.get_subcommands().filter(|c| !c.is_hide_set()).map(|c| c.get_name()).collect();
            if let Some(n) = crate::util::nearest(w, names.iter().copied()) {
                return Some(with(n));
            }
            let aliases: Vec<(&str, &str)> = parent.get_subcommands().flat_map(|c| c.get_all_aliases().map(move |a| (a, c.get_name()))).collect();
            let a = crate::util::nearest(w, aliases.iter().map(|(a, _)| *a))?;
            aliases.iter().find(|(x, _)| *x == a).map(|(_, n)| with(n))
        }
        Bad::Flag(tok) => {
            let flag = tok.split('=').next().unwrap_or(tok);
            let name = flag.trim_start_matches('-');
            if !flag.starts_with('-') || name.is_empty() {
                return None;
            }
            let i = args.iter().position(|a| a == tok || a.split('=').next() == Some(flag))?;
            let (cmds, _) = chain(root, &args[..i]);
            let leaf = *cmds.last()?;
            let has = |l: &str| cmds.iter().flat_map(|c| c.get_arguments()).any(|a| a.get_long() == Some(l) || a.get_all_aliases().is_some_and(|al| al.contains(&l)));
            let inline = tok.split_once('=').map(|(_, v)| v.to_string());
            let lname = name.to_ascii_lowercase();
            if matches!(lname.as_str(), "link" | "links" | "blocks" | "depends-on" | "depends_on" | "relates") && leaf.get_name() != "link" {
                let rel = if lname.starts_with("link") || lname == "relates" { String::new() } else { format!(" --rel {}", lname.replace('_', "-")) };
                return Some(format!("links are their own command: breadcrumb link <id> <other-id>{rel}, after the note exists"));
            }
            if FIELD_FLAGS.contains(&lname.as_str()) && has("field") {
                let (val, take) = match inline {
                    Some(v) => (v, 1),
                    None => (args.get(i + 1).cloned().unwrap_or_else(|| "<value>".into()), 2),
                };
                let field = format!("{}={val}", lname.replace('-', "_"));
                return Some(line(&splice(args, i, take.min(args.len() - i), ["--field".to_string(), field])));
            }
            let target = FLAGS.iter().find(|(f, _)| *f == lname).map(|(_, t)| *t).filter(|t| has(t)).map(String::from).or_else(|| {
                let longs: Vec<&str> = cmds.iter().flat_map(|c| c.get_arguments()).filter_map(|a| a.get_long()).collect();
                crate::util::nearest(name, longs.iter().copied()).map(String::from)
            })?;
            let fixed = match inline {
                Some(v) => format!("--{target}={v}"),
                None => format!("--{target}"),
            };
            let mut out = args.to_vec();
            out[i] = fixed;
            Some(line(&out))
        }
        Bad::Value(v, valid) => {
            let n = crate::util::nearest(v, valid.iter().map(String::as_str))?;
            let i = args.iter().rposition(|a| a == v || a.ends_with(&format!("={v}")))?;
            let fixed = match args[i].split_once('=') {
                Some((f, _)) if args[i] != *v => format!("{f}={n}"),
                _ => n.to_string(),
            };
            Some(line(&splice(args, i, 1, [fixed])))
        }
    }
}

/// `breadcrumb` and the words, quoted where a shell would split them.
pub fn line(args: &[String]) -> String {
    let words: Vec<String> = args
        .iter()
        .map(|a| if a.is_empty() || a.chars().any(|c| c.is_whitespace() || "\"'$`;&|<>()*?!#~\\".contains(c)) { format!("\"{}\"", a.replace('"', "\\\"")) } else { a.clone() })
        .collect();
    format!("breadcrumb {}", words.join(" "))
}

/// The commands the words name, root first, and the index past the last command word.
fn chain<'a>(root: &'a Command, args: &[String]) -> (Vec<&'a Command>, usize) {
    let mut cmds = vec![root];
    let mut end = 0;
    let mut i = 0;
    while i < args.len() {
        let t = &args[i];
        if t == "--" {
            break;
        }
        if t.starts_with('-') && t.len() > 1 {
            i += if crate::cli::value_follows(&cmds, t) == Some(true) { 2 } else { 1 };
            continue;
        }
        let cur: &'a Command = cmds[cmds.len() - 1];
        match cur.find_subcommand(t) {
            Some(c) => {
                cmds.push(c);
                i += 1;
                end = i;
            }
            None => break,
        }
    }
    (cmds, end)
}

fn snake_flags(args: &[String]) -> Option<Vec<String>> {
    let stop = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let out: Vec<String> = args
        .iter()
        .enumerate()
        .map(|(i, a)| match a.strip_prefix("--").filter(|_| i < stop) {
            Some(f) => {
                let (name, rest) = f.split_once('=').map_or((f, None), |(n, v)| (n, Some(v)));
                let name = if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') { name.replace('_', "-") } else { name.to_string() };
                format!("--{name}{}", rest.map(|v| format!("={v}")).unwrap_or_default())
            }
            None => a.clone(),
        })
        .collect();
    (out != args).then_some(out)
}

/// The value of an unknown flag at `i`, and how many words the flag takes up.
fn flag_value(args: &[String], i: usize) -> Option<(String, usize)> {
    match args[i].split_once('=') {
        Some((_, v)) => Some((v.to_string(), 1)),
        None => args.get(i + 1).filter(|v| !v.starts_with("--")).map(|v| (v.clone(), 2)),
    }
}

fn unknown_flag(cmds: &[&Command], args: &[String], name: &str) -> Option<usize> {
    let i = args.iter().take_while(|a| *a != "--").position(|a| a.split('=').next() == Some(format!("--{name}").as_str()))?;
    (crate::cli::value_follows(cmds, &args[i]).is_none()).then_some(i)
}

/// MCP `action` or `view`, given as a flag: `question --action ask` is `question ask`.
fn action_flag(root: &Command, args: &[String]) -> Option<Vec<String>> {
    let (cmds, end) = chain(root, args);
    if cmds.len() < 2 {
        return None;
    }
    let i = unknown_flag(&cmds, args, "action").or_else(|| unknown_flag(&cmds, args, "view"))?;
    let (v, take) = flag_value(args, i)?;
    let mut out = args.to_vec();
    out.drain(i..i + take);
    out.insert(end, v);
    Some(out)
}

fn mcp_pair(root: &Command, args: &[String]) -> Option<Vec<String>> {
    let mut at = 0;
    while args.get(at).is_some_and(|a| a.starts_with('-')) {
        at += if crate::cli::value_follows(&[root], &args[at]) == Some(true) { 2 } else { 1 };
    }
    let tool = args.get(at)?.to_ascii_lowercase();
    let action = args.get(at + 1)?.to_ascii_lowercase();
    let (_, _, cli) = MCP_PAIRS.iter().find(|(t, a, _)| *t == tool && *a == action)?;
    Some(splice(args, at, 2, [*cli]))
}

/// `args` with `take` words at `i` replaced by `with`.
fn splice<S: Into<String>>(args: &[String], i: usize, take: usize, with: impl IntoIterator<Item = S>) -> Vec<String> {
    let mut out = args.to_vec();
    out.splice(i..i + take, with.into_iter().map(Into::into));
    out
}

/// MCP move with a verdict, which the CLI gives as `review pass` or `review fail`.
fn verdict_move(root: &Command, args: &[String]) -> Option<Vec<String>> {
    let (cmds, end) = chain(root, args);
    if cmds.len() != 2 || cmds[1].get_name() != "move" {
        return None;
    }
    let (mut pos, mut verdict, mut evidence, mut by) = (vec![], None, None, None);
    let mut i = end;
    while i < args.len() {
        let t = &args[i];
        match t.strip_prefix("--").map(|f| f.split('=').next().unwrap_or(f)) {
            None if !t.starts_with('-') => pos.push(t.clone()),
            Some(n @ ("verdict" | "evidence" | "reason" | "by")) => {
                let (v, take) = flag_value(args, i)?;
                match n {
                    "verdict" => verdict = Some(v.to_ascii_lowercase()),
                    "by" => by = Some(v),
                    _ => evidence = Some(v),
                }
                i += take;
                continue;
            }
            _ => return None,
        }
        i += 1;
    }
    let id = match pos.as_slice() {
        [id] | [id, _] => id.clone(),
        _ => return None,
    };
    let mut out = args[..end - 1].to_vec();
    out.push("review".into());
    match verdict?.as_str() {
        "pass" => {
            out.extend(["pass".into(), id]);
            if let Some(e) = evidence {
                out.extend(["--evidence".into(), e]);
            }
        }
        "fail" => out.extend(["fail".into(), id, "--reason".into(), evidence?]),
        _ => return None,
    }
    if let Some(b) = by {
        out.extend(["--by".into(), b]);
    }
    Some(out)
}

/// An MCP parameter given as a flag where the CLI takes it as a positional: `get --id x` is `get x`.
fn flag_to_positional(root: &Command, args: &[String]) -> Option<Vec<String>> {
    let (cmds, end) = chain(root, args);
    let leaf = *cmds.last()?;
    if cmds.len() < 2 {
        return None;
    }
    for p in leaf.get_positionals() {
        let id = p.get_id().as_str();
        let names = [id.to_string(), format!("{id}s"), id.trim_end_matches('s').to_string()];
        let Some(i) = names.iter().find_map(|n| unknown_flag(&cmds, args, n)) else { continue };
        let (v, take) = flag_value(args, i)?;
        let mut out = args.to_vec();
        out.drain(i..i + take);
        if p.get_index() == Some(1) {
            out.insert(end, v);
        } else {
            let at = out.iter().position(|a| a == "--").unwrap_or(out.len());
            out.insert(at, v);
        }
        return Some(out);
    }
    None
}
