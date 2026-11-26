//! The `NO_READ_UP` labels on the daemon's folders (plan C1, "Windows", N2).
//!
//! The worker runs at Low integrity. A Low process may read any file whose mandatory label does
//! not forbid it, and Windows gives files no label by default (an implicit Medium `NO_WRITE_UP`),
//! so without this an escaped worker could read the endpoint file and its token, the secret
//! store, journals, memory and the MCP servers' plaintext settings. [`apply`] sets one inheritable
//! Medium `NO_READ_UP` label (object and container inherit) on each folder it is given (the data
//! folder, the config folder and the environments' working folders), so everything in them,
//! including files created or renamed into them later, is unreadable to a Low process.
//!
//! A folder that already carries the label is left alone, so a normal start costs one read per
//! folder. When the label is set, Windows propagates it to what the folder holds, and every file
//! and folder in it is then read back: one that does not carry it (one the user may not relabel)
//! fails the whole call, naming its path, and code mode is unavailable until it is fixed
//! ([`status`]).

use std::{
    io,
    path::{Path, PathBuf},
    ptr,
    sync::{Mutex, PoisonError},
};

use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        ACL,
        Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
            SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
        },
        GetSecurityDescriptorSacl, LABEL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    },
};

use super::{Descriptor, wide};

/// Medium integrity, `NO_READ_UP` and `NO_WRITE_UP`, inherited by files and folders.
pub const NO_READ_UP_LABEL: &str = "S:(ML;OICI;NWNR;;;ME)";

/// What [`apply`] did to one folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Labelled {
    /// The folder already carried the label; nothing was written.
    Already,
    /// The label was set and found on everything the folder holds.
    Set,
}

/// The outcome of every [`apply`] so far: `None` before the first.
static STATUS: Mutex<Option<Result<(), String>>> = Mutex::new(None);

/// Whether the daemon's folders carry their labels, so a worker may start. An error is the reason
/// code mode is unavailable; it names the path that could not be labelled.
pub fn status() -> Result<(), String> {
    match &*STATUS.lock().unwrap_or_else(PoisonError::into_inner) {
        None => Err("the daemon has not labelled its folders yet".to_owned()),
        Some(result) => result.clone(),
    }
}

/// Labels each folder (made first if it does not exist) and records the outcome for
/// [`status`]. A failure stays recorded until the daemon restarts, even if a later call
/// succeeds for other folders.
pub fn apply(folders: &[PathBuf]) -> Result<Vec<(PathBuf, Labelled)>, String> {
    let result = folders
        .iter()
        .map(|folder| label_folder(folder).map(|done| (folder.clone(), done)))
        .collect::<Result<Vec<_>, String>>();
    let mut status = STATUS.lock().unwrap_or_else(PoisonError::into_inner);
    // A failure stays recorded: the folder it names is still unlabelled.
    if !matches!(&*status, Some(Err(_))) {
        *status = Some(result.as_ref().map(|_| ()).map_err(Clone::clone));
    }
    drop(status);
    result
}

fn label_folder(folder: &Path) -> Result<Labelled, String> {
    std::fs::create_dir_all(folder).map_err(|err| format!("{}: {err}", folder.display()))?;
    let current = label_of(folder).map_err(|err| format!("{}: {err}", folder.display()))?;
    if carries(&current, true) {
        return Ok(Labelled::Already);
    }
    let set = set_label(folder, NO_READ_UP_LABEL);
    // Whatever the call said, find the first path that is not covered: that is the one to name.
    verify(folder)?;
    set.map_err(|err| format!("cannot label {}: {err}", folder.display()))?;
    Ok(Labelled::Set)
}

/// Checks every file and folder under `root` (links are not followed) carries the label.
fn verify(root: &Path) -> Result<(), String> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let label = label_of(&path)
            .map_err(|err| format!("cannot read the label of {}: {err}", path.display()))?;
        if !carries(&label, path == root) {
            return Err(format!(
                "{} does not take the daemon's NO_READ_UP label ({label}); code mode needs \
                 every file in the daemon's folders to be labelled, so it is unavailable until \
                 that file can be relabelled or is removed",
                path.display()
            ));
        }
        let meta =
            std::fs::symlink_metadata(&path).map_err(|err| format!("{}: {err}", path.display()))?;
        if meta.is_dir() && !meta.file_type().is_symlink() {
            let entries = std::fs::read_dir(&path)
                .map_err(|err| format!("cannot list {}: {err}", path.display()))?;
            for entry in entries {
                let entry =
                    entry.map_err(|err| format!("cannot list {}: {err}", path.display()))?;
                if !entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
                    pending.push(entry.path());
                }
            }
        }
    }
    Ok(())
}

/// Whether the SDDL label part `sddl` holds a mandatory label of at least Medium that forbids
/// reading up; `container` also requires it to be inheritable by files and folders.
pub fn carries(sddl: &str, container: bool) -> bool {
    let Some(sacl) = sddl.split("S:").nth(1) else {
        return false;
    };
    sacl.split('(').skip(1).any(|ace| {
        let fields: Vec<&str> = ace.trim_end_matches(')').split(';').collect();
        let [kind, flags, rights, _, _, sid, ..] = fields.as_slice() else {
            return false;
        };
        *kind == "ML"
            && rights.contains("NR")
            && matches!(
                *sid,
                "ME" | "HI" | "SI" | "S-1-16-8192" | "S-1-16-12288" | "S-1-16-16384"
            )
            && (!container || (flags.contains("OI") && flags.contains("CI")))
    })
}

/// The label part of `path`'s security descriptor, as SDDL (`S:(ML;...)`, or empty without one).
pub fn label_of(path: &Path) -> io::Result<String> {
    let name = wide(path);
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `name` is NUL-terminated; only the descriptor is asked for, into a valid
    // out-pointer; the other out-pointers are optional.
    let status = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            LABEL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let mut text: *mut u16 = ptr::null_mut();
    let mut len = 0_u32;
    // SAFETY: `descriptor` came from the call above; `text` and `len` are valid out-pointers.
    let converted = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            LABEL_SECURITY_INFORMATION,
            &raw mut text,
            &raw mut len,
        )
    };
    let error = io::Error::last_os_error();
    // SAFETY: allocated by GetNamedSecurityInfoW with LocalAlloc, freed once.
    unsafe { LocalFree(descriptor) };
    if converted == 0 {
        return Err(error);
    }
    // SAFETY: `text` holds `len` UTF-16 units (with its NUL) from LocalAlloc, read then freed.
    let sddl = unsafe {
        let units = std::slice::from_raw_parts(text, len as usize);
        let sddl = String::from_utf16_lossy(units)
            .trim_end_matches('\0')
            .to_owned();
        LocalFree(text.cast());
        sddl
    };
    Ok(sddl)
}

/// Sets the mandatory label of `path` to the label in `sddl` (`S:(ML;...)`). On a folder,
/// Windows propagates an inheritable label to what it holds.
pub fn set_label(path: &Path, sddl: &str) -> io::Result<()> {
    let descriptor = Descriptor::from_sddl(sddl)?;
    let mut present = 0;
    let mut defaulted = 0;
    let mut sacl: *mut ACL = ptr::null_mut();
    // SAFETY: the descriptor is valid; all out-pointers are valid.
    if unsafe {
        GetSecurityDescriptorSacl(
            descriptor.as_ptr(),
            &raw mut present,
            &raw mut sacl,
            &raw mut defaulted,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let name = wide(path);
    // SAFETY: `name` is NUL-terminated; `sacl` points into `descriptor`, alive for the call;
    // only the label is set.
    let status = unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            LABEL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
            sacl,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::carries;

    #[test]
    fn the_label_is_recognised_in_its_sddl_forms() {
        assert!(carries("S:(ML;OICI;NWNR;;;ME)", true));
        assert!(carries("S:(ML;OICIID;NWNR;;;ME)", true));
        assert!(carries("S:(ML;ID;NWNR;;;ME)", false));
        assert!(carries("S:AI(ML;OICIID;NRNW;;;S-1-16-8192)", true));
        // Not inheritable, not read-up, too low, or no label at all.
        assert!(!carries("S:(ML;;NWNR;;;ME)", true));
        assert!(!carries("S:(ML;OICI;NW;;;ME)", true));
        assert!(!carries("S:(ML;OICI;NWNR;;;LW)", true));
        assert!(!carries("", false));
    }
}
