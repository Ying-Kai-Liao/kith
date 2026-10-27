//! Who owns this computer is decided here, never by the cloud alone (R25): the host pins the account it was claimed
//! by, and a state pull whose owner is another account (or none while one is pinned) is held until someone on this
//! computer confirms it. Only local calls pin: `claimSign`, `unclaim` and `confirmOwner`. A host with no pin yet
//! (claimed before pins existed) pins the account this Mac already knows as its owner, and only when it knows none
//! takes the cloud's first answer, marked as such (`from_cloud`) until it is confirmed here.

use crate::remote::cloud::Owner;
use crate::store::{Store, now_ms};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::Path;

/// kv: the pinned owner, JSON `Owner` or `null` for no owner. Missing on a host claimed before pins existed.
const PIN: &str = "owner_pin";
/// kv: `cloud` while the pin is the cloud's first answer, taken when this Mac knew no owner account of its own.
const SOURCE: &str = "owner_pin_source";
/// kv: the account the last `claimSign` signed for, and when (JSON `Signed`).
const SIGNED: &str = "owner_signed";
/// How long a signature made here counts as this computer's yes to that account becoming its owner.
const SIGNED_MS: i64 = 60 * 60 * 1000;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Signed {
    user_id: String,
    at: i64,
}

/// What a state pull may do with the cloud's owner.
#[derive(Debug, PartialEq, Eq)]
pub enum Check {
    /// The pinned owner (now also the cloud's): apply the state.
    Trusted(Option<Owner>),
    /// The cloud names another owner than the pinned one: nothing owner-dependent is applied.
    Held { pinned: Option<Owner>, cloud: Option<Owner> },
}

/// Whether nothing is pinned yet: the pull then looks up what this Mac knows of its owner (`known`) first.
pub fn unpinned(store: &Store) -> bool {
    store.kv_get(PIN).is_none()
}

/// The owner account this Mac already knows, for a host with no pin yet: the account handle the host bound to the
/// owner person (`remote::account`), else the owner person's `kith` handle in kith-auth. Both are Kith account ids,
/// the same ids the cloud names its owner by.
pub async fn known(store: &Store, auth_bin: Option<&Path>) -> Option<String> {
    if let Some(uid) = super::account::applied(store) {
        return Some(uid);
    }
    super::account::owner_handle(auth_bin?).await
}

/// Compares the cloud's owner with the pin, pinning it when this computer agreed to it: a claim signed here within
/// the hour. With no pin yet and an owner named by the cloud, `known` (the owner account this Mac knows, see `known`)
/// is pinned and the cloud's owner compared with it; with nothing known, the cloud's owner is pinned and marked
/// `from_cloud`.
pub fn check(store: &Store, cloud: Option<&Owner>, known: Option<&str>) -> Result<Check> {
    let id = |o: Option<&Owner>| o.map(|o| o.user_id.clone());
    if let Some(o) = cloud
        && signed(store).is_some_and(|s| s == o.user_id)
    {
        pin(store, cloud)?;
        store.kv_delete(SIGNED)?;
        return Ok(Check::Trusted(cloud.cloned()));
    }
    if unpinned(store) {
        match (cloud, known) {
            // No owner gives no one anything: nothing to hold (and a host not yet claimed stays claimable).
            (None, _) => {
                pin(store, None)?;
                return Ok(Check::Trusted(None));
            }
            (Some(_), Some(uid)) => pin(store, Some(&Owner { user_id: uid.to_owned(), email: None }))?,
            (Some(_), None) => {
                write(store, cloud)?;
                store.kv_set(SOURCE, "cloud")?;
                return Ok(Check::Trusted(cloud.cloned()));
            }
        }
    }
    // A pin that doesn't read counts as no owner: the cloud's owner then waits for a confirmation.
    let pinned: Option<Owner> =
        store.kv_get(PIN).and_then(|raw| serde_json::from_str::<Option<Owner>>(&raw).ok()).flatten();
    if id(pinned.as_ref()) != id(cloud) {
        return Ok(Check::Held { pinned, cloud: cloud.cloned() });
    }
    if pinned.as_ref() != cloud {
        // The same account with a new email: the pin follows, and keeps where it came from.
        write(store, cloud)?;
    }
    Ok(Check::Trusted(cloud.cloned()))
}

/// Pins `owner` (None: no owner). Called for a local confirmation or an agreed claim only.
pub fn pin(store: &Store, owner: Option<&Owner>) -> Result<()> {
    write(store, owner)?;
    store.kv_delete(SOURCE)
}

fn write(store: &Store, owner: Option<&Owner>) -> Result<()> {
    store.kv_set(PIN, &serde_json::to_string(&owner)?)
}

/// Whether the pinned owner is the cloud's first answer, not yet confirmed on this computer.
pub fn from_cloud(store: &Store) -> bool {
    store.kv_get(SOURCE).as_deref() == Some("cloud")
}

/// `claimSign` signed this computer into `user_id`'s account: a pull that names it as owner within the hour pins it.
pub fn signed_for(store: &Store, user_id: &str) -> Result<()> {
    store.kv_set(SIGNED, &serde_json::to_string(&Signed { user_id: user_id.to_owned(), at: now_ms() })?)
}

fn signed(store: &Store) -> Option<String> {
    let s: Signed = serde_json::from_str(&store.kv_get(SIGNED)?).ok()?;
    (now_ms() - s.at < SIGNED_MS).then_some(s.user_id)
}

/// `kith-serve cloud --confirm-owner` runs only with a terminal on stdin, so no script or agent confirms an owner by
/// piping an answer in.
pub fn require_terminal(terminal: bool) -> Result<()> {
    if !terminal {
        bail!(
            "An owner change is confirmed by a person: run `kith-serve cloud --confirm-owner` in a terminal, or use the \
             Kith app (Settings > Computers)."
        );
    }
    Ok(())
}

/// The person at the terminal types the account they accept (`expected`, or `none`).
pub fn typed_confirmation(expected: &str, input: &mut impl BufRead, out: &mut impl Write) -> Result<()> {
    write!(out, "Type {expected} to accept it: ")?;
    out.flush()?;
    let mut typed = String::new();
    input.read_line(&mut typed)?;
    if typed.trim() != expected {
        bail!("Not confirmed: that isn't {expected}.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn store() -> Store {
        let dir = std::env::temp_dir().join(format!("kith-owner-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Store::open(&dir.join("t.db")).unwrap()
    }

    fn owner(id: &str) -> Owner {
        Owner { user_id: id.into(), email: None }
    }

    fn pinned(store: &Store) -> Option<Owner> {
        store.kv_get(PIN).and_then(|raw| serde_json::from_str(&raw).ok().flatten())
    }

    #[test]
    fn a_host_that_knows_no_owner_takes_the_clouds_first_answer_and_says_so() {
        let s = store();
        assert_eq!(check(&s, Some(&owner("user_kai")), None).unwrap(), Check::Trusted(Some(owner("user_kai"))));
        assert_eq!(pinned(&s), Some(owner("user_kai")));
        assert!(from_cloud(&s), "set from the cloud");
        let mailed = Owner { user_id: "user_kai".into(), email: Some("k@example.com".into()) };
        check(&s, Some(&mailed), None).unwrap();
        assert!(from_cloud(&s), "a new email doesn't make it confirmed");
        let mallory = check(&s, Some(&owner("user_mallory")), None).unwrap();
        assert_eq!(mallory, Check::Held { pinned: Some(mailed.clone()), cloud: Some(owner("user_mallory")) });
        assert_eq!(
            check(&s, None, None).unwrap(),
            Check::Held { pinned: Some(mailed.clone()), cloud: None },
            "missing"
        );
        assert_eq!(pinned(&s), Some(mailed.clone()), "the cloud never moves the pin");
        pin(&s, Some(&mailed)).unwrap();
        assert!(!from_cloud(&s), "confirmed here");
    }

    #[test]
    fn a_host_pins_the_owner_this_mac_knows_before_the_clouds() {
        let s = store();
        let held = check(&s, Some(&owner("user_mallory")), Some("user_kai")).unwrap();
        assert_eq!(held, Check::Held { pinned: Some(owner("user_kai")), cloud: Some(owner("user_mallory")) });
        assert_eq!(pinned(&s), Some(owner("user_kai")));
        assert!(!from_cloud(&s));
        assert!(matches!(check(&s, Some(&owner("user_mallory")), Some("user_mallory")).unwrap(), Check::Held { .. }));
        assert_eq!(pinned(&s), Some(owner("user_kai")), "known only seeds a missing pin");

        let s = store();
        assert_eq!(
            check(&s, Some(&owner("user_kai")), Some("user_kai")).unwrap(),
            Check::Trusted(Some(owner("user_kai")))
        );
        assert!(!from_cloud(&s), "the cloud agreed with this Mac");
        let s = store();
        assert_eq!(check(&s, None, Some("user_kai")).unwrap(), Check::Trusted(None), "no owner is nobody's gain");
        assert!(matches!(check(&s, Some(&owner("user_kai")), Some("user_kai")).unwrap(), Check::Held { .. }));
        let s = store();
        assert_eq!(check(&s, None, None).unwrap(), Check::Trusted(None));
        assert!(!from_cloud(&s), "no owner taken from anyone");
    }

    #[tokio::test]
    async fn what_the_mac_knows_is_the_bound_handle_then_kith_auths_owner() {
        let _g = crate::remote::account::ONE_AT_A_TIME.lock().await;
        let dir = std::env::temp_dir().join(format!("kith-owner-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../kernel/bin/kith-auth");
        *crate::channels::hq::TEST_ENV.lock().unwrap() = vec![("KITH_AUTH", dir.join("auth.json"))];
        let auth = |args: &[&str]| {
            let out = std::process::Command::new(&bin).args(args).env("KITH_AUTH", dir.join("auth.json")).output();
            assert!(out.unwrap().status.success(), "kith-auth {args:?}");
        };
        let s = store();
        assert_eq!(known(&s, Some(&bin)).await, None, "no people yet");
        auth(&["person", "kai", "owner"]);
        auth(&["person", "sam"]);
        auth(&["handle", "sam", "kith", "user_sam"]);
        assert_eq!(known(&s, Some(&bin)).await, None, "the owner has no account handle");
        auth(&["handle", "kai", "kith", "user_auth"]);
        assert_eq!(known(&s, Some(&bin)).await.as_deref(), Some("user_auth"));
        s.kv_set(crate::remote::account::APPLIED, "user_bound").unwrap();
        assert_eq!(known(&s, Some(&bin)).await.as_deref(), Some("user_bound"), "the host's bound handle first");
        assert_eq!(known(&s, None).await.as_deref(), Some("user_bound"), "and without kith-auth");
        crate::channels::hq::TEST_ENV.lock().unwrap().clear();
    }

    #[test]
    fn only_a_terminal_confirms_and_only_with_the_account_typed() {
        assert!(require_terminal(false).is_err(), "stdin that isn't a terminal is refused");
        assert!(require_terminal(true).is_ok());
        let mut shown = Vec::new();
        typed_confirmation("user_kai", &mut "user_kai\n".as_bytes(), &mut shown).unwrap();
        assert!(String::from_utf8(shown).unwrap().contains("Type user_kai"));
        assert!(typed_confirmation("user_kai", &mut "yes\n".as_bytes(), &mut Vec::new()).is_err());
        assert!(typed_confirmation("none", &mut "".as_bytes(), &mut Vec::new()).is_err(), "no answer is no");
    }

    #[test]
    fn the_same_owner_is_trusted_and_its_email_follows() {
        let s = store();
        pin(&s, Some(&owner("user_kai"))).unwrap();
        let mailed = Owner { user_id: "user_kai".into(), email: Some("k@example.com".into()) };
        assert_eq!(check(&s, Some(&mailed), None).unwrap(), Check::Trusted(Some(mailed.clone())));
        assert_eq!(pinned(&s), Some(mailed));
    }

    #[test]
    fn an_unclaimed_pin_holds_an_owner_this_computer_never_signed_for() {
        let s = store();
        pin(&s, None).unwrap();
        assert_eq!(check(&s, None, None).unwrap(), Check::Trusted(None));
        assert_eq!(
            check(&s, Some(&owner("user_mallory")), None).unwrap(),
            Check::Held { pinned: None, cloud: Some(owner("user_mallory")) }
        );
    }

    #[test]
    fn a_claim_signed_here_pins_its_account_once() {
        let s = store();
        pin(&s, Some(&owner("user_old"))).unwrap();
        signed_for(&s, "user_new").unwrap();
        assert!(
            matches!(check(&s, Some(&owner("user_other")), None).unwrap(), Check::Held { .. }),
            "only the signed account"
        );
        assert_eq!(check(&s, Some(&owner("user_new")), None).unwrap(), Check::Trusted(Some(owner("user_new"))));
        assert_eq!(pinned(&s), Some(owner("user_new")));
        pin(&s, None).unwrap();
        assert!(
            matches!(check(&s, Some(&owner("user_new")), None).unwrap(), Check::Held { .. }),
            "a signature pins once"
        );
    }

    #[test]
    fn an_old_signature_pins_nothing() {
        let s = store();
        pin(&s, None).unwrap();
        let old = Signed { user_id: "user_new".into(), at: now_ms() - SIGNED_MS - 1 };
        s.kv_set(SIGNED, &serde_json::to_string(&old).unwrap()).unwrap();
        assert!(matches!(check(&s, Some(&owner("user_new")), None).unwrap(), Check::Held { .. }));
    }

    #[test]
    fn an_unreadable_pin_is_no_owner() {
        let s = store();
        s.kv_set(PIN, "{not json").unwrap();
        assert!(matches!(check(&s, Some(&owner("user_kai")), None).unwrap(), Check::Held { pinned: None, .. }));
    }
}
