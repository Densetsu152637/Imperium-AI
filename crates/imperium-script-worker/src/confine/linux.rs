//! Linux confinement: `PR_SET_NO_NEW_PRIVS`, `RLIMIT_DATA`, Landlock where the kernel has it,
//! and a seccomp-bpf allowlist (plan C1, N3).
//!
//! The allowlist is what a running worker needs and nothing more: reads from stdin, writes to
//! stdout and stderr, memory management, futexes, clocks, `getrandom`, signal handling, exit,
//! `kill` and `tgkill` aimed at the worker itself, and `clone` for threads only. Everything else,
//! including `open`, `socket`, `connect`, `execve`, a process-creating `clone` and `ptrace`,
//! fails with `EPERM`; `clone3` fails with `ENOSYS` so the C library falls back to `clone`. A
//! different architecture kills the process (seccompiler's preamble), and an x32 system call
//! number matches no allowlist entry. No namespaces are needed, so the worker does not depend on
//! unprivileged user namespaces.
//!
//! The order matters: Landlock is applied before seccomp, and the permissive `clone3` filter
//! before the allowlist, because the allowlist forbids the system calls that install a filter.

use std::collections::BTreeMap;

use landlock::{ABI, Access, AccessFs, Ruleset, RulesetAttr, RulesetStatus};
use rustix::process::{Resource, Rlimit, setrlimit};
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch, apply_filter_all_threads,
};

use super::{ConfineError, Report, Settings, forced};

/// `PR_SET_NAME`: the only `prctl` the worker's threads may call (thread naming).
const PR_SET_NAME: u64 = 15;

/// `clone` flags that make a process rather than a thread, or escape its namespaces:
/// `CLONE_NEWTIME`, `CLONE_PTRACE`, `CLONE_VFORK`, `CLONE_NEWNS`, `CLONE_UNTRACED` and the other
/// `CLONE_NEW*` flags. A thread's `clone` carries none of them.
const CLONE_FORBIDDEN: u64 = 0x0000_0080
    | 0x0000_2000
    | 0x0000_4000
    | 0x0002_0000
    | 0x0080_0000
    | 0x0200_0000
    | 0x0400_0000
    | 0x0800_0000
    | 0x1000_0000
    | 0x2000_0000
    | 0x4000_0000;

/// `CLONE_THREAD`, which every thread's `clone` carries.
const CLONE_THREAD: u64 = 0x0001_0000;

pub(super) fn apply(settings: &Settings) -> Result<Report, ConfineError> {
    let mut report = Report::default();

    forced(settings, "no_new_privs")?;
    rustix::thread::set_no_new_privs(true).map_err(|err| ConfineError::new("no_new_privs", err))?;
    report.applied.push("no_new_privs".to_owned());

    // RLIMIT_DATA, not RLIMIT_AS: large virtual reservations trip the latter.
    forced(settings, "rlimit")?;
    for (resource, value) in [
        (Resource::Data, settings.memory_limit_bytes),
        (Resource::Core, 0),
    ] {
        setrlimit(
            resource,
            Rlimit {
                current: Some(value),
                maximum: Some(value),
            },
        )
        .map_err(|err| ConfineError::new("rlimit", err))?;
    }
    report
        .applied
        .push(format!("rlimit_data={}", settings.memory_limit_bytes));

    forced(settings, "landlock")?;
    report.applied.push(landlock()?);

    forced(settings, "seccomp")?;
    install_seccomp()?;
    report.applied.push("seccomp".to_owned());
    Ok(report)
}

/// Landlock with every file-system access handled and no rule granting any: no file access at
/// all. A kernel without Landlock reports `NotEnforced`, which is not an error (seccomp still
/// denies every file open); an error from a kernel that has it is.
fn landlock() -> Result<String, ConfineError> {
    let status = Ruleset::default()
        .handle_access(AccessFs::from_all(ABI::V3))
        .and_then(landlock::Ruleset::create)
        .and_then(landlock::RulesetCreated::restrict_self)
        .map_err(|err| ConfineError::new("landlock", err))?;
    let state = match status.ruleset {
        RulesetStatus::FullyEnforced => "fully enforced",
        RulesetStatus::PartiallyEnforced => "partially enforced",
        RulesetStatus::NotEnforced => "not enforced (kernel without Landlock)",
    };
    Ok(format!("landlock: {state}"))
}

/// Installs both filters on every thread.
fn install_seccomp() -> Result<(), ConfineError> {
    let (allowlist, clone3) = build_filters()?;
    // The permissive one first: the allowlist forbids installing anything afterwards. Both apply
    // to every call; for `clone3` the allowlist says yes and this one says `ENOSYS`, and the
    // most restrictive answer wins.
    apply_filter_all_threads(&clone3).map_err(|err| ConfineError::new("seccomp", err))?;
    apply_filter_all_threads(&allowlist).map_err(|err| ConfineError::new("seccomp", err))?;
    Ok(())
}

fn seccomp_error(err: impl std::fmt::Display) -> ConfineError {
    ConfineError::new("seccomp", err)
}

fn condition(
    arg: u8,
    op: SeccompCmpOp,
    value: u64,
    len: SeccompCmpArgLen,
) -> Result<SeccompCondition, ConfineError> {
    SeccompCondition::new(arg, len, op, value).map_err(seccomp_error)
}

fn rule(conditions: Vec<SeccompCondition>) -> Result<SeccompRule, ConfineError> {
    SeccompRule::new(conditions).map_err(seccomp_error)
}

/// The two programs: the allowlist (everything else `EPERM`), and a tiny one that makes `clone3`
/// fail with `ENOSYS` and allows the rest.
fn build_filters() -> Result<(BpfProgram, BpfProgram), ConfineError> {
    let arch: TargetArch = std::env::consts::ARCH.try_into().map_err(seccomp_error)?;
    let own_pid = u64::from(std::process::id());
    let dword = SeccompCmpArgLen::Dword;
    let qword = SeccompCmpArgLen::Qword;

    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();

    // `c_long` is `i64` on the 64-bit Linux targets this builds for.
    let unconditional: &[i64] = &[
        libc::SYS_close,
        libc::SYS_fstat,
        // Memory management.
        libc::SYS_mmap,
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_mprotect,
        libc::SYS_madvise,
        libc::SYS_brk,
        // Threads and synchronisation.
        libc::SYS_futex,
        libc::SYS_rseq,
        libc::SYS_set_robust_list,
        libc::SYS_set_tid_address,
        libc::SYS_sched_yield,
        libc::SYS_sched_getaffinity,
        libc::SYS_getpid,
        libc::SYS_gettid,
        libc::SYS_restart_syscall,
        // Clocks and entropy.
        libc::SYS_clock_gettime,
        libc::SYS_clock_getres,
        libc::SYS_clock_nanosleep,
        libc::SYS_nanosleep,
        libc::SYS_gettimeofday,
        libc::SYS_getrandom,
        // Signals and exit.
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_sigaltstack,
        libc::SYS_exit,
        libc::SYS_exit_group,
        // Reading a limit (the standard library asks for the stack size); setting one is below.
        libc::SYS_getrlimit,
        // Allowed here; the second filter turns it into ENOSYS.
        libc::SYS_clone3,
    ];
    for nr in unconditional {
        rules.insert(*nr, Vec::new());
    }

    // Reads on stdin; writes on stdout and stderr.
    rules.insert(
        libc::SYS_read,
        vec![rule(vec![condition(
            0,
            SeccompCmpOp::Eq,
            0,
            dword.clone(),
        )?])?],
    );
    rules.insert(
        libc::SYS_write,
        vec![
            rule(vec![condition(0, SeccompCmpOp::Eq, 1, dword.clone())?])?,
            rule(vec![condition(0, SeccompCmpOp::Eq, 2, dword.clone())?])?,
        ],
    );

    // `kill` and `tgkill`: only the worker's own process id (the C library's `abort` needs no
    // more, and the daemon kills the worker from outside).
    rules.insert(
        libc::SYS_kill,
        vec![rule(vec![condition(
            0,
            SeccompCmpOp::Eq,
            own_pid,
            dword.clone(),
        )?])?],
    );
    rules.insert(
        libc::SYS_tgkill,
        vec![rule(vec![condition(
            0,
            SeccompCmpOp::Eq,
            own_pid,
            dword.clone(),
        )?])?],
    );

    // `clone`: a thread only (CLONE_THREAD set, no namespace, ptrace, vfork or untraced flags).
    rules.insert(
        libc::SYS_clone,
        vec![rule(vec![
            condition(0, SeccompCmpOp::MaskedEq(CLONE_FORBIDDEN), 0, dword.clone())?,
            condition(
                0,
                SeccompCmpOp::MaskedEq(CLONE_THREAD),
                CLONE_THREAD,
                dword.clone(),
            )?,
        ])?],
    );

    // `prlimit64` only to read (a null new-limit pointer); `prctl` only to name a thread.
    rules.insert(
        libc::SYS_prlimit64,
        vec![rule(vec![condition(2, SeccompCmpOp::Eq, 0, qword)?])?],
    );
    rules.insert(
        libc::SYS_prctl,
        vec![rule(vec![condition(
            0,
            SeccompCmpOp::Eq,
            PR_SET_NAME,
            dword,
        )?])?],
    );

    let eperm = SeccompAction::Errno(u32::try_from(libc::EPERM).unwrap_or(1));
    let allowlist =
        SeccompFilter::new(rules, eperm, SeccompAction::Allow, arch).map_err(seccomp_error)?;

    let mut clone3_rules = BTreeMap::new();
    clone3_rules.insert(libc::SYS_clone3, Vec::new());
    let enosys = SeccompAction::Errno(u32::try_from(libc::ENOSYS).unwrap_or(38));
    let clone3 = SeccompFilter::new(clone3_rules, SeccompAction::Allow, enosys, arch)
        .map_err(seccomp_error)?;

    let allowlist: BpfProgram = allowlist.try_into().map_err(seccomp_error)?;
    let clone3: BpfProgram = clone3.try_into().map_err(seccomp_error)?;
    Ok((allowlist, clone3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_compile_for_this_architecture() {
        let (allowlist, clone3) = build_filters().expect("the filters build");
        assert!(!allowlist.is_empty());
        assert!(!clone3.is_empty());
    }
}
