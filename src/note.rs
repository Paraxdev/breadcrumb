use crate::util::{fmt_time, normalize_newlines, now, parse_time, path_str};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};
use std::path::PathBuf;

pub const RESERVED_KEYS: [&str; 11] = [
    "id", "title", "kind", "project", "tags", "links", "status", "pinned", "created", "updated", "source",
];

/// Keys the JSON output (get --json, export jsonl, the web API) adds next to the frontmatter.
pub const JSON_KEYS: [&str; 3] = ["body", "path", "record"];

pub const DEFAULT_REL: &str = "relates";

/// Optional fields breadcrumb itself maintains: file globs a note applies to, and the date it was last confirmed.
pub const PATHS_KEY: &str = "paths";
pub const VERIFIED_KEY: &str = "verified";
/// The date only `verify` sets, `verified` moves on every write so it says nothing about a pin's age.
pub const PIN_VERIFIED_KEY: &str = "pin_verified";
/// Typed pointers to things outside the vault, `type:value` like commit:abc123, run:wf_1, branch:fix/x, pr:12.
pub const REFS_KEY: &str = "refs";
/// When the note stops holding: a date, a time or `until:<id>`. See lifecycle.
pub const EXPIRES_KEY: &str = "expires";
pub const SAID_KEY: &str = "said";
pub const SAID_ON_KEY: &str = "said_on";

pub fn is_commit_hex(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Trimmed `type:value` entries with a lowercase type, deduplicated in order, a bare sha reads as `commit:<sha>`.
pub fn normalize_refs(raw: &[String]) -> anyhow::Result<Vec<String>> {
    let mut out: Vec<String> = vec![];
    for r in raw {
        let r = r.trim();
        if r.is_empty() {
            continue;
        }
        let bare = is_commit_hex(r).then(|| format!("commit:{}", r.to_ascii_lowercase()));
        let Some((k, val)) = bare.as_deref().unwrap_or(r).split_once(':') else {
            anyhow::bail!("ref '{r}' must be type:value, like commit:abc123, run:wf_1, branch:fix/x or pr:12");
        };
        let k = k.trim().to_ascii_lowercase();
        let val = val.trim();
        let val = if k == "pr" { val.trim_start_matches('#') } else { val };
        if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') || val.is_empty() || val.contains(char::is_whitespace) {
            anyhow::bail!("ref '{r}' must be type:value with a word type and a value without spaces");
        }
        let s = format!("{k}:{val}");
        if !out.contains(&s) {
            out.push(s);
        }
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub to: String,
    #[serde(default = "default_rel")]
    pub rel: String,
}

fn default_rel() -> String {
    DEFAULT_REL.to_string()
}

#[derive(Clone, Debug)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub project: Option<String>,
    pub tags: Vec<String>,
    pub links: Vec<Link>,
    pub status: String,
    pub pinned: bool,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub source: Option<String>,
    /// Kind fields and free-form keys, kept in file order so they round trip verbatim.
    pub extra: Mapping,
    pub body: String,
    pub path: Option<PathBuf>,
    pub had_frontmatter: bool,
    pub warnings: Vec<String>,
}

pub fn yaml_scalar_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Tagged(t) => yaml_scalar_string(&t.value),
        _ => None,
    }
}

/// Flattens any YAML value into display text (lists joined with ", ").
pub fn yaml_display(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Sequence(s) => s.iter().map(yaml_display).collect::<Vec<_>>().join(", "),
        Value::Mapping(_) => serde_yaml::to_string(v).unwrap_or_default().trim().replace('\n', "; "),
        other => yaml_scalar_string(other).unwrap_or_default(),
    }
}

fn first_heading(body: &str) -> Option<String> {
    body.lines()
        .map(str::trim)
        .find(|l| l.starts_with('#'))
        .map(|l| l.trim_start_matches('#').trim().to_string())
        .filter(|s| !s.is_empty())
}

pub const INVALID_YAML: &str = "invalid YAML frontmatter";

/// Hand-written frontmatter often has values like `description: "a" then b` that strict YAML rejects.
/// Each `key: value` line that does not parse on its own is re-read as a single-quoted string.
fn lenient_yaml(fm: &str) -> Option<Value> {
    let mut out = String::new();
    for line in fm.lines() {
        let t = line.trim_start();
        let indent = &line[..line.len() - t.len()];
        let fixed = match t.split_once(':') {
            Some((k, v))
                if !k.is_empty()
                    && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                    && !v.trim().is_empty()
                    && serde_yaml::from_str::<Value>(t).is_err() =>
            {
                format!("{indent}{k}: '{}'", v.trim().replace('\'', "''"))
            }
            _ => line.to_string(),
        };
        out.push_str(&fixed);
        out.push('\n');
    }
    serde_yaml::from_str(&out).ok()
}

/// Splits `---` frontmatter from the body. Input must already be LF normalized.
pub fn split_frontmatter(text: &str) -> Option<(String, String)> {
    let rest = text.strip_prefix("---\n").or_else(|| if text == "---" { Some("") } else { None })?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let l = line.trim_end_matches('\n').trim_end();
        if l == "---" || l == "..." {
            let fm = rest[..offset].to_string();
            let body = rest[offset + line.len()..].to_string();
            return Some((fm, body));
        }
        offset += line.len();
    }
    None
}

impl Note {
    pub fn new(id: &str, title: &str, kind: &str) -> Note {
        let t = now();
        Note {
            id: id.to_string(),
            title: title.to_string(),
            kind: kind.to_string(),
            project: None,
            tags: vec![],
            links: vec![],
            status: "active".to_string(),
            pinned: false,
            created: t,
            updated: t,
            source: None,
            extra: Mapping::new(),
            body: String::new(),
            path: None,
            had_frontmatter: true,
            warnings: vec![],
        }
    }

    /// Parses a note leniently. `file_stem` is used for id and title fallbacks.
    pub fn parse(text: &str, file_stem: Option<&str>) -> Result<Note> {
        let text = normalize_newlines(text);
        let stem_id = file_stem.and_then(crate::id::id_from_file_name);
        let (fm, body) = match split_frontmatter(&text) {
            Some((fm, body)) => (Some(fm), body),
            None => (None, text.clone()),
        };
        let body = body.trim_start_matches('\n').trim_end().to_string();
        let mut note = Note::new("", "", "note");
        note.body = body;
        let parsed = match &fm {
            Some(fm) if !fm.trim().is_empty() => match serde_yaml::from_str::<Value>(fm) {
                Ok(v) => v,
                Err(strict) => match lenient_yaml(fm) {
                    Some(v) => {
                        note.warnings.push(format!("{INVALID_YAML}: {strict}, values were read as plain strings"));
                        v
                    }
                    None => return Err(strict).context(INVALID_YAML),
                },
            },
            _ => Value::Null,
        };
        let mut map = match parsed {
            Value::Mapping(m) => m,
            Value::Null => Mapping::new(),
            _ => anyhow::bail!("frontmatter is not a YAML mapping"),
        };
        note.had_frontmatter = fm.is_some();
        if fm.is_none() {
            note.warnings.push("missing frontmatter".into());
        }

        let take = |m: &mut Mapping, k: &str| m.shift_remove(Value::String(k.to_string()));
        // Reserved keys whose value is not a plain scalar are kept verbatim instead of being dropped on rewrite.
        let mut kept: Vec<(String, Value)> = vec![];
        let mut scalar = |m: &mut Mapping, k: &str| -> Option<String> {
            let v = m.shift_remove(Value::String(k.to_string()))?;
            match yaml_scalar_string(&v) {
                Some(s) => Some(s),
                None => {
                    if !v.is_null() {
                        kept.push((k.to_string(), v));
                    }
                    None
                }
            }
        };

        note.id = scalar(&mut map, "id").map(|s| s.trim().to_ascii_lowercase()).unwrap_or_default();
        if note.id.is_empty()
            && let Some(sid) = &stem_id {
                note.id = sid.clone();
            }
        note.title = scalar(&mut map, "title").unwrap_or_default();
        if note.title.trim().is_empty() {
            note.title = first_heading(&note.body)
                .or_else(|| {
                    file_stem.map(|s| {
                        let s = [".md", ".markdown", ".txt"].iter().find_map(|e| s.strip_suffix(e)).unwrap_or(s);
                        let s = match &stem_id {
                            Some(id) => s.strip_prefix(id.as_str()).unwrap_or(s).trim_start_matches('-'),
                            None => s,
                        };
                        s.replace(['-', '_'], " ").trim().to_string()
                    })
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "untitled".to_string());
        }
        if let Some(k) = scalar(&mut map, "kind")
            && !k.trim().is_empty() {
                note.kind = k.trim().to_string();
            }
        note.project = scalar(&mut map, "project").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        if let Some(v) = take(&mut map, "tags") {
            note.tags = match v {
                Value::Sequence(seq) => seq.iter().filter_map(yaml_scalar_string).collect(),
                Value::String(s) => s.split(',').map(|t| t.trim().to_string()).collect(),
                Value::Null => vec![],
                other => yaml_scalar_string(&other).into_iter().collect(),
            };
            note.tags.retain(|t| !t.trim().is_empty());
        }
        if let Some(v) = take(&mut map, "links") {
            let items = match v {
                Value::Sequence(seq) => seq,
                Value::Null => vec![],
                other => vec![other],
            };
            for item in items {
                match item {
                    Value::Mapping(m) => {
                        let to = m.get("to").and_then(yaml_scalar_string);
                        let rel = m.get("rel").and_then(yaml_scalar_string).unwrap_or_else(default_rel);
                        match to {
                            Some(to) => note.links.push(Link { to: to.to_ascii_lowercase(), rel }),
                            None => note.warnings.push("link without 'to'".into()),
                        }
                    }
                    other => match yaml_scalar_string(&other) {
                        Some(to) => note.links.push(Link { to: to.to_ascii_lowercase(), rel: default_rel() }),
                        None => note.warnings.push("unreadable link entry".into()),
                    },
                }
            }
        }
        if let Some(s) = scalar(&mut map, "status") {
            note.status = s.trim().to_ascii_lowercase();
        }
        if let Some(v) = take(&mut map, "pinned") {
            note.pinned = match v {
                Value::Bool(b) => b,
                other => matches!(yaml_scalar_string(&other).as_deref().map(str::to_ascii_lowercase).as_deref(), Some("true" | "yes" | "1")),
            };
        }
        let created = scalar(&mut map, "created").and_then(|s| parse_time(&s));
        let updated = scalar(&mut map, "updated").and_then(|s| parse_time(&s));
        match (created, updated) {
            (Some(c), Some(u)) => {
                note.created = c;
                note.updated = u;
            }
            (Some(c), None) => {
                note.created = c;
                note.updated = c;
            }
            (None, Some(u)) => {
                note.created = u;
                note.updated = u;
            }
            (None, None) => {
                if note.had_frontmatter {
                    note.warnings.push("missing created/updated timestamps".into());
                }
            }
        }
        note.source = scalar(&mut map, "source").filter(|s| !s.is_empty());
        for (k, v) in kept {
            note.warnings.push(format!("'{k}' is not a plain value, it is kept as is but ignored"));
            map.insert(Value::String(k), v);
        }
        note.extra = map;
        Ok(note)
    }

    pub fn frontmatter(&self) -> Mapping {
        let mut m = Mapping::new();
        let mut put = |k: &str, v: Value| {
            m.insert(Value::String(k.to_string()), v);
        };
        put("id", Value::String(self.id.clone()));
        put("title", Value::String(self.title.clone()));
        put("kind", Value::String(self.kind.clone()));
        if let Some(p) = &self.project {
            put("project", Value::String(p.clone()));
        }
        put("tags", Value::Sequence(self.tags.iter().cloned().map(Value::String).collect()));
        let links = self
            .links
            .iter()
            .map(|l| {
                let mut lm = Mapping::new();
                lm.insert("to".into(), Value::String(l.to.clone()));
                lm.insert("rel".into(), Value::String(l.rel.clone()));
                Value::Mapping(lm)
            })
            .collect();
        put("links", Value::Sequence(links));
        put("status", Value::String(self.status.clone()));
        put("pinned", Value::Bool(self.pinned));
        put("created", Value::String(fmt_time(&self.created)));
        put("updated", Value::String(fmt_time(&self.updated)));
        if let Some(s) = &self.source {
            put("source", Value::String(s.clone()));
        }
        // A reserved key in extra holds a non-scalar value kept from the file and wins, so it round trips.
        for (k, v) in &self.extra {
            m.insert(k.clone(), v.clone());
        }
        m
    }

    /// Drops a kept non-scalar value for a reserved key once the key is set explicitly.
    pub fn clear_kept(&mut self, key: &str) {
        if RESERVED_KEYS.contains(&key) {
            self.extra.shift_remove(key);
        }
    }

    pub fn to_markdown(&self) -> String {
        let yaml = serde_yaml::to_string(&Value::Mapping(self.frontmatter())).unwrap_or_default();
        let body = self.body.trim_end();
        if body.is_empty() { format!("---\n{yaml}---\n") } else { format!("---\n{yaml}---\n\n{body}\n") }
    }

    /// Frontmatter plus body and path. Fields named like those output keys go under `shadowed_fields`.
    pub fn to_json(&self) -> serde_json::Value {
        let mut obj = match serde_json::to_value(Value::Mapping(self.frontmatter())) {
            Ok(serde_json::Value::Object(o)) => o,
            _ => serde_json::Map::new(),
        };
        let mut shadowed = serde_json::Map::new();
        for k in JSON_KEYS {
            if let Some(v) = obj.remove(k) {
                shadowed.insert(k.to_string(), v);
            }
        }
        if !shadowed.is_empty() {
            obj.insert("shadowed_fields".into(), serde_json::Value::Object(shadowed));
        }
        obj.insert("body".into(), serde_json::Value::String(self.body.clone()));
        if let Some(p) = &self.path {
            obj.insert("path".into(), serde_json::Value::String(path_str(p)));
        }
        serde_json::Value::Object(obj)
    }

    pub fn field(&self, key: &str) -> Option<&Value> {
        self.extra.get(key)
    }

    pub fn set_field(&mut self, key: &str, v: Value) {
        self.extra.insert(Value::String(key.to_string()), v);
    }

    pub fn unset_field(&mut self, key: &str) -> bool {
        self.extra.shift_remove(key).is_some()
    }

    pub fn path_globs(&self) -> Vec<String> {
        let items: Vec<String> = match self.field(PATHS_KEY) {
            Some(Value::Sequence(s)) => s.iter().filter_map(yaml_scalar_string).collect(),
            Some(Value::String(s)) => s.split(',').map(str::to_string).collect(),
            _ => vec![],
        };
        items.into_iter().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
    }

    pub fn refs(&self) -> Vec<String> {
        match self.field(REFS_KEY) {
            Some(Value::Sequence(s)) => s.iter().filter_map(yaml_scalar_string).collect(),
            Some(v) => yaml_scalar_string(v).map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default(),
            None => vec![],
        }
    }

    pub fn set_refs(&mut self, refs: Vec<String>) {
        if refs.is_empty() {
            self.unset_field(REFS_KEY);
        } else {
            self.set_field(REFS_KEY, Value::Sequence(refs.into_iter().map(Value::String).collect()));
        }
    }

    pub fn verified(&self) -> Option<DateTime<Utc>> {
        self.field(VERIFIED_KEY).and_then(yaml_scalar_string).and_then(|s| parse_time(&s))
    }

    /// Last time the note was known to be accurate: verified or updated, whichever is later.
    pub fn fresh_at(&self) -> DateTime<Utc> {
        self.verified().map_or(self.updated, |v| v.max(self.updated))
    }

    pub fn mark_verified(&mut self) {
        self.set_field(VERIFIED_KEY, Value::String(crate::util::fmt_date(&now())));
    }

    /// When someone last checked this pin on purpose, a note never verified counts from its creation.
    pub fn pin_checked_at(&self) -> DateTime<Utc> {
        self.field(PIN_VERIFIED_KEY).and_then(yaml_scalar_string).and_then(|s| parse_time(&s)).unwrap_or(self.created)
    }

    /// The short name, None for notes without one and for a value that could be mistaken for an id.
    pub fn slug(&self) -> Option<String> {
        self.field(crate::slug::SLUG_KEY).and_then(yaml_scalar_string).filter(|s| crate::slug::is_valid(s))
    }

    /// `slug id`, or the id alone. The id always stays since old MCP servers cannot resolve a slug.
    pub fn handle(&self) -> String {
        match self.slug() {
            Some(s) => format!("{s} {}", self.id),
            None => self.id.clone(),
        }
    }

    pub fn file_name(&self) -> String {
        let slug = crate::util::slugify(&self.title, 60);
        let slug = if slug.is_empty() { "note".to_string() } else { slug };
        format!("{}-{}.md", self.id, slug)
    }

    pub fn is_archived(&self) -> bool {
        self.status == "archived"
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t.eq_ignore_ascii_case(tag))
    }

    pub fn scope_label(&self) -> String {
        self.project.clone().unwrap_or_else(|| "global".to_string())
    }
}
