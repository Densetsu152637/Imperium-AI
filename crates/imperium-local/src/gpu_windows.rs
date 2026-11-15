//! AMD and Intel GPUs, and per-process GPU memory, on Windows.
//!
//! Neither vendor ships a command-line tool the way NVIDIA's `nvidia-smi` is, and reading
//! DXGI needs `unsafe` (forbidden in this workspace), so one PowerShell run asks Windows itself:
//!
//! - `Get-CimInstance Win32_VideoController`: each adapter's name, PCI id (which gives the
//!   vendor) and driver version. Its `AdapterRAM` is a 32-bit field that saturates at just under
//!   4 GiB, so the real size comes from the display class key's `HardwareInformation.qwMemorySize`
//!   (matched by the adapter's name); a saturated `AdapterRAM` with no registry size means the
//!   adapter is not listed rather than listed with a wrong size.
//! - `Get-Counter '\GPU Adapter Memory(*)\Dedicated Usage'`: dedicated memory in use per adapter,
//!   and `'\GPU Process Memory(*)\Dedicated Usage'`: per process (the performance counters behind
//!   Task Manager's GPU columns).
//!
//! Accuracy limits, all failing to "unknown" rather than to a guess:
//!
//! - The counters are named by adapter LUID, which nothing here maps to a vendor. An adapter's
//!   memory in use is therefore filled in only when it is the machine's only PCI display adapter
//!   (laptops with an integrated and a discrete GPU, or two cards, leave it unknown).
//! - A process's figure is its dedicated memory on every adapter together, not one vendor's; it
//!   is used only for a process `nvidia-smi` has no figure for (it gives none under WDDM).
//! - Counter paths are localised: on a Windows in another language `Get-Counter` finds no
//!   English counter and no memory in use or per-process figure is given.
//! - Utilisation and temperature are not read.
//! - An Intel adapter is listed only when its name says it has memory of its own (Arc, Flex,
//!   Data Center GPU): an integrated one shares system memory and, as on Linux, is not listed.
//!
//! Parsing ([`parse_snapshot`]) is separate from running so tests run on sample output. The
//! script is run at most every [`MIN_INTERVAL`] and shares the tool-failure bookkeeping of the
//! other vendor tools.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::gpu::{Reading, ToolState};

/// The most memory a GPU is believed to have; a larger figure is a parsing error.
const MAX_MEMORY_BYTES: f64 = 8.0 * 1024.0 * 1024.0 * 1024.0 * 1024.0;
/// `Win32_VideoController.AdapterRAM` is 32 bits wide and saturates here (4 GiB less 1 MiB).
const ADAPTER_RAM_SATURATED: u64 = 4_293_918_720;
/// Most adapters, registry entries and counter samples read.
const MAX_ADAPTERS: usize = 16;
const MAX_COUNTERS: usize = 65_536;
/// The script is not run again sooner than this; the monitor refreshes more often.
pub const MIN_INTERVAL: Duration = Duration::from_secs(5);

/// The PowerShell the probe runs: one compact JSON object with `adapters`, `memory` and
/// `counters`. Single quotes only, so the argument survives Windows command-line quoting.
pub const SCRIPT: &str = "$ErrorActionPreference='SilentlyContinue'; \
$a=@(Get-CimInstance Win32_VideoController | Select-Object Name,PNPDeviceID,AdapterRAM,DriverVersion); \
$m=@(Get-ItemProperty -Path 'HKLM:\\SYSTEM\\CurrentControlSet\\Control\\Class\\{4d36e968-e325-11cd-be10-08002be10318}\\0*' | \
Where-Object { $_.'HardwareInformation.qwMemorySize' } | ForEach-Object { $v=$_.'HardwareInformation.qwMemorySize'; \
if ($v -is [byte[]]) { $v=[BitConverter]::ToUInt64([byte[]]($v + @(0,0,0,0,0,0,0,0))[0..7],0) }; \
[pscustomobject]@{Desc=$_.DriverDesc;Bytes=[uint64]$v} }); \
$c=@((Get-Counter -Counter '\\GPU Adapter Memory(*)\\Dedicated Usage','\\GPU Process Memory(*)\\Dedicated Usage').CounterSamples | \
Select-Object Path,CookedValue); \
ConvertTo-Json -Compress -Depth 4 -InputObject @{adapters=$a;memory=$m;counters=$c}";

/// What one run of [`SCRIPT`] says.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// AMD and Intel adapters with memory of their own. NVIDIA's are read by `nvidia-smi`.
    pub gpus: Vec<Reading>,
    /// `(pid, bytes)` of dedicated GPU memory per process, on every adapter together.
    pub process_vram: Vec<(u32, u64)>,
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

/// A memory size: finite, non-negative and no larger than [`MAX_MEMORY_BYTES`].
fn bytes(value: &Value) -> Option<u64> {
    number(value)
        .filter(|n| (0.0..=MAX_MEMORY_BYTES).contains(n))
        .map(|n| n as u64)
}

/// The entries of a JSON array, or the one entry PowerShell wrote as a bare object.
fn entries(value: Option<&Value>, limit: usize) -> Vec<&Value> {
    match value {
        Some(Value::Array(items)) => items.iter().take(limit).collect(),
        Some(object @ Value::Object(_)) => vec![object],
        _ => Vec::new(),
    }
}

/// What a performance counter path names.
#[derive(Debug, PartialEq, Eq)]
enum Counter<'a> {
    /// `\\host\gpu adapter memory(<luid instance>)\dedicated usage`
    Adapter(&'a str),
    /// `\\host\gpu process memory(pid_<n>_<luid instance>)\dedicated usage`
    Process(u32),
}

/// Reads a (lower-case) counter path; `None` for any other counter.
fn counter(path: &str) -> Option<Counter<'_>> {
    let open = path.find('(')?;
    let close = path.rfind(")\\")?;
    if close < open || &path[close + 2..] != "dedicated usage" {
        return None;
    }
    let object = &path[..open];
    let instance = &path[open + 1..close];
    if object.ends_with("gpu adapter memory") {
        Some(Counter::Adapter(instance))
    } else if object.ends_with("gpu process memory") {
        let pid = instance.strip_prefix("pid_")?.split('_').next()?;
        Some(Counter::Process(pid.parse().ok()?))
    } else {
        None
    }
}

/// Whether an Intel adapter's name says it has memory of its own.
fn intel_has_own_memory(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["arc", "flex", "data center gpu"]
        .iter()
        .any(|word| name.contains(word))
}

/// The output of [`SCRIPT`]. Anything that is not understood gives an empty [`Snapshot`].
pub fn parse_snapshot(json: &str) -> Snapshot {
    let Ok(root) = serde_json::from_str::<Value>(json) else {
        return Snapshot::default();
    };
    let adapters = entries(root.get("adapters"), MAX_ADAPTERS);
    let registry: HashMap<String, u64> = entries(root.get("memory"), MAX_ADAPTERS)
        .into_iter()
        .filter_map(|entry| {
            Some((
                entry.get("Desc")?.as_str()?.trim().to_string(),
                entry.get("Bytes").and_then(bytes)?,
            ))
        })
        .collect();

    // Dedicated usage per adapter instance and per process.
    let mut adapter_usage: HashMap<&str, u64> = HashMap::new();
    let mut process_usage: HashMap<u32, u64> = HashMap::new();
    let samples = entries(root.get("counters"), MAX_COUNTERS);
    let lowered: Vec<(String, u64)> = samples
        .iter()
        .filter_map(|sample| {
            Some((
                sample.get("Path")?.as_str()?.to_ascii_lowercase(),
                sample.get("CookedValue").and_then(bytes)?,
            ))
        })
        .collect();
    for (path, value) in &lowered {
        match counter(path) {
            Some(Counter::Adapter(instance)) => {
                let luid = instance.split("_phys_").next().unwrap_or(instance);
                let entry = adapter_usage.entry(luid).or_insert(0);
                *entry = entry.saturating_add(*value);
            }
            Some(Counter::Process(pid)) => {
                let entry = process_usage.entry(pid).or_insert(0);
                *entry = entry.saturating_add(*value);
            }
            None => {}
        }
    }

    // With one display adapter and one adapter counter, the counter is that adapter's.
    let pci_adapters = adapters
        .iter()
        .filter(|adapter| {
            adapter
                .get("PNPDeviceID")
                .and_then(Value::as_str)
                .is_some_and(|id| id.to_ascii_uppercase().starts_with("PCI\\"))
        })
        .count();
    let sole_usage = (pci_adapters == 1 && adapter_usage.len() == 1)
        .then(|| adapter_usage.values().next().copied())
        .flatten();

    let gpus = adapters
        .iter()
        .filter_map(|adapter| {
            let pnp = adapter.get("PNPDeviceID")?.as_str()?.to_ascii_uppercase();
            let vendor = if pnp.contains("VEN_1002") {
                "amd"
            } else if pnp.contains("VEN_8086") {
                "intel"
            } else {
                return None;
            };
            let name = adapter
                .get("Name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())?;
            if vendor == "intel" && !intel_has_own_memory(name) {
                return None;
            }
            let total = registry.get(name).copied().or_else(|| {
                adapter
                    .get("AdapterRAM")
                    .and_then(bytes)
                    .filter(|ram| *ram < ADAPTER_RAM_SATURATED)
            })?;
            if total == 0 {
                return None;
            }
            Some(Reading {
                name: name.to_string(),
                vendor,
                vram_total_bytes: total,
                vram_used_bytes: sole_usage.filter(|used| *used <= total),
                utilization_percent: None,
                temperature_c: None,
                driver: adapter
                    .get("DriverVersion")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect();

    let mut process_vram: Vec<(u32, u64)> = process_usage.into_iter().collect();
    process_vram.sort_unstable();
    Snapshot { gpus, process_vram }
}

static POWERSHELL: ToolState = ToolState::new();
static LAST: Mutex<Option<(Instant, Snapshot)>> = Mutex::new(None);

/// A fresh snapshot, or the last one if it is under [`MIN_INTERVAL`] old. Empty when PowerShell
/// is missing, fails or finds nothing.
pub fn snapshot() -> Snapshot {
    let Ok(mut last) = LAST.lock() else {
        return Snapshot::default();
    };
    if let Some((at, snapshot)) = last.as_ref()
        && at.elapsed() < MIN_INTERVAL
    {
        return snapshot.clone();
    }
    let fresh = POWERSHELL
        .run(
            "powershell.exe",
            &["-NoProfile", "-NonInteractive", "-Command", SCRIPT],
        )
        .map(|json| parse_snapshot(&json))
        .unwrap_or_default();
    *last = Some((Instant::now(), fresh.clone()));
    fresh
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;
    const SINGLE_AMD: &str = include_str!("../tests/fixtures/gpu/windows-gpu-single-amd.json");
    const IGPU_AND_NVIDIA: &str =
        include_str!("../tests/fixtures/gpu/windows-gpu-igpu-and-nvidia.json");
    const INTEL_ARC: &str = include_str!("../tests/fixtures/gpu/windows-gpu-intel-arc.json");

    #[test]
    fn a_single_amd_card_has_its_real_size_and_usage() {
        let snapshot = parse_snapshot(SINGLE_AMD);
        assert_eq!(snapshot.gpus.len(), 1, "{snapshot:?}");
        let gpu = &snapshot.gpus[0];
        assert_eq!(gpu.name, "AMD Radeon RX 7800 XT");
        assert_eq!(gpu.vendor, "amd");
        // The registry's size, not the saturated AdapterRAM.
        assert_eq!(gpu.vram_total_bytes, 17_163_091_968);
        assert_eq!(gpu.vram_used_bytes, Some(2 * GIB));
        assert_eq!(gpu.driver.as_deref(), Some("32.0.21013.1000"));
        assert_eq!(gpu.utilization_percent, None);
        // Only the dedicated counters, summed per process.
        assert_eq!(
            snapshot.process_vram,
            vec![(4120, 1_610_612_736), (9920, 268_435_456)]
        );
    }

    #[test]
    fn two_display_adapters_leave_the_usage_unknown() {
        let snapshot = parse_snapshot(IGPU_AND_NVIDIA);
        // NVIDIA is read by nvidia-smi; the AMD integrated GPU's carve-out is listed.
        assert_eq!(snapshot.gpus.len(), 1, "{snapshot:?}");
        assert_eq!(snapshot.gpus[0].vram_total_bytes, 512 * 1024 * 1024);
        assert_eq!(snapshot.gpus[0].vram_used_bytes, None);
        assert_eq!(snapshot.process_vram, vec![(2288, 3_000_000_000)]);
    }

    #[test]
    fn an_intel_arc_without_a_registry_size_uses_adapter_ram_only_when_it_is_exact() {
        // AdapterRAM is saturated and there is no registry size: not listed, not guessed.
        assert!(parse_snapshot(INTEL_ARC).gpus.is_empty());
        let exact = INTEL_ARC.replace("4293918720", "3221225472");
        let snapshot = parse_snapshot(&exact);
        assert_eq!(snapshot.gpus.len(), 1);
        assert_eq!(snapshot.gpus[0].vendor, "intel");
        assert_eq!(snapshot.gpus[0].vram_total_bytes, 3 * GIB);
        assert_eq!(snapshot.gpus[0].vram_used_bytes, Some(734_003_200));
    }

    #[test]
    fn integrated_intel_graphics_are_not_listed() {
        let json = r#"{"adapters":[{"Name":"Intel(R) UHD Graphics 770","PNPDeviceID":"PCI\\VEN_8086&DEV_4680","AdapterRAM":1073741824,"DriverVersion":"31.0"}],"memory":[],"counters":[]}"#;
        assert!(parse_snapshot(json).gpus.is_empty());
    }

    #[test]
    fn a_single_adapter_may_be_written_as_a_bare_object() {
        let json = r#"{"adapters":{"Name":"AMD Radeon RX 6600","PNPDeviceID":"PCI\\VEN_1002&DEV_73FF","AdapterRAM":2147483648,"DriverVersion":"31.0"},"memory":null,"counters":null}"#;
        let snapshot = parse_snapshot(json);
        assert_eq!(snapshot.gpus.len(), 1);
        assert_eq!(snapshot.gpus[0].vram_total_bytes, 2 * GIB);
        assert_eq!(snapshot.gpus[0].vram_used_bytes, None);
    }

    #[test]
    fn counter_paths_are_told_apart() {
        assert_eq!(
            counter("\\\\pc\\gpu adapter memory(luid_0x0_0x1_phys_0)\\dedicated usage"),
            Some(Counter::Adapter("luid_0x0_0x1_phys_0"))
        );
        assert_eq!(
            counter("\\\\pc\\gpu process memory(pid_77_luid_0x0_0x1_phys_0)\\dedicated usage"),
            Some(Counter::Process(77))
        );
        for other in [
            "\\\\pc\\gpu adapter memory(luid_0x0_0x1_phys_0)\\shared usage",
            "\\\\pc\\gpu engine(pid_77_eng_0)\\utilization percentage",
            "\\\\pc\\gpu process memory(luid_0x0)\\dedicated usage",
            "\\\\pc\\gpu process memory(pid_x_luid)\\dedicated usage",
            "dedicated usage",
            ")\\dedicated usage(",
            "",
        ] {
            assert_eq!(counter(other), None, "{other}");
        }
    }

    #[test]
    fn garbage_gives_an_empty_snapshot() {
        for text in ["", "not json", "[]", "{}", "null", r#"{"adapters": 5}"#] {
            assert_eq!(parse_snapshot(text), Snapshot::default(), "{text}");
        }
    }

    #[test]
    fn absurd_numbers_are_not_believed() {
        let json = r#"{"adapters":[{"Name":"AMD Radeon RX 6600","PNPDeviceID":"PCI\\VEN_1002&DEV_73FF","AdapterRAM":2147483648}],
            "memory":[{"Desc":"AMD Radeon RX 6600","Bytes":1e30}],
            "counters":[{"Path":"\\\\pc\\gpu adapter memory(luid_0x0_0x1_phys_0)\\dedicated usage","CookedValue":99999999999999999999999},
                        {"Path":"\\\\pc\\gpu process memory(pid_5_luid_0x0_0x1_phys_0)\\dedicated usage","CookedValue":-1}]}"#;
        let snapshot = parse_snapshot(json);
        // The oversized registry figure is ignored, so AdapterRAM stands; no usage is believed.
        assert_eq!(snapshot.gpus.len(), 1);
        assert_eq!(snapshot.gpus[0].vram_total_bytes, 2 * GIB);
        assert_eq!(snapshot.gpus[0].vram_used_bytes, None);
        assert!(snapshot.process_vram.is_empty());
    }

    #[test]
    fn the_script_has_no_double_quotes() {
        // It is one command-line argument; double quotes would not survive Windows quoting.
        assert!(!SCRIPT.contains('"'));
    }
}
