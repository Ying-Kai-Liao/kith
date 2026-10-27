//! Who owns this computer is decided here, never by the cloud alone (R25): the host pins the account it was claimed
//! by, and a state pull whose owner is another account (or none while one is pinned) is held until someone on this
//! computer confirms it. Only local calls pin: `claimSign`, `unclaim` and `confirmOwner`.

use crate::remote::cloud::Owner;
use crate::store::{Store, now_ms};
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// kv: the pinned owner, JSON `Owner` or `null` for no owner. Missing on a host claimed before pins existed.
const PIN: &str = "owner_pin";
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

/// Compares the cloud's owner with the pin, pinning it when this computer agreed to it: a claim signed here within
/// the hour, or the first pull of a host claimed before pins existed.
pub fn check(store: &Store, cloud: Option<&Owner>) -> Result<Check> {
    let id = |o: Option<&Owner>| o.map(|o| o.user_id.clone());
    if let Some(o) = cloud
        && signed(store).is_some_and(|s| s == o.user_id)
    {
        pin(store, cloud)?;
        store.kv_delete(SIGNED)?;
        return Ok(Check::Trusted(cloud.cloned()));
    }
    let Some(raw) = store.kv_get(PIN) else {
        pin(store, cloud)?;
        return Ok(Check::Trusted(cloud.cloned()));
    };
    // A pin that doesn't read counts as no owner: the cloud's owner then waits for a confirmation.
    let pinned: Option<Owner> = serde_json::from_str(&raw).unwrap_or(None);
    if id(pinned.as_ref()) != id(cloud) {
        return Ok(Check::Held { pinned, cloud: cloud.cloned() });
    }
    if pinned.as_ref() != cloud {
        pin(store, cloud)?;
    }
    Ok(Check::Trusted(cloud.cloned()))
}

/// Pins `owner` (None: no owner). Called for a local confirmation or an agreed claim only.
pub fn pin(store: &Store, owner: Option<&Owner>) -> Result<()> {
    store.kv_set(PIN, &serde_json::to_string(&owner)?)
}

/// `claimSign` signed this computer into `user_id`'s account: a pull that names it as owner within the hour pins it.
pub fn signed_for(store: &Store, user_id: &str) -> Result<()> {
    store.kv_set(SIGNED, &serde_json::to_string(&Signed { user_id: user_id.to_owned(), at: now_ms() })?)
}

fn signed(store: &Store) -> Option<String> {
    let s: Signed = serde_json::from_str(&store.kv_get(SIGNED)?).ok()?;
    (now_ms() - s.at < SIGNED_MS).then_some(s.user_id)
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
    fn a_host_claimed_before_pins_pins_on_its_first_pull() {
        let s = store();
        assert_eq!(check(&s, Some(&owner("user_kai"))).unwrap(), Check::Trusted(Some(owner("user_kai"))));
        assert_eq!(pinned(&s), Some(owner("user_kai")));
        let mallory = check(&s, Some(&owner("user_mallory"))).unwrap();
        assert_eq!(mallory, Check::Held { pinned: Some(owner("user_kai")), cloud: Some(owner("user_mallory")) });
        assert_eq!(
            check(&s, None).unwrap(),
            Check::Held { pinned: Some(owner("user_kai")), cloud: None },
            "missing while pinned"
        );
        assert_eq!(pinned(&s), Some(owner("user_kai")), "the cloud never moves the pin");
    }

    #[test]
    fn the_same_owner_is_trusted_and_its_email_follows() {
        let s = store();
        pin(&s, Some(&owner("user_kai"))).unwrap();
        let mailed = Owner { user_id: "user_kai".into(), email: Some("k@example.com".into()) };
        assert_eq!(check(&s, Some(&mailed)).unwrap(), Check::Trusted(Some(mailed.clone())));
        assert_eq!(pinned(&s), Some(mailed));
    }

    #[test]
    fn an_unclaimed_pin_holds_an_owner_this_computer_never_signed_for() {
        let s = store();
        pin(&s, None).unwrap();
        assert_eq!(check(&s, None).unwrap(), Check::Trusted(None));
        assert_eq!(
            check(&s, Some(&owner("user_mallory"))).unwrap(),
            Check::Held { pinned: None, cloud: Some(owner("user_mallory")) }
        );
    }

    #[test]
    fn a_claim_signed_here_pins_its_account_once() {
        let s = store();
        pin(&s, Some(&owner("user_old"))).unwrap();
        signed_for(&s, "user_new").unwrap();
        assert!(
            matches!(check(&s, Some(&owner("user_other"))).unwrap(), Check::Held { .. }),
            "only the signed account"
        );
        assert_eq!(check(&s, Some(&owner("user_new"))).unwrap(), Check::Trusted(Some(owner("user_new"))));
        assert_eq!(pinned(&s), Some(owner("user_new")));
        pin(&s, None).unwrap();
        assert!(matches!(check(&s, Some(&owner("user_new"))).unwrap(), Check::Held { .. }), "a signature pins once");
    }

    #[test]
    fn an_old_signature_pins_nothing() {
        let s = store();
        pin(&s, None).unwrap();
        let old = Signed { user_id: "user_new".into(), at: now_ms() - SIGNED_MS - 1 };
        s.kv_set(SIGNED, &serde_json::to_string(&old).unwrap()).unwrap();
        assert!(matches!(check(&s, Some(&owner("user_new"))).unwrap(), Check::Held { .. }));
    }

    #[test]
    fn an_unreadable_pin_is_no_owner() {
        let s = store();
        s.kv_set(PIN, "{not json").unwrap();
        assert!(matches!(check(&s, Some(&owner("user_kai"))).unwrap(), Check::Held { pinned: None, .. }));
    }
}
