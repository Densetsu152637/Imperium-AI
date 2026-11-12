//! The Windows named-pipe transport.
//!
//! The pipe is created with an explicit security descriptor, because the default one lets every
//! local user read from a pipe. Creating it needs the Win32 security API, so this module (and
//! only this one) allows `unsafe`; each block says why it is sound.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::ptr::null_mut;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_PIPE_BUSY, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// How many times a client retries a pipe whose instances are all busy.
const BUSY_RETRIES: u32 = 40;
const BUSY_WAIT: Duration = Duration::from_millis(50);

/// One end of a connection: the daemon's, or a client's.
#[derive(Debug)]
pub enum Stream {
    Server(NamedPipeServer),
    Client(NamedPipeClient),
}

impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut *self {
            Stream::Server(pipe) => Pin::new(pipe).poll_read(cx, buf),
            Stream::Client(pipe) => Pin::new(pipe).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut *self {
            Stream::Server(pipe) => Pin::new(pipe).poll_write(cx, data),
            Stream::Client(pipe) => Pin::new(pipe).poll_write(cx, data),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            Stream::Server(pipe) => Pin::new(pipe).poll_flush(cx),
            Stream::Client(pipe) => Pin::new(pipe).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            Stream::Server(pipe) => Pin::new(pipe).poll_shutdown(cx),
            Stream::Client(pipe) => Pin::new(pipe).poll_shutdown(cx),
        }
    }
}

/// A named pipe the daemon serves. Its name is random at every start and recorded in the
/// endpoint file, which only the current user can read.
#[derive(Debug)]
pub struct Listener {
    name: String,
    /// The access list, as SDDL: the current user and nobody else.
    sddl: String,
    /// The instance waiting for the next client.
    next: Option<NamedPipeServer>,
}

impl Listener {
    /// Creates the pipe. `_data_dir` is unused: pipes live in their own namespace, not in files.
    pub fn bind(_data_dir: &Path) -> io::Result<Self> {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(io::Error::other)?;
        let name = format!(
            r"\\.\pipe\imperium-core-{}",
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        // Protected DACL, one entry: generic-all for the current user's SID.
        let sddl = format!("D:P(A;;GA;;;{})", current_user_sid()?);
        // `first_pipe_instance` fails if the name is already taken, so a squatting process
        // cannot stand in for the daemon.
        let first = create_instance(&name, &sddl, true)?;
        Ok(Listener {
            name,
            sddl,
            next: Some(first),
        })
    }

    /// The pipe's name, for the endpoint file.
    pub fn address(&self) -> String {
        self.name.clone()
    }

    pub async fn accept(&mut self) -> io::Result<Stream> {
        let server = match self.next.take() {
            Some(server) => server,
            None => create_instance(&self.name, &self.sddl, false)?,
        };
        server.connect().await?;
        // Have an instance waiting before handing this one over, so a client arriving now finds
        // the pipe. If that fails the next `accept` tries again.
        self.next = create_instance(&self.name, &self.sddl, false).ok();
        Ok(Stream::Server(server))
    }

    /// The access list of the instance that waits for a client, as SDDL.
    #[cfg(test)]
    fn waiting_acl(&self) -> io::Result<String> {
        use std::os::windows::io::AsRawHandle;
        let pipe = self.next.as_ref().expect("an instance is waiting");
        acl_of(pipe.as_raw_handle())
    }
}

/// Connects to a daemon's pipe, waiting briefly when every instance is busy.
pub async fn connect(address: &str) -> io::Result<Stream> {
    let mut attempt = 0;
    loop {
        match ClientOptions::new().open(address) {
            Ok(client) => return Ok(Stream::Client(client)),
            Err(error)
                if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
                    && attempt < BUSY_RETRIES =>
            {
                attempt += 1;
                tokio::time::sleep(BUSY_WAIT).await;
            }
            Err(error) => return Err(error),
        }
    }
}

/// A security descriptor made by Windows; freed when dropped.
struct Descriptor(*mut c_void);

impl Descriptor {
    fn from_sddl(sddl: &str) -> io::Result<Self> {
        let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut descriptor: *mut c_void = null_mut();
        // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call; `descriptor`
        // is a valid out-pointer; the size out-parameter is optional and passed as null.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        };
        if converted == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Descriptor(descriptor))
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: the pointer came from the conversion call above, which allocates with
        // LocalAlloc, and is freed exactly once here.
        unsafe { LocalFree(self.0) };
    }
}

fn create_instance(name: &str, sddl: &str, first: bool) -> io::Result<NamedPipeServer> {
    let descriptor = Descriptor::from_sddl(sddl)?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true);
    // SAFETY: `attributes` is a valid SECURITY_ATTRIBUTES whose descriptor stays alive until
    // after this call returns; Windows copies the descriptor into the pipe.
    unsafe { options.create_with_security_attributes_raw(name, (&raw mut attributes).cast()) }
}

/// The current user's SID as text, such as `S-1-5-21-...`.
fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = null_mut();
    // SAFETY: the pseudo-handle from GetCurrentProcess needs no closing; `token` is a valid
    // out-pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let sid = user_sid_of(token);
    // SAFETY: `token` was opened above and is closed once.
    unsafe { CloseHandle(token) };
    sid
}

fn user_sid_of(token: HANDLE) -> io::Result<String> {
    let mut needed = 0u32;
    // SAFETY: a null buffer of length 0 asks only for the size; `needed` is a valid out-pointer.
    unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64 elements keep the buffer aligned for the structure Windows writes into it.
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: `buffer` has at least `needed` bytes and is suitably aligned.
    let filled = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    if filled == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success the buffer holds a TOKEN_USER, and its SID points into the same buffer.
    let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut text: *mut u16 = null_mut();
    // SAFETY: `sid` is valid for the call; `text` is a valid out-pointer.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(take_wide(text))
}

/// Copies a NUL-terminated UTF-16 string that Windows allocated with LocalAlloc, then frees it.
fn take_wide(text: *mut u16) -> String {
    let mut length = 0;
    // SAFETY: the string is NUL-terminated, so reading up to the terminator stays in bounds.
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: `length` elements were just read; the string is freed once, after the copy.
    let string = unsafe { String::from_utf16_lossy(std::slice::from_raw_parts(text, length)) };
    unsafe { LocalFree(text.cast()) };
    string
}

/// The DACL of a kernel object (here a pipe handle), as SDDL.
#[cfg(test)]
fn acl_of(handle: std::os::windows::io::RawHandle) -> io::Result<String> {
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SE_KERNEL_OBJECT,
    };
    use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
    let mut descriptor: *mut c_void = null_mut();
    // SAFETY: `handle` is a live pipe handle; the owner, group, DACL and SACL out-pointers are
    // optional and null; `descriptor` is a valid out-pointer.
    let status = unsafe {
        GetSecurityInfo(
            handle as HANDLE,
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let descriptor = Descriptor(descriptor);
    let mut text: *mut u16 = null_mut();
    // SAFETY: `descriptor` is valid; `text` is a valid out-pointer; the length is optional.
    let converted = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor.0,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut text,
            null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(take_wide(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn the_pipe_grants_only_the_current_user_carries_bytes_and_serves_many_clients() {
        let mut listener = Listener::bind(Path::new("unused")).unwrap();
        let address = listener.address();
        assert!(address.starts_with(r"\\.\pipe\imperium-core-"));

        // One entry in a protected list, and it is for this user's SID.
        let acl = listener.waiting_acl().unwrap();
        let sid = current_user_sid().unwrap();
        assert!(acl.starts_with("D:P("), "{acl}");
        assert_eq!(acl.matches("(A;").count(), 1, "{acl}");
        // Windows may print a well-known account by alias (the built-in administrator comes
        // back as `LA`), so accept the SID or an alias that names a single account, and refuse
        // the broad groups.
        let trustee = acl.trim_end_matches(')').rsplit(';').next().unwrap();
        assert!(
            trustee == sid || matches!(trustee, "LA" | "LG"),
            "{acl} should grant only {sid}"
        );
        for broad in ["WD", "AU", "BU", "AN", "IU", "NU", "BA", "SY", "S-1-1-0"] {
            assert_ne!(trustee, broad, "{acl}");
        }

        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let mut stream = listener.accept().await.unwrap();
                let mut byte = [0u8; 1];
                stream.read_exact(&mut byte).await.unwrap();
                stream.write_all(&[byte[0] + 1]).await.unwrap();
                stream.flush().await.unwrap();
            }
            listener
        });
        for value in [41u8, 1] {
            let mut client = connect(&address).await.unwrap();
            client.write_all(&[value]).await.unwrap();
            let mut reply = [0u8; 1];
            client.read_exact(&mut reply).await.unwrap();
            assert_eq!(reply[0], value + 1);
        }
        let listener = server.await.unwrap();

        // Dropping the listener removes the pipe.
        drop(listener);
        assert!(connect(&address).await.is_err());
    }

    #[tokio::test]
    async fn a_taken_name_is_not_reused() {
        let listener = Listener::bind(Path::new("unused")).unwrap();
        assert!(create_instance(&listener.name, &listener.sddl, true).is_err());
    }
}
