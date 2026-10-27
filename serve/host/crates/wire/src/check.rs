//! A contract as data (`rpc::SCHEMA`, made by serve/contract/gen.mjs) and the check of a JSON value against one of
//! its types. The same check as the TypeScript `check` the generator writes:
//!
//! - [`Mode::Producer`] (what the host must send): every field is there (null where it may be, absent only where the
//!   contract says it may be left out), every enum value is a known one (never `unknown`), and nothing the contract
//!   doesn't name is sent.
//! - [`Mode::Consumer`] (what an app must read): optional fields may be absent or null; enum values and fields this
//!   version doesn't know are fine.

use serde_json::Value;

/// A contract: its enums, its types and its methods.
#[derive(Debug)]
pub struct Schema {
    pub enums: &'static [EnumSchema],
    pub types: &'static [TypeSchema],
    pub methods: &'static [MethodSchema],
}

/// An enum's values, and what a value a consumer doesn't know is read as (one of them, or `unknown`, its own state).
#[derive(Debug)]
pub struct EnumSchema {
    pub name: &'static str,
    pub values: &'static [&'static str],
    pub unknown: &'static str,
}

/// A type's fields, each with its spec (`T`, `T?`, `[T]`, `{T}`), and the optional ones a producer may leave out.
#[derive(Debug)]
pub struct TypeSchema {
    pub name: &'static str,
    pub fields: &'static [(&'static str, &'static str)],
    pub may_omit: &'static [&'static str],
}

/// A method: who may call it, the type of what it takes and of what it answers with.
#[derive(Debug)]
pub struct MethodSchema {
    pub name: &'static str,
    pub who: &'static [&'static str],
    pub params: &'static str,
    pub result: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Producer,
    Consumer,
}

struct Spec<'a> {
    opt: bool,
    list: bool,
    map: bool,
    of: &'a str,
}

fn parse(spec: &str) -> Spec<'_> {
    let (opt, s) = spec.strip_suffix('?').map_or((false, spec), |s| (true, s));
    let list = s.starts_with('[') && s.ends_with(']');
    let map = s.starts_with('{') && s.ends_with('}');
    Spec { opt, list, map, of: if list || map { &s[1..s.len() - 1] } else { s } }
}

impl Schema {
    pub fn method(&self, name: &str) -> Option<&MethodSchema> {
        self.methods.iter().find(|m| m.name == name)
    }

    pub fn enum_schema(&self, name: &str) -> Option<&EnumSchema> {
        self.enums.iter().find(|e| e.name == name)
    }

    pub fn type_schema(&self, name: &str) -> Option<&TypeSchema> {
        self.types.iter().find(|t| t.name == name)
    }

    /// The problems with `value` as the contract type `ty`; none is an empty list.
    pub fn check(&self, ty: &str, value: &Value, mode: Mode) -> Vec<String> {
        let mut out = Vec::new();
        self.check_type(ty, value, mode, ty, &mut out);
        out
    }

    fn check_type(&self, ty: &str, value: &Value, mode: Mode, at: &str, out: &mut Vec<String>) {
        let Some(t) = self.type_schema(ty) else {
            out.push(format!("{at}: no type {ty} in the contract"));
            return;
        };
        let Value::Object(obj) = value else {
            out.push(format!("{at}: not an object"));
            return;
        };
        for &(name, spec) in t.fields {
            match obj.get(name) {
                None | Some(Value::Null) => {
                    if !parse(spec).opt {
                        out.push(format!("{at}.{name}: missing"));
                    } else if mode == Mode::Producer && obj.get(name).is_none() && !t.may_omit.contains(&name) {
                        out.push(format!("{at}.{name}: missing (send null)"));
                    }
                }
                Some(v) => self.check_value(spec, v, mode, &format!("{at}.{name}"), out),
            }
        }
        if mode == Mode::Producer {
            for k in obj.keys() {
                if !t.fields.iter().any(|(f, _)| f == k) {
                    out.push(format!("{at}.{k}: not in the contract"));
                }
            }
        }
    }

    fn check_value(&self, spec: &str, v: &Value, mode: Mode, at: &str, out: &mut Vec<String>) {
        let s = parse(spec);
        if s.list {
            match v {
                Value::Array(xs) => {
                    for (i, x) in xs.iter().enumerate() {
                        self.check_value(s.of, x, mode, &format!("{at}[{i}]"), out);
                    }
                }
                _ => out.push(format!("{at}: not a list")),
            }
            return;
        }
        if s.map {
            match v {
                Value::Object(xs) => {
                    for (k, x) in xs {
                        self.check_value(s.of, x, mode, &format!("{at}.{k}"), out);
                    }
                }
                _ => out.push(format!("{at}: not a map")),
            }
            return;
        }
        let ok = match s.of {
            "json" => return,
            "string" => v.is_string(),
            "int" => v.is_i64() || v.is_u64(),
            "bool" => v.is_boolean(),
            of => {
                if let Some(e) = self.enum_schema(of) {
                    match v.as_str() {
                        None => false,
                        Some(w) => {
                            if mode == Mode::Producer && !e.values.contains(&w) {
                                out.push(format!("{at}: {v} is not a {of}"));
                            }
                            true
                        }
                    }
                } else {
                    self.check_type(of, v, mode, at, out);
                    return;
                }
            }
        };
        if !ok {
            let what = match s.of {
                "int" => "an integer",
                "bool" => "a bool",
                _ => "a string",
            };
            out.push(format!("{at}: not {what}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Mode, Schema};
    use crate::rpc::SCHEMA;
    use serde_json::json;

    fn problems(ty: &str, v: &serde_json::Value, mode: Mode) -> Vec<String> {
        let s: &Schema = &SCHEMA;
        s.check(ty, v, mode)
    }

    #[test]
    fn a_producer_sends_every_field_and_only_known_words() {
        let ok = json!({"agent": "a", "name": null, "role": "member"});
        assert_eq!(problems("GrantedAgent", &ok, Mode::Producer), Vec::<String>::new());
        let left_out = json!({"agent": "a", "role": "member"});
        assert_eq!(problems("GrantedAgent", &left_out, Mode::Producer), ["GrantedAgent.name: missing (send null)"]);
        assert_eq!(problems("GrantedAgent", &left_out, Mode::Consumer), Vec::<String>::new());
        let extra = json!({"agent": "a", "name": null, "role": "member", "new": 1});
        assert_eq!(problems("GrantedAgent", &extra, Mode::Producer), ["GrantedAgent.new: not in the contract"]);
        assert_eq!(problems("GrantedAgent", &extra, Mode::Consumer), Vec::<String>::new());
        let word = json!({"agent": "a", "name": null, "role": "unknown"});
        assert_eq!(
            problems("GrantedAgent", &word, Mode::Producer),
            [r#"GrantedAgent.role: "unknown" is not a AgentRole"#]
        );
        assert_eq!(problems("GrantedAgent", &word, Mode::Consumer), Vec::<String>::new());
    }

    #[test]
    fn lists_maps_and_json() {
        let bad = json!({"projects": [{"id": 1}], "error": null});
        assert_eq!(problems("Projects", &bad, Mode::Consumer), ["Projects.projects[0].id: not a string"]);
        let missing = json!({"agent": "a"});
        assert_eq!(problems("GrantedAgent", &missing, Mode::Consumer), ["GrantedAgent.role: missing"]);
        let entry = json!({"id": "e", "seq": 1, "botId": "b", "threadId": null, "rev": 0, "kind": "agent", "turn": 1,
            "data": {"anything": [1, "two"]}, "createdAt": 1, "updatedAt": 1});
        assert_eq!(problems("Entry", &entry, Mode::Producer), Vec::<String>::new());
    }

    #[test]
    fn every_method_names_types_the_contract_has() {
        for m in SCHEMA.methods {
            assert!(SCHEMA.type_schema(m.params).is_some(), "{}", m.name);
            assert!(SCHEMA.type_schema(m.result).is_some(), "{}", m.name);
        }
    }
}
