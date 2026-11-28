//! Windows: the daemon confines the worker at launch (plan C1): it starts it suspended with a
//! restricted, Low-integrity token, puts it in a job object and only then lets it run
//! (`imperium_script`'s Windows launch). A process cannot lower or jail itself as reliably, so
//! the worker's own part is a check that this happened: it refuses to read a program unless its
//! token is at Low integrity or below, holds no privilege, and it runs inside a job. Started any
//! other way (by hand, or by a launch that skipped a step) it fails closed.
//!
//! Reading the token and the job status needs Win32 calls, so this module, and only this one in
//! the worker, allows `unsafe`.
#![allow(unsafe_code)]

use std::{ffi::c_void, mem, ptr};

use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL,
        TOKEN_PRIVILEGES, TOKEN_QUERY, TokenIntegrityLevel, TokenPrivileges,
    },
    System::{
        JobObjects::IsProcessInJob,
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

use super::{ConfineError, Report, Settings, forced};

/// `SECURITY_MANDATORY_LOW_RID`.
const LOW_RID: u32 = 0x1000;

pub(super) fn apply(settings: &Settings) -> Result<Report, ConfineError> {
    let mut report = Report::default();

    forced(settings, "integrity")?;
    let token = Token::open().map_err(|err| ConfineError::new("integrity", err))?;
    let rid = token
        .integrity_rid()
        .map_err(|err| ConfineError::new("integrity", err))?;
    if rid > LOW_RID {
        return Err(ConfineError::new(
            "integrity",
            format!(
                "the worker runs at integrity level {rid:#x}, above Low; only the daemon's \
                 confined launch may start it"
            ),
        ));
    }
    report.applied.push(format!("integrity={rid:#x}"));

    forced(settings, "privileges")?;
    let privileges = token
        .privilege_count()
        .map_err(|err| ConfineError::new("privileges", err))?;
    if privileges != 0 {
        return Err(ConfineError::new(
            "privileges",
            format!("the worker's token holds {privileges} privileges; it must hold none"),
        ));
    }
    report.applied.push("privileges=0".to_owned());

    forced(settings, "job")?;
    let mut in_job = 0;
    // SAFETY: the pseudo-handle of the current process, no job handle (any job), and a valid
    // out-pointer.
    if unsafe { IsProcessInJob(GetCurrentProcess(), ptr::null_mut(), &raw mut in_job) } == 0 {
        return Err(ConfineError::new("job", std::io::Error::last_os_error()));
    }
    if in_job == 0 {
        return Err(ConfineError::new(
            "job",
            "the worker is not in a job object; only the daemon's confined launch may start it",
        ));
    }
    report.applied.push("job=yes".to_owned());
    Ok(report)
}

/// The process's own token, closed on drop.
struct Token(HANDLE);

impl Token {
    fn open() -> std::io::Result<Self> {
        let mut handle: HANDLE = ptr::null_mut();
        // SAFETY: the pseudo-handle of the current process and a valid out-pointer.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut handle) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self(handle))
    }

    /// The token's information of class `class`, as bytes aligned for the structure it holds.
    fn information(&self, class: i32) -> std::io::Result<Vec<u64>> {
        let mut needed = 0_u32;
        // SAFETY: a size query: no buffer, a valid out-pointer for the size.
        unsafe { GetTokenInformation(self.0, class, ptr::null_mut(), 0, &raw mut needed) };
        if needed == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut buffer = vec![0_u64; (needed as usize).div_ceil(mem::size_of::<u64>())];
        // SAFETY: `buffer` holds at least `needed` bytes, 8-byte aligned.
        let ok = unsafe {
            GetTokenInformation(
                self.0,
                class,
                buffer.as_mut_ptr().cast::<c_void>(),
                needed,
                &raw mut needed,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(buffer)
    }

    /// The last sub-authority of the token's integrity SID: its mandatory level.
    fn integrity_rid(&self) -> std::io::Result<u32> {
        let buffer = self.information(TokenIntegrityLevel)?;
        // SAFETY: the buffer holds a TOKEN_MANDATORY_LABEL whose SID points into the same
        // buffer, which outlives these reads.
        unsafe {
            let label = &*buffer.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
            let sid = label.Label.Sid;
            let count = *GetSidSubAuthorityCount(sid);
            if count == 0 {
                return Err(std::io::Error::other("the integrity SID has no level"));
            }
            Ok(*GetSidSubAuthority(sid, u32::from(count) - 1))
        }
    }

    fn privilege_count(&self) -> std::io::Result<u32> {
        let buffer = self.information(TokenPrivileges)?;
        // SAFETY: the buffer holds a TOKEN_PRIVILEGES, whose count comes first.
        Ok(unsafe { (*buffer.as_ptr().cast::<TOKEN_PRIVILEGES>()).PrivilegeCount })
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: a handle this struct opened and owns.
        unsafe { CloseHandle(self.0) };
    }
}
