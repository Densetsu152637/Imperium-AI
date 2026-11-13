//! GPUs other than NVIDIA's: AMD, Intel and Apple Silicon.
//!
//! Each vendor's tool is spawned without a shell and given a few seconds ([`run_tool`]); a tool
//! that is missing, fails or hangs gives no GPUs, never an error, and is not tried again by the
//! same [`VendorProbe`]. On Linux the kernel's `sysfs` is read when the vendor tool is not
//! installed. Parsing is separate from reading so tests run on captured output:
//!
//! - AMD: `rocm-smi --json`, else `sysfs` (`amdgpu`'s `mem_info_vram_*`).
//! - Intel: `xpu-smi` (JSON), else `sysfs` (`lmem_*` or `mem_info_vram_*` of a card with local
//!   memory; an integrated GPU has none of its own and is not listed).
//! - Apple Silicon: `sysctl` for the chip's name and the unified memory, which the GPU shares
//!   with the CPU.
//!
//! NVIDIA stays with `nvidia-smi` in [`crate::monitor`] and [`crate::hardware`].

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const MIB: u64 = 1024 * 1024;
/// Longest a vendor tool may take before it is given up on and killed.
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(10);
const SYSFS_DRM: &str = "/sys/class/drm";

/// One GPU as a vendor tool or the kernel reports it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reading {
    pub name: String,
    /// `amd`, `intel` or `apple`.
    pub vendor: &'static str,
    pub vram_total_bytes: u64,
    pub vram_used_bytes: u64,
    pub utilization_percent: Option<f32>,
    pub temperature_c: Option<f32>,
    pub driver: Option<String>,
}

impl Reading {
    pub fn vram_free_bytes(&self) -> u64 {
        self.vram_total_bytes.saturating_sub(self.vram_used_bytes)
    }
}

/// Runs `program` with `args`, without a shell; `None` when it is missing, fails, or takes longer
/// than [`TOOL_TIMEOUT`] (it is then killed).
pub fn run_tool(program: &str, args: &[&str]) -> Option<String> {
    run_tool_for(program, args, TOOL_TIMEOUT)
}

pub(crate) fn run_tool_for(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
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
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    // Read on a thread so a full pipe cannot stop the child, and give up on a hung driver.
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut text);
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
                return None;
            }
        }
    };
    let text = reader.join().ok()?;
    status.success().then_some(text)
}

/// Reads the AMD, Intel and Apple GPUs, remembering which tools are absent so that they are not
/// spawned again every few seconds.
#[derive(Debug)]
pub struct VendorProbe {
    rocm_smi: bool,
    xpu_smi: bool,
    /// The chip's name, read once.
    apple: Option<Option<String>>,
}

impl Default for VendorProbe {
    fn default() -> Self {
        VendorProbe {
            rocm_smi: cfg!(not(target_os = "macos")),
            xpu_smi: cfg!(not(target_os = "macos")),
            apple: None,
        }
    }
}

impl VendorProbe {
    /// Every AMD, Intel and Apple GPU now. `ram_total_bytes` and `ram_used_bytes` are the
    /// machine's, which Apple Silicon's GPU shares.
    pub fn read(&mut self, ram_total_bytes: u64, ram_used_bytes: u64) -> Vec<Reading> {
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            return self
                .apple_gpu(ram_total_bytes, ram_used_bytes)
                .into_iter()
                .collect();
        }
        if cfg!(target_os = "macos") {
            return Vec::new();
        }
        let mut gpus = self.amd();
        gpus.extend(self.intel());
        gpus
    }

    fn amd(&mut self) -> Vec<Reading> {
        if self.rocm_smi {
            match run_tool(
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
                Some(json) => {
                    let gpus = parse_rocm_smi(&json);
                    if !gpus.is_empty() {
                        return gpus;
                    }
                }
                None => self.rocm_smi = false,
            }
        }
        sysfs_gpus(Path::new(SYSFS_DRM))
            .into_iter()
            .filter(|gpu| gpu.vendor == "amd")
            .collect()
    }

    fn intel(&mut self) -> Vec<Reading> {
        if self.xpu_smi {
            match run_tool("xpu-smi", &["discovery", "-j"]) {
                Some(json) => {
                    let mut gpus = parse_xpu_smi_discovery(&json);
                    if !gpus.is_empty() {
                        for (id, gpu) in &mut gpus {
                            if let Some(stats) =
                                run_tool("xpu-smi", &["stats", "-d", &id.to_string(), "-j"])
                            {
                                apply_xpu_smi_stats(gpu, &stats);
                            }
                        }
                        return gpus.into_iter().map(|(_, gpu)| gpu).collect();
                    }
                }
                None => self.xpu_smi = false,
            }
        }
        sysfs_gpus(Path::new(SYSFS_DRM))
            .into_iter()
            .filter(|gpu| gpu.vendor == "intel")
            .collect()
    }

    fn apple_gpu(&mut self, ram_total_bytes: u64, ram_used_bytes: u64) -> Option<Reading> {
        let name = self
            .apple
            .get_or_insert_with(|| {
                run_tool("sysctl", &["-n", "machdep.cpu.brand_string"])
                    .map(|text| text.trim().to_string())
                    .filter(|name| !name.is_empty())
            })
            .clone()
            .unwrap_or_else(|| "Apple GPU".into());
        let total = run_tool("sysctl", &["-n", "hw.memsize"])
            .and_then(|text| parse_memsize(&text))
            .unwrap_or(ram_total_bytes);
        Some(Reading {
            name,
            vendor: "apple",
            vram_total_bytes: total,
            vram_used_bytes: ram_used_bytes.min(total),
            ..Reading::default()
        })
    }
}

/// `sysctl -n hw.memsize`: the machine's memory in bytes.
pub fn parse_memsize(text: &str) -> Option<u64> {
    text.trim().parse::<u64>().ok().filter(|bytes| *bytes > 0)
}

/// A JSON value that is a number or a string holding one.
fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn bytes(value: &Value) -> Option<u64> {
    number(value).filter(|n| *n >= 0.0).map(|n| n as u64)
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
            });
            Some(Reading {
                name: name.to_string(),
                vendor: "amd",
                vram_total_bytes: total,
                vram_used_bytes: card
                    .get("VRAM Total Used Memory (B)")
                    .and_then(bytes)
                    .unwrap_or(0),
                utilization_percent: card.get("GPU use (%)").and_then(number).map(|n| n as f32),
                temperature_c: temperature.map(|n| n as f32),
                driver: driver.clone(),
            })
        })
        .collect()
}

/// `xpu-smi discovery -j`: `(device id, GPU)` for each device with memory of its own.
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
                .and_then(|id| u32::try_from(id as u64).ok())?;
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
            "XPUM_STATS_MEMORY_USED" => gpu.vram_used_bytes = (value.max(0.0) as u64) * MIB,
            "XPUM_STATS_GPU_UTILIZATION" => gpu.utilization_percent = Some(value as f32),
            "XPUM_STATS_GPU_CORE_TEMPERATURE" => gpu.temperature_c = Some(value as f32),
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
    let (total, used) = if let Some(total) = read_number(&device.join("mem_info_vram_total")) {
        (
            total,
            read_number(&device.join("mem_info_vram_used")).unwrap_or(0),
        )
    } else {
        let total = read_number(&card.join("lmem_total_bytes"))?;
        let available = read_number(&card.join("lmem_avail_bytes")).unwrap_or(total);
        (total, total.saturating_sub(available))
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
        .map(|milli| milli as f32 / 1000.0);
    Some(Reading {
        name,
        vendor,
        vram_total_bytes: total,
        vram_used_bytes: used.min(total),
        utilization_percent: read_number(&device.join("gpu_busy_percent")).map(|n| n as f32),
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
        assert_eq!(gpus[0].vram_used_bytes, GIB);
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
        assert_eq!(gpu.vram_free_bytes(), 17_071_734_784);
        apply_xpu_smi_stats(gpu, XPU_STATS);
        assert_eq!(gpu.vram_used_bytes, 2048 * MIB);
        assert_eq!(gpu.utilization_percent, Some(7.0));
        assert_eq!(gpu.temperature_c, Some(55.5));
        // Stats that do not parse leave the discovery as it was.
        apply_xpu_smi_stats(gpu, "garbage");
        assert_eq!(gpu.vram_used_bytes, 2048 * MIB);
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
        assert_eq!(gpus[0].vram_used_bytes, 2 * GIB);
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
    fn a_missing_tool_gives_nothing() {
        assert_eq!(run_tool("imperium-no-such-gpu-tool", &[]), None);
    }
}
