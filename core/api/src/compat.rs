//! The break test: an OpenRPC document against the snapshot cut at the last release. Additions pass; these fail:
//! - a stable method removed (an experimental one in the snapshot may change shape or go: that is what the word says);
//! - a param removed or renamed, made required when it was optional, or added as required;
//! - a param type narrowed: a scalar type, an enum value, a list item, an object field removed or turned required, or
//!   a nullable made plain (a param may widen: the new version must accept everything the old one did);
//! - a result type widened or changed for an older reader: the new version may only send what the old one could.
//!   A result field going `T` -> `T?`, a required field made optional, a field dropped, a result becoming `json`
//!   (anything), or a new required field all fail. A new result field may only be added optional (`T?`), an enum may
//!   gain values (every enum has its `unknown` word), and an optional field made required is harmless.
//!
//! Types are compared as sets of alternatives (`anyOf`, `type: [..]`, `$ref`s resolved in their own document).

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
    cli(&om["x-kith-cli"], &nm["x-kith-cli"], name, out);
    compat_out(
        old,
        &om["result"]["schema"],
        new,
        &nm["result"]["schema"],
        &format!("{name}: result"),
        out,
    );
}

/// The command line is part of the contract too: scripts and hooks spell it. Spellings (the path, the aliases, a
/// flag's names, the presentation switches) may be added and never taken away, a required switch stays required, a
/// param keeps the way it is written, and positional words keep their order (new ones go at the end).
fn cli(old: &Value, new: &Value, name: &str, out: &mut Vec<String>) {
    if old.is_null() {
        return;
    }
    let spellings = |v: &Value| -> BTreeSet<String> {
        std::iter::once(&v["path"])
            .chain(v["aliases"].as_array().into_iter().flatten())
            .map(Value::to_string)
            .collect()
    };
    for gone in spellings(old).difference(&spellings(new)) {
        out.push(format!("{name}: command {gone} no longer spells this method"));
    }
    let words = |v: &Value, key: &str| -> BTreeSet<String> {
        v[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str().map(str::to_owned))
            .collect()
    };
    for gone in words(old, "formats").difference(&words(new, "formats")) {
        out.push(format!("{name}: switch {gone} is no longer accepted"));
    }
    if words(old, "requires") != words(new, "requires") {
        out.push(format!("{name}: the switches the command requires changed"));
    }
    let empty = serde_json::Map::new();
    let (old_args, new_args) = (
        old["args"].as_object().unwrap_or(&empty),
        new["args"].as_object().unwrap_or(&empty),
    );
    for (param, how) in old_args {
        let Some(now) = new_args.get(param) else {
            continue; // a removed param is reported with the params
        };
        if how["kind"] != now["kind"] {
            out.push(format!(
                "{name}: param {param} is written differently ({} became {})",
                how["kind"], now["kind"]
            ));
            continue;
        }
        for gone in words(how, "spellings").difference(&words(now, "spellings")) {
            out.push(format!("{name}: param {param} no longer accepts {gone}"));
        }
    }
    // Positional words by position (the args object keeps the table's param order): the old ones stay first.
    let positional = |args: &serde_json::Map<String, Value>| -> Vec<String> {
        args.iter()
            .filter(|(_, a)| a["kind"] == "positional")
            .map(|(n, _)| n.clone())
            .collect()
    };
    let (before, after) = (positional(old_args), positional(new_args));
    if !after.starts_with(&before) {
        out.push(format!(
            "{name}: positional words were reordered or removed ({before:?} became {after:?})"
        ));
    }
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

/// Results, the other way round: appends to `out` what `new` may send that `old` could not.
fn compat_out(old: &Value, o: &Value, new: &Value, n: &Value, at: &str, out: &mut Vec<String>) {
    let old_alts = alts(old, o, 0);
    if old_alts.iter().any(|a| matches!(a, Alt::Any)) {
        return;
    }
    for na in alts(new, n, 0) {
        let mut why: Vec<String> = Vec::new();
        let mut covered = false;
        for oa in &old_alts {
            let mut errs = Vec::new();
            if covers_out(old, oa, new, &na, at, &mut errs) {
                covered = true;
                break;
            }
            if std::mem::discriminant(&na) == std::mem::discriminant(oa) && why.is_empty() {
                why = errs;
            }
        }
        if !covered {
            if why.is_empty() {
                out.push(format!(
                    "{at}: may now be {}, which it never was",
                    label(&na)
                ));
            } else {
                out.append(&mut why);
            }
        }
    }
}

/// Whether what `n` (new) sends is something `o` (old) could send.
fn covers_out(
    old: &Value,
    o: &Alt<'_>,
    new: &Value,
    n: &Alt<'_>,
    at: &str,
    out: &mut Vec<String>,
) -> bool {
    match (o, n) {
        (Alt::Any, _) => true,
        (Alt::Null, Alt::Null)
        | (Alt::Bool, Alt::Bool)
        | (Alt::Int, Alt::Int)
        | (Alt::Str(None), Alt::Str(_))
        // An enum may gain values: a reader that does not know one reads the `unknown` word.
        | (Alt::Str(Some(_)), Alt::Str(Some(_))) => true,
        (Alt::Arr(oi), Alt::Arr(ni)) => {
            let before = out.len();
            compat_out(old, oi, new, ni, &format!("{at}[]"), out);
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
                    Some((_, nv)) => compat_out(old, ov, new, nv, &format!("{at}.{k}"), out),
                }
            }
            for k in oreq.difference(nreq) {
                out.push(format!("{at}: field {k} became optional"));
            }
            for (k, _) in np {
                if !op.iter().any(|(ok, _)| ok == k) && nreq.contains(k) {
                    out.push(format!("{at}: new field {k} is required (add it optional, `T?`)"));
                }
            }
            match (oextra, nextra) {
                (None, _) => {}
                (Some(oe), Some(ne)) => compat_out(old, oe, new, ne, &format!("{at}.*"), out),
                (Some(_), None) => out.push(format!("{at}: values may now be anything")),
            }
            out.len() == before
        }
        _ => false,
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

    fn result_doc(thing: Value) -> Value {
        let mut d = doc(json!([m(
            json!([]),
            json!({"$ref": "#/components/schemas/Thing"})
        )]));
        d["components"]["schemas"]["Thing"] = thing;
        d
    }
    fn thing(props: Value, required: Value) -> Value {
        json!({"type": "object", "properties": props, "required": required})
    }

    #[test]
    fn removing_or_renaming_a_result_field_breaks() {
        let mut new = base();
        new["components"]["schemas"]["Thing"]["properties"]
            .as_object_mut()
            .unwrap()
            .shift_remove("n");
        only(&base(), &new, "field n was removed or renamed");
    }

    #[test]
    fn a_result_field_made_nullable_breaks() {
        let old = result_doc(thing(json!({"id": {"type": "string"}}), json!(["id"])));
        let new = result_doc(thing(
            json!({"id": {"type": ["string", "null"]}}),
            json!(["id"]),
        ));
        only(&old, &new, "null, which it never was");
    }

    #[test]
    fn a_required_result_field_made_optional_breaks() {
        let old = result_doc(thing(json!({"id": {"type": "string"}}), json!(["id"])));
        let new = result_doc(thing(json!({"id": {"type": "string"}}), json!([])));
        only(&old, &new, "field id became optional");
    }

    #[test]
    fn a_result_turned_into_anything_breaks() {
        let old = doc(json!([m(json!([]), json!({"type": "string"}))]));
        let new = doc(json!([m(json!([]), json!({}))]));
        only(&old, &new, "result: may now be anything");
        let old = result_doc(thing(json!({"id": {"type": "string"}}), json!(["id"])));
        let new = result_doc(thing(json!({"id": {}}), json!(["id"])));
        only(&old, &new, "result.id: may now be anything");
    }

    #[test]
    fn a_new_required_result_field_breaks_but_an_optional_one_passes() {
        let old = result_doc(thing(json!({"id": {"type": "string"}}), json!(["id"])));
        let required = result_doc(thing(
            json!({"id": {"type": "string"}, "more": {"type": "string"}}),
            json!(["id", "more"]),
        ));
        only(&old, &required, "new field more is required");
        let optional = result_doc(thing(
            json!({"id": {"type": "string"}, "more": {"type": ["string", "null"]}}),
            json!(["id"]),
        ));
        assert_eq!(breaks(&old, &optional), Vec::<String>::new());
    }

    #[test]
    fn harmless_result_changes_pass() {
        // an optional field made required, a nullable made plain, an enum gaining a value
        let old = result_doc(thing(
            json!({"id": {"type": "string"}, "n": {"type": ["integer", "null"]}, "k": {"$ref": "#/components/schemas/Kind"}}),
            json!(["id"]),
        ));
        let mut new = result_doc(thing(
            json!({"id": {"type": "string"}, "n": {"type": "integer"}, "k": {"$ref": "#/components/schemas/Kind"}}),
            json!(["id", "n"]),
        ));
        new["components"]["schemas"]["Kind"]["enum"] = json!(["a", "b", "c"]);
        assert_eq!(breaks(&old, &new), Vec::<String>::new());
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

    /// A stable method whose command line is `cli` (the cli check reads `x-kith-cli`).
    fn with_cli(cli: Value) -> Value {
        let mut d = base();
        d["methods"][0]["x-kith-cli"] = cli;
        d
    }
    fn cli_doc(
        path: &[&str],
        aliases: Value,
        formats: &[&str],
        requires: &[&str],
        args: Value,
    ) -> Value {
        with_cli(
            json!({"path": path, "aliases": aliases, "formats": formats, "requires": requires, "args": args}),
        )
    }
    fn plain() -> Value {
        cli_doc(
            &["backup", "add"],
            json!([["bk", "add"]]),
            &["--json"],
            &[],
            json!({
                "entry": {"kind": "positional"},
                "name": {"kind": "positional"},
                "at": {"kind": "flag", "spellings": ["--at", "-a"]},
                "all": {"kind": "switch", "spellings": ["--all"]},
            }),
        )
    }

    #[test]
    fn the_command_line_may_only_grow() {
        let old = plain();
        assert!(breaks(&old, &old).is_empty());
        // a new alias, a new flag spelling, a new format, a new positional at the end, a new flag
        let grown = cli_doc(
            &["backup", "add"],
            json!([["bk", "add"], ["backups", "add"]]),
            &["--json", "--sh"],
            &[],
            json!({
                "entry": {"kind": "positional"},
                "name": {"kind": "positional"},
                "extra": {"kind": "positional"},
                "at": {"kind": "flag", "spellings": ["--at", "-a", "--when"]},
                "all": {"kind": "switch", "spellings": ["--all"]},
                "more": {"kind": "flag", "spellings": ["--more"]},
            }),
        );
        assert!(breaks(&old, &grown).is_empty(), "{:?}", breaks(&old, &grown));
    }

    #[test]
    fn a_path_or_alias_taken_away_breaks() {
        let old = plain();
        let args = old["methods"][0]["x-kith-cli"]["args"].clone();
        // the alias is gone
        only(
            &old,
            &cli_doc(&["backup", "add"], json!([]), &["--json"], &[], args.clone()),
            "no longer spells",
        );
        // the path moved and the old spelling is not kept as an alias
        only(
            &old,
            &cli_doc(
                &["backups", "add"],
                json!([["bk", "add"]]),
                &["--json"],
                &[],
                args.clone(),
            ),
            "no longer spells",
        );
        // the path moved and the old spelling is an alias now: fine
        let moved = cli_doc(
            &["backups", "add"],
            json!([["bk", "add"], ["backup", "add"]]),
            &["--json"],
            &[],
            args,
        );
        assert!(breaks(&old, &moved).is_empty());
    }

    #[test]
    fn a_flag_spelling_or_format_taken_away_breaks() {
        let old = plain();
        let mut args = old["methods"][0]["x-kith-cli"]["args"].clone();
        args["at"]["spellings"] = json!(["--at"]);
        only(
            &old,
            &cli_doc(&["backup", "add"], json!([["bk", "add"]]), &["--json"], &[], args),
            "no longer accepts -a",
        );
        let args = old["methods"][0]["x-kith-cli"]["args"].clone();
        only(
            &old,
            &cli_doc(&["backup", "add"], json!([["bk", "add"]]), &[], &[], args),
            "switch --json is no longer accepted",
        );
    }

    #[test]
    fn positional_words_keep_their_order() {
        let old = plain();
        let swapped = cli_doc(
            &["backup", "add"],
            json!([["bk", "add"]]),
            &["--json"],
            &[],
            json!({
                "name": {"kind": "positional"},
                "entry": {"kind": "positional"},
                "at": {"kind": "flag", "spellings": ["--at", "-a"]},
                "all": {"kind": "switch", "spellings": ["--all"]},
            }),
        );
        only(&old, &swapped, "positional words were reordered");
        // a positional word turned into a flag is also a different command line
        let flagged = cli_doc(
            &["backup", "add"],
            json!([["bk", "add"]]),
            &["--json"],
            &[],
            json!({
                "entry": {"kind": "positional"},
                "name": {"kind": "flag", "spellings": ["--name"]},
                "at": {"kind": "flag", "spellings": ["--at", "-a"]},
                "all": {"kind": "switch", "spellings": ["--all"]},
            }),
        );
        only(&old, &flagged, "is written differently");
    }

    #[test]
    fn a_param_written_another_way_or_a_changed_requirement_breaks() {
        let old = cli_doc(
            &["ls"],
            json!([]),
            &[],
            &["--json"],
            json!({"all": {"kind": "switch", "spellings": ["--all"]}}),
        );
        only(
            &old,
            &cli_doc(
                &["ls"],
                json!([]),
                &[],
                &[],
                json!({"all": {"kind": "switch", "spellings": ["--all"]}}),
            ),
            "switches the command requires changed",
        );
        only(
            &old,
            &cli_doc(
                &["ls"],
                json!([]),
                &[],
                &["--json"],
                json!({"all": {"kind": "flag", "spellings": ["--all"]}}),
            ),
            "is written differently",
        );
    }

    #[test]
    fn an_experimental_command_line_may_change() {
        let mut old = plain();
        old["methods"][0]["x-kith-stability"] = json!("experimental");
        let gone = cli_doc(&["other"], json!([]), &[], &[], json!({}));
        assert!(breaks(&old, &gone).is_empty());
    }
}
