//! The confined launch (plan C1, "Windows"; assumption A3).
//!
//! In order, failing closed at every step (each error names its stage):
//!
//! 1. `token`: a restricted copy of the daemon's token (`CreateRestrictedToken` with
//!    `DISABLE_MAX_PRIVILEGE`, the Administrators group deny-only), every remaining privilege
//!    removed but `SeChangeNotifyPrivilege` (bypass traverse checking, which every Low and
//!    AppContainer process holds; without it the container would need an entry on every folder
//!    above its executable), and its integrity lowered to Low.
//! 2. `appcontainer`: the worker's AppContainer ([`super::appcontainer`]): its package SID, and
//!    read and execute on the program for that SID when nothing grants it yet.
//! 3. `job`: a job object with a memory limit of twice `script_memory_mb`, one active process,
//!    kill-on-close, death on an unhandled exception, every UI restriction, and no breakaway.
//! 4. `folder`: an empty working folder outside the daemon's labelled folders, labelled Low and
//!    readable by the package SID.
//! 5. `pipes`: three named pipes (stdin, stdout, stderr) whose names are random and whose only
//!    access is the daemon's user at Medium integrity, each opened once by the daemon for the
//!    child (not inheritable) and accepting no other client.
//! 6. `process`: `CreateProcessAsUserW` with the token, in the AppContainer with no capabilities
//!    (`PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`), suspended, with an environment of only the
//!    folder variables an AppContainer launch needs ([`CONTAINER_VARIABLES`]), the
//!    working folder, and no handle inheritance at all. It is detached from any console: a
//!    console would need a `conhost` process, which the job's one-process limit refuses (the
//!    worker then dies with `STATUS_DLL_INIT_FAILED`).
//! 7. `handles`: the three pipe ends are duplicated into the suspended child (`DuplicateHandle`)
//!    and written as its standard handles into its process parameters, where `GetStdHandle`
//!    reads them. No handle the daemon holds is ever inheritable, so a process the daemon starts
//!    meanwhile (a `run_command`) cannot inherit a pipe end (N6, review M1).
//! 8. `assign`: the process joins the job while it is still suspended.
//! 9. `resume`: only then does its thread start.
//!
//! The worker itself refuses to run unless its token is Low, holds no privilege but
//! `SeChangeNotifyPrivilege`, is an AppContainer token and it is in a job (the worker crate's
//! `confine/windows.rs`), so a launch that skipped a step still runs no program.

use std::{
    ffi::{OsStr, OsString, c_void},
    io,
    mem::{offset_of, size_of, size_of_val},
    path::{Path, PathBuf},
    ptr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::{
    Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation},
    Win32::{
        Foundation::{
            DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_SUCCESS, GENERIC_READ, GENERIC_WRITE,
            GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
        },
        Security::{
            AdjustTokenPrivileges, Authorization::ConvertStringSidToSidW, CreateRestrictedToken,
            CreateWellKnownSid, DISABLE_MAX_PRIVILEGE, GetLengthSid, LUID_AND_ATTRIBUTES, PSID,
            SE_PRIVILEGE_REMOVED, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES,
            SetTokenInformation, TOKEN_ALL_ACCESS, TOKEN_MANDATORY_LABEL, TOKEN_PRIVILEGES,
            TokenIntegrityLevel, TokenPrivileges, WinBuiltinAdministratorsSid,
        },
        Storage::FileSystem::{CreateFileW, OPEN_EXISTING},
        System::{
            Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory},
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
                JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOB_OBJECT_UILIMIT_DESKTOP,
                JOB_OBJECT_UILIMIT_DISPLAYSETTINGS, JOB_OBJECT_UILIMIT_EXITWINDOWS,
                JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES,
                JOB_OBJECT_UILIMIT_READCLIPBOARD, JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS,
                JOB_OBJECT_UILIMIT_WRITECLIPBOARD, JOBOBJECT_BASIC_UI_RESTRICTIONS,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicUIRestrictions,
                JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
            },
            Threading::{
                CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW,
                DETACHED_PROCESS, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
                GetCurrentProcess, GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
                OpenProcessToken, PEB, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
                PROCESS_BASIC_INFORMATION, PROCESS_INFORMATION, RTL_USER_PROCESS_PARAMETERS,
                ResumeThread, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
                WaitForSingleObject,
            },
        },
    },
};

use super::{
    Descriptor, Handle, appcontainer, appcontainer::Sid, current_user_sid, labels,
    token_information, wide,
};

/// Low integrity: `SECURITY_MANDATORY_LOW_RID`.
const LOW_INTEGRITY_SID: &str = "S-1-16-4096";

/// The worker's working folder: labelled Low (and what it holds inherits that), so the Low worker
/// may use it.
const LOW_FOLDER_LABEL: &str = "S:(ML;OICI;NW;;;LW)";

/// `SE_GROUP_INTEGRITY`: the token group that carries the integrity level.
const SE_GROUP_INTEGRITY: u32 = 0x20;

/// `SECURITY_MAX_SID_SIZE`.
const MAX_SID_BYTES: usize = 68;

/// The locally unique id of `SeChangeNotifyPrivilege` (`SE_CHANGE_NOTIFY_PRIVILEGE`), the one
/// privilege the worker keeps.
const CHANGE_NOTIFY_LUID: u32 = 23;

/// A program launched confined, with the daemon's ends of its pipes.
#[derive(Debug)]
pub struct Confined {
    pub process: ConfinedProcess,
    pub stdin: NamedPipeServer,
    pub stdout: NamedPipeServer,
    pub stderr: NamedPipeServer,
    /// The working folder, removed when dropped.
    pub folder: LowFolder,
}

/// The confined process and its job. Dropping it closes the job, which kills the process.
#[derive(Debug)]
pub struct ConfinedProcess {
    process: Arc<Handle>,
    job: Handle,
    pid: u32,
}

impl ConfinedProcess {
    /// The process id.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// Ends every process in the job; does not wait.
    pub fn kill(&self) {
        // SAFETY: the job handle this struct owns.
        unsafe { TerminateJobObject(self.job.0, 1) };
    }

    /// Waits for the process to exit and returns its exit code.
    pub async fn wait(&self) -> Option<u32> {
        let process = Arc::clone(&self.process);
        tokio::task::spawn_blocking(move || {
            // SAFETY: the process handle stays open while `process` is held here.
            unsafe { WaitForSingleObject(process.0, INFINITE) };
            let mut code = 0_u32;
            // SAFETY: as above, with a valid out-pointer.
            (unsafe { GetExitCodeProcess(process.0, &raw mut code) } != 0).then_some(code)
        })
        .await
        .ok()
        .flatten()
    }
}

impl Drop for ConfinedProcess {
    fn drop(&mut self) {
        // Closing the job kills it too (kill-on-close); ending it first does not depend on this
        // being the last handle.
        self.kill();
    }
}

/// The worker's empty working folder, labelled Low, removed when dropped.
#[derive(Debug)]
pub struct LowFolder(PathBuf);

impl LowFolder {
    fn create() -> Result<Self, String> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "imperium-script-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).map_err(|err| format!("{}: {err}", path.display()))?;
        let folder = Self(path);
        labels::set_label(&folder.0, LOW_FOLDER_LABEL)
            .map_err(|err| format!("cannot label {} Low: {err}", folder.0.display()))?;
        appcontainer::grant_read_execute(&folder.0, true)?;
        Ok(folder)
    }

    /// The folder's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for LowFolder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

/// Launches `program` with `args`, confined. `memory_mb` sizes the job's memory limit (twice it).
/// `forced` names a stage to fail instead of applying it (a test hook). Errors name the stage.
pub fn spawn(
    program: &Path,
    args: &[OsString],
    memory_mb: u64,
    forced: Option<&str>,
) -> Result<Confined, String> {
    let fail_if = |stage: &str| -> Result<(), String> {
        if forced == Some(stage) {
            Err(format!("{stage}: forced to fail by the test hook"))
        } else {
            Ok(())
        }
    };

    fail_if("token")?;
    let token = low_token().map_err(|err| format!("token: {err}"))?;
    fail_if("appcontainer")?;
    let package = appcontainer::package_sid()
        .and_then(|sid| appcontainer::grant_read_execute(program, false).map(|()| sid))
        .map_err(|err| format!("appcontainer: {err}"))?;
    fail_if("job")?;
    let job = job(memory_mb).map_err(|err| format!("job: {err}"))?;
    fail_if("folder")?;
    let folder = LowFolder::create().map_err(|err| format!("folder: {err}"))?;
    fail_if("pipes")?;
    let pipes = Pipes::create().map_err(|err| format!("pipes: {err}"))?;

    fail_if("process")?;
    let mut command = quote(program.as_os_str());
    for arg in args {
        command.push(' ');
        command.push_str(&quote(arg));
    }
    let child = create_suspended(&token, program, &command, folder.path(), package)
        .map_err(|err| format!("process: {err}"))?;
    let process = Handle(child.hProcess);
    let thread = Handle(child.hThread);
    // From here on dropping `confined` ends the process, whatever fails.
    let confined = ConfinedProcess {
        process: Arc::new(process),
        job,
        pid: child.dwProcessId,
    };
    let Pipes {
        servers,
        child: child_ends,
    } = pipes;

    let assign = (|| {
        fail_if("handles")?;
        give_standard_handles(confined.process.0, &child_ends)
            .map_err(|err| format!("handles: {err}"))?;
        // The child holds its own copies of the pipe ends now.
        drop(child_ends);
        fail_if("assign")?;
        // SAFETY: both handles are open and owned here; the process is still suspended.
        if unsafe { AssignProcessToJobObject(confined.job.0, confined.process.0) } == 0 {
            return Err(format!("assign: {}", io::Error::last_os_error()));
        }
        fail_if("resume")?;
        // SAFETY: the primary thread of the suspended process, owned here.
        if unsafe { ResumeThread(thread.0) } == u32::MAX {
            return Err(format!("resume: {}", io::Error::last_os_error()));
        }
        Ok(())
    })();
    if let Err(err) = assign {
        // It may not be in the job yet, so end it directly.
        // SAFETY: the process handle owned by `confined`.
        unsafe { TerminateProcess(confined.process.0, 1) };
        return Err(err);
    }
    drop(thread);
    let [stdin, stdout, stderr] = servers;
    Ok(Confined {
        process: confined,
        stdin,
        stdout,
        stderr,
        folder,
    })
}

/// The restricted, privilege-free, Low-integrity token (step 1).
fn low_token() -> io::Result<Handle> {
    let mut own: HANDLE = ptr::null_mut();
    // SAFETY: the pseudo-handle of this process; `own` is a valid out-pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &raw mut own) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let own = Handle(own);

    let mut admins = [0_u8; MAX_SID_BYTES];
    let mut admins_len = MAX_SID_BYTES as u32;
    // SAFETY: `admins` is a buffer of `admins_len` bytes, the largest a SID can be.
    if unsafe {
        CreateWellKnownSid(
            WinBuiltinAdministratorsSid,
            ptr::null_mut(),
            admins.as_mut_ptr().cast(),
            &raw mut admins_len,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let disable = [SID_AND_ATTRIBUTES {
        Sid: admins.as_mut_ptr().cast(),
        Attributes: 0,
    }];
    let mut restricted: HANDLE = ptr::null_mut();
    // SAFETY: `own` is open; `disable` holds one valid SID that outlives the call; no
    // privileges or restricting SIDs are passed; `restricted` is a valid out-pointer.
    if unsafe {
        CreateRestrictedToken(
            own.0,
            DISABLE_MAX_PRIVILEGE,
            1,
            disable.as_ptr(),
            0,
            ptr::null(),
            0,
            ptr::null(),
            &raw mut restricted,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let restricted = Handle(restricted);
    remove_all_privileges(&restricted)?;
    set_low_integrity(&restricted)?;
    Ok(restricted)
}

/// Removes every privilege `DISABLE_MAX_PRIVILEGE` left but `SeChangeNotifyPrivilege` (step 1).
fn remove_all_privileges(token: &Handle) -> io::Result<()> {
    let buffer = token_information(token.0, TokenPrivileges)?;
    // SAFETY: the buffer holds a TOKEN_PRIVILEGES: a count, then that many entries.
    let entries: Vec<LUID_AND_ATTRIBUTES> = unsafe {
        let privileges = buffer.as_ptr().cast::<TOKEN_PRIVILEGES>();
        let count = (*privileges).PrivilegeCount as usize;
        std::slice::from_raw_parts((&raw const (*privileges).Privileges).cast(), count).to_vec()
    };
    let entries: Vec<LUID_AND_ATTRIBUTES> = entries
        .into_iter()
        .filter(|entry| !(entry.Luid.LowPart == CHANGE_NOTIFY_LUID && entry.Luid.HighPart == 0))
        .collect();
    if entries.is_empty() {
        return Ok(());
    }
    // A TOKEN_PRIVILEGES with `entries.len()` entries, built in an aligned buffer.
    let bytes = size_of::<TOKEN_PRIVILEGES>() + entries.len() * size_of::<LUID_AND_ATTRIBUTES>();
    let mut new_state = vec![0_u64; bytes.div_ceil(8)];
    // SAFETY: `new_state` is large enough for the count and every entry, and aligned.
    unsafe {
        let privileges = new_state.as_mut_ptr().cast::<TOKEN_PRIVILEGES>();
        (*privileges).PrivilegeCount = entries.len() as u32;
        let first = (&raw mut (*privileges).Privileges).cast::<LUID_AND_ATTRIBUTES>();
        for (index, entry) in entries.iter().enumerate() {
            first.add(index).write(LUID_AND_ATTRIBUTES {
                Luid: entry.Luid,
                Attributes: SE_PRIVILEGE_REMOVED,
            });
        }
    }
    // SAFETY: `token` is open with TOKEN_ALL_ACCESS; `new_state` is a valid TOKEN_PRIVILEGES;
    // no previous state is asked for.
    let adjusted = unsafe {
        AdjustTokenPrivileges(
            token.0,
            0,
            new_state.as_ptr().cast(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    // AdjustTokenPrivileges succeeds even when it changed only some; the last error says so.
    // SAFETY: reads this thread's last error.
    let last = unsafe { GetLastError() };
    if adjusted == 0 || last != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(last as i32));
    }
    Ok(())
}

fn set_low_integrity(token: &Handle) -> io::Result<()> {
    let text = wide(LOW_INTEGRITY_SID);
    let mut sid: PSID = ptr::null_mut();
    // SAFETY: `text` is NUL-terminated; `sid` is a valid out-pointer.
    if unsafe { ConvertStringSidToSidW(text.as_ptr(), &raw mut sid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let label = TOKEN_MANDATORY_LABEL {
        Label: SID_AND_ATTRIBUTES {
            Sid: sid,
            Attributes: SE_GROUP_INTEGRITY,
        },
    };
    // SAFETY: `sid` is a valid SID from the conversion.
    let size = size_of::<TOKEN_MANDATORY_LABEL>() as u32 + unsafe { GetLengthSid(sid) };
    // SAFETY: `token` is open with TOKEN_ALL_ACCESS; `label` and the SID it points to live
    // through the call.
    let set = unsafe {
        SetTokenInformation(
            token.0,
            TokenIntegrityLevel,
            (&raw const label).cast(),
            size,
        )
    };
    let error = io::Error::last_os_error();
    // SAFETY: the SID came from LocalAlloc in the conversion and is freed once.
    unsafe { LocalFree(sid) };
    if set == 0 {
        return Err(error);
    }
    Ok(())
}

/// The job object of step 2.
fn job(memory_mb: u64) -> io::Result<Handle> {
    // SAFETY: no attributes and no name: an anonymous job.
    let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
    if job.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = Handle(job);
    // SAFETY: an all-zero JOBOBJECT_EXTENDED_LIMIT_INFORMATION is a valid "no limits" value.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_PROCESS_MEMORY
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
    limits.BasicLimitInformation.ActiveProcessLimit = 1;
    limits.ProcessMemoryLimit =
        usize::try_from(memory_mb.saturating_mul(2 * 1024 * 1024)).unwrap_or(usize::MAX);
    // SAFETY: `job` is open; `limits` is the structure the class names, with its size.
    if unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
        UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
            | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
            | JOB_OBJECT_UILIMIT_EXITWINDOWS
            | JOB_OBJECT_UILIMIT_GLOBALATOMS
            | JOB_OBJECT_UILIMIT_HANDLES
            | JOB_OBJECT_UILIMIT_READCLIPBOARD
            | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
            | JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
    };
    // SAFETY: as above, for the UI restrictions.
    if unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectBasicUIRestrictions,
            (&raw const ui).cast(),
            size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

/// The three pipes of step 4: the daemon's ends (stdin, stdout, stderr) and the child's.
struct Pipes {
    servers: [NamedPipeServer; 3],
    child: [Handle; 3],
}

impl Pipes {
    fn create() -> io::Result<Self> {
        // Only this user, at Medium integrity: a Low process can neither open nor read them.
        let sddl = format!("D:P(A;;GA;;;{})S:(ML;;NWNR;;;ME)", current_user_sid()?);
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(io::Error::other)?;
        let stem: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let stdin = Self::pair(&format!(r"\\.\pipe\imperium-script-{stem}-in"), &sddl, true)?;
        let stdout = Self::pair(
            &format!(r"\\.\pipe\imperium-script-{stem}-out"),
            &sddl,
            false,
        )?;
        let stderr = Self::pair(
            &format!(r"\\.\pipe\imperium-script-{stem}-err"),
            &sddl,
            false,
        )?;
        Ok(Self {
            servers: [stdin.0, stdout.0, stderr.0],
            child: [stdin.1, stdout.1, stderr.1],
        })
    }

    /// One pipe: the daemon writes when `to_child`, reads otherwise. The child's end is opened
    /// here, not inheritable, and is the pipe's only client; step 7 duplicates it into the child.
    fn pair(name: &str, sddl: &str, to_child: bool) -> io::Result<(NamedPipeServer, Handle)> {
        let descriptor = Descriptor::from_sddl(sddl)?;
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.as_ptr(),
            bInheritHandle: 0,
        };
        let mut options = ServerOptions::new();
        options
            .first_pipe_instance(true)
            .max_instances(1)
            .reject_remote_clients(true)
            .access_inbound(!to_child)
            .access_outbound(to_child);
        // SAFETY: `attributes` is valid and its descriptor lives past the call, which copies it.
        let server = unsafe {
            options.create_with_security_attributes_raw(name, (&raw mut attributes).cast())
        }?;
        let access = if to_child {
            GENERIC_READ
        } else {
            GENERIC_WRITE
        };
        let path = wide(name);
        // SAFETY: `path` is NUL-terminated; no security attributes (so the handle is not
        // inheritable); a synchronous open of an existing pipe.
        let client = unsafe {
            CreateFileW(
                path.as_ptr(),
                access,
                0,
                ptr::null(),
                OPEN_EXISTING,
                0,
                ptr::null_mut(),
            )
        };
        if client == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok((server, Handle(client)))
    }
}

/// The only variables the worker's environment holds. An AppContainer launch rewrites the folder
/// variables to the container's own folders (under `%LOCALAPPDATA%\Packages\<name>\AC`) and
/// fails with `ERROR_ENVVAR_NOT_FOUND` when the block lacks them, so an empty block (stage 1b) no
/// longer works. They name folders, not secrets; nothing else of the daemon's is passed.
pub const CONTAINER_VARIABLES: [&str; 5] =
    ["APPDATA", "LOCALAPPDATA", "TEMP", "TMP", "USERPROFILE"];

/// A Unicode environment block of the daemon's values of [`CONTAINER_VARIABLES`], sorted, each
/// `NAME=value` NUL-terminated, then a final NUL.
fn container_environment() -> Vec<u16> {
    let mut block: Vec<u16> = Vec::new();
    for name in CONTAINER_VARIABLES {
        if let Some(value) = std::env::var_os(name) {
            let mut entry = OsString::from(name);
            entry.push("=");
            entry.push(value);
            block.extend(wide(entry));
        }
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

/// Step 6: `CreateProcessAsUserW` in the AppContainer, suspended, inheriting nothing.
fn create_suspended(
    token: &Handle,
    program: &Path,
    command: &str,
    folder: &Path,
    package: &Sid,
) -> io::Result<PROCESS_INFORMATION> {
    let capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: package.as_psid(),
        Capabilities: ptr::null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };
    let mut size = 0_usize;
    // SAFETY: a size query with no list.
    unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &raw mut size) };
    if size == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut list = vec![0_u64; size.div_ceil(8)];
    let list_ptr = list.as_mut_ptr().cast::<c_void>();
    // SAFETY: `list` holds `size` bytes, aligned.
    if unsafe { InitializeProcThreadAttributeList(list_ptr, 1, 0, &raw mut size) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        // SAFETY: the list was initialized for one attribute; `capabilities` and the SID it
        // points to outlive its use.
        if unsafe {
            UpdateProcThreadAttribute(
                list_ptr,
                0,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                (&raw const capabilities).cast(),
                size_of::<SECURITY_CAPABILITIES>(),
                ptr::null_mut(),
                ptr::null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.lpAttributeList = list_ptr;
        let application = wide(program);
        let mut command = wide(command);
        let folder = wide(folder);
        let environment = container_environment();
        let mut info = PROCESS_INFORMATION::default();
        // SAFETY: every pointer is valid and outlives the call; `command` is writable as the
        // call requires; no handle is inherited.
        let created = unsafe {
            CreateProcessAsUserW(
                token.0,
                application.as_ptr(),
                command.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                CREATE_SUSPENDED
                    | CREATE_UNICODE_ENVIRONMENT
                    | DETACHED_PROCESS
                    | EXTENDED_STARTUPINFO_PRESENT,
                environment.as_ptr().cast(),
                folder.as_ptr(),
                (&raw const startup).cast(),
                &raw mut info,
            )
        };
        if created == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(info)
    })();
    // SAFETY: the list was initialized above and is deleted once.
    unsafe { DeleteProcThreadAttributeList(list_ptr) };
    result
}

/// Step 7: duplicates `ends` (stdin, stdout, stderr) into the suspended `process` and writes their
/// values into its process parameters' `StandardInput`, `StandardOutput` and `StandardError`,
/// which `GetStdHandle` (and so Rust's standard streams) reads.
///
/// `RTL_USER_PROCESS_PARAMETERS` is documented only in part: the three handles are the third to
/// fifth pointer-sized fields of its `Reserved2`, after `ConsoleHandle` and `ConsoleFlags`, on
/// every Windows version since NT. `CreateProcess` has built the structure in the child before it
/// returns, and the child reads it only once it runs.
fn give_standard_handles(process: HANDLE, ends: &[Handle; 3]) -> io::Result<()> {
    let mut values: [HANDLE; 3] = [ptr::null_mut(); 3];
    for (end, value) in ends.iter().zip(values.iter_mut()) {
        // SAFETY: `end` is a handle this process owns; `process` is the child, opened with every
        // access by its creation; `value` is a valid out-pointer. The copy is not inheritable.
        if unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                end.0,
                process,
                value,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
    }

    // SAFETY: an all-zero PROCESS_BASIC_INFORMATION is a valid value to be overwritten.
    let mut basic: PROCESS_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let mut returned = 0_u32;
    // SAFETY: `basic` is the structure the class names, with its size; valid out-pointers.
    let status = unsafe {
        NtQueryInformationProcess(
            process,
            ProcessBasicInformation,
            (&raw mut basic).cast(),
            size_of::<PROCESS_BASIC_INFORMATION>() as u32,
            &raw mut returned,
        )
    };
    if status < 0 {
        return Err(io::Error::other(format!(
            "NtQueryInformationProcess: {status:#x}"
        )));
    }
    let peb = basic.PebBaseAddress.cast::<u8>();
    let mut parameters: *mut RTL_USER_PROCESS_PARAMETERS = ptr::null_mut();
    read_child(
        process,
        peb.wrapping_add(offset_of!(PEB, ProcessParameters)),
        &mut parameters,
    )?;
    if parameters.is_null() {
        return Err(io::Error::other("the child has no process parameters"));
    }
    let parameters = parameters.cast::<u8>();
    // `MaximumLength` and `Length`, the first two fields: the structure must reach the handles.
    let mut lengths = [0_u32; 2];
    read_child(process, parameters, &mut lengths)?;
    let standard = offset_of!(RTL_USER_PROCESS_PARAMETERS, Reserved2) + 2 * size_of::<HANDLE>();
    if (lengths[1] as usize) < standard + size_of_val(&values) {
        return Err(io::Error::other(format!(
            "the child's process parameters are {} bytes, too short to hold its standard handles",
            lengths[1]
        )));
    }
    let mut written = 0_usize;
    // SAFETY: the three handle fields inside the child's parameters, checked above to lie within
    // the structure; `values` is valid for its size; the child is suspended.
    if unsafe {
        WriteProcessMemory(
            process,
            parameters.wrapping_add(standard).cast(),
            values.as_ptr().cast(),
            size_of_val(&values),
            &raw mut written,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if written != size_of_val(&values) {
        return Err(io::Error::other(
            "the standard handles were written in part",
        ));
    }
    Ok(())
}

/// Reads a `T` from `address` in the child `process`.
fn read_child<T: Copy>(process: HANDLE, address: *const u8, value: &mut T) -> io::Result<()> {
    let mut read = 0_usize;
    // SAFETY: `value` is valid for writes of its size; the call checks `address` in the child.
    if unsafe {
        ReadProcessMemory(
            process,
            address.cast(),
            std::ptr::from_mut(value).cast(),
            size_of::<T>(),
            &raw mut read,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if read != size_of::<T>() {
        return Err(io::Error::other("a short read of the child's memory"));
    }
    Ok(())
}

/// Quotes one command-line argument the way the C runtime parses it back.
fn quote(arg: &OsStr) -> String {
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
