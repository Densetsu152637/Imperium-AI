//! Starting the browser with its debugger on a pipe instead of a TCP port
//! (`--remote-debugging-pipe`). A port on `127.0.0.1` can be reached by every process on the
//! machine, other users' included, and whatever attaches can read the signed-in profile's
//! cookies; an inherited pipe reaches only the daemon.
//!
//! - **Linux and macOS**: Chromium reads commands from fd 3 and writes to fd 4. Two pipes are
//!   made close-on-exec, and only in the child, between `fork` and `exec`, are their browser ends
//!   placed at 3 and 4 (which clears close-on-exec for those two alone).
//! - **Windows**: there are no numbered descriptors to inherit, so the browser is told the
//!   handle values instead (`--remote-debugging-io-pipes=<read>,<write>`, which Chrome and Edge
//!   read when `--remote-debugging-pipe` is set). The process is created with
//!   `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` naming only those two handles and the log file, so it
//!   inherits nothing else the daemon holds. The handles are anonymous pipes (`CreatePipe`), which
//!   no other process can open by name; they are inheritable only for the moment of the
//!   `CreateProcessW` call. That moment is not closed: a process the daemon starts by another
//!   route in it (Rust's own `Command` inherits every inheritable handle) could inherit them too
//!   (docs/05-tools-and-isolation.md, "Limits"; a residual of browser stage 0).

use std::ffi::OsString;
use std::fs::File;
use std::path::Path;

use crate::cdp::Pipe;

pub(crate) use platform::Process;

/// Starts `program` with `args` (which name `--remote-debugging-pipe`), its output going to
/// `log`, and returns the process and the daemon's ends of its debugging pipe.
pub(crate) fn spawn(
    program: &Path,
    args: &[OsString],
    log: File,
) -> std::io::Result<(Process, Pipe)> {
    // What the browser reads (its commands) and what it writes (answers and events).
    let (browser_reads, daemon_writes) = std::io::pipe()?;
    let (daemon_reads, browser_writes) = std::io::pipe()?;
    let process = platform::spawn(program, args, log, browser_reads, browser_writes)?;
    // The browser's ends were closed here once it held its own copies, so the daemon's reader
    // sees the pipe end when the browser goes.
    let pipe = Pipe::over(daemon_reads, daemon_writes)?;
    Ok((process, pipe))
}

#[cfg(unix)]
#[allow(unsafe_code)]
mod platform {
    use std::ffi::OsString;
    use std::fs::File;
    use std::io::{PipeReader, PipeWriter};
    use std::os::fd::{AsRawFd, OwnedFd, RawFd};
    use std::path::Path;
    use std::process::Stdio;

    /// The browser's process; dropping it kills it.
    pub(crate) struct Process(tokio::process::Child);

    impl Process {
        /// How it exited, once it has.
        pub(crate) fn exited(&mut self) -> Option<String> {
            match self.0.try_wait() {
                Ok(Some(status)) => Some(status.to_string()),
                Ok(None) => None,
                Err(error) => Some(format!("unknown ({error})")),
            }
        }

        #[cfg(test)]
        pub(crate) fn id(&self) -> Option<u32> {
            self.0.id()
        }
    }

    pub(super) fn spawn(
        program: &Path,
        args: &[OsString],
        log: File,
        reads: PipeReader,
        writes: PipeWriter,
    ) -> std::io::Result<Process> {
        let reads = OwnedFd::from(reads);
        let writes = OwnedFd::from(writes);
        let (read_fd, write_fd) = (reads.as_raw_fd(), writes.as_raw_fd());
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .kill_on_drop(true);
        // SAFETY: the closure runs in the child between fork and exec and makes only
        // async-signal-safe calls (fcntl, dup2, close) on descriptors this function owns, which
        // stay open until `spawn` has returned.
        unsafe {
            command.pre_exec(move || place(read_fd, write_fd));
        }
        let child = command.spawn()?;
        drop((reads, writes));
        Ok(Process(child))
    }

    /// In the child: the browser's ends at fd 3 (it reads) and fd 4 (it writes). Each is first
    /// copied above 4, so placing one cannot overwrite the other wherever they started.
    fn place(read: RawFd, write: RawFd) -> std::io::Result<()> {
        let check = |result: libc::c_int| {
            if result < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(result)
            }
        };
        // SAFETY: plain descriptor calls on descriptors the parent passed in.
        unsafe {
            let high_read = check(libc::fcntl(read, libc::F_DUPFD, 5))?;
            let high_write = check(libc::fcntl(write, libc::F_DUPFD, 5))?;
            check(libc::dup2(high_read, 3))?;
            check(libc::dup2(high_write, 4))?;
            libc::close(high_read);
            libc::close(high_write);
        }
        Ok(())
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod platform {
    use std::ffi::{OsStr, OsString, c_void};
    use std::fs::File;
    use std::io::{PipeReader, PipeWriter};
    use std::mem::{size_of, size_of_val};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::Path;
    use std::ptr;

    use windows_sys::Win32::Foundation::{
        HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_NO_WINDOW, CreateProcessW, DeleteProcThreadAttributeList,
        EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, InitializeProcThreadAttributeList,
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
        STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
    };

    /// The browser's process; dropping it ends it.
    pub(crate) struct Process {
        handle: OwnedHandle,
        #[cfg_attr(not(test), allow(dead_code))]
        pid: u32,
    }

    impl Process {
        /// How it exited, once it has.
        pub(crate) fn exited(&mut self) -> Option<String> {
            let handle = self.handle.as_raw_handle();
            // SAFETY: a process handle this struct owns; a zero timeout only polls.
            if unsafe { WaitForSingleObject(handle, 0) } != WAIT_OBJECT_0 {
                return None;
            }
            let mut code = 0_u32;
            // SAFETY: as above, with a valid out-pointer.
            if unsafe { GetExitCodeProcess(handle, &raw mut code) } == 0 {
                return Some("exit code unknown".into());
            }
            Some(format!("exit code {code:#x}"))
        }

        #[cfg(test)]
        pub(crate) fn id(&self) -> Option<u32> {
            Some(self.pid)
        }
    }

    impl Drop for Process {
        fn drop(&mut self) {
            // SAFETY: a process handle this struct owns; ending one that already exited fails
            // harmlessly.
            unsafe { TerminateProcess(self.handle.as_raw_handle(), 1) };
        }
    }

    pub(super) fn spawn(
        program: &Path,
        args: &[OsString],
        log: File,
        reads: PipeReader,
        writes: PipeWriter,
    ) -> std::io::Result<Process> {
        let reads = OwnedHandle::from(reads);
        let writes = OwnedHandle::from(writes);
        let log = OwnedHandle::from(log);
        let inherited: [HANDLE; 3] = [
            log.as_raw_handle(),
            reads.as_raw_handle(),
            writes.as_raw_handle(),
        ];
        // An inherited handle has the same value in the child, so these are what it opens.
        let mut command = quote(program.as_os_str());
        command.push_str(&format!(
            " --remote-debugging-io-pipes={},{}",
            inherited[1] as usize, inherited[2] as usize
        ));
        for arg in args {
            command.push(' ');
            command.push_str(&quote(arg));
        }
        let info = create(program, &command, &inherited)?;
        // SAFETY: both handles were just returned to us and are owned from here on.
        let (process, thread) = unsafe {
            (
                OwnedHandle::from_raw_handle(info.hProcess),
                OwnedHandle::from_raw_handle(info.hThread),
            )
        };
        drop(thread);
        // The child holds its own copies now.
        drop((reads, writes, log));
        Ok(Process {
            handle: process,
            pid: info.dwProcessId,
        })
    }

    /// `CreateProcessW` inheriting exactly `handles` (the log for standard output and error,
    /// then the two pipe ends), which are inheritable only for the duration of the call.
    fn create(
        program: &Path,
        command: &str,
        handles: &[HANDLE; 3],
    ) -> std::io::Result<PROCESS_INFORMATION> {
        let mut size = 0_usize;
        // SAFETY: a size query with no list.
        unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &raw mut size) };
        if size == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut list = vec![0_u64; size.div_ceil(8)];
        let list_ptr = list.as_mut_ptr().cast::<c_void>();
        // SAFETY: `list` holds `size` bytes, aligned.
        if unsafe { InitializeProcThreadAttributeList(list_ptr, 1, 0, &raw mut size) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let result = (|| {
            // SAFETY: the list was initialized for one attribute; `handles` outlives its use.
            if unsafe {
                UpdateProcThreadAttribute(
                    list_ptr,
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    handles.as_ptr().cast(),
                    size_of_val(handles),
                    ptr::null_mut(),
                    ptr::null(),
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            let mut startup = STARTUPINFOEXW::default();
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdInput = ptr::null_mut();
            startup.StartupInfo.hStdOutput = handles[0];
            startup.StartupInfo.hStdError = handles[0];
            startup.lpAttributeList = list_ptr;
            let application = wide(program.as_os_str());
            let mut command = wide(OsStr::new(command));
            let mut info = PROCESS_INFORMATION::default();
            set_inherit(handles, true)?;
            // SAFETY: every pointer is valid and outlives the call; `command` is writable as the
            // call requires; the attribute list names only inheritable handles we own.
            let created = unsafe {
                CreateProcessW(
                    application.as_ptr(),
                    command.as_mut_ptr(),
                    ptr::null(),
                    ptr::null(),
                    1,
                    EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW,
                    ptr::null(),
                    ptr::null(),
                    (&raw const startup).cast(),
                    &raw mut info,
                )
            };
            let failed = (created == 0).then(std::io::Error::last_os_error);
            // Not inheritable again at once. This only shortens the window: a process the daemon
            // started by another route while they were inheritable may hold them too.
            let _ = set_inherit(handles, false);
            match failed {
                Some(error) => Err(error),
                None => Ok(info),
            }
        })();
        // SAFETY: the list was initialized above and is deleted once.
        unsafe { DeleteProcThreadAttributeList(list_ptr) };
        result
    }

    fn set_inherit(handles: &[HANDLE], inherit: bool) -> std::io::Result<()> {
        for handle in handles {
            let flags = if inherit { HANDLE_FLAG_INHERIT } else { 0 };
            // SAFETY: a handle the caller owns.
            if unsafe { SetHandleInformation(*handle, HANDLE_FLAG_INHERIT, flags) } == 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }

    fn wide(text: &OsStr) -> Vec<u16> {
        text.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// Quotes one command-line argument the way the C runtime (and Chromium) parse it back.
    pub(super) fn quote(arg: &OsStr) -> String {
        let arg = arg.to_string_lossy();
        if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '"']) {
            return arg.into_owned();
        }
        let mut quoted = String::from('"');
        let mut backslashes = 0;
        for c in arg.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                    quoted.push('"');
                    backslashes = 0;
                }
                c => {
                    quoted.push_str(&"\\".repeat(backslashes));
                    quoted.push(c);
                    backslashes = 0;
                }
            }
        }
        quoted.push_str(&"\\".repeat(backslashes * 2));
        quoted.push('"');
        quoted
    }

    #[cfg(test)]
    mod tests {
        use super::quote;
        use std::ffi::OsStr;

        #[test]
        fn arguments_are_quoted_for_the_c_runtime() {
            assert_eq!(quote(OsStr::new("plain")), "plain");
            assert_eq!(quote(OsStr::new("two words")), "\"two words\"");
            assert_eq!(quote(OsStr::new(r"C:\a b\")), r#""C:\a b\\""#);
            assert_eq!(quote(OsStr::new(r#"say "hi""#)), r#""say \"hi\"""#);
            assert_eq!(quote(OsStr::new("")), "\"\"");
        }
    }
}
