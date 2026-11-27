//! Born switched (windows-owner WI13). On a Mac a module is "switched" when its cutover record says so: the core took over
//! from an old (Python or file) writer, and `kith rollback` can give it back. Windows has no old writer, so there is no
//! record, no tombstone and no rollback: the core is the only writer of a store from its first start, or it does not
//! write that store at all. One function answers for every module on Windows, from a constant fixed in code, so
//! "switched" never depends on a file a Windows install does not have.
//!
//! A module the core does not write on Windows yet answers `false`, and its verbs refuse by name: they never run in the
//! unswitched mode, which assumes a Python writer that is not there. A module turns `true` here in the package that makes
//! its core writer complete on Windows, and the test below lists every module's answer so that is a visible change.

/// Every module of the cutover framework (`cutover::registry`), with the answer a Windows core gives. Authority is
/// `true` from W22b: `people.*`, the ask tree and the doors' rows are written by the core alone.
#[cfg_attr(
    unix,
    allow(dead_code, reason = "Windows asks; a Mac has the cutover records")
)]
pub(crate) const MODULES: [(&str, bool); 6] = [
    ("accounts", false),
    ("authority", true),
    ("claims", false),
    ("schedules", false),
    ("transcripts", false),
    // Wake on Windows stays refused by name until the Windows owner package that runs its turns turns this on.
    ("wake", false),
];

/// Whether `module` is switched on a Windows core. An unknown module is not.
#[cfg_attr(
    unix,
    allow(dead_code, reason = "Windows asks; a Mac has the cutover records")
)]
pub(crate) fn on_windows(module: &str) -> bool {
    MODULES.iter().any(|(m, on)| *m == module && *on)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list of every module's answer: adding a module, or turning one on, changes this test.
    #[test]
    fn every_module_has_an_answer_and_only_authority_is_on() {
        let on: Vec<&str> = MODULES
            .iter()
            .filter(|(_, on)| *on)
            .map(|(m, _)| *m)
            .collect();
        assert_eq!(on, ["authority"]);
        for (m, on) in MODULES {
            assert_eq!(on_windows(m), on, "{m}");
        }
        assert!(!on_windows("nonesuch"));
    }

    /// The table names the modules the cutover framework registers, no more and no fewer.
    #[cfg(unix)]
    #[test]
    fn the_table_is_the_registry() {
        let mut registered: Vec<&str> = crate::cutover::registry()
            .iter()
            .map(|m| m.name())
            .collect();
        registered.sort_unstable();
        let mut listed: Vec<&str> = MODULES.iter().map(|(m, _)| *m).collect();
        listed.sort_unstable();
        assert_eq!(registered, listed);
    }
}
