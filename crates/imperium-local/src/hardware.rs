//! What this machine can run models on: GPUs and their memory, system memory, and cores.

use std::process::Command;

use imperium_config::HardwareSettings;

use crate::gpu::VendorProbe;

const MIB: u64 = 1024 * 1024;
/// Least memory an AMD or Intel GPU needs for models to be planned on it (and Vulkan chosen).
pub const MIN_VULKAN_VRAM_BYTES: u64 = 4 * 1024 * MIB;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gpu {
    pub name: String,
    pub vendor: String,
    pub vram_total_bytes: u64,
    pub vram_free_bytes: u64,
    /// The driver version, which decides the CUDA runtime it supports.
    pub driver: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hardware {
    pub gpus: Vec<Gpu>,
    pub ram_total_bytes: u64,
    pub ram_available_bytes: u64,
    pub cpu_cores: usize,
    /// GPU and CPU share system memory (Apple Silicon), so models are planned against RAM.
    pub unified_memory: bool,
    /// Values replaced by `[hardware]` settings.
    pub overridden: bool,
}

impl Hardware {
    /// The vendor whose GPUs models are planned on, since a runtime build drives one vendor's
    /// GPUs: NVIDIA when there is one, else the first AMD or Intel GPU with at least
    /// [`MIN_VULKAN_VRAM_BYTES`] (an integrated GPU's small carve-out is not worth offloading
    /// to; a `[hardware]` override lifts that minimum), else a GPU of a vendor the probe does
    /// not know (a `[hardware] vram_mb` override). Apple Silicon is planned as unified memory,
    /// not as VRAM. The GPUs of other vendors are shown, not planned on.
    fn planned_vendor(&self) -> Option<&str> {
        if self.gpus.iter().any(|gpu| gpu.vendor == "nvidia") {
            return Some("nvidia");
        }
        if let Some(gpu) = self.gpus.iter().find(|gpu| {
            matches!(gpu.vendor.as_str(), "amd" | "intel")
                && (self.overridden || gpu.vram_total_bytes >= MIN_VULKAN_VRAM_BYTES)
        }) {
            return Some(&gpu.vendor);
        }
        self.gpus
            .iter()
            .map(|gpu| gpu.vendor.as_str())
            .find(|vendor| !matches!(*vendor, "nvidia" | "amd" | "intel" | "apple"))
    }

    /// The GPUs models are planned on ([`Hardware::planned_vendor`]).
    pub fn planned_gpus(&self) -> impl Iterator<Item = &Gpu> {
        let vendor = self.planned_vendor();
        self.gpus
            .iter()
            .filter(move |gpu| Some(gpu.vendor.as_str()) == vendor)
    }

    /// Free memory of the GPUs models are planned on.
    pub fn vram_free_bytes(&self) -> u64 {
        self.planned_gpus().map(|gpu| gpu.vram_free_bytes).sum()
    }

    /// The runtime pack that suits this machine, for `runtime.llamacpp.backend = "auto"`.
    pub fn suggested_pack(&self) -> String {
        format!("{}-{}", platform(), self.backend_chain()[0])
    }

    /// The llama.cpp backends to try on this machine, best first, for
    /// `runtime.llamacpp.backend = "auto"`: on an NVIDIA GPU the newest CUDA its driver
    /// supports, then Vulkan (which needs no CUDA runtime), then the CPU; Vulkan then the CPU on
    /// an AMD or Intel GPU; Metal on Apple Silicon.
    pub fn backend_chain(&self) -> Vec<&'static str> {
        let platform = platform();
        if self.unified_memory {
            return vec!["metal", "cpu"];
        }
        if platform.starts_with("macos") {
            return vec!["cpu"];
        }
        let Some(gpu) = self.gpus.iter().find(|gpu| gpu.vendor == "nvidia") else {
            // AMD and Intel GPUs: Vulkan runs on their open drivers without a vendor runtime.
            // A GPU the probe cannot name (an override) or an integrated one stays on the CPU.
            return if matches!(self.planned_vendor(), Some("amd" | "intel")) {
                vec!["vulkan", "cpu"]
            } else {
                vec!["cpu"]
            };
        };
        let major = gpu
            .driver
            .as_deref()
            .and_then(|driver| driver.split('.').next()?.trim().parse::<u32>().ok())
            .unwrap_or(0);
        let mut chain = Vec::new();
        // CUDA 13 needs a 580-series or newer driver, CUDA 12 a 525-series or newer one;
        // llama.cpp publishes CUDA 12 builds for x64 only.
        if major >= 580 {
            chain.push("cuda13");
        }
        if major >= 525 && platform.ends_with("x64") {
            chain.push("cuda12");
        }
        chain.extend(["vulkan", "cpu"]);
        chain
    }
}

/// `<os>-<arch>` as runtime packs name it.
pub fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        _ => "x64",
    };
    format!("{os}-{arch}")
}

/// Probes the machine, then applies `overrides`.
pub fn probe(overrides: &HardwareSettings) -> Hardware {
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    let unified_memory = cfg!(all(target_os = "macos", target_arch = "aarch64"));
    let mut gpus = if unified_memory {
        Vec::new()
    } else {
        nvidia_gpus()
    };
    // AMD and Intel GPUs on Linux, and the Apple GPU that shares the machine's memory.
    gpus.extend(
        VendorProbe::default()
            .read(
                system.total_memory(),
                system
                    .total_memory()
                    .saturating_sub(system.available_memory()),
            )
            .into_iter()
            .map(|reading| Gpu {
                vram_free_bytes: reading.vram_free_bytes(),
                name: reading.name,
                vendor: reading.vendor.to_string(),
                vram_total_bytes: reading.vram_total_bytes,
                driver: reading.driver,
            }),
    );
    let mut hardware = Hardware {
        gpus,
        ram_total_bytes: system.total_memory(),
        ram_available_bytes: system.available_memory(),
        cpu_cores: std::thread::available_parallelism().map_or(1, usize::from),
        unified_memory,
        overridden: false,
    };
    apply(&mut hardware, overrides);
    hardware
}

fn apply(hardware: &mut Hardware, overrides: &HardwareSettings) {
    if let Some(vram) = overrides.vram_mb {
        let bytes = vram * MIB;
        // The override stands in for the GPU models are planned on.
        let basis = hardware
            .planned_gpus()
            .next()
            .or_else(|| hardware.gpus.first())
            .cloned();
        hardware.gpus = vec![Gpu {
            name: basis
                .as_ref()
                .map_or_else(|| "configured GPU".into(), |gpu| gpu.name.clone()),
            vendor: basis
                .as_ref()
                .map_or_else(|| "unknown".into(), |gpu| gpu.vendor.clone()),
            vram_total_bytes: bytes,
            vram_free_bytes: bytes,
            driver: basis.and_then(|gpu| gpu.driver),
        }];
        hardware.overridden = true;
    }
    if let Some(ram) = overrides.ram_mb {
        hardware.ram_total_bytes = ram * MIB;
        hardware.ram_available_bytes = ram * MIB;
        hardware.overridden = true;
    }
}

/// NVIDIA GPUs as `nvidia-smi` reports them; none when it is absent.
fn nvidia_gpus() -> Vec<Gpu> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total,memory.free,driver_version",
            "--format=csv,noheader,nounits",
        ])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            parse_nvidia_smi(&String::from_utf8_lossy(&output.stdout))
        }
        _ => Vec::new(),
    }
}

fn parse_nvidia_smi(csv: &str) -> Vec<Gpu> {
    csv.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(',').map(str::trim).collect();
            let [name, total, free, driver] = fields.as_slice() else {
                return None;
            };
            Some(Gpu {
                name: (*name).to_string(),
                vendor: "nvidia".into(),
                vram_total_bytes: total.parse::<u64>().ok()? * MIB,
                vram_free_bytes: free.parse::<u64>().ok()? * MIB,
                driver: Some((*driver).to_string()),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia_smi_rows_become_gpus() {
        let gpus = parse_nvidia_smi(
            "NVIDIA GeForce RTX 5080, 16303, 12707, 581.57\nbad line\n\
             NVIDIA RTX A4000, 16376, 16000, 550.12\n",
        );
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].vram_free_bytes, 12707 * MIB);
        assert_eq!(gpus[1].driver.as_deref(), Some("550.12"));
    }

    fn gpu(vendor: &str, total_gib: u64, free_gib: u64) -> Gpu {
        Gpu {
            name: format!("{vendor} GPU"),
            vendor: vendor.into(),
            vram_total_bytes: total_gib * 1024 * MIB,
            vram_free_bytes: free_gib * 1024 * MIB,
            driver: None,
        }
    }

    fn machine(gpus: Vec<Gpu>) -> Hardware {
        Hardware {
            gpus,
            ram_total_bytes: 0,
            ram_available_bytes: 0,
            cpu_cores: 8,
            unified_memory: false,
            overridden: false,
        }
    }

    #[test]
    fn the_budget_counts_one_vendors_gpus_not_all() {
        // A runtime build drives one vendor's GPUs: NVIDIA's; the AMD one only shows.
        let mixed = machine(vec![gpu("amd", 8, 8), gpu("nvidia", 12, 11)]);
        assert_eq!(mixed.vram_free_bytes(), 11 * 1024 * MIB);
        // Without NVIDIA, the first AMD or Intel GPU's vendor.
        let two = machine(vec![
            gpu("amd", 8, 6),
            gpu("intel", 16, 16),
            gpu("amd", 8, 7),
        ]);
        assert_eq!(two.vram_free_bytes(), 13 * 1024 * MIB);
        // Apple Silicon is planned as unified memory, not VRAM.
        assert_eq!(machine(vec![gpu("apple", 64, 40)]).vram_free_bytes(), 0);
        // An integrated GPU's carve-out is no budget, but a `[hardware]` override is believed.
        let mut small = machine(vec![gpu("amd", 1, 1)]);
        assert_eq!(small.vram_free_bytes(), 0);
        apply(
            &mut small,
            &HardwareSettings {
                vram_mb: Some(2048),
                ram_mb: None,
            },
        );
        assert_eq!(small.vram_free_bytes(), 2048 * MIB);
        // An override on a machine with no GPU is still a budget, planned on the CPU build.
        let mut none = machine(Vec::new());
        apply(
            &mut none,
            &HardwareSettings {
                vram_mb: Some(8192),
                ram_mb: None,
            },
        );
        assert_eq!(none.vram_free_bytes(), 8192 * MIB);
        if !cfg!(target_os = "macos") {
            assert_eq!(small.backend_chain(), ["vulkan", "cpu"]);
            assert_eq!(none.backend_chain(), ["cpu"]);
        }
    }

    #[test]
    fn packs_follow_the_driver_and_overrides_apply() {
        let mut hardware = Hardware {
            gpus: parse_nvidia_smi("GPU, 8000, 7000, 575.10"),
            ram_total_bytes: 0,
            ram_available_bytes: 0,
            cpu_cores: 8,
            unified_memory: false,
            overridden: false,
        };
        if platform().ends_with("x64") {
            assert!(hardware.suggested_pack().ends_with("-cuda12"));
            hardware.gpus[0].driver = Some("581.57".into());
            assert!(hardware.suggested_pack().ends_with("-cuda13"));
        }
        apply(
            &mut hardware,
            &HardwareSettings {
                vram_mb: Some(4096),
                ram_mb: Some(16384),
            },
        );
        assert_eq!(hardware.vram_free_bytes(), 4096 * MIB);
        assert_eq!(hardware.ram_available_bytes, 16384 * MIB);
        assert!(hardware.overridden);
        hardware.gpus.clear();
        assert!(hardware.suggested_pack().ends_with("-cpu"));
    }

    #[test]
    fn the_backend_chain_falls_back_from_the_newest_cuda_to_the_cpu() {
        if cfg!(target_os = "macos") {
            return;
        }
        let mut hardware = Hardware {
            gpus: parse_nvidia_smi("GPU, 8000, 7000, 581.57"),
            ram_total_bytes: 0,
            ram_available_bytes: 0,
            cpu_cores: 8,
            unified_memory: false,
            overridden: false,
        };
        let x64 = platform().ends_with("x64");
        let expect = |chain: &[&'static str]| -> Vec<&'static str> {
            chain
                .iter()
                .copied()
                .filter(|backend| x64 || *backend != "cuda12")
                .collect()
        };
        assert_eq!(
            hardware.backend_chain(),
            expect(&["cuda13", "cuda12", "vulkan", "cpu"])
        );
        hardware.gpus[0].driver = Some("552.22".into());
        assert_eq!(
            hardware.backend_chain(),
            expect(&["cuda12", "vulkan", "cpu"])
        );
        hardware.gpus[0].driver = Some("470.10".into());
        assert_eq!(hardware.backend_chain(), ["vulkan", "cpu"]);
        hardware.gpus.clear();
        assert_eq!(hardware.backend_chain(), ["cpu"]);
        hardware.gpus.push(Gpu {
            name: "Radeon RX 7900 XTX".into(),
            vendor: "amd".into(),
            vram_total_bytes: 24 * 1024 * MIB,
            vram_free_bytes: 20 * 1024 * MIB,
            driver: None,
        });
        assert_eq!(hardware.backend_chain(), ["vulkan", "cpu"]);
        // An integrated GPU with a small carve-out is not used.
        hardware.gpus[0].vram_total_bytes = 512 * MIB;
        assert_eq!(hardware.backend_chain(), ["cpu"]);
        assert_eq!(hardware.vram_free_bytes(), 0);
        hardware.gpus.clear();
        hardware.unified_memory = true;
        assert_eq!(hardware.backend_chain(), ["metal", "cpu"]);
    }
}
