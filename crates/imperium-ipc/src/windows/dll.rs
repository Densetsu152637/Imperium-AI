//! Narrowing where this process looks for DLLs (the parent module allows `unsafe`).

use std::io;

use windows_sys::Win32::System::LibraryLoader::{
    LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, SetDefaultDllDirectories,
};

/// Limits this process's DLL search to the application directory, `System32` and directories added
/// on purpose, so a DLL planted in the current directory or on `PATH` is never loaded by name. The
/// desktop app calls it first thing at start (plugin approvals, stage 8b).
pub fn harden_dll_search() -> io::Result<()> {
    // SAFETY: the call takes only a flags value, reads no memory of ours and has no preconditions;
    // it changes the process-wide DLL search order and reports failure through the return value.
    if unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn hardening_the_dll_search_succeeds() {
        super::harden_dll_search().unwrap();
    }
}
