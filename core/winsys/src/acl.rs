//! Owner-only access lists, set and read natively by SID.
//!
//! What counts as owner-only (docs/plans/windows-owner.md WI4): the file's DACL has allow entries for no SID but
//! the account running this process (the token's user), `NT AUTHORITY\SYSTEM`, `BUILTIN\Administrators` and the
//! inert `CREATOR OWNER` placeholder; and the file's owner is one of the first three. Nothing here reads an account
//! name, runs a program or looks at `USERNAME`, so it means the same on every display language.
//!
//! Decided, and refused when it does not hold:
//! - A NULL DACL (or none) grants everyone full control: refused.
//! - An allow entry for any other SID, whatever its rights, inherited or explicit, inherit-only or not: refused.
//! - An allow entry of a kind this check does not read (compound, object, callback-object): refused, never skipped.
//! - Deny entries are fine: they only narrow. So are the system-only kinds (audit, label) that a DACL does not hold.
//! - An owner other than those three can rewrite the list, so it is refused. A file made by an elevated process is
//!   owned by `Administrators`, which is allowed: they already have the machine.

use std::ffi::c_void;
use std::io;
use std::path::Path;

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::{
    GetNamedSecurityInfoW, SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_REVISION, ACL_SIZE_INFORMATION, AclSizeInformation,
    AddAccessAllowedAceEx, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, GetAce,
    GetAclInformation, InitializeAcl, InitializeSecurityDescriptor, OBJECT_INHERIT_ACE,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl,
};
use windows_sys::Win32::Storage::FileSystem::{CreateDirectoryW, FILE_ALL_ACCESS};

use crate::sid::{self, Sid};
use crate::{last_err, wide, win_err};

/// What reading one file's access list came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Check {
    /// Only the owner, SYSTEM and Administrators can get in.
    OwnerOnly,
    /// Someone else can, or the list cannot be trusted. The text names why, for the refusal message.
    Open(String),
}

// The ACE kinds a DACL can hold (winnt.h). Only the first and the callback form are read; the other allow kinds are refused.
const ACCESS_ALLOWED: u8 = 0;
const ACCESS_DENIED: u8 = 1;
const ACCESS_ALLOWED_CALLBACK: u8 = 9;
const ACCESS_DENIED_OBJECT: u8 = 6;
const ACCESS_DENIED_CALLBACK: u8 = 10;
const ACCESS_DENIED_CALLBACK_OBJECT: u8 = 12;

/// The SID of the account this process runs as, as `S-1-5-21-...` text, for a message or a command that fixes a list.
pub fn current_user_text() -> io::Result<String> {
    let me = sid::current_user()?;
    Ok(Sid::text(me.as_ptr()))
}

/// The owner-only list as the words of an `ACL`: full control for the current user, SYSTEM and Administrators. With
/// `inherit` (a folder) the entries pass to everything made inside it. Shared by [`restrict`] and [`create_dir_private`], so
/// the two cannot drift apart.
fn owner_acl(inherit: bool) -> io::Result<Vec<u32>> {
    let sids = [sid::current_user()?, sid::system()?, sid::administrators()?];
    let ace_fixed = size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>();
    let size = size_of::<ACL>()
        + sids
            .iter()
            .map(|s| ace_fixed + s.len() as usize)
            .sum::<usize>();
    let mut words = vec![0u32; size.div_ceil(4)];
    let acl = words.as_mut_ptr().cast::<ACL>();
    // SAFETY: `words` is `size` bytes (rounded up), enough for the header and the three entries sized above.
    if unsafe {
        InitializeAcl(
            acl,
            u32::try_from(words.len() * 4).unwrap_or(u32::MAX),
            ACL_REVISION,
        )
    } == 0
    {
        return Err(last_err());
    }
    let flags = if inherit {
        OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
    } else {
        0
    };
    for s in &sids {
        // SAFETY: `acl` was initialised above with room for this entry; `s` is a valid SID the call copies.
        if unsafe { AddAccessAllowedAceEx(acl, ACL_REVISION, flags, FILE_ALL_ACCESS, s.as_ptr()) }
            == 0
        {
            return Err(last_err());
        }
    }
    Ok(words)
}

/// Replaces the list of `path` with a protected one (nothing inherited from the parent): full control for the current
/// user, SYSTEM and Administrators. With `inherit` (a folder) the entries pass to everything made inside it.
pub fn restrict(path: &Path, inherit: bool) -> io::Result<()> {
    let mut words = owner_acl(inherit)?;
    let acl = words.as_mut_ptr().cast::<ACL>();
    let name = wide(path.as_os_str());
    // SAFETY: `name` is NUL-ended; `acl` is a complete list; owner, group and SACL are left alone (null). PROTECTED
    // cuts the inheritance from the parent.
    let code = unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
            std::ptr::null(),
        )
    };
    if code == 0 {
        Ok(())
    } else {
        Err(win_err(code))
    }
}

/// `SECURITY_DESCRIPTOR_REVISION` (winnt.h), which windows-sys keeps behind a feature this crate does not otherwise need.
const SECURITY_DESCRIPTOR_REVISION: u32 = 1;

/// Makes the folder `path` (its parent must exist) with the protected owner-only list of [`restrict`] in the same call that
/// creates it, so there is no moment at which it holds the list it would inherit: nobody else can have opened it or put
/// anything in it before it is private. An existing folder is reported as `AlreadyExists`, and is left as it is.
pub fn create_dir_private(path: &Path, inherit: bool) -> io::Result<()> {
    let mut words = owner_acl(inherit)?;
    let acl = words.as_mut_ptr().cast::<ACL>();
    let mut sd = std::mem::MaybeUninit::<SECURITY_DESCRIPTOR>::zeroed();
    let psd = sd.as_mut_ptr().cast::<c_void>();
    // SAFETY: `psd` points at zeroed storage the size of a security descriptor, which InitializeSecurityDescriptor fills.
    if unsafe { InitializeSecurityDescriptor(psd, SECURITY_DESCRIPTOR_REVISION) } == 0 {
        return Err(last_err());
    }
    // SAFETY: `psd` was initialised above; `acl` is a complete list that outlives the create call below (the descriptor
    // points at it rather than copying it).
    if unsafe { SetSecurityDescriptorDacl(psd, 1, acl, 0) } == 0 {
        return Err(last_err());
    }
    // PROTECTED: the parent's inheritable entries are not merged in, as with `restrict`.
    // SAFETY: `psd` holds a descriptor with a DACL.
    if unsafe { SetSecurityDescriptorControl(psd, SE_DACL_PROTECTED, SE_DACL_PROTECTED) } == 0 {
        return Err(last_err());
    }
    let attrs = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
        lpSecurityDescriptor: psd,
        bInheritHandle: 0,
    };
    let name = wide(path.as_os_str());
    // SAFETY: `name` is NUL-ended; `attrs` and the descriptor and list it points at live until the call returns.
    if unsafe { CreateDirectoryW(name.as_ptr(), &raw const attrs) } == 0 {
        // the last error carries ERROR_ALREADY_EXISTS, which `io::Error` maps to `AlreadyExists`
        return Err(last_err());
    }
    Ok(())
}

/// Reads the access list and owner of `path` and judges them. An `Err` means they could not be read (the file is gone,
/// access denied, a sharing violation): the caller decides whether to try again.
pub fn check(path: &Path) -> io::Result<Check> {
    let me = sid::current_user()?;
    let (system, admins, creator) = (
        sid::system()?,
        sid::administrators()?,
        sid::creator_owner()?,
    );
    let name = wide(path.as_os_str());
    let mut owner: *mut c_void = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `name` is NUL-ended; the out-pointers outlive the call; `owner` and `dacl` point into `sd`, which is
    // freed below after the last use.
    let code = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &raw mut owner,
            std::ptr::null_mut(),
            &raw mut dacl,
            std::ptr::null_mut(),
            &raw mut sd,
        )
    };
    if code != 0 {
        return Err(win_err(code));
    }
    let verdict = judge(owner, dacl, &me, &system, &admins, &creator);
    // SAFETY: `sd` was allocated by the call above with LocalAlloc and nothing points into it any more.
    unsafe { LocalFree(sd) };
    Ok(verdict)
}

fn judge(
    owner: *mut c_void,
    dacl: *mut ACL,
    me: &Sid,
    system: &Sid,
    admins: &Sid,
    creator: &Sid,
) -> Check {
    if !(me.is(owner) || system.is(owner) || admins.is(owner)) {
        return Check::Open(format!("it is owned by {}", Sid::text(owner)));
    }
    if dacl.is_null() {
        return Check::Open("it has no access list, which lets everyone in".to_owned());
    }
    let mut info = ACL_SIZE_INFORMATION {
        AceCount: 0,
        AclBytesInUse: 0,
        AclBytesFree: 0,
    };
    // SAFETY: `dacl` is a list returned by GetNamedSecurityInfoW; `info` is the size the class asks for.
    let ok = unsafe {
        GetAclInformation(
            dacl,
            (&raw mut info).cast(),
            u32::try_from(size_of::<ACL_SIZE_INFORMATION>()).unwrap_or(0),
            AclSizeInformation,
        )
    };
    if ok == 0 {
        return Check::Open("its access list cannot be read".to_owned());
    }
    for i in 0..info.AceCount {
        let mut ace: *mut c_void = std::ptr::null_mut();
        // SAFETY: `i` is below the entry count of `dacl`.
        if unsafe { GetAce(dacl, i, &raw mut ace) } == 0 || ace.is_null() {
            return Check::Open("its access list cannot be read".to_owned());
        }
        let header = ace.cast::<ACE_HEADER>();
        // SAFETY: GetAce returned a pointer to a whole entry, which starts with its header.
        let kind = unsafe { (*header).AceType };
        match kind {
            ACCESS_DENIED
            | ACCESS_DENIED_OBJECT
            | ACCESS_DENIED_CALLBACK
            | ACCESS_DENIED_CALLBACK_OBJECT => {}
            ACCESS_ALLOWED | ACCESS_ALLOWED_CALLBACK => {
                // SAFETY: both kinds are an ACCESS_ALLOWED_ACE (header, mask, SID); the SID starts at `SidStart`.
                let who = unsafe {
                    (&raw mut (*ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart).cast::<c_void>()
                };
                if !(me.is(who) || system.is(who) || admins.is(who) || creator.is(who)) {
                    return Check::Open(format!("it lets {} in", Sid::text(who)));
                }
            }
            other => {
                return Check::Open(format!(
                    "its access list has an entry of a kind kith does not read ({other})"
                ));
            }
        }
    }
    Check::OwnerOnly
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Adds a read entry for a SID with `icacls`, whose `*S-...` form takes a SID, so it is language-free.
    fn grant(path: &Path, sid: &str) {
        let out = Command::new("icacls")
            .arg(path)
            .args(["/grant", &format!("*{sid}:(R)")])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "icacls: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    #[test]
    fn a_restricted_folder_and_the_files_made_in_it_are_owner_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("core");
        std::fs::create_dir(&dir).unwrap();
        restrict(&dir, true).unwrap();
        assert_eq!(check(&dir).unwrap(), Check::OwnerOnly);
        let file = dir.join("data.db");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(
            check(&file).unwrap(),
            Check::OwnerOnly,
            "a file takes the folder's list"
        );
    }

    #[test]
    fn restricting_replaces_what_the_folder_inherited() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("x");
        std::fs::create_dir(&dir).unwrap();
        grant(&dir, "S-1-5-32-545");
        assert!(matches!(check(&dir).unwrap(), Check::Open(_)));
        restrict(&dir, true).unwrap();
        assert_eq!(check(&dir).unwrap(), Check::OwnerOnly);
    }

    #[test]
    fn an_entry_for_everyone_or_users_is_refused() {
        for sid in ["S-1-1-0", "S-1-5-32-545", "S-1-5-11"] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().join("core");
            std::fs::create_dir(&dir).unwrap();
            restrict(&dir, true).unwrap();
            grant(&dir, sid);
            match check(&dir).unwrap() {
                Check::Open(why) => assert!(why.contains(sid), "{why}"),
                Check::OwnerOnly => panic!("{sid} was let through"),
            }
        }
    }

    #[test]
    fn a_folder_made_private_is_owner_only_from_the_first_moment() {
        let tmp = tempfile::tempdir().unwrap();
        // the parent lets Users in, as a shared temp folder may
        grant(tmp.path(), "S-1-5-32-545");
        let dir = tmp.path().join("fresh");
        create_dir_private(&dir, true).unwrap();
        assert_eq!(check(&dir).unwrap(), Check::OwnerOnly);
        let file = dir.join("data");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(check(&file).unwrap(), Check::OwnerOnly);
        // an existing folder is reported, and left as it was
        let e = create_dir_private(&dir, true).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(check(&dir).unwrap(), Check::OwnerOnly);
    }

    #[test]
    fn an_unreadable_path_is_an_error_not_a_pass() {
        let tmp = tempfile::tempdir().unwrap();
        let e = check(&tmp.path().join("nothing-here")).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn the_user_sid_is_a_sid_not_a_name() {
        assert!(current_user_text().unwrap().starts_with("S-1-5-"));
    }
}
