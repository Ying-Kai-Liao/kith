//! Who serves a pipe, checked by a client before it writes anything.
//!
//! Pipe names are one namespace for the machine: another account can create `kith-holder-<hash>` (or the owner, attach and
//! run pipes, which come through [`super::connect`]) before the real server does, and would then read what the client
//! writes (the hello, an owner token's proof, a secret). The pipe's access list keeps others from opening a pipe, not from
//! serving one, so the client vouches for the server:
//! - the process serving the pipe runs as this account (its token's user), and
//! - the pipe itself is owned by this account or by `BUILTIN\Administrators` (an elevated process makes its objects that
//!   way). A process id can be reused, so the process alone is not proof; the pipe's owner is what its maker was.
//!
//! Anything that cannot be read is a refusal. The holder links no other crate of ours (scripts/check-holder-deps.sh), so
//! this is its own copy of `kith_winsys::pipe`'s check, with the same rule; a change to one is a change to the other.

use std::io;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
use windows_sys::Win32::Security::{
    CreateWellKnownSid, EqualSid, GetLengthSid, GetTokenInformation, IsValidSid,
    OWNER_SECURITY_INFORMATION, PSID, SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER, TokenUser,
    WELL_KNOWN_SID_TYPE, WinBuiltinAdministratorsSid,
};
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// An owned SID, in memory aligned for the Win32 calls.
struct Sid(Vec<u64>);

impl Sid {
    /// Copies the SID at `p`, which must point at a valid SID.
    fn copy_from(p: PSID) -> io::Result<Self> {
        // SAFETY: IsValidSid guards the length read.
        if p.is_null() || unsafe { IsValidSid(p) } == 0 {
            return Err(io::Error::other("not a valid SID"));
        }
        // SAFETY: valid SID, per the check above.
        let len = unsafe { GetLengthSid(p) } as usize;
        let mut words = vec![0u64; len.div_ceil(8)];
        // SAFETY: `p` holds `len` readable bytes and `words` has room for them; the buffers do not overlap.
        unsafe {
            std::ptr::copy_nonoverlapping(p.cast::<u8>(), words.as_mut_ptr().cast::<u8>(), len)
        };
        Ok(Self(words))
    }

    fn ptr(&self) -> PSID {
        self.0.as_ptr().cast_mut().cast()
    }

    fn is(&self, other: &Sid) -> bool {
        // SAFETY: both hold valid SIDs (checked when copied or made).
        unsafe { EqualSid(self.ptr(), other.ptr()) != 0 }
    }
}

fn well_known(kind: WELL_KNOWN_SID_TYPE) -> io::Result<Sid> {
    let mut words = vec![0u64; (SECURITY_MAX_SID_SIZE as usize).div_ceil(8)];
    let mut len = SECURITY_MAX_SID_SIZE;
    // SAFETY: `words` has SECURITY_MAX_SID_SIZE bytes, the most a SID needs; no domain SID is wanted for these.
    if unsafe {
        CreateWellKnownSid(
            kind,
            std::ptr::null_mut(),
            words.as_mut_ptr().cast(),
            &mut len,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(Sid(words))
}

/// The user of the token of `process` (a handle with at least `PROCESS_QUERY_LIMITED_INFORMATION`).
fn process_user(process: HANDLE) -> io::Result<Sid> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `process` is open; `token` is an out-pointer.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let user = token_user(token);
    // SAFETY: the token was opened above and is closed once.
    unsafe { CloseHandle(token) };
    user
}

fn token_user(token: HANDLE) -> io::Result<Sid> {
    let mut need = 0u32;
    // SAFETY: the first call only asks for the size; it fails with "insufficient buffer" and sets `need`.
    unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut need) };
    if need == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64 words: TOKEN_USER holds a pointer, so the buffer must be aligned for one.
    let mut buf = vec![0u64; (need as usize).div_ceil(8)];
    // SAFETY: `buf` is at least `need` bytes.
    if unsafe { GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), need, &mut need) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: success wrote a TOKEN_USER at the start of `buf`; its SID points into the same buffer.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    Sid::copy_from(sid)
}

/// The owner of the kernel object `handle` (opened with `READ_CONTROL`, which `GENERIC_READ` carries).
fn object_owner(handle: HANDLE) -> io::Result<Sid> {
    let mut owner: PSID = std::ptr::null_mut();
    let mut sd = std::ptr::null_mut();
    // SAFETY: `handle` is open; the owner points into the descriptor `sd`, freed below after the SID is copied.
    let code = unsafe {
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    if code != 0 {
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    let copied = Sid::copy_from(owner);
    // SAFETY: `sd` was allocated by GetSecurityInfo (LocalAlloc) and is freed once.
    unsafe { LocalFree(sd) };
    copied
}

/// Who a client trusts to serve a pipe. Held apart so a test can name others.
pub(super) struct Trust {
    ours: Sid,
    admins: Sid,
}

impl Trust {
    pub(super) fn this_account() -> io::Result<Self> {
        // SAFETY: the pseudo handle of this process needs no close.
        let ours = process_user(unsafe { GetCurrentProcess() })?;
        Ok(Self {
            ours,
            admins: well_known(WinBuiltinAdministratorsSid)?,
        })
    }
}

/// The rule over what was read: the server process runs as us, and the pipe was made by us or by `Administrators`.
fn judge(
    server: io::Result<Sid>,
    owner: io::Result<Sid>,
    trust: &Trust,
) -> Result<(), &'static str> {
    let server = server.map_err(|_| "its account could not be read")?;
    let owner = owner.map_err(|_| "the pipe's owner could not be read")?;
    if !server.is(&trust.ours) {
        return Err("another account made this name first");
    }
    if !(owner.is(&trust.ours) || owner.is(&trust.admins)) {
        return Err("the pipe belongs to another account");
    }
    Ok(())
}

/// Refuses (`PermissionDenied`, nothing sent) a pipe whose server `handle` (a client end, just opened) is not this account's.
pub(super) fn verify_server(handle: HANDLE, trust: &Trust) -> io::Result<()> {
    let refuse = |why: &str| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the pipe's server is not this account's ({why}); nothing was sent"),
        )
    };
    let mut pid = 0u32;
    // SAFETY: the handle is an open pipe; `pid` is an out-pointer.
    if unsafe { GetNamedPipeServerProcessId(handle, &mut pid) } == 0 || pid == 0 {
        return Err(refuse("its process could not be found"));
    }
    // SAFETY: a query-only handle on that process, closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    let server = if process.is_null() {
        Err(io::Error::last_os_error())
    } else {
        let user = process_user(process);
        // SAFETY: opened above, closed once.
        unsafe { CloseHandle(process) };
        user
    };
    judge(server, object_owner(handle), trust).map_err(refuse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn me() -> Sid {
        Trust::this_account().unwrap().ours
    }
    fn admins() -> Sid {
        well_known(WinBuiltinAdministratorsSid).unwrap()
    }
    /// SYSTEM stands in for a foreign account: a well formed SID that is not ours.
    fn other() -> Sid {
        well_known(windows_sys::Win32::Security::WinLocalSystemSid).unwrap()
    }
    fn unreadable() -> io::Result<Sid> {
        Err(io::Error::other("unreadable"))
    }
    fn trust(ours: Sid, admins: Sid) -> Trust {
        Trust { ours, admins }
    }
    fn pipe_name(tag: &str) -> PathBuf {
        PathBuf::from(format!(
            r"\\.\pipe\kith-holder-verify-{tag}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn the_judgement_refuses_another_account_and_what_it_cannot_read() {
        let t = trust(me(), admins());
        assert!(judge(Ok(me()), Ok(me()), &t).is_ok());
        // an elevated maker owns its objects as Administrators
        assert!(judge(Ok(me()), Ok(admins()), &t).is_ok());
        assert!(judge(Ok(other()), Ok(me()), &t).is_err());
        // a reused pid: the process now is ours, the pipe is not
        assert!(judge(Ok(me()), Ok(other()), &t).is_err());
        assert!(judge(unreadable(), Ok(me()), &t).is_err());
        assert!(judge(Ok(me()), unreadable(), &t).is_err());
    }

    #[test]
    fn our_own_server_is_connected_to() {
        let name = pipe_name("good");
        let l = Arc::new(super::super::listen(&name).unwrap());
        let server = {
            let l = l.clone();
            std::thread::spawn(move || drop(super::super::accept(&l).unwrap()))
        };
        assert!(super::super::connect(&name).is_ok());
        server.join().unwrap();
    }

    #[test]
    fn an_impostor_server_is_refused_before_a_byte_is_written() {
        // A real pipe with a real server (this process), and a client told to trust SYSTEM only: to it the server is an
        // impostor. The server records every byte it is sent.
        let name = pipe_name("impostor");
        let l = Arc::new(super::super::listen(&name).unwrap());
        let got = Arc::new(Mutex::new(Vec::new()));
        let server = {
            let (l, got) = (l.clone(), got.clone());
            std::thread::spawn(move || {
                let mut s = super::super::accept(&l).unwrap();
                let mut buf = [0u8; 64];
                // the client drops its end after the refusal: end of stream, with nothing before it
                while let Ok(n) = s.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    got.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            })
        };
        let e = super::super::connect_trusting(&name, &trust(other(), other()))
            .err()
            .unwrap();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert!(e.to_string().contains("nothing was sent"), "{e}");
        server.join().unwrap();
        assert!(got.lock().unwrap().is_empty());
    }
}
