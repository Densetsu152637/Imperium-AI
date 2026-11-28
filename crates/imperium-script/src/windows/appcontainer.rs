//! The worker's AppContainer (plan C9, "Windows confinement"; stage 1c).
//!
//! A Low-integrity token alone still carries the user's SID, so a Low process can read every file
//! the user can read outside the labelled folders and can ask the Local Security Authority for the
//! user's Credential Manager entries (`CredReadW`, found in stage 1b). The confined launch
//! therefore also puts the worker in an AppContainer with no capabilities: the kernel checks a
//! second identity, the package SID, against every access list, and objects that do not name it
//! (or `ALL APPLICATION PACKAGES`) are closed to it, whatever they grant the user. That takes the
//! user's own files, the Credential Manager, the network and the daemon's endpoints out of reach.
//!
//! The worker needs only two things the user owns: its own executable (read and execute) and its
//! empty working folder (read). [`grant_read_execute`] adds the package SID to their access lists
//! when nothing grants it yet. A per-machine install under `Program Files` already grants `ALL
//! APPLICATION PACKAGES`, and is left alone.
//!
//! The profile is made once per user with `CreateAppContainerProfile` (it records the name under
//! the user's registry and makes a folder under `%LOCALAPPDATA%\Packages`); a later start finds it
//! there and derives the same SID.

use std::{io, path::Path, ptr, sync::OnceLock};

use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
        Authorization::{
            ConvertStringSidToSidW, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW,
            NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW,
            TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
        },
        CopySid, DACL_SECURITY_INFORMATION, EqualSid, FreeSid, GetAce, GetAclInformation,
        GetLengthSid, INHERIT_ONLY_ACE,
        Isolation::CreateAppContainerProfile,
        Isolation::DeriveAppContainerSidFromAppContainerName,
        NO_INHERITANCE, PSECURITY_DESCRIPTOR, PSID, SUB_CONTAINERS_AND_OBJECTS_INHERIT,
    },
    Storage::FileSystem::{FILE_EXECUTE, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_READ_DATA},
};

use super::wide;

/// The worker's AppContainer name (at most 64 characters, no spaces).
pub const NAME: &str = "Imperium.ScriptWorker";

/// `ALL APPLICATION PACKAGES`: an access list entry for it reaches every AppContainer.
const ALL_APPLICATION_PACKAGES: &str = "S-1-15-2-1";

/// `ACCESS_ALLOWED_ACE_TYPE`.
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

/// `GENERIC_READ | GENERIC_EXECUTE`, as an access list entry may hold them unmapped.
const GENERIC_READ_EXECUTE: u32 = 0x8000_0000 | 0x2000_0000;

/// `GENERIC_ALL`.
const GENERIC_ALL: u32 = 0x1000_0000;

/// The package SID, made once per daemon.
static PACKAGE_SID: OnceLock<Result<Sid, String>> = OnceLock::new();

/// A SID held in memory this code owns.
#[derive(Debug)]
pub struct Sid(Vec<u64>);

impl Sid {
    /// Copies the SID at `sid` (valid for the call).
    fn copy_from(sid: PSID) -> io::Result<Self> {
        // SAFETY: `sid` is a valid SID for the call.
        let len = unsafe { GetLengthSid(sid) };
        let mut buffer = vec![0_u64; (len as usize).div_ceil(8)];
        // SAFETY: `buffer` holds at least `len` bytes; `sid` is valid.
        if unsafe { CopySid(len, buffer.as_mut_ptr().cast(), sid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(buffer))
    }

    /// Parses SDDL SID text such as `S-1-15-2-1`.
    fn parse(text: &str) -> io::Result<Self> {
        let text = wide(text);
        let mut sid: PSID = ptr::null_mut();
        // SAFETY: `text` is NUL-terminated; `sid` is a valid out-pointer.
        if unsafe { ConvertStringSidToSidW(text.as_ptr(), &raw mut sid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let copied = Self::copy_from(sid);
        // SAFETY: allocated by the conversion with LocalAlloc, freed once.
        unsafe { LocalFree(sid) };
        copied
    }

    /// The SID, for calls that read it while `self` lives.
    #[must_use]
    pub fn as_psid(&self) -> PSID {
        self.0.as_ptr().cast_mut().cast()
    }

    fn equals(&self, other: PSID) -> bool {
        // SAFETY: both are valid SIDs for the call.
        unsafe { EqualSid(self.as_psid(), other) != 0 }
    }
}

/// The worker's package SID, making its profile the first time. An error names what failed.
pub fn package_sid() -> Result<&'static Sid, String> {
    PACKAGE_SID
        .get_or_init(|| make_package_sid().map_err(|err| format!("the profile {NAME}: {err}")))
        .as_ref()
        .map_err(Clone::clone)
}

fn make_package_sid() -> io::Result<Sid> {
    let name = wide(NAME);
    let display = wide("Imperium code mode worker");
    let mut sid: PSID = ptr::null_mut();
    // SAFETY: NUL-terminated strings, no capabilities, and a valid out-pointer.
    let created = unsafe {
        CreateAppContainerProfile(
            name.as_ptr(),
            display.as_ptr(),
            display.as_ptr(),
            ptr::null(),
            0,
            &raw mut sid,
        )
    };
    if created < 0 {
        // Usually `HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)`: an earlier start made it, so derive
        // its SID. Any other failure (no loaded user profile, say) still leaves the derived SID
        // usable: the launch is what proves the container works, and it fails closed.
        // SAFETY: a NUL-terminated name and a valid out-pointer.
        let derived =
            unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &raw mut sid) };
        if derived < 0 {
            return Err(io::Error::from_raw_os_error(derived));
        }
    }
    let copied = Sid::copy_from(sid);
    // SAFETY: both calls return a SID to be freed with FreeSid, freed once.
    unsafe { FreeSid(sid) };
    copied
}

/// Makes `path` readable and executable by the worker's AppContainer: unless an entry already
/// grants it to the package SID or to `ALL APPLICATION PACKAGES`, adds one for the package SID.
/// `inherit` makes the entry apply to what a folder holds too.
pub fn grant_read_execute(path: &Path, inherit: bool) -> Result<(), String> {
    let package = package_sid()?;
    let all = Sid::parse(ALL_APPLICATION_PACKAGES).map_err(|err| err.to_string())?;
    let name = wide(path);
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `name` is NUL-terminated; only the access list is asked for, into valid
    // out-pointers; the descriptor that holds it is freed below.
    let status = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut dacl,
            ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if status != 0 {
        return Err(format!(
            "cannot read the access list of {}: {}",
            path.display(),
            io::Error::from_raw_os_error(status as i32)
        ));
    }
    let result = (|| {
        // SAFETY: `dacl` points into `descriptor`, alive until it is freed below.
        if unsafe { grants(dacl, &[package, &all]) } {
            return Ok(());
        }
        let access = EXPLICIT_ACCESS_W {
            grfAccessPermissions: FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: if inherit {
                SUB_CONTAINERS_AND_OBJECTS_INHERIT
            } else {
                NO_INHERITANCE
            },
            Trustee: TRUSTEE_W {
                pMultipleTrustee: ptr::null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: package.as_psid().cast(),
            },
        };
        let mut merged: *mut ACL = ptr::null_mut();
        // SAFETY: one valid entry whose SID outlives the call; `dacl` is the current list (or
        // null); `merged` is a valid out-pointer, freed below.
        let status = unsafe { SetEntriesInAclW(1, &raw const access, dacl, &raw mut merged) };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        // SAFETY: `name` is NUL-terminated; `merged` is a valid access list; only it is set.
        let status = unsafe {
            SetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                merged,
                ptr::null(),
            )
        };
        // SAFETY: allocated by SetEntriesInAclW with LocalAlloc, freed once.
        unsafe { LocalFree(merged.cast()) };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(())
    })()
    .map_err(|err| {
        format!(
            "cannot let the worker's AppContainer read {}: {err}",
            path.display()
        )
    });
    // SAFETY: allocated by GetNamedSecurityInfoW with LocalAlloc, freed once.
    unsafe { LocalFree(descriptor) };
    result
}

/// Whether `dacl` has an allow entry, effective on the object itself, that grants one of `sids`
/// read and execute (or everything).
///
/// # Safety
///
/// `dacl` is null or a valid access list for the call.
unsafe fn grants(dacl: *const ACL, sids: &[&Sid]) -> bool {
    if dacl.is_null() {
        // No access list at all: everyone may do anything.
        return true;
    }
    let mut size = ACL_SIZE_INFORMATION::default();
    // SAFETY: `dacl` is valid; `size` is the structure the class names, with its size.
    if unsafe {
        GetAclInformation(
            dacl,
            (&raw mut size).cast(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    } == 0
    {
        return false;
    }
    (0..size.AceCount).any(|index| {
        let mut ace: *mut std::ffi::c_void = ptr::null_mut();
        // SAFETY: `index` is below the count; `ace` is a valid out-pointer into `dacl`.
        if unsafe { GetAce(dacl, index, &raw mut ace) } == 0 {
            return false;
        }
        // SAFETY: every entry starts with its header.
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE
            || u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0
        {
            return false;
        }
        // SAFETY: an allow entry: its mask follows the header, then its SID.
        let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        let mask = allowed.Mask;
        let reads = mask & (FILE_READ_DATA | FILE_EXECUTE) == FILE_READ_DATA | FILE_EXECUTE
            || mask & GENERIC_READ_EXECUTE == GENERIC_READ_EXECUTE
            || mask & GENERIC_ALL != 0;
        let sid: PSID = (&raw const allowed.SidStart).cast_mut().cast();
        reads && sids.iter().any(|candidate| candidate.equals(sid))
    })
}
