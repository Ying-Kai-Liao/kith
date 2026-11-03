//! The type language of the table (the one `serve/contract/kernel/contract.json` is written in), and its JSON Schema.
//!
//! | written | means |
//! |---|---|
//! | `string`, `bool`, `int` | the JSON scalar |
//! | `json` | anything |
//! | `Name` | a named enum or object type of [`crate::enums`] / [`crate::named_types`] |
//! | `[T]` | a list of T |
//! | `{T}` | an object whose values are T |
//! | `T?` | T or null |

use serde_json::{Map, Value, json};

use crate::{EnumDef, TypeDef};

/// A type name taken apart.
#[derive(Debug, PartialEq, Eq)]
pub enum Ty<'a> {
    String,
    Bool,
    Int,
    Json,
    Named(&'a str),
    List(Box<Ty<'a>>),
    Map(Box<Ty<'a>>),
    /// `T?`
    Nullable(Box<Ty<'a>>),
}

/// Reads a type expression; `None` when it is not one.
pub fn parse(expr: &str) -> Option<Ty<'_>> {
    if let Some(inner) = expr.strip_suffix('?') {
        return Some(Ty::Nullable(Box::new(parse(inner)?)));
    }
    if let Some(inner) = expr.strip_prefix('[').and_then(|e| e.strip_suffix(']')) {
        return Some(Ty::List(Box::new(parse(inner)?)));
    }
    if let Some(inner) = expr.strip_prefix('{').and_then(|e| e.strip_suffix('}')) {
        return Some(Ty::Map(Box::new(parse(inner)?)));
    }
    match expr {
        "string" => Some(Ty::String),
        "bool" => Some(Ty::Bool),
        "int" => Some(Ty::Int),
        "json" => Some(Ty::Json),
        n if !n.is_empty()
            && n.chars().all(|c| c.is_ascii_alphanumeric())
            && n.starts_with(|c: char| c.is_ascii_uppercase()) =>
        {
            Some(Ty::Named(n))
        }
        _ => None,
    }
}

/// The JSON Schema of a type expression. A name stays a `$ref` into the document's `components`.
pub fn schema(expr: &str) -> Value {
    ty_schema(&parse(expr).unwrap_or_else(|| panic!("kith-api: not a type: {expr}")))
}

fn ty_schema(ty: &Ty<'_>) -> Value {
    match ty {
        Ty::String => json!({"type": "string"}),
        Ty::Bool => json!({"type": "boolean"}),
        Ty::Int => json!({"type": "integer"}),
        Ty::Json => json!({}),
        Ty::Named(n) => json!({"$ref": format!("#/components/schemas/{n}")}),
        Ty::List(t) => json!({"type": "array", "items": ty_schema(t)}),
        Ty::Map(t) => json!({"type": "object", "additionalProperties": ty_schema(t)}),
        Ty::Nullable(t) => match ty_schema(t) {
            Value::Object(m) if m.is_empty() => Value::Object(m),
            Value::Object(mut m) if m.len() == 1 && m.get("type").is_some_and(Value::is_string) => {
                let t = m["type"].clone();
                m.insert("type".into(), json!([t, "null"]));
                Value::Object(m)
            }
            inner => json!({"anyOf": [inner, {"type": "null"}]}),
        },
    }
}

/// The named types a type expression refers to (for the consistency test).
pub fn names<'a>(expr: &'a str, out: &mut Vec<&'a str>) {
    fn walk<'a>(t: &Ty<'a>, out: &mut Vec<&'a str>) {
        match t {
            Ty::Named(n) => out.push(n),
            Ty::List(t) | Ty::Map(t) | Ty::Nullable(t) => walk(t, out),
            _ => {}
        }
    }
    if let Some(t) = parse(expr) {
        walk(&t, out);
    }
}

pub(crate) fn enum_schema(e: &EnumDef) -> Value {
    json!({
        "description": e.doc,
        "type": "string",
        "enum": e.values,
        "x-kith-unknown": e.unknown,
    })
}

pub(crate) fn object_schema(t: &TypeDef) -> Value {
    let mut props = Map::new();
    let mut required = Vec::new();
    for (name, ty) in t.fields {
        props.insert((*name).to_owned(), schema(ty));
        if !ty.ends_with('?') {
            required.push(*name);
        }
    }
    let mut o = Map::new();
    o.insert("description".into(), t.doc.into());
    o.insert("type".into(), "object".into());
    o.insert("properties".into(), Value::Object(props));
    o.insert("required".into(), json!(required));
    if !t.may_omit.is_empty() {
        o.insert("x-kith-may-omit".into(), json!(t.may_omit));
    }
    Value::Object(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_contract_language() {
        assert_eq!(parse("string?"), Some(Ty::Nullable(Box::new(Ty::String))));
        assert_eq!(
            parse("[Agent]"),
            Some(Ty::List(Box::new(Ty::Named("Agent"))))
        );
        assert_eq!(parse("{string}"), Some(Ty::Map(Box::new(Ty::String))));
        assert_eq!(parse("json?"), Some(Ty::Nullable(Box::new(Ty::Json))));
        assert_eq!(parse("nonsense"), None);
        assert_eq!(parse("[string"), None);
    }

    #[test]
    fn writes_json_schema() {
        assert_eq!(schema("string?"), json!({"type": ["string", "null"]}));
        assert_eq!(schema("json?"), json!({}));
        assert_eq!(
            schema("Harness?"),
            json!({"anyOf": [{"$ref": "#/components/schemas/Harness"}, {"type": "null"}]})
        );
        assert_eq!(
            schema("[int]"),
            json!({"type": "array", "items": {"type": "integer"}})
        );
    }
}
