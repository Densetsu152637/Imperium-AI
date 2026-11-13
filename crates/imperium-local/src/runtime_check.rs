//! Whether a `llama-server` runs on this machine and which devices it sees, in sentences a user
//! can act on: `--version` and `--list-devices` are run (neither loads a model), and their
//! output is read together with the hardware probe, so "llama-server can't see your GPU" is told
//! apart from "this build has no GPU support" and "a library it needs is missing".

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::hardware::Hardware;

/// How long each probe may take; a CUDA build can take seconds to initialise the driver.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Windows' `STATUS_DLL_NOT_FOUND`: the program could not start because a DLL is missing.
const DLL_NOT_FOUND: i32 = 0xC000_0135_u32 as i32;

/// A device `llama-server --list-devices` reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// Such as `CUDA0` or `Vulkan0`.
    pub id: String,
    /// Such as `NVIDIA GeForce RTX 5080 (16275 MiB, 12742 MiB free)`.
    pub description: String,
}

impl Device {
    /// Whether this is a GPU rather than the CPU or a remote (RPC) device.
    pub fn is_gpu(&self) -> bool {
        let id = self.id.to_ascii_uppercase();
        !(id.starts_with("CPU") || id.starts_with("BLAS") || id.starts_with("RPC"))
    }
}

/// What running the server said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// There is no file at the path.
    NotFound,
    /// It could not be started at all (permissions, not an executable).
    SpawnFailed(String),
    /// It did not answer within the timeout.
    TimedOut,
    /// It ran: its exit code and what it printed (stdout and stderr together).
    Ran {
        code: Option<i32>,
        version: String,
        devices: Option<String>,
    },
}

/// The verdict on a `llama-server`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCheck {
    /// The server starts.
    pub runs: bool,
    /// Its `--version` line, such as `version: 6000 (abc123)`.
    pub version: Option<String>,
    pub devices: Vec<Device>,
    /// Whether it sees a GPU; `None` when that could not be told (an old build without
    /// `--list-devices`).
    pub gpu_visible: Option<bool>,
    /// What is wrong and what to do, in one or two sentences; `None` when nothing is.
    pub problem: Option<String>,
    /// How models will run, such as "Models run on the GPU (CUDA0: NVIDIA ...)".
    pub summary: String,
}

/// Runs `server --version` and `server --list-devices`.
pub async fn probe(server: &Path) -> ProbeOutcome {
    if !server.is_file() {
        return ProbeOutcome::NotFound;
    }
    let version = match run(server, "--version").await {
        Ok(output) => output,
        Err(outcome) => return outcome,
    };
    if version.0.is_some_and(|code| code != 0) {
        return ProbeOutcome::Ran {
            code: version.0,
            version: version.1,
            devices: None,
        };
    }
    let devices = run(server, "--list-devices")
        .await
        .ok()
        .filter(|(code, _)| *code == Some(0))
        .map(|(_, text)| text);
    ProbeOutcome::Ran {
        code: version.0,
        version: version.1,
        devices,
    }
}

async fn run(server: &Path, arg: &str) -> Result<(Option<i32>, String), ProbeOutcome> {
    let mut command = tokio::process::Command::new(server);
    command
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::process::library_path(&mut command, server);
    #[cfg(windows)]
    {
        // No console window flashes up when the daemon runs without one.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let child = command
        .spawn()
        .map_err(|error| ProbeOutcome::SpawnFailed(error.to_string()))?;
    match tokio::time::timeout(PROBE_TIMEOUT, child.wait_with_output()).await {
        Err(_) => Err(ProbeOutcome::TimedOut),
        Ok(Err(error)) => Err(ProbeOutcome::SpawnFailed(error.to_string())),
        Ok(Ok(output)) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            Ok((output.status.code(), text))
        }
    }
}

/// Probes `server` and explains the outcome against `hardware`.
pub async fn check(server: &Path, hardware: &Hardware) -> RuntimeCheck {
    let outcome = probe(server).await;
    explain(
        server,
        &outcome,
        hardware,
        has_cuda_backend(server),
        &hardware.suggested_pack(),
    )
}

/// Flags Imperium starts `llama-server` with that older builds lack; a server found on the
/// machine is only used when its `--help` lists them all.
pub const REQUIRED_FLAGS: &[&str] = &[
    "--kv-unified",
    "--kv-unified-per-slot",
    "--no-cache-idle-slots",
    "--cache-ram",
    "--jinja",
    "--device",
];

/// The [`REQUIRED_FLAGS`] that `--help` output does not mention.
pub fn missing_in_help(help: &str) -> Vec<&'static str> {
    REQUIRED_FLAGS
        .iter()
        .copied()
        .filter(|flag| {
            !help
                .split(|c: char| c.is_whitespace() || c == ',')
                .any(|word| word == *flag)
        })
        .collect()
}

/// The [`REQUIRED_FLAGS`] `server` lacks; `None` when `--help` could not be read.
pub async fn missing_flags(server: &Path) -> Option<Vec<&'static str>> {
    let (code, help) = run(server, "--help").await.ok()?;
    (code == Some(0)).then(|| missing_in_help(&help))
}

/// The devices in `--list-devices` output: lines such as `  CUDA0: NVIDIA ... (16275 MiB, ...)`
/// after `Available devices:`.
pub fn parse_devices(text: &str) -> Vec<Device> {
    text.lines()
        .skip_while(|line| !line.trim_start().starts_with("Available devices"))
        .skip(1)
        .filter_map(|line| {
            let (id, description) = line.trim().split_once(": ")?;
            let id = id.trim();
            (!id.is_empty() && !id.contains(' ')).then(|| Device {
                id: id.to_string(),
                description: description.trim().to_string(),
            })
        })
        .collect()
}

/// Whether the server's folder holds a CUDA backend library, so it is a CUDA build.
pub fn has_cuda_backend(server: &Path) -> bool {
    let Some(dir) = server.parent() else {
        return false;
    };
    let candidates = [dir.to_path_buf(), dir.join("..").join("lib")];
    candidates.iter().any(|dir| {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                name.contains("ggml-cuda")
            })
    }) || server
        .to_string_lossy()
        .to_ascii_lowercase()
        .contains("cuda")
}

/// Reads `outcome` for the server at `server` against `hardware`; `cuda_build` says whether it
/// is a CUDA build ([`has_cuda_backend`]) and `suggested_pack` is the pack that suits this
/// machine.
pub fn explain(
    server: &Path,
    outcome: &ProbeOutcome,
    hardware: &Hardware,
    cuda_build: bool,
    suggested_pack: &str,
) -> RuntimeCheck {
    let failed = |problem: String| RuntimeCheck {
        runs: false,
        version: None,
        devices: Vec::new(),
        gpu_visible: None,
        problem: Some(problem),
        summary: "Local GGUF models cannot run until this is fixed.".into(),
    };
    let missing_library = if cuda_build {
        "llama-server could not start because a library it needs is missing — for this CUDA build, \
         most likely the NVIDIA CUDA runtime (cudart/cublas DLLs). Install the CUDA toolkit or \
         runtime that matches the build, or install the runtime pack instead, which brings its own."
            .to_string()
    } else {
        "llama-server could not start because a library it needs is missing. Choose a complete \
         llama.cpp build (with its DLLs next to it), or install the runtime pack."
            .to_string()
    };
    let (code, version, devices) = match outcome {
        ProbeOutcome::NotFound => {
            // The path is shown beside the verdict; it is long, so it is not repeated here.
            return failed(format!(
                "No llama.cpp runtime yet. Install one (llama.cpp's {suggested_pack} build), \
                 or choose a llama-server you already have."
            ));
        }
        ProbeOutcome::SpawnFailed(error) => {
            return failed(format!(
                "{} could not be started ({error}). Choose the llama-server executable itself.",
                server.display()
            ));
        }
        ProbeOutcome::TimedOut => {
            return failed(format!(
                "llama-server did not answer within {} s. It may be the wrong program, or the GPU \
                 driver may be hanging.",
                PROBE_TIMEOUT.as_secs()
            ));
        }
        ProbeOutcome::Ran {
            code,
            version,
            devices,
        } => (*code, version, devices),
    };
    if code == Some(DLL_NOT_FOUND) || mentions_missing_library(version) {
        return failed(missing_library);
    }
    if code.is_some_and(|code| code != 0) {
        let detail = version.lines().last().unwrap_or_default().trim();
        return failed(format!(
            "llama-server exited with code {} when asked for its version{}. It may not be \
             llama-server, or it may be broken.",
            code.unwrap_or_default(),
            if detail.is_empty() {
                String::new()
            } else {
                format!(" ({detail})")
            }
        ));
    }
    let version_line = version
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("version"))
        .map(str::to_string);
    let Some(devices_text) = devices else {
        return RuntimeCheck {
            runs: true,
            version: version_line,
            devices: Vec::new(),
            gpu_visible: None,
            problem: None,
            summary: "llama-server runs; this build cannot list its devices, so whether it uses \
                      the GPU shows once a model loads."
                .into(),
        };
    };
    let devices = parse_devices(devices_text);
    let gpus: Vec<&Device> = devices.iter().filter(|device| device.is_gpu()).collect();
    let has_gpu = !hardware.gpus.is_empty() || hardware.unified_memory;
    let problem = if !gpus.is_empty() || !has_gpu {
        None
    } else if cuda_build {
        Some(
            "llama-server can't see your GPU — the CUDA runtime is missing or the NVIDIA driver \
             is too old for this build, so models would run on the CPU, much slower. Update the \
             NVIDIA driver, or install the runtime pack that suits this machine."
                .to_string(),
        )
    } else {
        Some(format!(
            "This llama-server has no GPU support (a CPU-only build), so models run on the CPU, \
             much slower. Install the {suggested_pack} runtime pack, or choose a GPU build of \
             llama-server."
        ))
    };
    let summary = match gpus.as_slice() {
        [] if has_gpu => "Models will run on the CPU only.".to_string(),
        [] => "No GPU found on this machine; models run on the CPU.".to_string(),
        gpus => format!(
            "Models run on the GPU ({}).",
            gpus.iter()
                .map(|gpu| format!("{}: {}", gpu.id, gpu.description))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    };
    RuntimeCheck {
        runs: true,
        version: version_line,
        gpu_visible: Some(!gpus.is_empty()),
        devices,
        problem,
        summary,
    }
}

fn mentions_missing_library(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("error while loading shared libraries")
        || text.contains("was not found")
        || text.contains("cannot open shared object")
        || text.contains("library not loaded")
}

/// The user's home folder, where llama.cpp builds are often left.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// `llama-server` executables already on this machine: on `PATH`, and in the places llama.cpp
/// builds are usually installed or left on this OS ([`crate::runtime_locations::places`]:
/// package managers, LM Studio's backends, Unsloth's and `llama.cpp` checkouts in fixed
/// folders; checkouts and unpacked archives in the home and Downloads folders are not here, see
/// [`find_unverified`]), read at most one folder level deep.
pub fn find_existing(home: Option<&Path>) -> Vec<PathBuf> {
    use crate::runtime_locations::{Os, expand, places};
    let os = Os::current();
    expand(
        &places(os, home, &|name| std::env::var_os(name)),
        os.server_name(),
    )
}

/// `llama-server` files in `llama.cpp*` and `llama-b*` folders directly in the home and
/// Downloads folders. They are unverified downloads: this only looks at file names, and the
/// result is offered to the user, never probed, run or chosen automatically.
pub fn find_unverified(home: Option<&Path>) -> Vec<PathBuf> {
    use crate::runtime_locations::{Os, expand, unverified_places};
    expand(&unverified_places(home), Os::current().server_name())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::Gpu;

    const LIST: &str = "ggml_cuda_init: found 1 CUDA devices\nAvailable devices:\n  CUDA0: NVIDIA GeForce RTX 5080 (16275 MiB, 12742 MiB free)\n";

    fn machine(gpu: bool) -> Hardware {
        Hardware {
            gpus: if gpu {
                vec![Gpu {
                    name: "NVIDIA GeForce RTX 5080".into(),
                    vendor: "nvidia".into(),
                    vram_total_bytes: 16 << 30,
                    vram_free_bytes: 12 << 30,
                    driver: Some("580.10".into()),
                }]
            } else {
                Vec::new()
            },
            ram_total_bytes: 64 << 30,
            ram_available_bytes: 32 << 30,
            cpu_cores: 16,
            unified_memory: false,
            overridden: false,
            vram_overridden: false,
        }
    }

    fn ran(code: i32, devices: Option<&str>) -> ProbeOutcome {
        ProbeOutcome::Ran {
            code: Some(code),
            version: "version: 6000 (58670d1)\nbuilt with MSVC".into(),
            devices: devices.map(str::to_string),
        }
    }

    fn check(outcome: &ProbeOutcome, gpu: bool, cuda: bool) -> RuntimeCheck {
        explain(
            Path::new("C:/llama/llama-server.exe"),
            outcome,
            &machine(gpu),
            cuda,
            "windows-x64-cuda13",
        )
    }

    #[test]
    fn devices_are_parsed_from_the_listing() {
        let devices = parse_devices(LIST);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, "CUDA0");
        assert!(
            devices[0]
                .description
                .starts_with("NVIDIA GeForce RTX 5080")
        );
        assert!(devices[0].is_gpu());
        assert!(parse_devices("Available devices:\n").is_empty());
    }

    #[test]
    fn a_server_that_sees_the_gpu_is_ready() {
        let verdict = check(&ran(0, Some(LIST)), true, true);
        assert!(verdict.runs);
        assert_eq!(verdict.gpu_visible, Some(true));
        assert_eq!(verdict.problem, None);
        assert_eq!(verdict.version.as_deref(), Some("version: 6000 (58670d1)"));
        assert!(verdict.summary.contains("CUDA0"), "{}", verdict.summary);
    }

    #[test]
    fn a_cuda_build_without_the_gpu_blames_the_cuda_runtime() {
        let verdict = check(&ran(0, Some("Available devices:\n")), true, true);
        assert!(verdict.runs);
        assert_eq!(verdict.gpu_visible, Some(false));
        let problem = verdict.problem.unwrap();
        assert!(problem.contains("can't see your GPU"), "{problem}");
        assert!(problem.contains("CUDA runtime"), "{problem}");
    }

    #[test]
    fn a_cpu_build_on_a_gpu_machine_suggests_the_gpu_pack() {
        let problem = check(&ran(0, Some("Available devices:\n")), true, false)
            .problem
            .unwrap();
        assert!(problem.contains("no GPU support"), "{problem}");
        assert!(problem.contains("windows-x64-cuda13"), "{problem}");
    }

    #[test]
    fn a_machine_without_a_gpu_has_no_problem_running_on_the_cpu() {
        let verdict = check(&ran(0, Some("Available devices:\n")), false, false);
        assert_eq!(verdict.problem, None);
        assert!(verdict.summary.contains("No GPU"), "{}", verdict.summary);
    }

    #[test]
    fn missing_dlls_and_missing_files_are_explained() {
        let dll = check(&ran(DLL_NOT_FOUND, None), true, true);
        assert!(!dll.runs);
        assert!(dll.problem.unwrap().contains("CUDA runtime"));
        let linux = ProbeOutcome::Ran {
            code: Some(127),
            version: "llama-server: error while loading shared libraries: libcudart.so.13".into(),
            devices: None,
        };
        assert!(
            check(&linux, true, true)
                .problem
                .unwrap()
                .contains("library it needs is missing")
        );
        let missing = check(&ProbeOutcome::NotFound, true, true);
        assert!(!missing.runs);
        assert!(
            missing
                .problem
                .unwrap()
                .contains("No llama.cpp runtime yet")
        );
    }

    #[test]
    fn help_without_the_flags_imperium_uses_is_refused() {
        let help = "-kvu,  --kv-unified, -no-kvu, --no-kv-unified\n\
                    --kv-unified-per-slot N   context limit per slot\n\
                    --cache-idle-slots, --no-cache-idle-slots\n\
                    -cram, --cache-ram N\n--jinja\n-dev,  --device <dev1,dev2,..>\n";
        assert!(missing_in_help(help).is_empty());
        let old = "-kvu,  --kv-unified\n-cram, --cache-ram N\n--jinja\n-dev,  --device <dev>\n";
        assert_eq!(
            missing_in_help(old),
            ["--kv-unified-per-slot", "--no-cache-idle-slots"]
        );
    }

    #[test]
    fn an_old_build_without_device_listing_still_runs() {
        let verdict = check(&ran(0, None), true, true);
        assert!(verdict.runs);
        assert_eq!(verdict.gpu_visible, None);
        assert_eq!(verdict.problem, None);
    }

    #[tokio::test]
    async fn probing_a_missing_file_reports_not_found() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            probe(&dir.path().join("llama-server")).await,
            ProbeOutcome::NotFound
        );
        let fake = dir.path().join("llama-server.exe");
        std::fs::write(&fake, "not a program").unwrap();
        assert!(matches!(probe(&fake).await, ProbeOutcome::SpawnFailed(_)));
        assert!(find_existing(Some(dir.path())).iter().all(|p| p != &fake));
    }
}
