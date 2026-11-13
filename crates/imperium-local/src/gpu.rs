//! GPUs other than NVIDIA's: AMD, Intel and Apple Silicon.
//!
//! Each vendor's tool is spawned without a shell and given a few seconds ([`run_tool_for`]).
//! A tool that is not installed ([`Tool::Missing`]) is remembered for the life of the process so
//! no refresh spawns it again; one that fails or hangs is retried, but given up on after
//! [`MAX_FAILURES`] failures in a row, so a broken driver cannot add its timeout to every
//! refresh. Either way the probe gives no GPUs, never an error. On Linux the kernel's `sysfs`
//! is read when the vendor tool gives nothing. Parsing is separate from reading so tests run on
//! sample output:
//!
//! - AMD (Linux): `rocm-smi --json`, else `sysfs` (`amdgpu`'s `mem_info_vram_*`).
//! - Intel (Linux): `xpu-smi` (JSON), else `sysfs` (`lmem_*` of a card with local memory; an
//!   integrated GPU has none of its own and is not listed).
//! - Apple Silicon: `sysctl` for the chip's name and the unified memory, which the GPU shares
//!   with the CPU.
//!
//! The formats are written from the vendors' documentation, not captured from hardware. Every
//! number is range-checked, and a GPU whose memory in use is not reported has it unknown
//! ([`Reading::vram_used_bytes`] is `None`), never zero.
//!
//! NVIDIA stays with `nvidia-smi` in [`crate::monitor`] and [`crate::hardware`].

use std::io::ErrorKind;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

const MIB: u64 = 1024 * 1024;
/// The most memory a GPU is believed to have; a larger figure is a parsing error.
const MAX_MEMORY_BYTES: f64 = 8.0 * 1024.0 * 1024.0 * 1024.0 * 1024.0;
/// Most output read from a tool.
const MAX_OUTPUT_BYTES: u64 = 1024 * 1024;
/// Longest a vendor tool may take before it is given up on and killed.
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(10);
/// Failures in a row (not counting "not installed") after which a tool is not run again.
pub const MAX_FAILURES: u32 = 3;
const SYSFS_DRM: &str = "/sys/class/drm";

/// One GPU as a vendor tool or the kernel reports it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reading {
    pub name: String,
    /// `amd`, `intel` or `apple`.
    pub vendor: &'static str,
    pub vram_total_bytes: u64,
    /// `None` when the tool does not say.
    pub vram_used_bytes: Option<u64>,
    pub utilization_percent: Option<f32>,
    pub temperature_c: Option<f32>,
    pub driver: Option<String>,
}

impl Reading {
    /// Memory free for planning. When the memory in use is unknown, half the total: neither all
    /// of it nor none.
    pub fn vram_free_bytes(&self) -> u64 {
        match self.vram_used_bytes {
            Some(used) => self.vram_total_bytes.saturating_sub(used),
            None => self.vram_total_bytes / 2,
        }
    }
}

/// How a tool run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tool {
    /// It exited successfully; this is what it printed (at most [`MAX_OUTPUT_BYTES`]).
    Ran(String),
    /// The program is not installed.
    Missing,
    /// It could not start for another reason, failed, or took too long.
    Failed,
}

/// Runs `program` with `args`, without a shell; `None` when it is missing, fails, or takes longer
/// than [`TOOL_TIMEOUT`] (it is then killed).
pub fn run_tool(program: &str, args: &[&str]) -> Option<String> {
    match run_tool_for(program, args, TOOL_TIMEOUT) {
        Tool::Ran(text) => Some(text),
        Tool::Missing | Tool::Failed => None,
    }
}

pub(crate) fn run_tool_for(program: &str, args: &[&str], timeout: Duration) -> Tool {
    let mut command = Command::new(program);
    command.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: the daemon has no console, so a child would otherwise open one.
        command.creation_flags(0x0800_0000);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == ErrorKind::NotFound => return Tool::Missing,
        Err(_) => return Tool::Failed,
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Tool::Failed;
    };
    // Read on a thread so a full pipe cannot stop the child, and give up on a hung driver.
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let mut limited = std::io::Read::take(stdout, MAX_OUTPUT_BYTES);
        let _ = std::io::Read::read_to_string(&mut limited, &mut text);
        text
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Tool::Failed;
            }
        }
    };
    match reader.join() {
        Ok(text) if status.success() => Tool::Ran(text),
        _ => Tool::Failed,
    }
}

/// What the process has learned about one vendor tool.
struct ToolState {
    missing: AtomicBool,
    failures: AtomicU32,
}

impl ToolState {
    const fn new() -> Self {
        ToolState {
            missing: AtomicBool::new(false),
            failures: AtomicU32::new(0),
        }
    }

    /// Runs the tool unless it is known to be missing or has failed too often.
    fn run(&self, program: &str, args: &[&str]) -> Option<String> {
        if self.missing.load(Ordering::Relaxed)
            || self.failures.load(Ordering::Relaxed) >= MAX_FAILURES
        {
            return None;
        }
        match run_tool_for(program, args, TOOL_TIMEOUT) {
            Tool::Ran(text) => {
                self.failures.store(0, Ordering::Relaxed);
                Some(text)
            }
            Tool::Missing => {
                self.missing.store(true, Ordering::Relaxed);
                None
            }
            Tool::Failed => {
                self.failures.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }
}

// Process-wide, so the probes made on every refresh do not spawn an absent tool again.
static ROCM_SMI: ToolState = ToolState::new();
static XPU_SMI: ToolState = ToolState::new();
/// The Apple chip's name and the machine's memory, which do not change while running.
static APPLE: OnceLock<(Option<String>, Option<u64>)> = OnceLock::new();

/// Reads the AMD, Intel and Apple GPUs.
#[derive(Debug, Default)]
pub struct VendorProbe;

impl VendorProbe {
    /// Every AMD, Intel and Apple GPU now. `ram_total_bytes` and `ram_used_bytes` are the
    /// machine's, which Apple Silicon's GPU shares. AMD and Intel GPUs are read on Linux only.
    pub fn read(&mut self, ram_total_bytes: u64, ram_used_bytes: u64) -> Vec<Reading> {
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            return apple_gpu(ram_total_bytes, ram_used_bytes)
                .into_iter()
                .collect();
        }
        if !cfg!(target_os = "linux") {
            return Vec::new();
        }
        let mut gpus = amd();
        gpus.extend(intel());
        gpus
    }
}

fn amd() -> Vec<Reading> {
    if let Some(json) = ROCM_SMI.run(
        "rocm-smi",
        &[
            "--showproductname",
            "--showmeminfo",
            "vram",
            "--showuse",
            "--showtemp",
            "--showdriverversion",
            "--json",
        ],
    ) {
        let gpus = parse_rocm_smi(&json);
        if !gpus.is_empty() {
            return gpus;
        }
    }
    sysfs_gpus(Path::new(SYSFS_DRM))
        .into_iter()
        .filter(|gpu| gpu.vendor == "amd")
        .collect()
}

fn intel() -> Vec<Reading> {
    if let Some(json) = XPU_SMI.run("xpu-smi", &["discovery", "-j"]) {
        let mut gpus = parse_xpu_smi_discovery(&json);
        if !gpus.is_empty() {
            for (id, gpu) in &mut gpus {
                if let Some(stats) = XPU_SMI.run("xpu-smi", &["stats", "-d", &id.to_string(), "-j"])
                {
                    apply_xpu_smi_stats(gpu, &stats);
                }
            }
            return gpus.into_iter().map(|(_, gpu)| gpu).collect();
        }
    }
    sysfs_gpus(Path::new(SYSFS_DRM))
        .into_iter()
        .filter(|gpu| gpu.vendor == "intel")
        .collect()
}

fn apple_gpu(ram_total_bytes: u64, ram_used_bytes: u64) -> Option<Reading> {
    let (name, memsize) = APPLE.get_or_init(|| {
        (
            run_tool("sysctl", &["-n", "machdep.cpu.brand_string"])
                .map(|text| text.trim().to_string())
                .filter(|name| !name.is_empty()),
            run_tool("sysctl", &["-n", "hw.memsize"]).and_then(|text| parse_memsize(&text)),
        )
    });
    let total = memsize.unwrap_or(ram_total_bytes);
    Some(Reading {
        name: format!(
            "{} (unified memory)",
            name.as_deref().unwrap_or("Apple GPU")
        ),
        vendor: "apple",
        vram_total_bytes: total,
        vram_used_bytes: Some(ram_used_bytes.min(total)),
        ..Reading::default()
    })
}

/// `sysctl -n hw.memsize`: the machine's memory in bytes.
pub fn parse_memsize(text: &str) -> Option<u64> {
    text.trim()
        .parse::<u64>()
        .ok()
        .filter(|bytes| *bytes > 0 && (*bytes as f64) <= MAX_MEMORY_BYTES)
}

/// A JSON value that is a finite number or a string holding one.
fn number(value: &Value) -> Option<f64> {
    let parsed = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    };
    parsed.filter(|n| n.is_finite())
}

/// A memory size: a finite, non-negative number no larger than [`MAX_MEMORY_BYTES`].
fn bytes(value: &Value) -> Option<u64> {
    number(value)
        .filter(|n| (0.0..=MAX_MEMORY_BYTES).contains(n))
        .map(|n| n as u64)
}

/// A share of 0 to 100.
fn percent(value: &Value) -> Option<f32> {
    number(value)
        .filter(|n| (0.0..=100.0).contains(n))
        .map(|n| n as f32)
}

/// A temperature in degrees Celsius a GPU could plausibly report.
fn celsius(value: f64) -> Option<f32> {
    (-50.0..=200.0).contains(&value).then_some(value as f32)
}

/// `rocm-smi --json`: an object with one entry per card (`card0`, ...) and a `system` entry.
/// Cards without a memory total are left out.
pub fn parse_rocm_smi(json: &str) -> Vec<Reading> {
    let Ok(Value::Object(root)) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let driver = root
        .get("system")
        .and_then(|system| system.get("Driver version"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut cards: Vec<(&String, &Value)> = root
        .iter()
        .filter(|(key, _)| key.starts_with("card"))
        .collect();
    cards.sort_by_key(|(key, _)| key[4..].parse::<u32>().unwrap_or(u32::MAX));
    cards
        .into_iter()
        .filter_map(|(_, card)| {
            let total = card.get("VRAM Total Memory (B)").and_then(bytes)?;
            if total == 0 {
                return None;
            }
            let text = |key: &str| {
                card.get(key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
            };
            let name = text("Card Series")
                .or_else(|| text("Card Model"))
                .or_else(|| text("Card SKU"))
                .unwrap_or("AMD GPU");
            let temperature = ["edge", "junction", "mem"].iter().find_map(|sensor| {
                card.get(format!("Temperature (Sensor {sensor}) (C)"))
                    .and_then(number)
                    .and_then(celsius)
            });
            Some(Reading {
                name: name.to_string(),
                vendor: "amd",
                vram_total_bytes: total,
                // A figure past the total is no figure.
                vram_used_bytes: card
                    .get("VRAM Total Used Memory (B)")
                    .and_then(bytes)
                    .filter(|used| *used <= total),
                utilization_percent: card.get("GPU use (%)").and_then(percent),
                temperature_c: temperature,
                driver: driver.clone(),
            })
        })
        .collect()
}

/// `xpu-smi discovery -j`: `(device id, GPU)` for each device with memory of its own. The memory
/// in use is unknown until [`apply_xpu_smi_stats`].
pub fn parse_xpu_smi_discovery(json: &str) -> Vec<(u32, Reading)> {
    let Ok(root) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let Some(devices) = root.get("device_list").and_then(Value::as_array) else {
        return Vec::new();
    };
    devices
        .iter()
        .filter_map(|device| {
            let id = device
                .get("device_id")
                .and_then(number)
                .filter(|id| (0.0..=f64::from(u32::MAX)).contains(id))
                .map(|id| id as u32)?;
            let total = device.get("memory_physical_size_byte").and_then(bytes)?;
            if total == 0 {
                return None;
            }
            Some((
                id,
                Reading {
                    name: device
                        .get("device_name")
                        .and_then(Value::as_str)
                        .unwrap_or("Intel GPU")
                        .to_string(),
                    vendor: "intel",
                    vram_total_bytes: total,
                    driver: device
                        .get("driver_version")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    ..Reading::default()
                },
            ))
        })
        .collect()
}

/// `xpu-smi stats -d <id> -j`: fills in the memory in use (reported in MiB), utilisation and
/// temperature when the device reports them.
pub fn apply_xpu_smi_stats(gpu: &mut Reading, json: &str) {
    let Ok(root) = serde_json::from_str::<Value>(json) else {
        return;
    };
    let Some(metrics) = root.get("device_level").and_then(Value::as_array) else {
        return;
    };
    for metric in metrics {
        let (Some(kind), Some(value)) = (
            metric.get("metrics_type").and_then(Value::as_str),
            metric.get("value").and_then(number),
        ) else {
            continue;
        };
        match kind {
            "XPUM_STATS_MEMORY_USED" => {
                let used = value.max(0.0) * MIB as f64;
                if used <= gpu.vram_total_bytes as f64 {
                    gpu.vram_used_bytes = Some(used as u64);
                }
            }
            "XPUM_STATS_GPU_UTILIZATION" if (0.0..=100.0).contains(&value) => {
                gpu.utilization_percent = Some(value as f32);
            }
            "XPUM_STATS_GPU_CORE_TEMPERATURE" => gpu.temperature_c = celsius(value),
            _ => {}
        }
    }
}

/// GPUs with their own memory under `root` (`/sys/class/drm`): AMD (vendor `0x1002`) and Intel
/// (`0x8086`) cards. Connectors (`card0-DP-1`) and NVIDIA cards are skipped.
pub fn sysfs_gpus(root: &Path) -> Vec<Reading> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut cards: Vec<(u32, std::path::PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let index = name.strip_prefix("card")?.parse::<u32>().ok()?;
            Some((index, entry.path()))
        })
        .collect();
    cards.sort_by_key(|(index, _)| *index);
    cards
        .into_iter()
        .filter_map(|(_, path)| sysfs_card(&path))
        .collect()
}

fn read_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn read_number(path: &Path) -> Option<u64> {
    read_text(path)?.parse().ok()
}

fn read_memory(path: &Path) -> Option<u64> {
    read_number(path).filter(|n| (*n as f64) <= MAX_MEMORY_BYTES)
}

fn sysfs_card(card: &Path) -> Option<Reading> {
    let device = card.join("device");
    let vendor = match read_text(&device.join("vendor"))?
        .to_ascii_lowercase()
        .as_str()
    {
        "0x1002" => "amd",
        "0x8086" => "intel",
        _ => return None,
    };
    // amdgpu and the kernel drivers that follow its names; i915 exposes local memory on the
    // card itself.
    let (total, used) = if let Some(total) = read_memory(&device.join("mem_info_vram_total")) {
        (total, read_memory(&device.join("mem_info_vram_used")))
    } else {
        let total = read_memory(&card.join("lmem_total_bytes"))?;
        let available = read_memory(&card.join("lmem_avail_bytes"));
        (
            total,
            available.map(|available| total.saturating_sub(available)),
        )
    };
    if total == 0 {
        return None;
    }
    let default = if vendor == "amd" {
        "AMD GPU"
    } else {
        "Intel GPU"
    };
    let name = read_text(&device.join("product_name")).unwrap_or_else(|| {
        match read_text(&device.join("device")) {
            Some(id) => format!("{default} {id}"),
            None => default.to_string(),
        }
    });
    let temperature = std::fs::read_dir(device.join("hwmon"))
        .ok()
        .and_then(|dirs| {
            dirs.filter_map(Result::ok)
                .find_map(|dir| read_number(&dir.path().join("temp1_input")))
        })
        .and_then(|milli| celsius(milli as f64 / 1000.0));
    Some(Reading {
        name,
        vendor,
        vram_total_bytes: total,
        vram_used_bytes: used.filter(|used| *used <= total),
        utilization_percent: read_number(&device.join("gpu_busy_percent"))
            .filter(|n| *n <= 100)
            .map(|n| n as f32),
        temperature_c: temperature,
        driver: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const GIB: u64 = 1024 * 1024 * 1024;

    // Output in the shape ROCm's `rocm-smi --json` prints.
    const ROCM_SMI: &str = r#"{
        "card0": {
            "Card Series": "Radeon RX 7900 XTX",
            "Card Model": "0x744c",
            "GPU use (%)": "12",
            "VRAM Total Memory (B)": "25753026560",
            "VRAM Total Used Memory (B)": "1073741824",
            "Temperature (Sensor edge) (C)": "41.0",
            "Temperature (Sensor junction) (C)": "44.0"
        },
        "card1": {
            "Card Series": "",
            "Card Model": "0x1002",
            "VRAM Total Memory (B)": 17163091968,
            "VRAM Total Used Memory (B)": 0,
            "Temperature (Sensor junction) (C)": "50.5"
        },
        "card2": {"Card Series": "Integrated", "VRAM Total Memory (B)": "0"},
        "system": {"Driver version": "6.8.5"}
    }"#;

    #[test]
    fn rocm_smi_cards_become_gpus() {
        let gpus = parse_rocm_smi(ROCM_SMI);
        assert_eq!(gpus.len(), 2, "{gpus:?}");
        assert_eq!(gpus[0].name, "Radeon RX 7900 XTX");
        assert_eq!(gpus[0].vendor, "amd");
        assert_eq!(gpus[0].vram_total_bytes, 25_753_026_560);
        assert_eq!(gpus[0].vram_used_bytes, Some(GIB));
        assert_eq!(gpus[0].vram_free_bytes(), 25_753_026_560 - GIB);
        assert_eq!(gpus[0].utilization_percent, Some(12.0));
        assert_eq!(gpus[0].temperature_c, Some(41.0));
        assert_eq!(gpus[0].driver.as_deref(), Some("6.8.5"));
        // A blank series falls back to the model; numbers may be bare.
        assert_eq!(gpus[1].name, "0x1002");
        assert_eq!(gpus[1].temperature_c, Some(50.5));
        assert_eq!(gpus[1].utilization_percent, None);
    }

    #[test]
    fn rocm_smi_garbage_gives_no_gpus() {
        assert!(parse_rocm_smi("").is_empty());
        assert!(parse_rocm_smi("not json").is_empty());
        assert!(parse_rocm_smi("[1, 2]").is_empty());
        assert!(parse_rocm_smi(r#"{"system": {}}"#).is_empty());
    }

    // Output in the shape Intel's `xpu-smi discovery -j` and `xpu-smi stats -j` print.
    const XPU_DISCOVERY: &str = r#"{"device_list": [
        {"device_id": 0, "device_name": "Intel(R) Arc(TM) A770 Graphics",
         "memory_physical_size_byte": "17071734784", "driver_version": "1.3.29735"},
        {"device_id": 1, "device_name": "Integrated", "memory_physical_size_byte": "0"}
    ]}"#;
    const XPU_STATS: &str = r#"{"device_id": 0, "device_level": [
        {"metrics_type": "XPUM_STATS_GPU_UTILIZATION", "value": 7},
        {"metrics_type": "XPUM_STATS_MEMORY_USED", "value": 2048},
        {"metrics_type": "XPUM_STATS_GPU_CORE_TEMPERATURE", "value": 55.5},
        {"metrics_type": "XPUM_STATS_POWER", "value": 30}
    ]}"#;

    #[test]
    fn xpu_smi_devices_and_stats_become_a_gpu() {
        let mut devices = parse_xpu_smi_discovery(XPU_DISCOVERY);
        assert_eq!(devices.len(), 1);
        let (id, gpu) = &mut devices[0];
        assert_eq!(*id, 0);
        assert_eq!(gpu.name, "Intel(R) Arc(TM) A770 Graphics");
        assert_eq!(gpu.vendor, "intel");
        assert_eq!(gpu.vram_total_bytes, 17_071_734_784);
        // Until the stats say what is in use, half is taken as free: not all, not none.
        assert_eq!(gpu.vram_used_bytes, None);
        assert_eq!(gpu.vram_free_bytes(), 17_071_734_784 / 2);
        apply_xpu_smi_stats(gpu, XPU_STATS);
        assert_eq!(gpu.vram_used_bytes, Some(2048 * MIB));
        assert_eq!(gpu.utilization_percent, Some(7.0));
        assert_eq!(gpu.temperature_c, Some(55.5));
        // Stats that do not parse leave the discovery as it was.
        apply_xpu_smi_stats(gpu, "garbage");
        assert_eq!(gpu.vram_used_bytes, Some(2048 * MIB));
        assert!(parse_xpu_smi_discovery("{}").is_empty());
    }

    #[test]
    fn sysctl_memory_parses() {
        assert_eq!(parse_memsize("68719476736\n"), Some(64 * GIB));
        assert_eq!(parse_memsize("0\n"), None);
        assert_eq!(parse_memsize("sysctl: unknown oid"), None);
    }

    fn card(root: &Path, name: &str, files: &[(&str, &str)]) {
        for (file, text) in files {
            let path = root.join(name).join(file);
            fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
            fs::write(path, text).expect("write");
        }
    }

    #[test]
    fn sysfs_cards_are_read_by_vendor() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        // An AMD card, as amdgpu exposes it.
        card(
            root,
            "card0",
            &[
                ("device/vendor", "0x1002\n"),
                ("device/device", "0x744c\n"),
                ("device/mem_info_vram_total", "25753026560\n"),
                ("device/mem_info_vram_used", "2147483648\n"),
                ("device/gpu_busy_percent", "33\n"),
                ("device/hwmon/hwmon3/temp1_input", "47000\n"),
            ],
        );
        // An Intel card with local memory, and an integrated one without.
        card(
            root,
            "card1",
            &[
                ("device/vendor", "0x8086\n"),
                ("device/product_name", "Intel Arc A380\n"),
                ("lmem_total_bytes", "6442450944\n"),
                ("lmem_avail_bytes", "5368709120\n"),
            ],
        );
        card(root, "card2", &[("device/vendor", "0x8086\n")]);
        // NVIDIA is read through its driver, connectors are not cards.
        card(
            root,
            "card3",
            &[
                ("device/vendor", "0x10de\n"),
                ("device/mem_info_vram_total", "1\n"),
            ],
        );
        card(root, "card0-DP-1", &[("status", "connected\n")]);

        let gpus = sysfs_gpus(root);
        assert_eq!(gpus.len(), 2, "{gpus:?}");
        assert_eq!(gpus[0].vendor, "amd");
        assert_eq!(gpus[0].name, "AMD GPU 0x744c");
        assert_eq!(gpus[0].vram_total_bytes, 25_753_026_560);
        assert_eq!(gpus[0].vram_used_bytes, Some(2 * GIB));
        assert_eq!(gpus[0].utilization_percent, Some(33.0));
        assert_eq!(gpus[0].temperature_c, Some(47.0));
        assert_eq!(gpus[1].vendor, "intel");
        assert_eq!(gpus[1].name, "Intel Arc A380");
        assert_eq!(gpus[1].vram_total_bytes, 6 * GIB);
        assert_eq!(gpus[1].vram_free_bytes(), 5 * GIB);
    }

    #[test]
    fn a_missing_sysfs_gives_no_gpus() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(sysfs_gpus(&dir.path().join("absent")).is_empty());
    }

    #[test]
    fn a_missing_tool_is_told_from_one_that_failed() {
        assert_eq!(run_tool("imperium-no-such-gpu-tool", &[]), None);
        assert_eq!(
            run_tool_for("imperium-no-such-gpu-tool", &[], Duration::from_secs(1)),
            Tool::Missing
        );
    }

    #[test]
    fn absurd_numbers_are_not_believed() {
        let json = r#"{"card0": {
            "Card Series": "Odd",
            "VRAM Total Memory (B)": "25753026560",
            "VRAM Total Used Memory (B)": "99999999999999999",
            "GPU use (%)": "400",
            "Temperature (Sensor edge) (C)": "99999"
        }, "card1": {"VRAM Total Memory (B)": "1e30"}, "card2": {"VRAM Total Memory (B)": "NaN"}}"#;
        let gpus = parse_rocm_smi(json);
        assert_eq!(gpus.len(), 1, "{gpus:?}");
        // Unknown, so only half counts as free.
        assert_eq!(gpus[0].vram_used_bytes, None);
        assert_eq!(gpus[0].vram_free_bytes(), 25_753_026_560 / 2);
        assert_eq!(gpus[0].utilization_percent, None);
        assert_eq!(gpus[0].temperature_c, None);
        assert_eq!(parse_memsize("99999999999999999999"), None);
    }

    #[test]
    fn sysfs_without_a_used_figure_has_it_unknown() {
        let dir = tempfile::tempdir().expect("temp dir");
        card(
            dir.path(),
            "card0",
            &[
                ("device/vendor", "0x1002\n"),
                ("device/mem_info_vram_total", "8589934592\n"),
            ],
        );
        let gpus = sysfs_gpus(dir.path());
        assert_eq!(gpus[0].vram_used_bytes, None);
        assert_eq!(gpus[0].vram_free_bytes(), 4 * GIB);
    }
}
