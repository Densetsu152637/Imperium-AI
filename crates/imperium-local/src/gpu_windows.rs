//! AMD and Intel GPUs, and per-process GPU memory, on Windows.
//!
//! Neither vendor ships a command-line tool the way NVIDIA's `nvidia-smi` is, and reading
//! DXGI needs `unsafe` (forbidden in this workspace), so PowerShell asks Windows itself, in two
//! scripts run from `%SystemRoot%\System32\WindowsPowerShell\v1.0`:
//!
//! - [`ADAPTER_SCRIPT`]: `Get-CimInstance Win32_VideoController` (name, PCI id, which gives the
//!   vendor, driver version) and the display class registry key's
//!   `HardwareInformation.qwMemorySize`. `AdapterRAM` is a 32-bit field that saturates just under
//!   4 GiB, so the real size comes from the registry (matched by the adapter's name); a saturated
//!   `AdapterRAM` with no registry size means the adapter is not listed rather than listed with a
//!   wrong size. Run once per process and kept: adapters do not change while the daemon runs.
//! - [`COUNTER_SCRIPT`]: the `Win32_PerfFormattedData_GPUPerformanceCounters_GPUAdapterMemory` and
//!   `..._GPUProcessMemory` WMI classes (the counters behind Task Manager's GPU columns), whose
//!   class and property names do not depend on the language of Windows the way `Get-Counter`
//!   paths do. Run at most every [`MIN_INTERVAL`].
//!
//! Cost: a machine with no AMD or Intel PCI display adapter (NVIDIA-only, or none) runs the
//! adapter script once and never spawns PowerShell for the GPU list again. With one, the counter
//! script also runs, at most every 5 s, whenever the GPU list is read (every 30 s by the budget
//! refresh) or the resource monitor wants a process's GPU memory that `nvidia-smi` lacks. A run
//! that fails, is not JSON, or finds no PowerShell is retried after a growing delay (30 s up to
//! 15 min), never given up on for good.
//!
//! Accuracy limits, all failing to "unknown" rather than to a guess:
//!
//! - The counters are named by adapter LUID, which nothing here maps to a vendor. An adapter's
//!   memory in use is therefore filled in only when it is the machine's only PCI display adapter
//!   (laptops with an integrated and a discrete GPU, or two cards, leave it unknown), and never
//!   when it would exceed the adapter's size.
//! - A process's figure is its dedicated memory on every adapter together, not one vendor's; it
//!   is used only for a process `nvidia-smi` has no figure for (it gives none under WDDM).
//! - Utilisation and temperature are not read.
//! - An Intel adapter is listed only when its name is a discrete one ([`intel_has_own_memory`]);
//!   an integrated one, including the Core Ultra "Arc Graphics", shares system memory and, as on
//!   Linux, is not listed.
//! - PowerShell's output is read as bytes and decoded lossily; a non-ASCII adapter name may be
//!   mangled, and one that no longer matches its registry entry falls back to `AdapterRAM`.
//!
//! Parsing ([`parse_adapters`], [`parse_counters`]) and the run schedule ([`Probe`]) are separate
//! from running PowerShell so tests run on sample output and a fake runner.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::gpu::{Reading, TOOL_TIMEOUT, Tool, run_tool_for};

/// The most memory a GPU is believed to have; a larger figure is a parsing error.
const MAX_MEMORY_BYTES: f64 = 8.0 * 1024.0 * 1024.0 * 1024.0 * 1024.0;
/// `Win32_VideoController.AdapterRAM` is 32 bits wide and saturates here (4 GiB less 1 MiB).
const ADAPTER_RAM_SATURATED: u64 = 4_293_918_720;
/// Most adapters and registry entries read.
const MAX_ADAPTERS: usize = 16;
/// Most counter instances read: the tool's 1 MiB output cap holds about this many (each is
/// 60 bytes or more), so more would be a truncated, unparseable document anyway.
const MAX_COUNTERS: usize = 16_384;
/// The counter script is not run again sooner than this; the monitor refreshes more often.
pub const MIN_INTERVAL: Duration = Duration::from_secs(5);
/// The wait after a first failed run; it doubles with each further one up to [`RETRY_MAX`].
const RETRY_BASE: Duration = Duration::from_secs(30);
const RETRY_MAX: Duration = Duration::from_secs(15 * 60);

/// Adapters: one compact JSON object with `adapters` and `memory`. Single quotes only, so the
/// argument survives Windows command-line quoting.
pub const ADAPTER_SCRIPT: &str = "$ErrorActionPreference='SilentlyContinue'; \
$a=@(Get-CimInstance Win32_VideoController | Select-Object Name,PNPDeviceID,AdapterRAM,DriverVersion); \
$m=@(Get-ItemProperty -Path 'HKLM:\\SYSTEM\\CurrentControlSet\\Control\\Class\\{4d36e968-e325-11cd-be10-08002be10318}\\0*' | \
Where-Object { $_.'HardwareInformation.qwMemorySize' } | ForEach-Object { $v=$_.'HardwareInformation.qwMemorySize'; \
if ($v -is [byte[]]) { $v=[BitConverter]::ToUInt64([byte[]]($v + @(0,0,0,0,0,0,0,0))[0..7],0) }; \
[pscustomobject]@{Desc=$_.DriverDesc;Bytes=[uint64]$v} }); \
ConvertTo-Json -Compress -Depth 4 -InputObject @{adapters=$a;memory=$m}";

/// Memory counters: one compact JSON object with `adapter_memory` and `process_memory`, each a
/// list of `{Name, DedicatedUsage}`.
pub const COUNTER_SCRIPT: &str = "$ErrorActionPreference='SilentlyContinue'; \
$a=@(Get-CimInstance Win32_PerfFormattedData_GPUPerformanceCounters_GPUAdapterMemory | Select-Object Name,DedicatedUsage); \
$p=@(Get-CimInstance Win32_PerfFormattedData_GPUPerformanceCounters_GPUProcessMemory | Select-Object Name,DedicatedUsage); \
ConvertTo-Json -Compress -Depth 3 -InputObject @{adapter_memory=$a;process_memory=$p}";

/// The AMD and Intel adapters, as the adapter script says.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Adapters {
    /// AMD and Intel adapters with memory of their own; memory in use not filled in. NVIDIA's are
    /// read by `nvidia-smi`.
    pub gpus: Vec<Reading>,
    /// PCI display adapters of any vendor.
    pub pci_adapters: usize,
    /// Whether an AMD or Intel PCI adapter exists, listed or not.
    pub vendor_present: bool,
}

/// Dedicated memory in use, as the counter script says.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Counters {
    /// Per adapter (LUID), in bytes.
    pub adapters: Vec<u64>,
    /// `(pid, bytes)` per process, on every adapter together.
    pub processes: Vec<(u32, u64)>,
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

/// The JSON object a script printed (a leading byte-order mark is ignored).
fn root(json: &str) -> Option<Value> {
    let value = serde_json::from_str::<Value>(json.trim_start_matches('\u{feff}').trim()).ok()?;
    value.is_object().then_some(value)
}

/// Whether an Intel adapter's name is a discrete one with memory of its own: Arc `A`, `B`, `C`
/// or `S` series cards (`A770`, `B580`, `A370M`, `Pro A60`), Flex, and Data Center GPUs. The
/// integrated "Arc Graphics" and Arc `140V` of Core Ultra processors share system memory.
pub fn intel_has_own_memory(name: &str) -> bool {
    let name = name
        .to_ascii_lowercase()
        .replace("(tm)", " ")
        .replace("(r)", " ");
    if name.contains("data center gpu") {
        return true;
    }
    let tokens: Vec<&str> = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    if tokens.contains(&"flex") {
        return true;
    }
    let Some(arc) = tokens.iter().position(|token| *token == "arc") else {
        return false;
    };
    let mut rest = tokens[arc + 1..].iter();
    let mut next = rest.next();
    if next == Some(&"pro") {
        next = rest.next();
    }
    next.is_some_and(|model| {
        let model: &str = model;
        let model = model.strip_suffix('m').unwrap_or(model);
        let mut chars = model.chars();
        let series = chars
            .next()
            .is_some_and(|c| matches!(c, 'a' | 'b' | 'c' | 's'));
        let digits: String = chars.collect();
        series && (2..=3).contains(&digits.len()) && digits.chars().all(|c| c.is_ascii_digit())
    })
}

/// The adapter script's output; `None` when it is not what the script prints.
pub fn parse_adapters(json: &str) -> Option<Adapters> {
    let root = root(json)?;
    // A list (or the one entry PowerShell wrote bare), not any other value.
    root.get("adapters")
        .filter(|v| v.is_array() || v.is_object())?;
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
    let mut pci_adapters = 0;
    let mut vendor_present = false;
    let mut gpus = Vec::new();
    for adapter in adapters {
        let Some(pnp) = adapter
            .get("PNPDeviceID")
            .and_then(Value::as_str)
            .map(str::to_ascii_uppercase)
            .filter(|pnp| pnp.starts_with("PCI\\"))
        else {
            continue;
        };
        pci_adapters += 1;
        let vendor = if pnp.contains("VEN_1002") {
            "amd"
        } else if pnp.contains("VEN_8086") {
            "intel"
        } else {
            continue;
        };
        vendor_present = true;
        let Some(name) = adapter
            .get("Name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        if vendor == "intel" && !intel_has_own_memory(name) {
            continue;
        }
        let Some(total) = registry.get(name).copied().or_else(|| {
            adapter
                .get("AdapterRAM")
                .and_then(bytes)
                .filter(|ram| *ram < ADAPTER_RAM_SATURATED)
        }) else {
            continue;
        };
        if total == 0 {
            continue;
        }
        gpus.push(Reading {
            name: name.to_string(),
            vendor,
            vram_total_bytes: total,
            vram_used_bytes: None,
            utilization_percent: None,
            temperature_c: None,
            driver: adapter
                .get("DriverVersion")
                .and_then(Value::as_str)
                .map(str::to_string),
        });
    }
    Some(Adapters {
        gpus,
        pci_adapters,
        vendor_present,
    })
}

/// The counter script's output; `None` when it is not what the script prints. Either list may be
/// empty (a Windows without the GPU counters).
pub fn parse_counters(json: &str) -> Option<Counters> {
    let root = root(json)?;
    if root.get("adapter_memory").is_none() && root.get("process_memory").is_none() {
        return None;
    }
    let usage = |entry: &Value| -> Option<(String, u64)> {
        Some((
            entry.get("Name")?.as_str()?.to_ascii_lowercase(),
            entry.get("DedicatedUsage").and_then(bytes)?,
        ))
    };
    // One adapter may appear as several `phys_N` instances; they share a LUID.
    let mut adapters: HashMap<String, u64> = HashMap::new();
    for (name, value) in entries(root.get("adapter_memory"), MAX_COUNTERS)
        .into_iter()
        .filter_map(usage)
    {
        let luid = name.split("_phys_").next().unwrap_or(&name).to_string();
        let total = adapters.entry(luid).or_insert(0);
        *total = total.saturating_add(value);
    }
    let mut processes: HashMap<u32, u64> = HashMap::new();
    for (name, value) in entries(root.get("process_memory"), MAX_COUNTERS)
        .into_iter()
        .filter_map(usage)
    {
        let Some(pid) = name
            .strip_prefix("pid_")
            .and_then(|rest| rest.split('_').next())
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            continue;
        };
        let total = processes.entry(pid).or_insert(0);
        *total = total.saturating_add(value);
    }
    let mut adapters: Vec<u64> = adapters.into_values().collect();
    adapters.sort_unstable();
    let mut processes: Vec<(u32, u64)> = processes.into_iter().collect();
    processes.sort_unstable();
    Some(Counters {
        adapters,
        processes,
    })
}

/// The listed GPUs with their memory in use where it is known: with one PCI display adapter and
/// one adapter counter, the counter is that adapter's (unless it exceeds the adapter's size).
pub fn combine(adapters: &Adapters, counters: Option<&Counters>) -> Vec<Reading> {
    let sole = counters
        .filter(|counters| adapters.pci_adapters == 1 && counters.adapters.len() == 1)
        .map(|counters| counters.adapters[0]);
    adapters
        .gpus
        .iter()
        .map(|gpu| Reading {
            vram_used_bytes: sole.filter(|used| *used <= gpu.vram_total_bytes),
            ..gpu.clone()
        })
        .collect()
}

/// Which script to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    Adapters,
    Counters,
}

/// When a failing script may be tried again.
#[derive(Debug, Default)]
struct Backoff {
    failures: u32,
    retry_at: Option<Instant>,
}

impl Backoff {
    const fn new() -> Self {
        Backoff {
            failures: 0,
            retry_at: None,
        }
    }

    fn ready(&self, now: Instant) -> bool {
        self.retry_at.is_none_or(|at| now >= at)
    }

    fn failed(&mut self, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        let wait = RETRY_BASE
            .saturating_mul(1 << (self.failures - 1).min(10))
            .min(RETRY_MAX);
        self.retry_at = Some(now + wait);
    }

    fn succeeded(&mut self) {
        *self = Backoff::new();
    }
}

/// What has been learned about this machine, and when to run PowerShell again.
#[derive(Debug)]
pub struct Probe {
    adapters: Option<Adapters>,
    adapters_backoff: Backoff,
    counters: Option<(Instant, Counters)>,
    counters_backoff: Backoff,
}

impl Probe {
    pub const fn new() -> Self {
        Probe {
            adapters: None,
            adapters_backoff: Backoff::new(),
            counters: None,
            counters_backoff: Backoff::new(),
        }
    }

    fn ensure_adapters(&mut self, now: Instant, run: &mut dyn FnMut(Script) -> Tool) {
        if self.adapters.is_some() || !self.adapters_backoff.ready(now) {
            return;
        }
        match run(Script::Adapters)
            .into_text()
            .and_then(|text| parse_adapters(&text))
        {
            Some(adapters) => {
                self.adapters = Some(adapters);
                self.adapters_backoff.succeeded();
            }
            None => self.adapters_backoff.failed(now),
        }
    }

    fn ensure_counters(&mut self, now: Instant, run: &mut dyn FnMut(Script) -> Tool) {
        let fresh = self
            .counters
            .as_ref()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) < MIN_INTERVAL);
        if fresh || !self.counters_backoff.ready(now) {
            return;
        }
        match run(Script::Counters)
            .into_text()
            .and_then(|text| parse_counters(&text))
        {
            Some(counters) => {
                self.counters = Some((now, counters));
                self.counters_backoff.succeeded();
            }
            None => {
                // An old figure is not kept past its welcome.
                self.counters = None;
                self.counters_backoff.failed(now);
            }
        }
    }

    /// The AMD and Intel GPUs. A machine without an AMD or Intel PCI adapter never runs the
    /// counter script, and runs the adapter script once.
    pub fn gpus(&mut self, now: Instant, run: &mut dyn FnMut(Script) -> Tool) -> Vec<Reading> {
        self.ensure_adapters(now, run);
        if self.adapters.as_ref().is_none_or(|a| a.gpus.is_empty()) {
            return Vec::new();
        }
        self.ensure_counters(now, run);
        let Some(adapters) = &self.adapters else {
            return Vec::new();
        };
        combine(
            adapters,
            self.counters.as_ref().map(|(_, counters)| counters),
        )
    }

    /// `(pid, bytes)` for those of `pids` with a figure. Runs the counter script (at most every
    /// [`MIN_INTERVAL`]) only when asked about at least one process.
    pub fn process_vram(
        &mut self,
        now: Instant,
        pids: &[u32],
        run: &mut dyn FnMut(Script) -> Tool,
    ) -> Vec<(u32, u64)> {
        if pids.is_empty() {
            return Vec::new();
        }
        self.ensure_counters(now, run);
        self.counters
            .as_ref()
            .map(|(_, counters)| {
                counters
                    .processes
                    .iter()
                    .filter(|(pid, _)| pids.contains(pid))
                    .copied()
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Default for Probe {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool {
    /// What the tool printed, if it ran.
    fn into_text(self) -> Option<String> {
        match self {
            Tool::Ran(text) => Some(text),
            Tool::Missing | Tool::Failed => None,
        }
    }
}

/// `powershell.exe` from the system folder, not whatever the search path finds first.
fn powershell_exe() -> String {
    let system_root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\Windows"));
    system_root
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe")
        .to_string_lossy()
        .into_owned()
}

fn run_script(script: Script) -> Tool {
    let command = match script {
        Script::Adapters => ADAPTER_SCRIPT,
        Script::Counters => COUNTER_SCRIPT,
    };
    run_tool_for(
        &powershell_exe(),
        &["-NoProfile", "-NonInteractive", "-Command", command],
        TOOL_TIMEOUT,
    )
}

static PROBE: Mutex<Probe> = Mutex::new(Probe::new());

/// The AMD and Intel GPUs now; empty when there are none or PowerShell does not answer.
pub fn gpus() -> Vec<Reading> {
    match PROBE.lock() {
        Ok(mut probe) => probe.gpus(Instant::now(), &mut run_script),
        Err(_) => Vec::new(),
    }
}

/// `(pid, bytes)` of dedicated GPU memory, on every adapter together, for those of `pids` the
/// counters list.
pub fn process_vram(pids: &[u32]) -> Vec<(u32, u64)> {
    match PROBE.lock() {
        Ok(mut probe) => probe.process_vram(Instant::now(), pids, &mut run_script),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;
    const SINGLE_AMD: &str = include_str!("../tests/fixtures/gpu/windows-gpu-single-amd.json");
    const IGPU_AND_NVIDIA: &str =
        include_str!("../tests/fixtures/gpu/windows-gpu-igpu-and-nvidia.json");
    const INTEL_ARC: &str = include_str!("../tests/fixtures/gpu/windows-gpu-intel-arc.json");

    /// The adapters and counters of one sample file (which holds both, as one document).
    fn read(sample: &str) -> (Adapters, Counters) {
        (
            parse_adapters(sample).expect("adapters"),
            parse_counters(sample).expect("counters"),
        )
    }

    #[test]
    fn a_single_amd_card_has_its_real_size_and_usage() {
        let (adapters, counters) = read(SINGLE_AMD);
        let gpus = combine(&adapters, Some(&counters));
        assert_eq!(gpus.len(), 1, "{gpus:?}");
        assert_eq!(gpus[0].name, "AMD Radeon RX 7800 XT");
        assert_eq!(gpus[0].vendor, "amd");
        // The registry's size, not the saturated AdapterRAM.
        assert_eq!(gpus[0].vram_total_bytes, 17_163_091_968);
        assert_eq!(gpus[0].vram_used_bytes, Some(2 * GIB));
        assert_eq!(gpus[0].driver.as_deref(), Some("32.0.21013.1000"));
        assert_eq!(gpus[0].utilization_percent, None);
        assert_eq!(
            counters.processes,
            vec![(4120, 1_610_612_736), (9920, 268_435_456)]
        );
        // Without counters the memory in use is unknown, not zero.
        assert_eq!(combine(&adapters, None)[0].vram_used_bytes, None);
    }

    #[test]
    fn two_display_adapters_leave_the_usage_unknown() {
        let (adapters, counters) = read(IGPU_AND_NVIDIA);
        assert_eq!(adapters.pci_adapters, 2);
        let gpus = combine(&adapters, Some(&counters));
        // NVIDIA is read by nvidia-smi; the AMD integrated GPU's carve-out is listed.
        assert_eq!(gpus.len(), 1, "{gpus:?}");
        assert_eq!(gpus[0].vram_total_bytes, 512 * 1024 * 1024);
        assert_eq!(gpus[0].vram_used_bytes, None);
        assert_eq!(counters.processes, vec![(2288, 3_000_000_000)]);
    }

    #[test]
    fn usage_past_the_adapters_size_is_not_believed() {
        let json = r#"{"adapters":[{"Name":"AMD Radeon RX 6600","PNPDeviceID":"PCI\\VEN_1002&DEV_73FF","AdapterRAM":2147483648}],
            "adapter_memory":[{"Name":"luid_0x0_0x1_phys_0","DedicatedUsage":3221225472}],"process_memory":[]}"#;
        let (adapters, counters) = read(json);
        assert_eq!(counters.adapters, vec![3 * GIB]);
        assert_eq!(combine(&adapters, Some(&counters))[0].vram_used_bytes, None);
    }

    #[test]
    fn one_process_on_two_adapters_sums_and_phys_instances_share_a_luid() {
        let json = r#"{"adapter_memory":[
              {"Name":"luid_0x00000000_0x0000AAAA_phys_0","DedicatedUsage":100},
              {"Name":"luid_0x00000000_0x0000AAAA_phys_1","DedicatedUsage":50},
              {"Name":"luid_0x00000000_0x0000BBBB_phys_0","DedicatedUsage":7}],
            "process_memory":[
              {"Name":"pid_77_luid_0x00000000_0x0000AAAA_phys_0","DedicatedUsage":1000},
              {"Name":"pid_77_luid_0x00000000_0x0000BBBB_phys_0","DedicatedUsage":234},
              {"Name":"pid_x_luid_0x0","DedicatedUsage":5},
              {"Name":"pid_78_luid_0x0","DedicatedUsage":-5}]}"#;
        let counters = parse_counters(json).expect("counters");
        assert_eq!(counters.adapters, vec![7, 150]);
        assert_eq!(counters.processes, vec![(77, 1234)]);
    }

    #[test]
    fn an_intel_arc_without_a_registry_size_uses_adapter_ram_only_when_it_is_exact() {
        // AdapterRAM is saturated and there is no registry size: not listed, not guessed.
        assert!(read(INTEL_ARC).0.gpus.is_empty());
        let exact = INTEL_ARC.replace("4293918720", "3221225472");
        let (adapters, counters) = read(&exact);
        let gpus = combine(&adapters, Some(&counters));
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].vendor, "intel");
        assert_eq!(gpus[0].vram_total_bytes, 3 * GIB);
        assert_eq!(gpus[0].vram_used_bytes, Some(734_003_200));
    }

    #[test]
    fn only_discrete_intel_names_have_memory_of_their_own() {
        for discrete in [
            "Intel(R) Arc(TM) A770 Graphics",
            "Intel(R) Arc(TM) B580 Graphics",
            "Intel(R) Arc(TM) A370M Graphics",
            "Intel(R) Arc(TM) Pro A60 Graphics",
            "Intel Arc A380",
            "Intel(R) Arc(TM) C750 Graphics",
            "Intel(R) Data Center GPU Max 1550",
            "Intel(R) Data Center GPU Flex 170",
            "Intel(R) Flex 140",
        ] {
            assert!(intel_has_own_memory(discrete), "{discrete}");
        }
        for integrated in [
            "Intel(R) Arc(TM) Graphics",
            "Intel(R) Arc(TM) 140V GPU (16GB)",
            "Intel(R) Arc(TM) 130V GPU",
            "Intel(R) UHD Graphics 770",
            "Intel(R) Iris(R) Xe Graphics",
            "Intel(R) Graphics",
            "Microsoft Basic Display Adapter",
            "",
        ] {
            assert!(!intel_has_own_memory(integrated), "{integrated}");
        }
    }

    #[test]
    fn an_integrated_arc_is_not_listed_but_counts_as_a_vendor_adapter() {
        let json = r#"{"adapters":[{"Name":"Intel(R) Arc(TM) Graphics","PNPDeviceID":"PCI\\VEN_8086&DEV_7D55","AdapterRAM":2147483648}],"memory":[]}"#;
        let adapters = parse_adapters(json).expect("adapters");
        assert!(adapters.gpus.is_empty());
        assert!(adapters.vendor_present);
    }

    #[test]
    fn a_single_adapter_may_be_written_as_a_bare_object() {
        let json = r#"{"adapters":{"Name":"AMD Radeon RX 6600","PNPDeviceID":"PCI\\VEN_1002&DEV_73FF","AdapterRAM":2147483648,"DriverVersion":"31.0"},"memory":null}"#;
        let adapters = parse_adapters(json).expect("adapters");
        assert_eq!(adapters.gpus.len(), 1);
        assert_eq!(adapters.gpus[0].vram_total_bytes, 2 * GIB);
    }

    #[test]
    fn garbage_is_no_output_and_an_empty_list_is_still_output() {
        for text in [
            "",
            "not json",
            "[]",
            "{}",
            "null",
            r#"{"adapters": 5}"#,
            "\u{feff}",
        ] {
            assert_eq!(parse_adapters(text), None, "{text}");
            assert_eq!(parse_counters(text), None, "{text}");
        }
        // PowerShell may print a byte-order mark before the document.
        let adapters = parse_adapters("\u{feff}{\"adapters\":[],\"memory\":[]}").expect("adapters");
        assert_eq!(adapters, Adapters::default());
        assert_eq!(
            parse_counters(r#"{"adapter_memory":[],"process_memory":[]}"#),
            Some(Counters::default())
        );
    }

    #[test]
    fn truncated_output_never_panics_and_is_no_output() {
        for sample in [SINGLE_AMD, IGPU_AND_NVIDIA, INTEL_ARC] {
            for end in 0..sample.trim_end().len() {
                let Some(cut) = sample.get(..end) else {
                    continue;
                };
                // A document cut short is not JSON; only the whole of it parses.
                assert_eq!(parse_adapters(cut), None, "{end}");
                assert_eq!(parse_counters(cut), None, "{end}");
            }
        }
    }

    #[test]
    fn absurd_numbers_are_not_believed() {
        let json = r#"{"adapters":[{"Name":"AMD Radeon RX 6600","PNPDeviceID":"PCI\\VEN_1002&DEV_73FF","AdapterRAM":2147483648}],
            "memory":[{"Desc":"AMD Radeon RX 6600","Bytes":1e30}],
            "adapter_memory":[{"Name":"luid_0x0_0x1_phys_0","DedicatedUsage":99999999999999999999999}],
            "process_memory":[{"Name":"pid_5_luid_0x0_0x1_phys_0","DedicatedUsage":-1}]}"#;
        let (adapters, counters) = read(json);
        // The oversized registry figure is ignored, so AdapterRAM stands; no usage is believed.
        assert_eq!(adapters.gpus.len(), 1);
        assert_eq!(adapters.gpus[0].vram_total_bytes, 2 * GIB);
        assert_eq!(counters, Counters::default());
    }

    #[test]
    fn the_scripts_have_no_double_quotes() {
        // Each is one command-line argument; double quotes would not survive Windows quoting.
        assert!(!ADAPTER_SCRIPT.contains('"'));
        assert!(!COUNTER_SCRIPT.contains('"'));
    }

    /// A runner that answers from the sample files and counts what it was asked.
    struct Fake {
        sample: &'static str,
        adapter_runs: u32,
        counter_runs: u32,
        fail: bool,
    }

    impl Fake {
        fn new(sample: &'static str) -> Self {
            Fake {
                sample,
                adapter_runs: 0,
                counter_runs: 0,
                fail: false,
            }
        }

        fn run(&mut self, script: Script) -> Tool {
            match script {
                Script::Adapters => self.adapter_runs += 1,
                Script::Counters => self.counter_runs += 1,
            }
            if self.fail {
                Tool::Ran("not json".into())
            } else {
                Tool::Ran(self.sample.to_string())
            }
        }
    }

    const NVIDIA_ONLY: &str = r#"{"adapters":[{"Name":"NVIDIA GeForce RTX 4090","PNPDeviceID":"PCI\\VEN_10DE&DEV_2684","AdapterRAM":4293918720}],"memory":[],"adapter_memory":[],"process_memory":[{"Name":"pid_9_luid_0x0_0x1_phys_0","DedicatedUsage":4096}]}"#;

    #[test]
    fn a_machine_without_amd_or_intel_never_spawns_again() {
        let mut probe = Probe::new();
        let mut fake = Fake::new(NVIDIA_ONLY);
        let t0 = Instant::now();
        for step in 0..5u64 {
            let now = t0 + Duration::from_secs(30 * step);
            assert!(probe.gpus(now, &mut |s| fake.run(s)).is_empty());
        }
        assert_eq!((fake.adapter_runs, fake.counter_runs), (1, 0));
    }

    #[test]
    fn process_figures_run_the_counters_only_when_asked_and_not_more_often_than_the_interval() {
        let mut probe = Probe::new();
        let mut fake = Fake::new(NVIDIA_ONLY);
        let t0 = Instant::now();
        // No process asked about: nothing runs.
        assert!(probe.process_vram(t0, &[], &mut |s| fake.run(s)).is_empty());
        assert_eq!((fake.adapter_runs, fake.counter_runs), (0, 0));
        let asked = |probe: &mut Probe, fake: &mut Fake, at: Instant| {
            probe.process_vram(at, &[9, 10], &mut |s| fake.run(s))
        };
        assert_eq!(asked(&mut probe, &mut fake, t0), vec![(9, 4096)]);
        assert_eq!(
            asked(&mut probe, &mut fake, t0 + Duration::from_secs(4)),
            vec![(9, 4096)]
        );
        assert_eq!(fake.counter_runs, 1);
        asked(&mut probe, &mut fake, t0 + MIN_INTERVAL);
        assert_eq!(fake.counter_runs, 2);
    }

    #[test]
    fn with_an_amd_card_the_adapters_are_read_once_and_the_counters_every_interval() {
        let mut probe = Probe::new();
        let mut fake = Fake::new(SINGLE_AMD);
        let t0 = Instant::now();
        let first = probe.gpus(t0, &mut |s| fake.run(s));
        assert_eq!(first[0].vram_used_bytes, Some(2 * GIB));
        probe.gpus(t0 + Duration::from_secs(2), &mut |s| fake.run(s));
        assert_eq!((fake.adapter_runs, fake.counter_runs), (1, 1));
        probe.gpus(t0 + Duration::from_secs(30), &mut |s| fake.run(s));
        assert_eq!((fake.adapter_runs, fake.counter_runs), (1, 2));
    }

    #[test]
    fn a_failing_script_is_retried_later_with_a_growing_delay_never_given_up_on() {
        let mut probe = Probe::new();
        let mut fake = Fake::new(SINGLE_AMD);
        fake.fail = true;
        let t0 = Instant::now();
        let at = |probe: &mut Probe, fake: &mut Fake, secs: u64| {
            probe.gpus(t0 + Duration::from_secs(secs), &mut |s| fake.run(s))
        };
        assert!(at(&mut probe, &mut fake, 0).is_empty());
        assert!(at(&mut probe, &mut fake, 10).is_empty());
        assert_eq!(fake.adapter_runs, 1, "waits 30 s");
        at(&mut probe, &mut fake, 31);
        assert_eq!(fake.adapter_runs, 2);
        at(&mut probe, &mut fake, 31 + 59);
        assert_eq!(fake.adapter_runs, 2, "then 60 s");
        // PowerShell recovers (a driver update finished): the GPU appears.
        fake.fail = false;
        let gpus = at(&mut probe, &mut fake, 31 + 61);
        assert_eq!(gpus.len(), 1);
        // The delay never passes the cap.
        let mut backoff = Backoff::new();
        let now = Instant::now();
        for _ in 0..40 {
            backoff.failed(now);
        }
        assert_eq!(backoff.retry_at, Some(now + RETRY_MAX));
    }

    #[test]
    fn powershell_comes_from_the_system_folder() {
        let path = powershell_exe();
        assert!(path.ends_with("powershell.exe"), "{path}");
        assert!(path.contains("WindowsPowerShell"), "{path}");
    }
}
