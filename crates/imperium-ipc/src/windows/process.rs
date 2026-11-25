//! The process on the other end of an accepted pipe, held by a handle from accept onwards, so
//! after the accept a reused process id never stands in for it; a pid reused between the client
//! opening the pipe and the accept is not detected. Win32 calls through `windows-sys`; each `unsafe`
//! block says why it is sound (the parent module allows `unsafe`).

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{FILETIME, HANDLE, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
};

use super::{Stream, peer_pid};

/// 100-nanosecond intervals from 1601-01-01 (FILETIME's epoch) to 1970-01-01.
const UNIX_EPOCH_FILETIME: u64 = 116_444_736_000_000_000;

/// A process opened by id, with a handle that stays its own until dropped (and closed).
#[derive(Debug)]
pub struct ClientProcess {
    pid: u32,
    handle: OwnedHandle,
}

/// The client process of an accepted connection, opened at once.
pub fn client_process(stream: &Stream) -> Option<ClientProcess> {
    ClientProcess::open(peer_pid(stream)?)
}

impl ClientProcess {
    /// Opens process `pid` for querying and waiting.
    pub fn open(pid: u32) -> Option<ClientProcess> {
        // SAFETY: OpenProcess takes plain values and returns a new handle or null; nothing is
        // borrowed.
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        if raw.is_null() {
            return None;
        }
        // SAFETY: `raw` is a valid handle just opened and owned by nothing else; OwnedHandle
        // closes it exactly once on drop.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        Some(ClientProcess { pid, handle })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    fn raw(&self) -> HANDLE {
        self.handle.as_raw_handle() as HANDLE
    }

    /// When the process was created.
    pub fn creation_time(&self) -> Option<SystemTime> {
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        // SAFETY: the handle is open for this call (owned by `self`); the four out-pointers are
        // valid, distinct FILETIMEs.
        let found = unsafe {
            GetProcessTimes(
                self.raw(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        };
        if found == 0 {
            return None;
        }
        let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        let since_unix = ticks.checked_sub(UNIX_EPOCH_FILETIME)?;
        UNIX_EPOCH.checked_add(Duration::from_nanos(since_unix.checked_mul(100)?))
    }

    /// The full path of the process's executable.
    pub fn image_path(&self) -> Option<PathBuf> {
        let mut buffer = vec![0u16; 32_768];
        let mut length = u32::try_from(buffer.len()).ok()?;
        // SAFETY: the handle is open; `buffer` holds `length` UTF-16 units, and `length` is a
        // valid in/out pointer that the call sets to the units written.
        let found = unsafe {
            QueryFullProcessImageNameW(
                self.raw(),
                PROCESS_NAME_WIN32,
                buffer.as_mut_ptr(),
                &mut length,
            )
        };
        if found == 0 {
            return None;
        }
        buffer.truncate(usize::try_from(length).ok()?);
        Some(PathBuf::from(OsString::from_wide(&buffer)))
    }

    /// Whether the process has exited. The handle keeps referring to this process, never to one
    /// that reuses its id. Only a wait that times out means it is alive: a failed wait counts as
    /// exited, so the caller refuses.
    pub fn has_exited(&self) -> bool {
        // SAFETY: the handle is open and was opened with SYNCHRONIZE; a zero timeout only polls.
        unsafe { WaitForSingleObject(self.raw(), 0) != WAIT_TIMEOUT }
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use super::*;

    #[test]
    fn a_process_is_known_by_its_handle_until_it_exits() {
        let mut child = Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let before = SystemTime::now() - Duration::from_secs(5);
        let process = ClientProcess::open(child.id()).unwrap();
        assert_eq!(process.pid(), child.id());
        assert!(!process.has_exited());
        assert!(process.creation_time().unwrap() > before);
        let path = process.image_path().unwrap();
        assert!(
            path.to_string_lossy().to_lowercase().ends_with("ping.exe"),
            "{}",
            path.display()
        );
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(process.has_exited());
    }
}
