//! `imperium models local` and `imperium runtime install`.

use anyhow::Context;
use imperium_config::Paths;
use imperium_local::hardware::probe;
use imperium_local::runtime::{RELEASES_URL, install, pack_for};
use imperium_protocol::methods::{LocalModelList, LocalModelListParams};

use crate::{block_on, connect};

const MIB: f64 = 1024.0 * 1024.0;
const GIB: f64 = 1024.0 * MIB;

pub fn list() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let listing = client
            .call::<LocalModelList>(LocalModelListParams {})
            .await?;
        if !listing.enabled {
            println!("No model folders configured. Add some to config.toml:\n");
            println!("[models]\nfolders = [\"D:/Models\"]");
            return Ok(());
        }
        if let Some(hardware) = &listing.hardware {
            for gpu in &hardware.gpus {
                println!(
                    "GPU  {} ({}): {:.1} of {:.1} GiB free",
                    gpu.name,
                    gpu.vendor,
                    gpu.vram_free_bytes as f64 / GIB,
                    gpu.vram_total_bytes as f64 / GIB
                );
            }
            println!(
                "RAM  {:.1} of {:.1} GiB available, {} cores{}{}",
                hardware.ram_available_bytes as f64 / GIB,
                hardware.ram_total_bytes as f64 / GIB,
                hardware.cpu_cores,
                if hardware.unified_memory {
                    ", unified memory"
                } else {
                    ""
                },
                if hardware.overridden {
                    " (from [hardware] settings)"
                } else {
                    ""
                },
            );
        }
        if let Some(runtime) = &listing.runtime {
            println!(
                "llama.cpp {} pack {}: {}",
                runtime.llama_cpp_ref,
                runtime.pack,
                if runtime.installed {
                    runtime.server_path.clone()
                } else {
                    "not installed; run `imperium runtime install`".into()
                }
            );
        }
        println!();
        if listing.models.is_empty() {
            println!("No GGUF models in {}", listing.folders.join(", "));
        }
        for model in &listing.models {
            let state = if model.loaded {
                format!("loaded, {}/{} slots busy", model.slots_in_use, model.slots)
            } else {
                match model.placement.as_str() {
                    "partial" => format!(
                        "fits partially ({} GPU layers)",
                        model.gpu_layers.unwrap_or(0)
                    ),
                    "none" => "does not fit".into(),
                    placement => format!("fits on {placement}"),
                }
            };
            println!(
                "{:<40} {:>8} {:>7.1} GiB  needs {:>6.1} GiB  {}",
                format!("{}:{}", model.runtime, model.id),
                model.quantization.as_deref().unwrap_or("?"),
                model.size_bytes as f64 / GIB,
                model.required_bytes as f64 / GIB,
                state
            );
        }
        for skipped in &listing.skipped {
            println!("skipped {}: {}", skipped.path, skipped.reason);
        }
        if !listing.models.is_empty() {
            println!("\nUse a model by the name shown, in [[models.catalog]] or with --model.");
        }
        Ok(())
    })
}

pub fn install_runtime(pack: Option<String>) -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let settings = imperium_config::load(&paths, None, std::env::vars())?.settings()?;
    let pack = match pack {
        Some(pack) => pack,
        None => pack_for(
            &settings.runtime.llamacpp.backend,
            &probe(&settings.hardware),
        ),
    };
    println!("installing llama.cpp runtime pack {pack}");
    block_on(async {
        let server = install(
            &reqwest::Client::new(),
            RELEASES_URL,
            paths.data_dir(),
            &pack,
        )
        .await
        .with_context(|| format!("could not install {pack}"))?;
        println!(
            "installed {} ({:.0} MiB)",
            server.display(),
            size(&server) / MIB
        );
        Ok(())
    })
}

fn size(path: &std::path::Path) -> f64 {
    std::fs::metadata(path).map_or(0.0, |meta| meta.len() as f64)
}
