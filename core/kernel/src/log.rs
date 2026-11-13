//! A place for the kernel's rules to say "this failed, and I carried on": a hook the hosting program sets (kith-core points
//! it at the core log), so a lock or a write that failed is not lost without a trace. kith-kernel depends on no other kith
//! crate and has no log of its own: with no hook set, a note goes nowhere, as before.

use std::sync::{Arc, Mutex, PoisonError};

type Hook = Arc<dyn Fn(&str) + Send + Sync>;

static HOOK: Mutex<Option<Hook>> = Mutex::new(None);

/// Sends the kernel's notes to `hook` from now on (replacing any earlier one: a program that moves its logs folder says
/// so by setting it again).
pub fn set_hook(hook: impl Fn(&str) + Send + Sync + 'static) {
    *HOOK.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(hook));
}

/// Leaves a note: one line, no secrets. Never fails and never panics; the caller's own answer matters more than its note.
pub fn note(what: &str) {
    let hook = HOOK.lock().unwrap_or_else(PoisonError::into_inner).clone();
    if let Some(hook) = hook {
        hook(what);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod tests {
    use super::*;

    /// Every note any test in this crate leaves since [`collect`] was first called (one process-wide hook: tests run in
    /// parallel, so each filters for its own marker).
    pub(crate) fn collect() -> Arc<Mutex<Vec<String>>> {
        static SEEN: std::sync::OnceLock<Arc<Mutex<Vec<String>>>> = std::sync::OnceLock::new();
        SEEN.get_or_init(|| {
            let seen = Arc::new(Mutex::new(Vec::<String>::new()));
            let sink = seen.clone();
            set_hook(move |m| {
                sink.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(m.to_owned());
            });
            seen
        })
        .clone()
    }

    #[test]
    fn a_note_reaches_the_hook() {
        let seen = collect();
        note("log-test-one");
        assert!(seen.lock().unwrap().iter().any(|m| m == "log-test-one"));
    }
}
