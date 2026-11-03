use crate::Error;
use std::fs::OpenOptions;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

/// This account's SID as text.
fn my_sid() -> io::Result<String> {
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut n = 0u32;
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut n);
        let mut buf = vec![0usize; (n as usize).div_ceil(size_of::<usize>()).max(1)];
        let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), n, &mut n);
        let text = if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            let mut sid = null_mut();
            if ConvertSidToStringSidW((*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid, &mut sid) == 0
            {
                Err(io::Error::last_os_error())
            } else {
                let len = (0..).take_while(|&i| *sid.add(i) != 0).count();
                let s = String::from_utf16_lossy(std::slice::from_raw_parts(sid, len));
                LocalFree(sid.cast());
                Ok(s)
            }
        };
        CloseHandle(token);
        text
    }
}

/// Creates the folder with an access list naming only this account and SYSTEM (protected: nothing inherited from
/// the parent), passed down to the lock files. An existing folder is used as found.
pub fn private_folder(path: &Path) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let sddl = wide(format!("D:P(A;OICI;GA;;;SY)(A;OICI;GA;;;{})", my_sid()?).as_ref());
    unsafe {
        let mut sd = null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        };
        let made = CreateDirectoryW(wide(path.as_os_str()).as_ptr(), &sa);
        let err = GetLastError();
        LocalFree(sd.cast());
        if made == 0 && err != ERROR_ALREADY_EXISTS {
            return Err(io::Error::from_raw_os_error(err as i32).into());
        }
    }
    Ok(())
}

/// The folder's access list is inherited and std opens files non-inheritable. Sharing excludes DELETE (std's default
/// allows it): a same-user process could otherwise delete a held lock file, and the next taker would lock a fresh one.
pub fn file_private(open: &mut OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_WRITE: u32 = 2;
    open.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
}
