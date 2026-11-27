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

/// The pid of the process serving this named pipe (the holder, if the pipe is the holder's).
pub fn pipe_server_pid(pipe: &std::fs::File) -> Option<u32> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
    let mut pid = 0u32;
    // SAFETY: the handle is a valid open file for the call; pid is written.
    let ok = unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle() as HANDLE, &mut pid) };
    (ok != 0).then_some(pid)
}

/// True when the pipe was made by this account or by `BUILTIN\Administrators` (an elevated process owns its objects that
/// way). The parent-pid check alone has the reused-pid gap: the pipe's owner is what its maker was. Unreadable is false.
pub fn pipe_owned_by_us(pipe: &std::fs::File) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
    use windows_sys::Win32::Security::OWNER_SECURITY_INFORMATION;
    let Ok(ours) = my_sid() else { return false };
    let mut owner = null_mut();
    let mut sd = null_mut();
    // SAFETY: the handle is an open pipe (read access carries READ_CONTROL); the owner points into `sd`, freed below
    // once the text is copied.
    unsafe {
        let code = GetSecurityInfo(
            pipe.as_raw_handle() as HANDLE,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sd,
        );
        if code != 0 || owner.is_null() {
            return false;
        }
        let mut text = null_mut();
        let ok = ConvertSidToStringSidW(owner, &mut text) != 0;
        let s = if ok {
            let len = (0..).take_while(|&i| *text.add(i) != 0).count();
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
            LocalFree(text.cast());
            Some(s)
        } else {
            None
        };
        LocalFree(sd);
        s.is_some_and(|s| s == ours || s == "S-1-5-32-544")
    }
}

/// The pid of the process that started this one.
pub fn parent_pid() -> Option<u32> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    let me = std::process::id();
    // SAFETY: a process snapshot walked with the documented first/next calls, closed after.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap.is_null() || snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut found = None;
        let mut ok = Process32FirstW(snap, &mut e);
        while ok != 0 {
            if e.th32ProcessID == me {
                found = Some(e.th32ParentProcessID);
                break;
            }
            ok = Process32NextW(snap, &mut e);
        }
        CloseHandle(snap);
        found
    }
}

/// Makes this process's standard handles copies of the parent's handles `from` (stdin, stdout, stderr, by value). The parent
/// keeps its originals non-inheritable: a handle that is inheritable in a multi-threaded host leaks into any child it starts
/// at the same moment, and then a dead stub no longer reads as end of file on the other end of its pipes.
pub fn take_parent_stdio(from: [usize; 3]) -> io::Result<()> {
    use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle};
    use windows_sys::Win32::System::Console::{
        STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_DUP_HANDLE};
    let parent = parent_pid().ok_or_else(|| io::Error::other("the parent is unknown"))?;
    // SAFETY: OpenProcess takes plain values; the handle is closed below.
    let process = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, parent) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut result = Ok(());
    for (value, which) in
        from.into_iter()
            .zip([STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE])
    {
        let mut mine: HANDLE = null_mut();
        // SAFETY: `process` is open with duplicate access; the copy goes to a handle of this process, not inheritable.
        let ok = unsafe {
            DuplicateHandle(
                process,
                value as HANDLE,
                GetCurrentProcess(),
                &mut mine,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        // SAFETY: `mine` is a handle this process now owns.
        if ok == 0 || unsafe { SetStdHandle(which, mine) } == 0 {
            result = Err(io::Error::last_os_error());
            break;
        }
    }
    // SAFETY: `process` is open and ours.
    unsafe { CloseHandle(process) };
    result
}
