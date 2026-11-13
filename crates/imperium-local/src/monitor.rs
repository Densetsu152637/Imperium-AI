//! Live machine usage for the resource monitor: CPU, memory, GPUs, and chosen processes.
//!
//! [`SystemProbe`] reads CPU, memory, and processes through `sysinfo` and NVIDIA GPUs through
//! `nvidia-smi` (which reports NVML's counters); AMD, Intel and Apple Silicon GPUs come from
//! [`crate::gpu`]. Without any of them the GPU list is empty. Callers depend on the [`Probe`] trait so tests can use fakes.

use crate::gpu::{VendorProbe, run_tool};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

const MIB: u64 = 1024 * 1024;

/// One reading of the machine.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sample {
    /// Busy share of every core together, 0 to 100.
    pub cpu_percent: f32,
    /// Busy share of each logical core, 0 to 100.
    pub per_core: Vec<f32>,
    pub ram_used_bytes: u64,
    pub ram_total_bytes: u64,
    pub swap_used_bytes: u64,
    pub swap_total_bytes: u64,
    pub gpus: Vec<GpuSample>,
    /// The processes asked for that are still running.
    pub processes: Vec<ProcessSample>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GpuSample {
    pub index: u32,
    pub name: String,
    pub vendor: String,
    /// Busy share over the driver's last sample period; `None` when it does not say.
    pub utilization_percent: Option<f32>,
    pub vram_used_bytes: u64,
    pub vram_total_bytes: u64,
    pub temperature_c: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProcessSample {
    pub pid: u32,
    pub name: String,
    /// Share of the whole machine (every core), 0 to 100.
    pub cpu_percent: f32,
    pub ram_bytes: u64,
    /// GPU memory the process holds; `None` when the driver does not report it per process
    /// (as on Windows under WDDM) or there is no NVIDIA GPU.
    pub vram_bytes: Option<u64>,
}

/// Reads the machine. Stateful: CPU usage is measured between consecutive calls.
pub trait Probe: Send {
    /// Reads system usage plus the processes with these ids.
    fn sample(&mut self, pids: &[u32]) -> Sample;
}

/// The real machine.
pub struct SystemProbe {
    system: System,
    /// Whether CPU counters have a previous reading to measure against.
    primed: bool,
    /// Cleared once `nvidia-smi` cannot be started, so it is not tried every few seconds.
    nvidia: bool,
    /// AMD, Intel and Apple Silicon GPUs.
    vendors: VendorProbe,
}

impl std::fmt::Debug for SystemProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemProbe")
            .field("primed", &self.primed)
            .field("nvidia", &self.nvidia)
            .finish_non_exhaustive()
    }
}

impl Default for SystemProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemProbe {
    pub fn new() -> Self {
        SystemProbe {
            system: System::new(),
            primed: false,
            // Apple Silicon shares memory between GPU and CPU and has no nvidia-smi.
            nvidia: !cfg!(target_os = "macos"),
            vendors: VendorProbe,
        }
    }

    fn refresh(&mut self, pids: &[Pid]) {
        self.system.refresh_cpu_usage();
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(pids),
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
    }

    fn gpus(&mut self) -> (Vec<GpuSample>, Vec<(u32, u64)>) {
        let (mut gpus, apps) = self.nvidia_gpus();
        let first = u32::try_from(gpus.len()).unwrap_or(u32::MAX);
        let others = self
            .vendors
            .read(self.system.total_memory(), self.system.used_memory());
        gpus.extend(
            others
                .into_iter()
                .zip(first..)
                .map(|(gpu, index)| GpuSample {
                    index,
                    name: gpu.name,
                    vendor: gpu.vendor.to_string(),
                    utilization_percent: gpu.utilization_percent,
                    // The protocol has no "unknown"; planning treats it as half-used ([`Reading::vram_free_bytes`]).
                    vram_used_bytes: gpu.vram_used_bytes.unwrap_or(0),
                    vram_total_bytes: gpu.vram_total_bytes,
                    temperature_c: gpu.temperature_c,
                }),
        );
        (gpus, apps)
    }

    fn nvidia_gpus(&mut self) -> (Vec<GpuSample>, Vec<(u32, u64)>) {
        if !self.nvidia {
            return (Vec::new(), Vec::new());
        }
        let gpus = match nvidia_smi(&[
            "--query-gpu=index,name,utilization.gpu,memory.used,memory.total,temperature.gpu",
            "--format=csv,noheader,nounits",
        ]) {
            Some(csv) => parse_gpus(&csv),
            None => {
                self.nvidia = false;
                return (Vec::new(), Vec::new());
            }
        };
        let apps = nvidia_smi(&[
            "--query-compute-apps=pid,used_memory",
            "--format=csv,noheader,nounits",
        ])
        .map(|csv| parse_compute_apps(&csv))
        .unwrap_or_default();
        (gpus, apps)
    }
}

impl Probe for SystemProbe {
    fn sample(&mut self, pids: &[u32]) -> Sample {
        let wanted: Vec<Pid> = pids.iter().copied().map(Pid::from_u32).collect();
        self.refresh(&wanted);
        if !self.primed {
            // CPU usage is the change between two readings at least this far apart.
            std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
            self.refresh(&wanted);
            self.primed = true;
        }
        self.system.refresh_memory();
        let cores = self.system.cpus().len().max(1);
        let (gpus, apps) = self.gpus();
        let processes = wanted
            .iter()
            .filter_map(|pid| {
                let process = self.system.process(*pid)?;
                let pid = pid.as_u32();
                let vram = apps
                    .iter()
                    .filter(|(app, _)| *app == pid)
                    .map(|(_, bytes)| *bytes)
                    .reduce(|a, b| a + b);
                Some(ProcessSample {
                    pid,
                    name: process.name().to_string_lossy().into_owned(),
                    cpu_percent: process.cpu_usage() / cores as f32,
                    ram_bytes: process.memory(),
                    vram_bytes: vram,
                })
            })
            .collect();
        Sample {
            cpu_percent: self.system.global_cpu_usage(),
            per_core: self
                .system
                .cpus()
                .iter()
                .map(sysinfo::Cpu::cpu_usage)
                .collect(),
            ram_used_bytes: self.system.used_memory(),
            ram_total_bytes: self.system.total_memory(),
            swap_used_bytes: self.system.used_swap(),
            swap_total_bytes: self.system.total_swap(),
            gpus,
            processes,
        }
    }
}

/// Runs `nvidia-smi` with `args`; `None` when it is missing or fails.
fn nvidia_smi(args: &[&str]) -> Option<String> {
    run_tool("nvidia-smi", args)
}

/// A number field, or `None` for `[N/A]` and the like.
fn number(field: &str) -> Option<f32> {
    field.trim().parse::<f32>().ok()
}

fn parse_gpus(csv: &str) -> Vec<GpuSample> {
    csv.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(',').map(str::trim).collect();
            let [index, name, utilization, used, total, temperature] = fields.as_slice() else {
                return None;
            };
            Some(GpuSample {
                index: index.parse().ok()?,
                name: (*name).to_string(),
                vendor: "nvidia".into(),
                utilization_percent: number(utilization),
                vram_used_bytes: used.parse::<u64>().ok()? * MIB,
                vram_total_bytes: total.parse::<u64>().ok()? * MIB,
                temperature_c: number(temperature),
            })
        })
        .collect()
}

/// `(pid, bytes)` for each process using a GPU; rows without a figure are left out.
fn parse_compute_apps(csv: &str) -> Vec<(u32, u64)> {
    csv.lines()
        .filter_map(|line| {
            let (pid, used) = line.split_once(',')?;
            Some((
                pid.trim().parse().ok()?,
                used.trim().parse::<u64>().ok()? * MIB,
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia_rows_parse_and_missing_figures_are_none() {
        let gpus = parse_gpus(
            "0, NVIDIA GeForce RTX 5080, 37, 4096, 16303, 61\n\
             1, NVIDIA RTX A4000, [N/A], 100, 16376, [N/A]\nbroken\n",
        );
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].utilization_percent, Some(37.0));
        assert_eq!(gpus[0].vram_used_bytes, 4096 * MIB);
        assert_eq!(gpus[0].temperature_c, Some(61.0));
        assert_eq!(gpus[1].index, 1);
        assert_eq!(gpus[1].utilization_percent, None);
        assert_eq!(gpus[1].temperature_c, None);
        let apps = parse_compute_apps("1234, 2048\n5678, [N/A]\n");
        assert_eq!(apps, vec![(1234, 2048 * MIB)]);
    }

    #[test]
    fn the_real_probe_reads_this_process() {
        let mut probe = SystemProbe::new();
        let pid = std::process::id();
        let sample = probe.sample(&[pid]);
        assert!(sample.ram_total_bytes > 0);
        assert!(!sample.per_core.is_empty());
        assert_eq!(sample.processes.len(), 1);
        assert_eq!(sample.processes[0].pid, pid);
        assert!(sample.processes[0].ram_bytes > 0);
    }
}
