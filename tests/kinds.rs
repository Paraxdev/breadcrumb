mod common;

use breadcrumb::kind::{self, FieldType, KindDef};
use breadcrumb::ops::{self, AddReq, FieldInput, UpdateReq};
use serde_yaml::Value;

fn try_add(v: &breadcrumb::vault::Vault, kind: &str, fields: &[(&str, &str)]) -> anyhow::Result<breadcrumb::note::Note> {
    ops::add(
        v,
        AddReq {
            title: "x".into(),
            kind: Some(kind.into()),
            fields: fields.iter().map(|(k, val)| (k.to_string(), FieldInput::Raw(val.to_string()))).collect(),
            ..Default::default()
        },
    )
}

#[test]
fn seeded_kinds_exist() {
    let (_d, v) = common::vault();
    let names: Vec<String> = v.load_kinds().into_iter().map(|k| k.name).collect();
    for k in ["note", "decision", "gotcha", "todo", "handoff", "reference", "plan", "chat-summary", "feedback", "user", "idea", "epic", "feature", "task", "bug"] {
        assert!(names.contains(&k.to_string()), "missing kind {k}");
    }
}

#[test]
fn defaults_types_and_undeclared_fields() {
    let (_d, v) = common::vault();
    let n = common::add(&v, "Do it", "todo", None, &[("extra_thing", "12"), ("flag", "yes please")]);
    assert_eq!(n.field("done"), Some(&Value::Bool(false)));
    assert_eq!(n.field("priority"), Some(&Value::String("p2".into())));
    assert_eq!(n.field("extra_thing"), Some(&Value::Number(12.into())));
    assert_eq!(n.field("flag"), Some(&Value::String("yes please".into())));

    let n = common::add(&v, "Done thing", "todo", None, &[("done", "true"), ("priority", "p1")]);
    assert_eq!(n.field("done"), Some(&Value::Bool(true)));

    assert!(try_add(&v, "todo", &[("priority", "urgent")]).unwrap_err().to_string().contains("must be one of"));
    assert!(try_add(&v, "todo", &[("done", "maybe")]).unwrap_err().to_string().contains("bool"));
    assert!(try_add(&v, "reference", &[("url", "not a url")]).is_err());
    assert!(try_add(&v, "reference", &[("url", "https://example.com/x")]).is_ok());
}

#[test]
fn unknown_kind_tells_agent_how_to_define_it() {
    let (_d, v) = common::vault();
    let err = try_add(&v, "meeting", &[]).unwrap_err().to_string();
    assert!(err.contains("kinds tool") && err.contains("breadcrumb kind define"), "{err}");
}

#[test]
fn define_kind_required_fields_and_redefinition() {
    let (_d, v) = common::vault();
    let mut k = KindDef::new("meeting", "Meeting notes");
    k.fields.push(kind::parse_field_spec("date:date:required").unwrap());
    k.fields.push(kind::parse_field_spec("mood:enum:good|bad").unwrap());
    let (_, created) = ops::define_kind(&v, k, false, &[]).unwrap();
    assert!(created);
    assert!(try_add(&v, "meeting", &[]).unwrap_err().to_string().contains("missing required field 'date'"));
    let n = common::add(&v, "m", "meeting", None, &[("date", "2026-10-18"), ("mood", "good")]);
    assert_eq!(n.field("date"), Some(&Value::String("2026-10-18".into())));
    assert!(try_add(&v, "meeting", &[("date", "18/10/2026")]).is_err());
    assert!(try_add(&v, "meeting", &[("date", "2026-10-18T12:00:00Z")]).is_ok());

    let mut more = KindDef::new("meeting", "");
    more.fields.push(kind::parse_field_spec("attendees:list").unwrap());
    let (k2, created) = ops::define_kind(&v, more, false, &[]).unwrap();
    assert!(!created);
    assert_eq!(k2.description, "Meeting notes");
    assert_eq!(k2.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), vec!["date", "mood", "attendees"]);
    let reloaded = v.load_kind("meeting").unwrap().unwrap();
    assert_eq!(reloaded.field("attendees").unwrap().ty, FieldType::List);

    let u = ops::update(&v, &n.id, UpdateReq { fields: vec![("attendees".into(), FieldInput::Raw("ann, bob".into()))], ..Default::default() }).unwrap();
    assert_eq!(u.field("attendees"), Some(&Value::Sequence(vec![Value::String("ann".into()), Value::String("bob".into())])));
}

#[test]
fn field_spec_parsing() {
    let f = kind::parse_field_spec("priority:enum:low|med|high").unwrap();
    assert_eq!(f.ty, FieldType::Enum);
    assert_eq!(f.values, vec!["low", "med", "high"]);
    let f = kind::parse_field_spec("count:int:required:default=3").unwrap();
    assert!(f.required);
    assert_eq!(f.default, Some(toml::Value::Integer(3)));
    assert!(kind::parse_field_spec("x:weird").is_err());
    assert!(kind::parse_field_spec("title:string").is_err());
    assert!(kind::parse_field_spec("e:enum").is_err());
}

#[test]
fn ref_fields_resolve_prefixes() {
    let (_d, v) = common::vault();
    let parent = common::add(&v, "Epic", "epic", None, &[]);
    let child = common::add(&v, "Feature", "feature", None, &[("parent", &parent.id[..5])]);
    assert_eq!(child.field("parent"), Some(&Value::String(parent.id.clone())));
    assert!(try_add(&v, "task", &[("parent", "zzzzzzzz")]).is_err());
}

#[test]
fn init_is_idempotent_and_never_clobbers() {
    let (_d, v) = common::vault();
    let custom = "name = \"note\"\ndescription = \"customized\"\n";
    std::fs::write(v.root.join("kinds/note.toml"), custom).unwrap();
    std::fs::remove_file(v.root.join("kinds/plan.toml")).unwrap();
    let n = common::add(&v, "keep me", "note", None, &[]);
    let (_, rep) = breadcrumb::vault::Vault::init(&v.root, None).unwrap();
    assert_eq!(rep.created_files, vec!["kinds/plan.toml".to_string()]);
    assert_eq!(std::fs::read_to_string(v.root.join("kinds/note.toml")).unwrap(), custom);
    assert!(v.get(&n.id).is_ok());
    for f in ["breadcrumb.toml", "AGENTS.md", ".gitignore", ".gitattributes"] {
        assert!(v.root.join(f).exists(), "{f}");
    }
}
