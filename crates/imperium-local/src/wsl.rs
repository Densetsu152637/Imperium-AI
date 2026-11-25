//! vLLM on native Windows runs inside a WSL2 distribution: the managed `uv` environment lives in
//! the distro's own filesystem and `vllm serve` is started with `wsl.exe -d <distro> --exec ...`
//! (no shell), reached from Windows through WSL2's localhost forwarding.
//!
//! Everything here that decides something is a pure function over text, so it is tested on every
//! platform: the `wsl.exe -l -v` listing ([`decode_output`], [`parse_distros`],
//! [`choose_distro`]), Windows to WSL paths ([`windows_to_wsl_path`], [`command_in_wsl`]), the
//! launch arguments ([`serve_args`]), what stops a server ([`stop_pattern`]) and the networking
//! check ([`parse_wslconfig`], [`forwarding_problem`]). Only [`list_distros`] and
//! [`forwarding_problem_here`] touch the machine, and only on Windows.

use std::time::Duration;

/// The program that reaches WSL from Windows.
pub const WSL_EXE: &str = "wsl.exe";

/// Where the managed environment lives inside the distro, below the user's home.
pub const HOME_SUBDIR: &str = ".local/share/imperium/runtimes/vllm";

/// One line of `wsl.exe -l -v`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Distro {
    pub name: String,
    /// Marked with `*`: what `wsl.exe` uses without `-d`.
    pub default: bool,
    /// As `wsl.exe` printed it (`Running`, `Stopped`, localised).
    pub state: String,
    /// 1 or 2; only WSL2 gives a distro a GPU.
    pub version: u8,
}

/// The text of `wsl.exe`'s own output. It writes UTF-16LE (with a byte order mark or, when
/// redirected, without one) unless `WSL_UTF8=1` is set, so both are read; NUL characters, which
/// are what a UTF-16 message looks like when read as bytes of text, are dropped.
pub fn decode_output(bytes: &[u8]) -> String {
    let utf16 = bytes.starts_with(&[0xFF, 0xFE])
        || (bytes.len() >= 4
            && bytes
                .iter()
                .skip(1)
                .step_by(2)
                .take(16)
                .all(|byte| *byte == 0));
    let text = if utf16 {
        let units: Vec<u16> = bytes
            .iter()
            .step_by(2)
            .zip(bytes.iter().skip(1).step_by(2))
            .map(|(low, high)| u16::from_le_bytes([*low, *high]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    text.chars()
        .filter(|c| *c != '\u{feff}' && *c != '\0')
        .collect()
}

/// The distros in the decoded text of `wsl.exe -l -v`: a header, then one row per distro,
/// `* NAME STATE VERSION` (the star marks the default). Rows that do not end in a version number
/// (a message instead of a listing) are skipped.
pub fn parse_distros(text: &str) -> Vec<Distro> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    lines.next();
    lines
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let (default, rest) = match trimmed.strip_prefix('*') {
                Some(rest) => (true, rest),
                None => (false, trimmed),
            };
            let tokens: Vec<&str> = rest.split_whitespace().collect();
            let [name, state, .., version] = tokens.as_slice() else {
                return None;
            };
            Some(Distro {
                name: (*name).to_string(),
                default,
                state: (*state).to_string(),
                version: version.parse().ok()?,
            })
        })
        .collect()
}

/// A distro name as `wsl.exe -d` takes it, restricted to what is safe in an argument.
pub fn valid_distro(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with(['-', '.'])
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    ok.then_some(()).ok_or_else(|| {
        format!("{name:?} is not a WSL distribution name (letters, digits, `.`, `_` and `-`)")
    })
}

/// The distro to use: `configured` (`runtime.vllm.wsl_distro`) when set, else the default one.
/// It must be a WSL2 distro of its own; Docker Desktop's internal ones cannot run vLLM.
pub fn choose_distro(distros: &[Distro], configured: &str) -> Result<Distro, String> {
    let configured = configured.trim();
    let names = || {
        distros
            .iter()
            .map(|distro| distro.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let chosen = if configured.is_empty() {
        distros
            .iter()
            .find(|distro| distro.default)
            .ok_or_else(|| {
                "WSL has no default distribution; install one with `wsl --install` or set \
                 runtime.vllm.wsl_distro"
                    .to_string()
            })?
    } else {
        distros
            .iter()
            .find(|distro| distro.name.eq_ignore_ascii_case(configured))
            .ok_or_else(|| {
                format!(
                    "runtime.vllm.wsl_distro is {configured:?}, which is not an installed WSL \
                     distribution (installed: {})",
                    names()
                )
            })?
    };
    valid_distro(&chosen.name)?;
    if chosen
        .name
        .to_ascii_lowercase()
        .starts_with("docker-desktop")
    {
        return Err(format!(
            "{} is Docker Desktop's internal distribution; set runtime.vllm.wsl_distro to a \
             Linux distribution (installed: {})",
            chosen.name,
            names()
        ));
    }
    if chosen.version != 2 {
        return Err(format!(
            "{} runs under WSL {}; vLLM needs WSL2 for the GPU (`wsl --set-version {} 2`)",
            chosen.name, chosen.version, chosen.name
        ));
    }
    Ok(chosen.clone())
}

/// What a `\\wsl$\<distro>\...` or `\\wsl.localhost\<distro>\...` path names: the distro and the
/// path inside it. `rest` is the text after the leading `//` with `/` separators.
fn wsl_share(rest: &str) -> Option<(&str, &str)> {
    let mut parts = rest.splitn(3, '/');
    let host = parts.next()?;
    if !(host.eq_ignore_ascii_case("wsl$") || host.eq_ignore_ascii_case("wsl.localhost")) {
        return None;
    }
    let name = parts.next().filter(|name| !name.is_empty())?;
    Some((name, parts.next().unwrap_or("")))
}

/// `/a/b` from the segments of `tail`: empty and `.` segments dropped, `..` refused (a path is
/// translated, never resolved).
fn clean(tail: &str, shown: &str) -> Result<String, String> {
    let mut out = String::new();
    for segment in tail.split('/') {
        match segment {
            "" | "." => {}
            ".." => return Err(format!("{shown:?} contains `..`, which is not translated")),
            segment => {
                out.push('/');
                out.push_str(segment);
            }
        }
    }
    Ok(out)
}

fn unc_refusal(shown: &str) -> String {
    format!(
        "{shown:?} is a network path, which WSL cannot reach; copy the model to a local drive \
         or into the distribution"
    )
}

/// The WSL path of the Windows path `path`, for the distro `distro`:
///
/// - `C:\Users\me\models` becomes `/mnt/c/Users/me/models` (WSL's default automount root);
/// - `\\wsl$\<distro>\home\me\m` and `\\wsl.localhost\<distro>\home\me\m` become `/home/me/m`
///   when `<distro>` is `distro`, and are refused for another one;
/// - other UNC paths, paths relative to a drive or the current drive, and `..` are refused.
///
/// Spaces are fine: the path travels as one argument, never through a shell.
pub fn windows_to_wsl_path(path: &str, distro: &str) -> Result<String, String> {
    if path.is_empty() || path.contains(['\0', '\n', '\r']) {
        return Err(format!("{path:?} is not a usable model folder path"));
    }
    let normal = path.replace('\\', "/");
    let mut text = normal.as_str();
    if let Some(rest) = text.strip_prefix("//?/") {
        // The `\\?\` prefix: `\\?\C:\...` is a drive path, `\\?\UNC\...` a network one.
        if rest
            .get(..4)
            .is_some_and(|p| p.eq_ignore_ascii_case("unc/"))
        {
            return Err(unc_refusal(path));
        }
        text = rest;
    } else if let Some(rest) = text.strip_prefix("//") {
        let Some((name, tail)) = wsl_share(rest) else {
            return Err(unc_refusal(path));
        };
        if !name.eq_ignore_ascii_case(distro) {
            return Err(format!(
                "{path:?} is in the WSL distribution {name}, but vLLM runs in {distro}; set \
                 runtime.vllm.wsl_distro to {name} or move the model"
            ));
        }
        let inside = clean(tail, path)?;
        return Ok(if inside.is_empty() {
            "/".into()
        } else {
            inside
        });
    }
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        if bytes.len() > 2 && bytes[2] != b'/' {
            return Err(format!(
                "{path:?} is relative to the current folder of drive {}:; use a full path",
                bytes[0] as char
            ));
        }
        let drive = (bytes[0] as char).to_ascii_lowercase();
        return Ok(format!("/mnt/{drive}{}", clean(&text[2..], path)?));
    }
    Err(format!(
        "{path:?} is not a full Windows path such as C:\\models\\name"
    ))
}

/// A `runtime.vllm.command` that points into WSL: a path starting with `/`, or a
/// `\\wsl$\<distro>\...` / `\\wsl.localhost\<distro>\...` one. The distro (when the path names
/// it) and the path inside it; `None` for anything else, which is a Windows command.
pub fn command_in_wsl(command: &str) -> Option<(Option<String>, String)> {
    let normal = command.trim().replace('\\', "/");
    if normal.contains(['\0', '\n', '\r']) {
        return None;
    }
    if let Some(rest) = normal.strip_prefix("//") {
        let (name, tail) = wsl_share(rest)?;
        let inside = clean(tail, command)
            .ok()
            .filter(|inside| !inside.is_empty())?;
        return Some((Some(name.to_string()), inside));
    }
    if normal.starts_with('/') {
        let inside = clean(&normal, command)
            .ok()
            .filter(|inside| !inside.is_empty())?;
        return Some((None, inside));
    }
    None
}

/// Whether `path` can be an executable path inside a distro: absolute, no control characters,
/// no `..`.
pub fn valid_linux_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 512
        && !path.chars().any(char::is_control)
        && !path.split('/').any(|segment| segment == "..")
}

/// The arguments of `wsl.exe` that run `vllm serve` for one model in `distro`: `serve_args` are
/// the `vllm serve` ones ([`crate::backend::Vllm`]'s launch args, without the port) whose model
/// folder is a Windows path, translated here and passed as an argument (never mounted). `program`
/// is `vllm` inside the distro. There is no shell: `--exec` starts `env` (which sets the
/// no-telemetry variables) and `env` starts vLLM. The server always binds `127.0.0.1` inside
/// WSL2, whatever `extra_args` ask for, and is reached through WSL2's localhost forwarding.
pub fn serve_args(
    distro: &str,
    program: &str,
    port: u16,
    serve_args: &[String],
) -> Result<Vec<String>, String> {
    valid_distro(distro)?;
    if !valid_linux_path(program) {
        return Err(format!("{program:?} is not a path inside {distro}"));
    }
    let [serve, folder, rest @ ..] = serve_args else {
        return Err("no model folder to serve".into());
    };
    if serve != "serve" {
        return Err("expected vllm serve arguments".into());
    }
    let mut args: Vec<String> = [
        "-d",
        distro,
        "--exec",
        "env",
        "VLLM_NO_USAGE_STATS=1",
        "DO_NOT_TRACK=1",
        program,
        "serve",
    ]
    .map(String::from)
    .to_vec();
    args.push(windows_to_wsl_path(folder, distro)?);
    let mut bound = false;
    let mut rest = rest.iter();
    while let Some(arg) = rest.next() {
        if arg == "--host" {
            rest.next();
            args.extend(["--host".into(), "127.0.0.1".into()]);
            bound = true;
        } else if arg.starts_with("--host=") {
            args.push("--host=127.0.0.1".into());
            bound = true;
        } else {
            args.push(arg.clone());
        }
    }
    if !bound {
        args.extend(["--host".into(), "127.0.0.1".into()]);
    }
    args.extend(["--port".into(), port.to_string()]);
    Ok(args)
}

/// `text` with the characters of an extended regular expression escaped.
fn regex_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\.^$|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The `pkill -f` / `pgrep -f` pattern of the server [`serve_args`] started on `port`: its
/// program, `serve`, and the `--port` it ends with (a port is free for one server at a time).
pub fn stop_pattern(program: &str, port: u16) -> String {
    format!("{} serve .* --port {port}$", regex_escape(program))
}

/// The arguments of `wsl.exe` that run `command` (a program and its arguments) inside `distro`,
/// without a shell.
pub fn exec_args(distro: &str, command: &[&str]) -> Vec<String> {
    ["-d", distro, "--exec"]
        .into_iter()
        .chain(command.iter().copied())
        .map(String::from)
        .collect()
}

/// What `%USERPROFILE%\.wslconfig` says about reaching WSL2 from Windows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WslConfig {
    /// `localhostForwarding` under `[wsl2]`.
    pub localhost_forwarding: Option<bool>,
    /// `networkingMode` under `[wsl2]`, lower case.
    pub networking_mode: Option<String>,
}

/// The `[wsl2]` settings in the text of a `.wslconfig` that matter here.
pub fn parse_wslconfig(text: &str) -> WslConfig {
    let mut config = WslConfig::default();
    let mut in_wsl2 = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_wsl2 = section.trim().eq_ignore_ascii_case("wsl2");
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !in_wsl2 {
            continue;
        }
        let value = value.split(['#', ';']).next().unwrap_or("").trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "localhostforwarding" => {
                config.localhost_forwarding = match value.to_ascii_lowercase().as_str() {
                    "true" => Some(true),
                    "false" => Some(false),
                    _ => None,
                }
            }
            "networkingmode" => config.networking_mode = Some(value.to_ascii_lowercase()),
            _ => {}
        }
    }
    config
}

/// Why Windows cannot reach a server on WSL2's loopback with this configuration, naming the
/// setting; `None` when it can (the defaults, or mirrored networking, where `localhost` is shared).
pub fn forwarding_problem(config: &WslConfig) -> Option<String> {
    let mode = config.networking_mode.as_deref().unwrap_or("nat");
    if mode == "mirrored" {
        return None;
    }
    if matches!(mode, "bridged" | "virtioproxy" | "none") {
        return Some(format!(
            "WSL2's `networkingMode={mode}` (in %USERPROFILE%\\.wslconfig) does not forward \
             localhost to Windows, so a vLLM server in WSL2 cannot be reached; remove it or set \
             `networkingMode=mirrored` under [wsl2], then run `wsl --shutdown`"
        ));
    }
    (config.localhost_forwarding == Some(false)).then(|| {
        "WSL2's `localhostForwarding=false` (in %USERPROFILE%\\.wslconfig) stops Windows reaching \
         a vLLM server in WSL2; remove it or set `networkingMode=mirrored` under [wsl2], then run \
         `wsl --shutdown`"
            .to_string()
    })
}

/// [`forwarding_problem`] for this machine's `.wslconfig` (none when there is none, and always
/// `None` off Windows).
pub fn forwarding_problem_here() -> Option<String> {
    #[cfg(windows)]
    {
        let home = std::env::var_os("USERPROFILE")?;
        let text = std::fs::read_to_string(std::path::Path::new(&home).join(".wslconfig")).ok()?;
        forwarding_problem(&parse_wslconfig(&text))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// The installed distros, from `wsl.exe -l -v`; the call is killed after `timeout`. Off Windows
/// there is no `wsl.exe` to ask and this is an error.
pub fn list_distros(timeout: Duration) -> Result<Vec<Distro>, String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        const NO_WINDOW: u32 = 0x0800_0000;
        let child = Command::new(WSL_EXE)
            .args(["-l", "-v"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(NO_WINDOW)
            .spawn()
            .map_err(|error| format!("could not run {WSL_EXE}: {error}; is WSL installed?"))?;
        let id = child.id();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(child.wait_with_output());
        });
        match receiver.recv_timeout(timeout) {
            Ok(Ok(output)) => {
                let text = decode_output(&output.stdout);
                if !output.status.success() {
                    let why = decode_output(&output.stderr);
                    return Err(format!(
                        "wsl.exe -l -v failed: {}",
                        [text.trim(), why.trim()].join(" ").trim()
                    ));
                }
                Ok(parse_distros(&text))
            }
            Ok(Err(error)) => Err(format!("wsl.exe -l -v failed: {error}")),
            Err(_) => {
                let _ = Command::new("taskkill")
                    .args(["/PID", &id.to_string(), "/F"])
                    .creation_flags(NO_WINDOW)
                    .output();
                Err(format!("wsl.exe -l -v did not answer within {timeout:?}"))
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = timeout;
        Err("WSL is reached from Windows only".into())
    }
}

/// The name of WSL's default distribution when it can run vLLM ([`choose_distro`]); `None` when
/// WSL does not answer or has none.
pub fn default_distro_name() -> Option<String> {
    let distros = list_distros(Duration::from_secs(10)).ok()?;
    choose_distro(&distros, "").ok().map(|distro| distro.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(text: &str, bom: bool) -> Vec<u8> {
        let mut bytes = if bom { vec![0xFF, 0xFE] } else { Vec::new() };
        for unit in text.encode_utf16() {
            bytes.extend(unit.to_le_bytes());
        }
        bytes
    }

    const LISTING: &str = "  NAME              STATE           VERSION\r\n\
        * Ubuntu-24.04      Running         2\r\n\
          docker-desktop    Stopped         2\r\n\
          Debian            Stopped         1\r\n";

    #[test]
    fn the_utf16_listing_is_decoded_with_or_without_a_byte_order_mark() {
        for bom in [true, false] {
            let text = decode_output(&utf16(LISTING, bom));
            assert!(text.starts_with("  NAME"), "{text:?}");
            let distros = parse_distros(&text);
            assert_eq!(distros.len(), 3, "{distros:?}");
            assert_eq!(
                distros[0],
                Distro {
                    name: "Ubuntu-24.04".into(),
                    default: true,
                    state: "Running".into(),
                    version: 2
                }
            );
            assert!(!distros[1].default);
            assert_eq!(distros[2].version, 1);
        }
    }

    #[test]
    fn a_utf8_listing_is_read_as_it_is() {
        let distros = parse_distros(&decode_output(LISTING.as_bytes()));
        assert_eq!(distros.len(), 3);
        assert_eq!(distros[0].name, "Ubuntu-24.04");
    }

    #[test]
    fn a_message_instead_of_a_listing_has_no_distros() {
        let message = "Windows Subsystem for Linux has no installed distributions.\r\n\
            Use 'wsl.exe --install' to install.\r\n";
        assert!(parse_distros(&decode_output(&utf16(message, true))).is_empty());
        assert!(parse_distros("").is_empty());
    }

    #[test]
    fn a_utf16_error_message_loses_its_nul_characters() {
        let text = decode_output(&utf16(
            "There is no distribution with the supplied name.",
            false,
        ));
        assert_eq!(text, "There is no distribution with the supplied name.");
    }

    #[test]
    fn the_distro_is_the_configured_one_or_the_default() {
        let distros = parse_distros(LISTING);
        assert_eq!(choose_distro(&distros, "").unwrap().name, "Ubuntu-24.04");
        assert_eq!(
            choose_distro(&distros, " ubuntu-24.04 ").unwrap().name,
            "Ubuntu-24.04"
        );
        let missing = choose_distro(&distros, "Fedora").unwrap_err();
        assert!(
            missing.contains("Fedora") && missing.contains("Debian"),
            "{missing}"
        );
        let wsl1 = choose_distro(&distros, "Debian").unwrap_err();
        assert!(wsl1.contains("WSL2"), "{wsl1}");
        let docker = choose_distro(&distros, "docker-desktop").unwrap_err();
        assert!(docker.contains("Docker Desktop"), "{docker}");
    }

    #[test]
    fn a_default_that_is_docker_desktop_or_missing_is_refused() {
        let docker_default =
            parse_distros("  NAME STATE VERSION\n* docker-desktop Running 2\n  Ubuntu Stopped 2\n");
        assert!(choose_distro(&docker_default, "").is_err());
        assert_eq!(
            choose_distro(&docker_default, "Ubuntu").unwrap().name,
            "Ubuntu"
        );
        assert!(choose_distro(&[], "").is_err());
    }

    #[test]
    fn distro_names_are_checked_before_they_reach_a_command() {
        for good in ["Ubuntu", "Ubuntu-24.04", "my_distro"] {
            assert!(valid_distro(good).is_ok(), "{good}");
        }
        for bad in ["", "-d", "a b", ".x", "a;b", "a/b", "$(x)"] {
            assert!(valid_distro(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn drive_paths_map_under_mnt_with_a_lower_case_letter() {
        let map = |path: &str| windows_to_wsl_path(path, "Ubuntu");
        assert_eq!(map(r"C:\Models\Qwen").unwrap(), "/mnt/c/Models/Qwen");
        assert_eq!(map("D:/AI-Models/x").unwrap(), "/mnt/d/AI-Models/x");
        assert_eq!(map(r"f:\a\b\").unwrap(), "/mnt/f/a/b");
        assert_eq!(map(r"C:\").unwrap(), "/mnt/c");
        assert_eq!(map("C:").unwrap(), "/mnt/c");
        assert_eq!(map(r"C:\a\.\b").unwrap(), "/mnt/c/a/b");
        assert_eq!(map(r"\\?\C:\Models\x").unwrap(), "/mnt/c/Models/x");
    }

    #[test]
    fn spaces_stay_in_the_one_argument() {
        assert_eq!(
            windows_to_wsl_path(r"D:\My Models\Qwen 3 (4B)", "Ubuntu").unwrap(),
            "/mnt/d/My Models/Qwen 3 (4B)"
        );
    }

    #[test]
    fn network_paths_are_refused_with_a_reason() {
        for path in [
            r"\\server\share\models",
            r"\\?\UNC\server\share\models",
            "//server/share",
            r"\\wsl.example\x",
        ] {
            let error = windows_to_wsl_path(path, "Ubuntu").unwrap_err();
            assert!(error.contains("network path"), "{path}: {error}");
        }
    }

    #[test]
    fn wsl_shares_map_to_the_path_inside_the_distro() {
        let map = |path: &str, distro: &str| windows_to_wsl_path(path, distro);
        assert_eq!(
            map(r"\\wsl$\Ubuntu\home\me\models\x", "Ubuntu").unwrap(),
            "/home/me/models/x"
        );
        assert_eq!(
            map(r"\\wsl.localhost\Ubuntu\home\me\x", "ubuntu").unwrap(),
            "/home/me/x"
        );
        assert_eq!(map(r"\\wsl$\Ubuntu", "Ubuntu").unwrap(), "/");
        let other = map(r"\\wsl$\Debian\home\me\x", "Ubuntu").unwrap_err();
        assert!(
            other.contains("Debian") && other.contains("Ubuntu"),
            "{other}"
        );
    }

    #[test]
    fn relative_and_odd_paths_are_refused() {
        for path in [
            "",
            "models",
            r"..\models",
            r"C:models",
            r"\models",
            "/home/me",
            r"C:\a\..\b",
            "C:\\a\nb",
        ] {
            assert!(windows_to_wsl_path(path, "Ubuntu").is_err(), "{path:?}");
        }
    }

    #[test]
    fn a_command_points_into_wsl_by_a_slash_or_a_wsl_share() {
        assert_eq!(
            command_in_wsl("/home/me/venv/bin/vllm"),
            Some((None, "/home/me/venv/bin/vllm".into()))
        );
        assert_eq!(
            command_in_wsl(r"\\wsl$\Ubuntu\home\me\venv\bin\vllm"),
            Some((Some("Ubuntu".into()), "/home/me/venv/bin/vllm".into()))
        );
        assert_eq!(
            command_in_wsl(r"\\wsl.localhost\Debian\opt\vllm"),
            Some((Some("Debian".into()), "/opt/vllm".into()))
        );
        for other in [
            "vllm",
            r"C:\tools\vllm.exe",
            r"\\server\share\vllm",
            "",
            "/",
            r"\\wsl$\U",
        ] {
            assert_eq!(command_in_wsl(other), None, "{other:?}");
        }
    }

    fn serve(extra: &[&str]) -> Vec<String> {
        let mut args: Vec<String> = [
            "serve",
            r"D:\AI Models\Qwen",
            "--host",
            "127.0.0.1",
            "--served-model-name",
            "Qwen",
            "--max-model-len",
            "8192",
        ]
        .map(String::from)
        .to_vec();
        args.extend(extra.iter().map(|arg| arg.to_string()));
        args
    }

    const PROGRAM: &str = "/home/me/.local/share/imperium/runtimes/vllm/0.11.0/venv/bin/vllm";

    #[test]
    fn the_wsl_argv_uses_exec_loopback_and_no_shell() {
        let args = serve_args("Ubuntu", PROGRAM, 41000, &serve(&[])).unwrap();
        assert_eq!(
            args,
            [
                "-d",
                "Ubuntu",
                "--exec",
                "env",
                "VLLM_NO_USAGE_STATS=1",
                "DO_NOT_TRACK=1",
                PROGRAM,
                "serve",
                "/mnt/d/AI Models/Qwen",
                "--host",
                "127.0.0.1",
                "--served-model-name",
                "Qwen",
                "--max-model-len",
                "8192",
                "--port",
                "41000",
            ]
        );
        for shell in ["sh", "bash", "-c", "--"] {
            assert!(!args.iter().any(|arg| arg == shell), "{shell}");
        }
        assert!(!args.iter().any(|arg| arg.contains("0.0.0.0")));
    }

    #[test]
    fn a_host_in_the_extra_arguments_cannot_open_the_server_to_the_network() {
        let args = serve_args(
            "Ubuntu",
            PROGRAM,
            41000,
            &serve(&[
                "--host",
                "0.0.0.0",
                "--host=0.0.0.0",
                "--enable-auto-tool-choice",
            ]),
        )
        .unwrap();
        assert!(!args.iter().any(|arg| arg.contains("0.0.0.0")), "{args:?}");
        assert!(args.contains(&"--host=127.0.0.1".to_string()));
        assert!(args.contains(&"--enable-auto-tool-choice".to_string()));
        // No host at all still binds the loopback.
        let bare = serve_args("Ubuntu", PROGRAM, 1, &["serve".into(), r"C:\m".into()]).unwrap();
        assert_eq!(
            &bare[bare.len() - 4..],
            ["--host", "127.0.0.1", "--port", "1"]
        );
    }

    #[test]
    fn the_wsl_argv_refuses_what_it_cannot_translate_or_trust() {
        let unc = serve_args(
            "Ubuntu",
            PROGRAM,
            1,
            &["serve".into(), r"\\nas\models\x".into()],
        );
        assert!(unc.unwrap_err().contains("network path"));
        assert!(serve_args("Ubuntu", "vllm", 1, &serve(&[])).is_err());
        assert!(serve_args("Ubuntu", "/a/../b", 1, &serve(&[])).is_err());
        assert!(serve_args("-d", PROGRAM, 1, &serve(&[])).is_err());
        assert!(serve_args("Ubuntu", PROGRAM, 1, &[]).is_err());
        assert!(serve_args("Ubuntu", PROGRAM, 1, &["run".into(), "x".into()]).is_err());
    }

    #[test]
    fn the_stop_pattern_names_the_program_and_the_port() {
        let pattern = stop_pattern("/home/me/.local/venv/bin/vllm", 41000);
        assert_eq!(
            pattern,
            r"/home/me/\.local/venv/bin/vllm serve .* --port 41000$"
        );
        assert_ne!(
            pattern,
            stop_pattern("/home/me/.local/venv/bin/vllm", 41001)
        );
    }

    #[test]
    fn exec_args_have_no_shell() {
        assert_eq!(
            exec_args("Ubuntu", &["uv", "--version"]),
            ["-d", "Ubuntu", "--exec", "uv", "--version"]
        );
    }

    #[test]
    fn wslconfig_is_read_from_the_wsl2_section() {
        let config = parse_wslconfig(
            "# comment\n[wsl2]\nMemory=8GB\nlocalhostForwarding = false ; off\n\
             networkingMode=NAT\n[experimental]\nlocalhostForwarding=true\n",
        );
        assert_eq!(config.localhost_forwarding, Some(false));
        assert_eq!(config.networking_mode.as_deref(), Some("nat"));
        assert_eq!(parse_wslconfig(""), WslConfig::default());
        // Outside [wsl2] nothing counts.
        assert_eq!(
            parse_wslconfig("localhostForwarding=false\n"),
            WslConfig::default()
        );
    }

    #[test]
    fn forwarding_off_is_reported_with_the_setting_named() {
        let off = parse_wslconfig("[wsl2]\nlocalhostForwarding=false\n");
        let problem = forwarding_problem(&off).unwrap();
        assert!(problem.contains("localhostForwarding=false"), "{problem}");
        assert!(problem.contains(".wslconfig"), "{problem}");
        let bridged = parse_wslconfig("[wsl2]\nnetworkingMode=bridged\n");
        assert!(
            forwarding_problem(&bridged)
                .unwrap()
                .contains("networkingMode=bridged")
        );
        // The defaults, an explicit true, and mirrored networking are fine.
        for fine in [
            "",
            "[wsl2]\nlocalhostForwarding=true\n",
            "[wsl2]\nnetworkingMode=mirrored\nlocalhostForwarding=false\n",
            "[wsl2]\nnetworkingMode=nat\n",
        ] {
            assert_eq!(forwarding_problem(&parse_wslconfig(fine)), None, "{fine:?}");
        }
    }
}
