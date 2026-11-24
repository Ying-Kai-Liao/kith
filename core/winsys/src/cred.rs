//! One generic Windows Credential Manager item: where the secrets envelope's master key lives (D4).
//!
//! The item is stored for this user on this machine (`CRED_PERSIST_LOCAL_MACHINE`: it does not roam). Credential
//! Manager has no per-program rule: any process of the same user can read it, as with the Secret Service on Linux.

use std::ffi::OsStr;
use std::io;

use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, FILETIME};
use windows_sys::Win32::Security::Credentials::{
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW,
};
use zeroize::Zeroizing;

use crate::{last_err, wide};

/// What Credential Manager accepts as a blob: 5 * 512 bytes.
pub const MAX_BLOB: usize = 2560;

/// Reads the item named `target`. `Ok(None)` only when Credential Manager says there is no such item.
pub fn read(target: &str) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
    let name = wide(OsStr::new(target));
    let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: `name` is NUL-ended; on success `cred` is a buffer owned by Credential Manager, freed below.
    if unsafe { CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &raw mut cred) } == 0 {
        let e = last_err();
        return if e.raw_os_error().and_then(|c| u32::try_from(c).ok()) == Some(ERROR_NOT_FOUND) {
            Ok(None)
        } else {
            Err(e)
        };
    }
    // SAFETY: success: `cred` points at a CREDENTIALW whose blob is `CredentialBlobSize` readable bytes.
    let blob = unsafe {
        let c = &*cred;
        let bytes = if c.CredentialBlob.is_null() || c.CredentialBlobSize == 0 {
            &[][..]
        } else {
            std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize)
        };
        Zeroizing::new(bytes.to_vec())
    };
    // SAFETY: `cred` came from CredReadW and is freed once.
    unsafe { CredFree(cred.cast()) };
    Ok(Some(blob))
}

/// Writes the item named `target`, replacing any that is there (so a repeat is harmless).
pub fn write(target: &str, blob: &[u8]) -> io::Result<()> {
    if blob.is_empty() || blob.len() > MAX_BLOB {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "kith-winsys: a credential blob is 1 to 2560 bytes",
        ));
    }
    let mut name = wide(OsStr::new(target));
    let mut user = wide(OsStr::new("kith"));
    let mut data = Zeroizing::new(blob.to_vec());
    let cred = CREDENTIALW {
        Flags: 0,
        Type: CRED_TYPE_GENERIC,
        TargetName: name.as_mut_ptr(),
        Comment: std::ptr::null_mut(),
        LastWritten: FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        },
        CredentialBlobSize: u32::try_from(data.len()).unwrap_or(0),
        CredentialBlob: data.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        AttributeCount: 0,
        Attributes: std::ptr::null_mut(),
        TargetAlias: std::ptr::null_mut(),
        UserName: user.as_mut_ptr(),
    };
    // SAFETY: every pointer in `cred` is to a buffer that outlives the call; the call copies them.
    if unsafe { CredWriteW(&raw const cred, 0) } == 0 {
        return Err(last_err());
    }
    Ok(())
}

/// Deletes the item named `target`. `Ok(false)` when there was none.
pub fn delete(target: &str) -> io::Result<bool> {
    let name = wide(OsStr::new(target));
    // SAFETY: `name` is NUL-ended.
    if unsafe { CredDeleteW(name.as_ptr(), CRED_TYPE_GENERIC, 0) } != 0 {
        return Ok(true);
    }
    let e = last_err();
    if e.raw_os_error().and_then(|c| u32::try_from(c).ok()) == Some(ERROR_NOT_FOUND) {
        Ok(false)
    } else {
        Err(e)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn write_read_overwrite_and_delete_round_trip() {
        let target = format!("kith-winsys-test-{}", std::process::id());
        assert!(read(&target).unwrap().is_none());
        write(&target, b"first").unwrap();
        assert_eq!(read(&target).unwrap().unwrap().as_slice(), b"first");
        write(&target, b"second").unwrap();
        assert_eq!(read(&target).unwrap().unwrap().as_slice(), b"second");
        assert!(delete(&target).unwrap());
        assert!(read(&target).unwrap().is_none());
        assert!(!delete(&target).unwrap());
    }

    #[test]
    fn a_blob_that_is_empty_or_too_big_is_refused() {
        assert!(write("kith-winsys-test-bad", b"").is_err());
        assert!(write("kith-winsys-test-bad", &vec![0u8; MAX_BLOB + 1]).is_err());
    }
}
