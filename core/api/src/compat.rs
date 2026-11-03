//! The break test: an OpenRPC document against the snapshot cut at the last release. Additions pass; these fail:
//! - a stable method removed (an experimental one in the snapshot may change shape or go: that is what the word says);
//! - a param removed or renamed, made required when it was optional, or added as required;
//! - a type narrowed: a scalar type, an enum value, a list item, an object field removed or turned required, or a
//!   nullable made plain. One rule for params and results alike, because the contract's rule is "additive only".
//!
//! Types are compared as sets of alternatives (`anyOf`, `type: [..]`, `$ref`s resolved in their own document), so
//! `string` -> `string | null` is a widening and the reverse is a break.

use std::collections::BTreeSet;

use serde_json::Value;

/// Every way `new` breaks what `old` promised; empty when it only adds.
pub fn breaks(old: &Value, new: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let new_methods: Vec<&Value> = new["methods"].as_array().into_iter().flatten().collect();
    for om in old["methods"].as_array().into_iter().flatten() {
        let name = om["name"].as_str().unwrap_or("?");
        if om["x-kith-stability"] == "experimental" {
            continue;
        }
        let Some(nm) = new_methods.iter().find(|m| m["name"] == om["name"]) else {
            out.push(format!("method {name} was removed"));
            continue;
        };
        method(old, om, new, nm, name, &mut out);
    }
    out
}

fn method(old: &Value, om: &Value, new: &Value, nm: &Value, name: &str, out: &mut Vec<String>) {
    let new_params: Vec<&Value> = nm["params"].as_array().into_iter().flatten().collect();
    let old_params: Vec<&Value> = om["params"].as_array().into_iter().flatten().collect();
    for op in &old_params {
        let pname = op["name"].as_str().unwrap_or("?");
        let Some(np) = new_params.iter().find(|p| p["name"] == op["name"]) else {
            out.push(format!("{name}: param {pname} was removed or renamed"));
            continue;
        };
        if op["required"] != true && np["required"] == true {
            out.push(format!("{name}: param {pname} became required"));
        }
        compat(
            old,
            &op["schema"],
            new,
            &np["schema"],
            &format!("{name}: param {pname}"),
            out,
        );
    }
    for np in &new_params {
        if np["required"] == true && !old_params.iter().any(|p| p["name"] == np["name"]) {
            out.push(format!(
                "{name}: new param {} is required",
                np["name"].as_str().unwrap_or("?")
            ));
        }
    }
    compat(
        old,
        &om["result"]["schema"],
        new,
        &nm["result"]["schema"],
        &format!("{name}: result"),
        out,
    );
}

/// One alternative of a schema, with its `$ref`s and `anyOf`s flattened.
#[derive(Debug)]
enum Alt<'a> {
    Any,
    Null,
    Bool,
    Int,
    Str(Option<BTreeSet<&'a str>>),
    Arr(&'a Value),
    Obj {
        props: Vec<(&'a str, &'a Value)>,
        required: BTreeSet<&'a str>,
        extra: Option<&'a Value>,
    },
}

fn alts<'a>(doc: &'a Value, s: &'a Value, depth: usize) -> Vec<Alt<'a>> {
    if depth > 16 {
        return vec![Alt::Any];
    }
    if let Some(r) = s["$ref"].as_str() {
        let name = r.rsplit('/').next().unwrap_or(r);
        return match doc["components"]["schemas"].get(name) {
            Some(target) => alts(doc, target, depth + 1),
            None => vec![Alt::Any],
        };
    }
    if let Some(any) = s["anyOf"].as_array() {
        return any.iter().flat_map(|a| alts(doc, a, depth + 1)).collect();
    }
    let types: Vec<&str> = match &s["type"] {
        Value::String(t) => vec![t.as_str()],
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        _ => return vec![Alt::Any],
    };
    types
        .into_iter()
        .map(|t| match t {
            "null" => Alt::Null,
            "boolean" => Alt::Bool,
            "integer" => Alt::Int,
            "string" => Alt::Str(
                s["enum"]
                    .as_array()
                    .map(|e| e.iter().filter_map(Value::as_str).collect()),
            ),
            "array" => Alt::Arr(&s["items"]),
            "object" => Alt::Obj {
                props: s["properties"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(k, v)| (k.as_str(), v))
                    .collect(),
                required: s["required"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect(),
                extra: s.get("additionalProperties").filter(|v| v.is_object()),
            },
            _ => Alt::Any,
        })
        .collect()
}

fn label(a: &Alt<'_>) -> &'static str {
    match a {
        Alt::Any => "anything",
        Alt::Null => "null",
        Alt::Bool => "a boolean",
        Alt::Int => "an integer",
        Alt::Str(Some(_)) => "an enum",
        Alt::Str(None) => "a string",
        Alt::Arr(_) => "a list",
        Alt::Obj { .. } => "an object",
    }
}

/// Appends to `out` what `new` no longer accepts of `old`.
fn compat(old: &Value, o: &Value, new: &Value, n: &Value, at: &str, out: &mut Vec<String>) {
    let new_alts = alts(new, n, 0);
    if new_alts.iter().any(|a| matches!(a, Alt::Any)) {
        return;
    }
    for oa in alts(old, o, 0) {
        let mut why: Vec<String> = Vec::new();
        let mut covered = false;
        for na in &new_alts {
            let mut errs = Vec::new();
            if covers(old, &oa, new, na, at, &mut errs) {
                covered = true;
                break;
            }
            // The first alternative of the same kind explains best (which field, which value).
            if std::mem::discriminant(&oa) == std::mem::discriminant(na) && why.is_empty() {
                why = errs;
            }
        }
        if !covered {
            if why.is_empty() {
                out.push(format!("{at}: {} is no longer accepted", label(&oa)));
            } else {
                out.append(&mut why);
            }
        }
    }
}

fn covers(
    old: &Value,
    o: &Alt<'_>,
    new: &Value,
    n: &Alt<'_>,
    at: &str,
    out: &mut Vec<String>,
) -> bool {
    match (o, n) {
        (_, Alt::Any) => true,
        (Alt::Null, Alt::Null)
        | (Alt::Bool, Alt::Bool)
        | (Alt::Int, Alt::Int)
        | (Alt::Str(_), Alt::Str(None)) => true,
        (Alt::Str(Some(os)), Alt::Str(Some(ns))) => {
            let gone: Vec<&&str> = os.difference(ns).collect();
            for g in &gone {
                out.push(format!("{at}: enum value {g} was removed"));
            }
            gone.is_empty()
        }
        (Alt::Arr(oi), Alt::Arr(ni)) => {
            let before = out.len();
            compat(old, oi, new, ni, &format!("{at}[]"), out);
            out.len() == before
        }
        (
            Alt::Obj {
                props: op,
                required: oreq,
                extra: oextra,
            },
            Alt::Obj {
                props: np,
                required: nreq,
                extra: nextra,
            },
        ) => {
            let before = out.len();
            for (k, ov) in op {
                match np.iter().find(|(nk, _)| nk == k) {
                    None => out.push(format!("{at}: field {k} was removed or renamed")),
                    Some((_, nv)) => compat(old, ov, new, nv, &format!("{at}.{k}"), out),
                }
            }
            for k in nreq.difference(oreq) {
                out.push(format!("{at}: field {k} became required"));
            }
            match (oextra, nextra) {
                (_, None) => {}
                (Some(oe), Some(ne)) => compat(old, oe, new, ne, &format!("{at}.*"), out),
                (None, Some(_)) => out.push(format!("{at}: values were narrowed to one type")),
            }
            out.len() == before
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn doc(methods: Value) -> Value {
        json!({"methods": methods, "components": {"schemas": {
            "Thing": {"type": "object", "properties": {"id": {"type": "string"}, "n": {"type": "integer"}}, "required": ["id"]},
            "Kind": {"type": "string", "enum": ["a", "b"]},
        }}})
    }
    fn m(params: Value, result: Value) -> Value {
        json!({"name": "x.get", "x-kith-stability": "stable", "params": params, "result": {"name": "result", "schema": result}})
    }
    fn p(name: &str, required: bool, schema: Value) -> Value {
        json!({"name": name, "required": required, "schema": schema})
    }
    fn base() -> Value {
        doc(json!([m(
            json!([
                p("id", true, json!({"type": "string"})),
                p("limit", false, json!({"type": "integer"}))
            ]),
            json!({"$ref": "#/components/schemas/Thing"})
        )]))
    }
    fn only(old: &Value, new: &Value, needle: &str) {
        let got = breaks(old, new);
        assert!(
            got.iter().any(|b| b.contains(needle)),
            "wanted a break mentioning {needle:?}, got {got:?}"
        );
    }

    #[test]
    fn the_same_document_breaks_nothing() {
        assert!(breaks(&base(), &base()).is_empty());
    }

    #[test]
    fn additions_pass() {
        let mut new = base();
        // a new method, a new optional param, a new result field, a new enum value, a nullable
        new["methods"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name": "x.new", "params": [], "result": {"schema": {}}}));
        new["methods"][0]["params"].as_array_mut().unwrap().push(p(
            "extra",
            false,
            json!({"type": "string"}),
        ));
        new["methods"][0]["params"][1]["schema"] = json!({"type": ["integer", "null"]});
        new["components"]["schemas"]["Thing"]["properties"]["more"] = json!({"type": "string"});
        new["components"]["schemas"]["Kind"]["enum"] = json!(["a", "b", "c"]);
        new["methods"][0]["params"][0]["required"] = json!(true);
        assert_eq!(breaks(&base(), &new), Vec::<String>::new());
    }

    #[test]
    fn removing_a_method_breaks() {
        only(&base(), &doc(json!([])), "method x.get was removed");
    }

    #[test]
    fn removing_or_renaming_a_param_breaks() {
        let mut new = base();
        new["methods"][0]["params"][1]["name"] = json!("max");
        only(&base(), &new, "param limit was removed or renamed");
        new["methods"][0]["params"].as_array_mut().unwrap().pop();
        only(&base(), &new, "param limit was removed or renamed");
    }

    #[test]
    fn an_optional_param_made_required_breaks() {
        let mut new = base();
        new["methods"][0]["params"][1]["required"] = json!(true);
        only(&base(), &new, "param limit became required");
        let mut new = base();
        new["methods"][0]["params"].as_array_mut().unwrap().push(p(
            "must",
            true,
            json!({"type": "string"}),
        ));
        only(&base(), &new, "new param must is required");
    }

    #[test]
    fn narrowing_a_type_breaks() {
        let mut new = base();
        new["methods"][0]["params"][0]["schema"] = json!({"type": "integer"});
        only(&base(), &new, "param id");
        // an enum loses a value
        let old = doc(json!([m(
            json!([p("k", true, json!({"$ref": "#/components/schemas/Kind"}))]),
            json!({})
        )]));
        let mut new = old.clone();
        new["components"]["schemas"]["Kind"]["enum"] = json!(["a"]);
        only(&old, &new, "enum value b was removed");
        // a nullable made plain
        let old = doc(json!([m(
            json!([p("s", true, json!({"type": ["string", "null"]}))]),
            json!({})
        )]));
        let mut new = old.clone();
        new["methods"][0]["params"][0]["schema"] = json!({"type": "string"});
        only(&old, &new, "null is no longer accepted");
    }

    #[test]
    fn removing_or_renaming_a_result_field_breaks() {
        let mut new = base();
        new["components"]["schemas"]["Thing"]["properties"]
            .as_object_mut()
            .unwrap()
            .shift_remove("n");
        only(&base(), &new, "field n was removed or renamed");
        let mut new = base();
        new["components"]["schemas"]["Thing"]["required"] = json!(["id", "n"]);
        only(&base(), &new, "field n became required");
    }

    #[test]
    fn a_list_item_or_map_value_narrowed_breaks() {
        let old = doc(json!([m(
            json!([]),
            json!({"type": "array", "items": {"$ref": "#/components/schemas/Thing"}})
        )]));
        let mut new = old.clone();
        new["components"]["schemas"]["Thing"]["properties"]
            .as_object_mut()
            .unwrap()
            .shift_remove("id");
        only(&old, &new, "[]");
    }

    #[test]
    fn an_experimental_method_may_change() {
        let mut old = base();
        old["methods"][0]["x-kith-stability"] = json!("experimental");
        assert!(breaks(&old, &doc(json!([]))).is_empty());
    }
}
