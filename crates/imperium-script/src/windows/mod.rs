//! Windows confinement of the worker, applied by the daemon (plan C1, "Windows").
//!
//! [`launch`] starts a program (the worker, or a test probe) suspended, with a restricted token at
//! Low integrity, in an AppContainer with no capabilities ([`appcontainer`]), in a job object,
//! with an empty environment, an empty Low-labelled working folder, nothing inherited and only its
//! three pipe ends duplicated into it; only then does it let it run. [`labels`] puts an
//! inheritable `NO_READ_UP` label at Medium on the daemon's folders, so a Low process cannot read
//! anything Imperium keeps; the launcher refuses to start a worker until that is done.
//!
//! These need the Win32 security, process and job calls, so this module and its children are
//! the only code in the crate on Windows that allows `unsafe`; each block says why it is sound.
#![allow(unsafe_code)]

pub mod appcontainer;
pub mod labels;
pub mod launch;

use std::{ffi::OsStr, io, os::windows::ffi::OsStrExt as _, ptr};

use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, LocalFree},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        GetTokenInformation, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

pub use launch::{Confined, ConfinedProcess, spawn};

/// `text` as a NUL-terminated UTF-16 string.
pub(crate) fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

/// A security descriptor made from SDDL by Windows; freed when dropped.
pub(crate) struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    pub(crate) fn from_sddl(sddl: &str) -> io::Result<Self> {
        let text = wide(sddl);
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `text` is NUL-terminated and outlives the call; `descriptor` is a valid
        // out-pointer; the size out-parameter is optional.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                ptr::null_mut(),
            )
        };
        if converted == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    pub(crate) fn as_ptr(&self) -> PSECURITY_DESCRIPTOR {
        self.0
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by the conversion with LocalAlloc, freed exactly once here.
        unsafe { LocalFree(self.0) };
    }
}

/// A handle this code owns, closed when dropped.
#[derive(Debug)]
pub(crate) struct Handle(pub(crate) HANDLE);

// SAFETY: a kernel handle is a process-wide value with no thread affinity.
unsafe impl Send for Handle {}
// SAFETY: as above; the calls made through a shared handle are thread-safe kernel calls.
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a handle this struct owns, closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// The current user's SID as text, such as `S-1-5-21-...`.
pub(crate) fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: the pseudo-handle of this process needs no closing; `token` is a valid out-pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Handle(token);
    let buffer = token_information(token.0, TokenUser)?;
    // SAFETY: the buffer holds a TOKEN_USER whose SID points into the same buffer.
    let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut text: *mut u16 = ptr::null_mut();
    // SAFETY: `sid` is valid while `buffer` lives; `text` is a valid out-pointer.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `text` is a NUL-terminated string from LocalAlloc, read then freed once.
    let sid = unsafe {
        let len = (0..).take_while(|&i| *text.add(i) != 0).count();
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        LocalFree(text.cast());
        sid
    };
    Ok(sid)
}

/// A token's information of class `class`, in a buffer aligned for the structure it holds.
pub(crate) fn token_information(token: HANDLE, class: i32) -> io::Result<Vec<u64>> {
    let mut needed = 0_u32;
    // SAFETY: a size query: no buffer, and a valid out-pointer for the size.
    unsafe { GetTokenInformation(token, class, ptr::null_mut(), 0, &raw mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0_u64; (needed as usize).div_ceil(8)];
    // SAFETY: `buffer` holds at least `needed` bytes, 8-byte aligned.
    let filled = unsafe {
        GetTokenInformation(
            token,
            class,
            buffer.as_mut_ptr().cast(),
            needed,
            &raw mut needed,
        )
    };
    if filled == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buffer)
}
