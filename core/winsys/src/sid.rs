//! Security identifiers: the process owner's, the two well-known system ones, comparison and text.

use std::ffi::c_void;
use std::io;

use windows_sys::Win32::Foundation::{HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{
    CreateWellKnownSid, EqualSid, GetLengthSid, GetTokenInformation, IsValidSid,
    SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER, TokenUser, WELL_KNOWN_SID_TYPE,
    WinBuiltinAdministratorsSid, WinCreatorOwnerSid, WinLocalSystemSid,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::{Handle, last_err};

/// An owned SID (its bytes), in memory aligned for the Win32 calls.
#[derive(Clone)]
pub(crate) struct Sid(Vec<u32>);

impl Sid {
    /// Copies the SID at `p`, which must point at a valid SID.
    pub(crate) fn copy_from(p: *mut c_void) -> io::Result<Self> {
        // SAFETY: `p` is checked by IsValidSid before its length is read.
        if p.is_null() || unsafe { IsValidSid(p) } == 0 {
            return Err(io::Error::other("kith-winsys: not a valid SID"));
        }
        // SAFETY: valid SID, per the check above.
        let len = unsafe { GetLengthSid(p) } as usize;
        let mut words = vec![0u32; len.div_ceil(4)];
        // SAFETY: `p` holds `len` readable bytes and `words` has room for them; the buffers do not overlap.
        unsafe {
            std::ptr::copy_nonoverlapping(p.cast::<u8>(), words.as_mut_ptr().cast::<u8>(), len)
        };
        Ok(Self(words))
    }

    pub(crate) fn as_ptr(&self) -> *mut c_void {
        self.0.as_ptr().cast_mut().cast()
    }

    /// The SID's length in bytes.
    pub(crate) fn len(&self) -> u32 {
        // SAFETY: `self` holds a valid SID.
        unsafe { GetLengthSid(self.as_ptr()) }
    }

    /// Whether the SID at `p` (valid, or not) is this one.
    pub(crate) fn is(&self, p: *mut c_void) -> bool {
        // SAFETY: IsValidSid guards EqualSid; both are plain reads of memory the caller holds.
        !p.is_null() && unsafe { IsValidSid(p) != 0 && EqualSid(self.as_ptr(), p) != 0 }
    }

    /// The `S-1-5-...` text, for a message. Never used for a decision.
    pub(crate) fn text(p: *mut c_void) -> String {
        let mut out: windows_sys::core::PWSTR = std::ptr::null_mut();
        // SAFETY: `p` is a SID the caller holds; on success `out` is a LocalAlloc'd string freed below.
        if p.is_null() || unsafe { ConvertSidToStringSidW(p, &raw mut out) } == 0 {
            return "an unreadable SID".to_owned();
        }
        let mut n = 0;
        // SAFETY: `out` is NUL-ended; the slice is read before the buffer is freed.
        unsafe {
            while *out.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(out, n));
            LocalFree(out.cast());
            s
        }
    }
}

/// The user of this process's token: the owner SID, whatever the account is called and however the console is
/// localised. Elevated or not, the token's user is the same account.
pub(crate) fn current_user() -> io::Result<Sid> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the pseudo handle of this process needs no close; `token` is an out-pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(last_err());
    }
    let token = Handle(token);
    let mut need = 0u32;
    // SAFETY: the first call only asks for the size; it fails with "insufficient buffer" and sets `need`.
    unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &raw mut need) };
    if need == 0 {
        return Err(last_err());
    }
    // u64 words: TOKEN_USER holds a pointer, so the buffer must be aligned for one.
    let mut buf = vec![0u64; (need as usize).div_ceil(8)];
    // SAFETY: `buf` is at least `need` bytes.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buf.as_mut_ptr().cast(),
            need,
            &raw mut need,
        )
    } == 0
    {
        return Err(last_err());
    }
    // SAFETY: success wrote a TOKEN_USER at the start of `buf`; its SID points into the same buffer.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    Sid::copy_from(sid)
}

fn well_known(kind: WELL_KNOWN_SID_TYPE) -> io::Result<Sid> {
    let mut words = vec![0u32; (SECURITY_MAX_SID_SIZE as usize).div_ceil(4)];
    let mut len = SECURITY_MAX_SID_SIZE;
    // SAFETY: `words` has SECURITY_MAX_SID_SIZE bytes, the most a SID needs; no domain SID is wanted for these.
    if unsafe {
        CreateWellKnownSid(
            kind,
            std::ptr::null_mut(),
            words.as_mut_ptr().cast(),
            &raw mut len,
        )
    } == 0
    {
        return Err(last_err());
    }
    Ok(Sid(words))
}

/// `NT AUTHORITY\SYSTEM` (S-1-5-18), whatever the language.
pub(crate) fn system() -> io::Result<Sid> {
    well_known(WinLocalSystemSid)
}

/// `BUILTIN\Administrators` (S-1-5-32-544).
pub(crate) fn administrators() -> io::Result<Sid> {
    well_known(WinBuiltinAdministratorsSid)
}

/// `CREATOR OWNER` (S-1-3-0): a placeholder only an inheritable entry carries, which Windows replaces with the creator
/// of each new child. It names nobody else.
pub(crate) fn creator_owner() -> io::Result<Sid> {
    well_known(WinCreatorOwnerSid)
}
