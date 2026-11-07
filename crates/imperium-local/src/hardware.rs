//! What this machine can run models on: GPUs and their memory, system memory, and cores.

use std::process::Command;

use imperium_config::HardwareSettings;

const MIB: u64 = 1024 * 1024;

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
    pub fn vram_free_bytes(&self) -> u64 {
        self.gpus.iter().map(|gpu| gpu.vram_free_bytes).sum()
    }

    /// The runtime pack that suits this machine, for `runtime.llamacpp.backend = "auto"`.
    pub fn suggested_pack(&self) -> String {
        let platform = platform();
        if self.unified_memory {
            return format!("{platform}-metal");
        }
        let nvidia = self.gpus.iter().find(|gpu| gpu.vendor == "nvidia");
        match nvidia {
            // CUDA 13 needs a 580-series or newer driver.
            Some(gpu) if platform.ends_with("x64") => {
                let major = gpu
                    .driver
                    .as_deref()
                    .and_then(|driver| driver.split('.').next()?.parse::<u32>().ok())
                    .unwrap_or(0);
                let cuda = if major >= 580 { "cuda13" } else { "cuda12" };
                format!("{platform}-{cuda}")
            }
            _ => format!("{platform}-cpu"),
        }
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
    let mut hardware = Hardware {
        gpus: if unified_memory {
            Vec::new()
        } else {
            nvidia_gpus()
        },
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
        hardware.gpus = vec![Gpu {
            name: hardware
                .gpus
                .first()
                .map_or_else(|| "configured GPU".into(), |gpu| gpu.name.clone()),
            vendor: hardware
                .gpus
                .first()
                .map_or_else(|| "unknown".into(), |gpu| gpu.vendor.clone()),
            vram_total_bytes: bytes,
            vram_free_bytes: bytes,
            driver: hardware.gpus.first().and_then(|gpu| gpu.driver.clone()),
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
}
