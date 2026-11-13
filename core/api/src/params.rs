//! Checking a params object against a row, before anything runs.

use serde_json::Value;

use crate::Method;
use crate::schema::{Ty, parse};

/// Whether `params` is what `method` takes: an object, every required param there, none unknown, each of its type.
/// A named type (an object the caller made up) is not looked into: params are scalars and lists of them today.
pub fn validate(method: &Method, params: &Value) -> Result<(), String> {
    let Some(obj) = params.as_object() else {
        return Err(format!("{}: params must be an object", method.name));
    };
    for key in obj.keys() {
        if !method.params.iter().any(|p| p.name == key) {
            return Err(format!("{}: unknown param {key}", method.name));
        }
    }
    for p in method.params {
        match obj.get(p.name) {
            None if p.required => return Err(format!("{}: missing param {}", method.name, p.name)),
            Some(v) if !v.is_null() && !fits(p.ty, v) => {
                return Err(format!(
                    "{}: param {} is not a {}",
                    method.name, p.name, p.ty
                ));
            }
            Some(Value::Null) if p.required => {
                return Err(format!("{}: missing param {}", method.name, p.name));
            }
            _ => {}
        }
    }
    Ok(())
}

fn fits(expr: &str, v: &Value) -> bool {
    fn go(t: &Ty<'_>, v: &Value) -> bool {
        match t {
            Ty::String => v.is_string(),
            Ty::Bool => v.is_boolean(),
            Ty::Int => v.is_i64() || v.is_u64(),
            Ty::Json | Ty::Named(_) => true,
            Ty::List(t) => v.as_array().is_some_and(|a| a.iter().all(|x| go(t, x))),
            Ty::Map(t) => v.as_object().is_some_and(|o| o.values().all(|x| go(t, x))),
            Ty::Nullable(t) => v.is_null() || go(t, v),
        }
    }
    parse(expr).is_some_and(|t| go(&t, v))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::method;

    #[test]
    fn checks_names_types_and_required() {
        let m = method("backup.add").unwrap();
        assert!(validate(m, &json!({"entry": "codex", "at": 1})).is_ok());
        assert!(
            validate(m, &json!({"at": 1}))
                .unwrap_err()
                .contains("missing param entry")
        );
        assert!(
            validate(m, &json!({"entry": "codex", "at": "1"}))
                .unwrap_err()
                .contains("not a int")
        );
        assert!(
            validate(m, &json!({"entry": "codex", "nope": 1}))
                .unwrap_err()
                .contains("unknown param")
        );
        assert!(validate(m, &json!([])).is_err());
    }
}
