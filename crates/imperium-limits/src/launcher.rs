//! The Linux launcher (plan L3, "Joining with no window"): it puts its own process in the unit,
//! checks that it is there, sets the rlimits, and then `exec`s the program. `exec` replaces it,
//! so no extra process stays and nothing of the program runs outside the unit. It needs no
//! `unsafe` (file writes, `setrlimit` and `CommandExt::exec`).
//!
//! Arguments: `[--class NAME] [--cgroup PATH] [--rlimit-data BYTES] [--oom-adj N] -- PROGRAM
//! ARGS...`. Every child gets `RLIMIT_CORE = 0` and `IMPERIUM_LIMITS_UNIT=<class>` (a hint for
//! a daemon started inside a unit, which grants nothing).
//!
//! Exit codes: 125 when the unit could not be joined or set (`refused_start`), 127 when `exec`
//! failed.

use std::{
    ffi::OsString,
    os::unix::process::CommandExt as _,
    path::{Path, PathBuf},
};

use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

/// The exit code of a launch refused before the program ran.
pub const REFUSED: i32 = 125;

#[derive(Debug, Default)]
struct Options {
    class: Option<OsString>,
    cgroup: Option<PathBuf>,
    rlimit_data: Option<u64>,
    oom_adj: Option<i32>,
    program: Option<OsString>,
    args: Vec<OsString>,
}

fn parse(args: Vec<OsString>) -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{} needs a value", arg.to_string_lossy()))
        };
        let number = |value: OsString| -> Result<i64, String> {
            value
                .to_str()
                .and_then(|text| text.parse().ok())
                .ok_or_else(|| format!("{} is not a number", value.to_string_lossy()))
        };
        match arg.to_str() {
            Some("--class") => options.class = Some(value()?),
            Some("--cgroup") => options.cgroup = Some(PathBuf::from(value()?)),
            Some("--rlimit-data") => {
                options.rlimit_data =
                    Some(u64::try_from(number(value()?)?).map_err(|err| err.to_string())?);
            }
            Some("--oom-adj") => {
                options.oom_adj =
                    Some(i32::try_from(number(value()?)?).map_err(|err| err.to_string())?);
            }
            Some("--") => {
                options.program = args.next();
                options.args = args.collect();
                break;
            }
            _ => return Err(format!("unknown option {}", arg.to_string_lossy())),
        }
    }
    if options.program.is_none() {
        return Err("no program after --".to_owned());
    }
    Ok(options)
}

/// Joins `cgroup` and checks `/proc/self/cgroup` shows it.
fn join(cgroup: &Path) -> Result<(), String> {
    std::fs::write(cgroup.join("cgroup.procs"), std::process::id().to_string())
        .map_err(|err| format!("{}: {err}", cgroup.display()))?;
    let own = std::fs::read_to_string("/proc/self/cgroup").map_err(|err| err.to_string())?;
    let now = own
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(|path| path.trim().trim_start_matches('/'))
        .ok_or("not in a cgroup v2 hierarchy")?;
    if now.is_empty() || !cgroup.ends_with(now) {
        return Err(format!("joined {now:?}, not {}", cgroup.display()));
    }
    Ok(())
}

fn limit(resource: Resource, bytes: u64) -> Result<(), String> {
    // Never above the hard limit already set: raising it is not ours to do.
    let hard = getrlimit(resource).maximum;
    let value = hard.map_or(bytes, |hard| hard.min(bytes));
    setrlimit(
        resource,
        Rlimit {
            current: Some(value),
            maximum: Some(value),
        },
    )
    .map_err(|err| format!("setrlimit: {err}"))
}

/// Raises this process's `oom_score_adj` by `by` (an unprivileged process may raise it), so the
/// kernel's OOM killer picks a unit before the desktop or the daemon.
fn raise_oom_score(by: i32) -> Result<(), String> {
    let path = "/proc/self/oom_score_adj";
    let now: i32 = std::fs::read_to_string(path)
        .map_err(|err| err.to_string())?
        .trim()
        .parse()
        .map_err(|_| "unreadable oom_score_adj".to_owned())?;
    std::fs::write(path, now.saturating_add(by).clamp(-1000, 1000).to_string())
        .map_err(|err| format!("oom_score_adj: {err}"))
}

/// Runs the launcher with its arguments (after the subcommand's name). Returns only on failure,
/// with the exit code to use.
#[must_use]
pub fn run(args: Vec<OsString>) -> i32 {
    let options = match parse(args) {
        Ok(options) => options,
        Err(err) => {
            eprintln!("imperium limit: {err}");
            return REFUSED;
        }
    };
    let set = (|| -> Result<(), String> {
        if let Some(cgroup) = &options.cgroup {
            join(cgroup)?;
        }
        limit(Resource::Core, 0)?;
        if let Some(bytes) = options.rlimit_data {
            limit(Resource::Data, bytes)?;
        }
        if let Some(by) = options.oom_adj {
            raise_oom_score(by)?;
        }
        Ok(())
    })();
    if let Err(err) = set {
        eprintln!("imperium limit: refused to start: {err}");
        return REFUSED;
    }
    let Some(program) = options.program else {
        return REFUSED;
    };
    let mut command = std::process::Command::new(&program);
    command.args(&options.args);
    if let Some(class) = &options.class {
        command.env("IMPERIUM_LIMITS_UNIT", class);
    }
    let err = command.exec();
    eprintln!("imperium limit: {}: {err}", program.to_string_lossy());
    127
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn options_parse_up_to_the_program() {
        let options = parse(args(&[
            "--class",
            "command",
            "--cgroup",
            "/sys/fs/cgroup/x/u-1",
            "--rlimit-data",
            "1024",
            "--oom-adj",
            "500",
            "--",
            "sh",
            "-c",
            "--cgroup",
        ]))
        .unwrap();
        assert_eq!(options.class.unwrap(), "command");
        assert_eq!(options.cgroup.unwrap(), Path::new("/sys/fs/cgroup/x/u-1"));
        assert_eq!(options.rlimit_data, Some(1024));
        assert_eq!(options.oom_adj, Some(500));
        assert_eq!(options.program.unwrap(), "sh");
        assert_eq!(options.args, args(&["-c", "--cgroup"]));
    }

    #[test]
    fn bad_options_are_refused() {
        assert!(parse(args(&["--cgroup"])).is_err());
        assert!(parse(args(&["--rlimit-data", "lots", "--", "sh"])).is_err());
        assert!(parse(args(&["--class", "command"])).is_err());
        assert!(parse(args(&["--bogus", "--", "sh"])).is_err());
    }
}
