use crate::note::yaml_scalar_string;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    String,
    Int,
    Float,
    Bool,
    Date,
    Enum,
    List,
    Ref,
    Url,
}

impl FieldType {
    pub fn parse(s: &str) -> Result<FieldType> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "string" | "str" | "text" => FieldType::String,
            "int" | "integer" => FieldType::Int,
            "float" | "number" => FieldType::Float,
            "bool" | "boolean" => FieldType::Bool,
            "date" | "datetime" => FieldType::Date,
            "enum" => FieldType::Enum,
            "list" | "tags" => FieldType::List,
            "ref" => FieldType::Ref,
            "url" => FieldType::Url,
            other => bail!("unknown field type '{other}', expected one of: string, int, float, bool, date, enum, list, ref, url"),
        })
    }
}

impl fmt::Display for FieldType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            FieldType::String => "string",
            FieldType::Int => "int",
            FieldType::Float => "float",
            FieldType::Bool => "bool",
            FieldType::Date => "date",
            FieldType::Enum => "enum",
            FieldType::List => "list",
            FieldType::Ref => "ref",
            FieldType::Url => "url",
        };
        f.write_str(s)
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FieldDef {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: FieldType,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<toml::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KindDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default)]
    pub fields: Vec<FieldDef>,
}

pub fn is_valid_kind_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 40 && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Closest valid kind name, used in hints for names like `TODO` or `My Kind`.
pub fn suggest_kind_name(s: &str) -> String {
    let out: String = s.trim().chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c.to_ascii_lowercase() } else { '-' }).collect();
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "my-kind".into() } else { out }
}

pub fn toml_to_yaml(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::Number((*i).into()),
        toml::Value::Float(f) => Value::Number((*f).into()),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(a) => Value::Sequence(a.iter().map(toml_to_yaml).collect()),
        toml::Value::Table(t) => {
            let mut m = Mapping::new();
            for (k, v) in t {
                m.insert(Value::String(k.clone()), toml_to_yaml(v));
            }
            Value::Mapping(m)
        }
    }
}

pub fn yaml_to_toml(v: &Value) -> Option<toml::Value> {
    Some(match v {
        Value::Null => return None,
        Value::Bool(b) => toml::Value::Boolean(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml::Value::Integer(i)
            } else {
                toml::Value::Float(n.as_f64()?)
            }
        }
        Value::String(s) => toml::Value::String(s.clone()),
        Value::Sequence(s) => toml::Value::Array(s.iter().filter_map(yaml_to_toml).collect()),
        Value::Mapping(m) => {
            let mut t = toml::map::Map::new();
            for (k, v) in m {
                if let (Some(k), Some(v)) = (yaml_scalar_string(k), yaml_to_toml(v)) {
                    t.insert(k, v);
                }
            }
            toml::Value::Table(t)
        }
        Value::Tagged(t) => return yaml_to_toml(&t.value),
    })
}

pub fn json_to_yaml(v: &serde_json::Value) -> Value {
    serde_yaml::to_value(v).unwrap_or(Value::Null)
}

/// Parses a raw string for a field that no kind declares: YAML scalar or flow list, otherwise a plain string.
pub fn parse_untyped(raw: &str) -> Value {
    match serde_yaml::from_str::<Value>(raw) {
        Ok(Value::Mapping(_)) | Ok(Value::Tagged(_)) | Err(_) => Value::String(raw.to_string()),
        Ok(Value::Null) if !raw.trim().is_empty() && !matches!(raw.trim(), "null" | "~") => Value::String(raw.to_string()),
        Ok(v) => v,
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "y" | "1" | "on" => Some(true),
        "false" | "no" | "n" | "0" | "off" => Some(false),
        _ => None,
    }
}

fn is_date(s: &str) -> bool {
    let s = s.trim();
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok() || chrono::DateTime::parse_from_rfc3339(s).is_ok()
}

fn is_url(s: &str) -> bool {
    let s = s.trim();
    if s.contains(char::is_whitespace) {
        return false;
    }
    if let Some((scheme, rest)) = s.split_once("://") {
        return !scheme.is_empty()
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
            && !rest.is_empty();
    }
    s.starts_with("mailto:")
}

impl FieldDef {
    pub fn new(name: &str, ty: FieldType) -> FieldDef {
        FieldDef { name: name.to_string(), ty, required: false, description: None, default: None, values: vec![] }
    }

    /// Converts user text into a typed YAML value for this field.
    pub fn parse_value(&self, raw: &str) -> Result<Value> {
        let r = raw.trim();
        let v = match self.ty {
            FieldType::String | FieldType::Url | FieldType::Ref | FieldType::Date | FieldType::Enum => {
                Value::String(if self.ty == FieldType::String { raw.to_string() } else { r.to_string() })
            }
            FieldType::Int => match r.parse::<i64>() {
                Ok(i) => Value::Number(i.into()),
                Err(_) => bail!("field '{}' expects an int, got '{raw}'", self.name),
            },
            FieldType::Float => match r.parse::<f64>() {
                Ok(f) => Value::Number(f.into()),
                Err(_) => bail!("field '{}' expects a float, got '{raw}'", self.name),
            },
            FieldType::Bool => match parse_bool(r) {
                Some(b) => Value::Bool(b),
                None => bail!("field '{}' expects a bool (true/false), got '{raw}'", self.name),
            },
            FieldType::List => {
                if r.starts_with('[') {
                    match serde_yaml::from_str::<Value>(r) {
                        Ok(Value::Sequence(s)) => Value::Sequence(s),
                        _ => bail!("field '{}' expects a list, got '{raw}'", self.name),
                    }
                } else if r.is_empty() {
                    Value::Sequence(vec![])
                } else {
                    Value::Sequence(r.split(',').map(|s| Value::String(s.trim().to_string())).filter(|v| v.as_str() != Some("")).collect())
                }
            }
        };
        if let Err(e) = self.check(&v) {
            bail!("{e}");
        }
        Ok(v)
    }

    /// Coerces a value that arrived as JSON or YAML, turning strings into the declared type where possible.
    pub fn coerce(&self, v: Value) -> Result<Value> {
        if let Value::String(s) = &v
            && !matches!(self.ty, FieldType::String) {
                return self.parse_value(s);
            }
        if let (FieldType::String, Some(s)) = (self.ty, yaml_scalar_string(&v))
            && !matches!(v, Value::String(_)) {
                return Ok(Value::String(s));
            }
        if let Err(e) = self.check(&v) {
            bail!("{e}");
        }
        Ok(v)
    }

    pub fn check(&self, v: &Value) -> std::result::Result<(), String> {
        let n = &self.name;
        let ok = match self.ty {
            FieldType::String => matches!(v, Value::String(_)),
            FieldType::Int => matches!(v, Value::Number(x) if x.is_i64() || x.is_u64()),
            FieldType::Float => matches!(v, Value::Number(_)),
            FieldType::Bool => matches!(v, Value::Bool(_)),
            FieldType::Date => matches!(v, Value::String(s) if is_date(s)),
            FieldType::Enum => {
                return match v {
                    Value::String(s) if self.values.is_empty() || self.values.iter().any(|x| x == s) => Ok(()),
                    _ => Err(format!(
                        "field '{n}' must be one of [{}], got {}",
                        self.values.join(", "),
                        crate::note::yaml_display(v)
                    )),
                };
            }
            FieldType::List => matches!(v, Value::Sequence(s) if s.iter().all(|x| yaml_scalar_string(x).is_some())),
            FieldType::Ref => matches!(v, Value::String(s) if s.len() >= crate::id::MIN_PREFIX && s.chars().all(crate::id::is_id_char)),
            FieldType::Url => matches!(v, Value::String(s) if is_url(s)),
        };
        if ok {
            Ok(())
        } else {
            let what = match self.ty {
                FieldType::Date => "a date (YYYY-MM-DD or RFC3339)".to_string(),
                FieldType::Ref => "a note id".to_string(),
                FieldType::Url => "a url (scheme://...)".to_string(),
                FieldType::List => "a list of strings".to_string(),
                t => format!("a {t}"),
            };
            Err(format!("field '{n}' expects {what}, got '{}'", crate::note::yaml_display(v)))
        }
    }

    pub fn default_yaml(&self) -> Option<Value> {
        self.default.as_ref().map(toml_to_yaml)
    }

    pub fn summary(&self) -> String {
        let mut s = format!("{}: {}", self.name, self.ty);
        if !self.values.is_empty() {
            s.push_str(&format!(" [{}]", self.values.join("|")));
        }
        if self.required {
            s.push_str(" (required)");
        }
        if let Some(d) = &self.default {
            s.push_str(&format!(" default={d}"));
        }
        if let Some(d) = &self.description
            && !d.is_empty() {
                s.push_str(&format!(", {d}"));
            }
        s
    }
}

impl KindDef {
    pub fn new(name: &str, description: &str) -> KindDef {
        KindDef { name: name.to_string(), description: description.to_string(), template: None, fields: vec![] }
    }

    pub fn field(&self, name: &str) -> Option<&FieldDef> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// Applies defaults, type checks declared fields and enforces required ones. Undeclared fields pass through.
    pub fn validate(&self, extra: &mut Mapping) -> Result<()> {
        let mut errors = vec![];
        for f in &self.fields {
            let key = Value::String(f.name.clone());
            let present = matches!(extra.get(&key), Some(v) if !v.is_null());
            if !present {
                if let Some(d) = f.default_yaml() {
                    extra.insert(key, d);
                    continue;
                }
                if f.required {
                    errors.push(format!("missing required field '{}' ({})", f.name, f.ty));
                }
                continue;
            }
            let v = extra.get(&key).cloned().unwrap();
            match f.coerce(v) {
                Ok(c) => {
                    extra.insert(key, c);
                }
                Err(e) => errors.push(e.to_string()),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            bail!("kind '{}' validation failed: {}", self.name, errors.join("; "))
        }
    }

    /// Non-mutating check used by doctor.
    pub fn problems(&self, extra: &Mapping) -> Vec<String> {
        let mut out = vec![];
        for f in &self.fields {
            match extra.get(f.name.as_str()) {
                None | Some(Value::Null) => {
                    if f.required && f.default.is_none() {
                        out.push(format!("missing required field '{}'", f.name));
                    }
                }
                Some(v) => {
                    if let Err(e) = f.check(v) {
                        out.push(e);
                    }
                }
            }
        }
        out
    }

    pub fn to_toml(&self) -> Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    pub fn from_toml(s: &str) -> Result<KindDef> {
        let k: KindDef = toml::from_str(s)?;
        if !is_valid_kind_name(&k.name) {
            bail!("invalid kind name '{}', use lowercase letters, digits, '-' or '_'", k.name);
        }
        Ok(k)
    }

    /// Merges a redefinition: provided description and template replace, fields replace by name or get appended.
    pub fn merge(&mut self, other: KindDef) {
        if !other.description.is_empty() {
            self.description = other.description;
        }
        if other.template.is_some() {
            self.template = other.template;
        }
        for f in other.fields {
            match self.fields.iter_mut().find(|x| x.name == f.name) {
                Some(slot) => *slot = f,
                None => self.fields.push(f),
            }
        }
    }
}

/// Parses `name:type[:required][:a|b|c][:default=x][:desc=text]`. The values of default= and desc= may contain
/// colons (urls, timestamps), so they run until the next `:default=` or `:desc=` part.
pub fn parse_field_spec(spec: &str) -> Result<FieldDef> {
    let mut parts = spec.split(':');
    let name = parts.next().unwrap_or("").trim();
    if name.is_empty() || name.contains(char::is_whitespace) {
        bail!("invalid field spec '{spec}', expected name:type[:required][:a|b|c]");
    }
    if crate::note::RESERVED_KEYS.contains(&name) {
        bail!("field name '{name}' is reserved for note metadata");
    }
    let ty = FieldType::parse(parts.next().unwrap_or("string"))?;
    let mut f = FieldDef::new(name, ty);
    let rest: Vec<&str> = parts.collect();
    let keyed = |p: &str| {
        let p = p.trim_start();
        p.starts_with("default=") || p.starts_with("desc=")
    };
    let mut i = 0;
    while i < rest.len() {
        let p = rest[i].trim();
        i += 1;
        if keyed(p) {
            let mut val = p.to_string();
            while i < rest.len() && !keyed(rest[i]) {
                val.push(':');
                val.push_str(rest[i]);
                i += 1;
            }
            let val = val.trim();
            if let Some(d) = val.strip_prefix("default=") {
                let d = d.trim();
                f.default = yaml_to_toml(&f.parse_value(d).unwrap_or_else(|_| parse_untyped(d)));
            } else if let Some(d) = val.strip_prefix("desc=") {
                f.description = Some(d.trim().to_string());
            }
            continue;
        }
        if p.is_empty() {
            continue;
        }
        if p.eq_ignore_ascii_case("required") {
            f.required = true;
        } else if p.eq_ignore_ascii_case("optional") {
            f.required = false;
        } else if p.contains('|') || ty == FieldType::Enum {
            f.values = p.split('|').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        } else {
            bail!("unrecognized part '{p}' in field spec '{spec}'");
        }
    }
    if f.ty == FieldType::Enum && f.values.is_empty() {
        bail!("enum field '{name}' needs values, e.g. {name}:enum:low|med|high");
    }
    Ok(f)
}

pub const ENV: &str = "env";

pub fn seed_kinds() -> Vec<KindDef> {
    let enum_field = |name: &str, values: &[&str], default: &str, desc: &str| FieldDef {
        name: name.into(),
        ty: FieldType::Enum,
        required: false,
        description: Some(desc.into()),
        default: Some(toml::Value::String(default.into())),
        values: values.iter().map(|s| s.to_string()).collect(),
    };
    let mut note = KindDef::new("note", "General knowledge, facts about a project or topic");
    note.fields = vec![];
    let mut decision = KindDef::new("decision", "A decision that was made, with context and consequences");
    decision.fields = vec![enum_field("decision_status", &["proposed", "accepted", "superseded"], "accepted", "lifecycle of the decision")];
    decision.template = Some("## Context\n\n## Decision\n\n## Consequences\n".into());
    let gotcha = KindDef::new("gotcha", "A trap, quirk or non-obvious behavior worth remembering");
    let mut todo = KindDef::new("todo", "Actionable item");
    todo.fields = vec![
        FieldDef {
            name: "done".into(),
            ty: FieldType::Bool,
            required: false,
            description: Some("whether the item is finished".into()),
            default: Some(toml::Value::Boolean(false)),
            values: vec![],
        },
        enum_field("priority", &["p0", "p1", "p2", "p3"], "p2", "how urgent, p0 highest"),
    ];
    let mut handoff = KindDef::new("handoff", "End of session state for the next agent: what was done, what is next, open questions");
    handoff.template = Some("## Done\n\n## Next\n\n## Open questions\n".into());
    let mut reference = KindDef::new("reference", "Pointer to external docs, dashboards, tickets or resources");
    reference.fields = vec![FieldDef {
        name: "url".into(),
        ty: FieldType::Url,
        required: false,
        description: Some("where the resource lives".into()),
        default: None,
        values: vec![],
    }];
    let plan = KindDef::new("plan", "An implementation plan or roadmap");
    let chat = KindDef::new("chat-summary", "Summary of a chat or session worth keeping");
    let feedback = KindDef::new("feedback", "User preferences and corrections about how agents should work");
    let user = KindDef::new("user", "Facts about the user: role, environment, preferences");
    let str_field = |name: &str, desc: &str| FieldDef {
        name: name.into(),
        ty: FieldType::String,
        required: false,
        description: Some(desc.into()),
        default: None,
        values: vec![],
    };
    let mut workflow = KindDef::new("workflow", "A Claude Code workflow script stored in the vault, restore it with `breadcrumb workflow install`");
    workflow.fields = vec![
        str_field("script", "vault relative path of the .js script"),
        str_field("workflow_name", "file name without .js under .claude/workflows"),
        str_field("when_to_use", "when an agent should run it"),
    ];
    let mut env = KindDef::new(ENV, "A fact about one machine: where a file or folder lives or which tool exists, listed by `breadcrumb env`, not in the digest");
    env.fields = vec![
        str_field("machine", "computer it holds on, this one when not given"),
        str_field("location", "the file, folder or tool it is about"),
        FieldDef {
            name: "access".into(),
            ty: FieldType::Enum,
            required: false,
            description: Some("whether agents may change it".into()),
            default: None,
            values: vec!["read-only".into(), "read-write".into()],
        },
    ];
    let mut all = vec![note, decision, gotcha, todo, handoff, reference, plan, chat, feedback, user, workflow, env];
    all.extend(crate::backlog::seed_kinds());
    all
}
