//! The real cutover modules, one file each. A module is registered in `cutover::registry` and switched by nobody but
//! `kith migrate step MODULE`; a registered module that was never stepped changes nothing.
//!
//! S1 (M3-10e): [`accounts`] and [`transcripts`]. Neither has a secret in its sources, so neither overrides `restore_secrets`
//! (the framework's default writes nothing back): `state.json` holds ids, times, reasons and usage percentages only, and the
//! session registry holds session ids, paths and a working folder. Both are Unix only, like the framework (Windows says
//! "not available on this operating system yet" for every cutover verb, and the accounts failover there stays behind its
//! `meta.accounts_core_writes` flag, which nothing sets).

//!
//! S2 (M3-10c): [`claims`], the live writer claim files; registered for its import, never stepped (the Python writes them).
//!
//! M4-5a: [`schedules`], the host's routines and `kith-sched`'s state. It does have secrets (the webhook keys), which it
//! writes into the envelope itself and puts back into serve.db at a rollback; see its doc.

pub(crate) mod accounts;
pub(crate) mod claims;
pub(crate) mod schedules;
#[cfg(test)]
mod schedules_tests;
#[cfg(test)]
mod tests;
pub(crate) mod transcripts;
