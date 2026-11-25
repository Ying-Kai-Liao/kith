//! `kith kernel wake <verb>`: `kith wake` as a client of the core's wake rows (`kith-kernel`'s `wake` module), with no Python.
//! Like `threads_cmd`, this is the one place that reaches the core, because the crate never does: each call is `core-call`'s
//! own, in this process (`core_front::core_call_within`). `kernel/bin/kith` routes `kith wake` here only once the queue is the
//! core's (the tombstone in `sessions/queue/`) and the program lists the `wake` module; before that kith-wake answers.

use std::time::Duration;

use kith_kernel::paths::process_env;
use kith_kernel::wake::{Call, Fail, run};
use serde_json::Value;

use crate::core_front::{self, CallEnd};

/// One call: the Python gave eight seconds to find the program and eight to the call (`kith_claims.call`).
const CALL_SECS: u64 = 16;

/// `wake.*` rows reach the owner socket through `core-call`'s own list (`core_front::CORE_CALL_METHODS`).
fn call(method: &str, params: &Value) -> Result<Value, Fail> {
    match core_front::core_call_within(method, params, Duration::from_secs(CALL_SECS)) {
        CallEnd::Answered(v) if v.is_object() => Ok(v),
        CallEnd::Answered(_) => Err(Fail::NoAnswer("the answer is not a JSON object".to_owned())),
        CallEnd::Refused(why) => Err(Fail::Refused(if why.is_empty() { "refused".to_owned() } else { why })),
        CallEnd::NoAnswer(why, code) => {
            Err(Fail::NoAnswer(if why.is_empty() { format!("core-call exited {code}") } else { why }))
        }
    }
}

/// `kith kernel wake VERB ...`; returns the exit code.
pub fn main(args: &[String]) -> i32 {
    let (mut out, mut err) = (std::io::stdout().lock(), std::io::stderr().lock());
    let c: &Call = &call;
    run(args, &process_env, c, &mut out, &mut err)
}

// The daemon the test starts listens on a Unix socket.
#[cfg(all(test, unix))]
mod tests;
