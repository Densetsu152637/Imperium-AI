//! `imperium models local|folders|use|test` and `imperium runtime install|status|use`.

use std::path::Path;

use anyhow::Context;
use imperium_config::Paths;
use imperium_local::hardware::probe;
use imperium_local::runtime::{
    GITHUB_API, LLAMA_CPP_REPOSITORY, install, llama_cpp_ref, packs_for,
};
use imperium_protocol::methods::{
    LocalModelList, LocalModelListParams, LocalModelListResult, ModelFolderAdd, ModelFolderParams,
    ModelFolderRemove, ModelTest, ModelTestParams, ModelUse, ModelUseParams, RuntimeSetServer,
    RuntimeSetServerParams, RuntimeStatus, RuntimeStatusParams, RuntimeStatusResult,
};

use crate::{block_on, connect};

/// `path` made absolute against the current folder, as the daemon needs.
fn absolute(path: &Path) -> anyhow::Result<String> {
    let path = std::path::absolute(path)
        .with_context(|| format!("could not resolve {}", path.display()))?;
    Ok(path.display().to_string())
}

fn print_folders(listing: &LocalModelListResult) {
    if listing.configured_folders.is_empty() {
        println!("No model folders. Add one with `imperium models folders add <path>`.");
    }
    for folder in &listing.configured_folders {
        println!("{folder}");
    }
    for folder in &listing.folders {
        if !listing.configured_folders.contains(folder) {
            println!("{folder}  (download folder)");
        }
    }
    println!(
        "{} model{} found.",
        listing.models.len(),
        if listing.models.len() == 1 { "" } else { "s" }
    );
}

pub fn folders() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let listing = client
            .call::<LocalModelList>(LocalModelListParams {})
            .await?;
        print_folders(&listing);
        Ok(())
    })
}

pub fn add_folder(path: &Path) -> anyhow::Result<()> {
    let path = absolute(path)?;
    block_on(async {
        let (mut client, _) = connect().await?;
        let listing = client
            .call::<ModelFolderAdd>(ModelFolderParams { path })
            .await?;
        print_folders(&listing);
        Ok(())
    })
}

pub fn remove_folder(path: &Path) -> anyhow::Result<()> {
    let path = absolute(path)?;
    block_on(async {
        let (mut client, _) = connect().await?;
        let listing = client
            .call::<ModelFolderRemove>(ModelFolderParams { path })
            .await?;
        print_folders(&listing);
        Ok(())
    })
}

pub fn use_model(model: String) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let used = client.call::<ModelUse>(ModelUseParams { model }).await?;
        println!("New sessions now use {}.", used.default);
        Ok(())
    })
}

pub fn test_model(model: String, prompt: Option<String>) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        println!("Testing {model} (loading it first if needed)…");
        let result = client
            .call::<ModelTest>(ModelTestParams { model, prompt })
            .await?;
        if !result.ok {
            anyhow::bail!(
                "{} failed: {}",
                result.model,
                result.message.as_deref().unwrap_or("no reason given")
            );
        }
        println!("{}", result.reply);
        println!(
            "\n{}: {} tokens in {:.1} s{} (waited {:.1} s to load)",
            result.model,
            result.completion_tokens,
            result.seconds,
            result
                .tokens_per_s
                .map_or_else(String::new, |rate| format!(", {rate:.1} tok/s")),
            result.load_seconds
        );
        Ok(())
    })
}

fn print_runtime(status: &RuntimeStatusResult) {
    let source = match status.source.as_str() {
        "custom" => "your llama-server".to_string(),
        "detected" => {
            "llama-server found on this machine, used until a runtime is installed".into()
        }
        _ => format!("{} build {}", status.llama_cpp_ref, status.pack),
    };
    println!("llama.cpp ({source}): {}", status.server_path);
    if let Some(version) = &status.version {
        println!("{version}");
    }
    for device in &status.devices {
        println!("  {}: {}", device.id, device.description);
    }
    println!("{}", status.summary);
    if let Some(problem) = &status.problem {
        println!("\nProblem: {problem}");
    }
    if !status.installed && !status.candidates.is_empty() {
        println!("\nFound llama-server here; use one with `imperium runtime use <path>`:");
        for candidate in &status.candidates {
            println!("  {candidate}");
        }
    }
}

pub fn runtime_status() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let status = client.call::<RuntimeStatus>(RuntimeStatusParams {}).await?;
        print_runtime(&status);
        Ok(())
    })
}

pub fn runtime_candidates() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let status = client.call::<RuntimeStatus>(RuntimeStatusParams {}).await?;
        if status.candidates.is_empty() {
            println!("No other llama-server found on this machine.");
        } else {
            println!("Found llama-server (none is run until you adopt it):");
            for candidate in &status.candidates {
                println!("  {candidate}");
            }
            println!(
                "
Adopt one with `imperium runtime use <path>`."
            );
        }
        Ok(())
    })
}

pub fn use_server(path: Option<std::path::PathBuf>) -> anyhow::Result<()> {
    let path = path.as_deref().map(absolute).transpose()?;
    block_on(async {
        let (mut client, _) = connect().await?;
        let status = client
            .call::<RuntimeSetServer>(RuntimeSetServerParams { path })
            .await?;
        print_runtime(&status);
        Ok(())
    })
}

const MIB: f64 = 1024.0 * 1024.0;
const GIB: f64 = 1024.0 * MIB;

pub fn list() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let listing = client
            .call::<LocalModelList>(LocalModelListParams {})
            .await?;
        if !listing.enabled {
            println!("No model folders yet. Add the folder your models are in:\n");
            println!("  imperium models folders add D:/Models\n");
            println!("or download one with `imperium models pull <owner/name>`.");
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
                "llama.cpp {} ({}): {}",
                runtime.llama_cpp_ref,
                runtime.pack,
                if runtime.installed {
                    runtime.server_path.clone()
                } else {
                    "no runtime yet; run `imperium runtime install`, or use your own with \
                     `imperium runtime use <path to llama-server>`"
                        .into()
                }
            );
        }
        println!();
        if listing.models.is_empty() {
            println!("No GGUF models in {}", listing.folders.join(", "));
        }
        for model in &listing.models {
            let state = if model.loaded {
                let queue = match model.queued {
                    0 => String::new(),
                    queued => format!(", {queued} waiting"),
                };
                format!(
                    "loaded, {}/{} slots busy{queue}",
                    model.slots_in_use, model.slots
                )
            } else {
                match model.placement.as_str() {
                    "cpu" if model.cpu_lane => "CPU lane (pinned to RAM)".into(),
                    "partial" => format!(
                        "fits partially ({} GPU layers)",
                        model.gpu_layers.unwrap_or(0)
                    ),
                    "none" => "does not fit".into(),
                    placement => format!("fits on {placement}"),
                }
            };
            let state = match &model.problem {
                Some(problem) => format!("cannot run here: {problem}"),
                None => state,
            };
            let default = listing
                .default_model
                .as_deref()
                .is_some_and(|spec| spec == format!("{}:{}", model.runtime, model.id));
            let speed = model
                .tokens_per_s
                .map_or_else(String::new, |rate| format!(", {rate:.1} tok/s"));
            println!(
                "{:<40} {:>8} {:>7.1} GiB  needs {:>6.1} GiB  {}{}",
                format!("{}:{}", model.runtime, model.id),
                model.quantization.as_deref().unwrap_or("?"),
                model.size_bytes as f64 / GIB,
                model.required_bytes as f64 / GIB,
                state,
                speed
            );
            if let Some(alias) = &model.alias {
                println!("    \"{alias}\"");
            }
            if let Some(repo) = &model.repo {
                println!(
                    "    {repo}{}{}",
                    if model.mmproj.is_some() {
                        " · vision"
                    } else {
                        ""
                    },
                    if default { " · default" } else { "" }
                );
            }
        }
        for skipped in &listing.skipped {
            println!("skipped {}: {}", skipped.path, skipped.reason);
        }
        if !listing.models.is_empty() {
            println!(
                "\nMake one the default with `imperium models use <id>`, try it with \
                 `imperium models test <id>`, or pick it with --model."
            );
        }
        Ok(())
    })
}

pub fn install_runtime(pack: Option<String>) -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let settings = imperium_config::load(&paths, None, std::env::vars())?.settings()?;
    let hardware = probe(&settings.hardware);
    let packs = match pack {
        Some(pack) => vec![pack],
        None => packs_for(&settings.runtime.llamacpp.backend, &hardware),
    };
    println!(
        "installing llama.cpp {} from {LLAMA_CPP_REPOSITORY} (trying {})",
        llama_cpp_ref(),
        packs.join(", ")
    );
    // Whole percents, printed as they change.
    let shown = std::sync::Mutex::new((String::new(), u64::MAX));
    let progress = |pack: &str, done: u64, total: u64| {
        let percent = (done * 100).checked_div(total).unwrap_or(0);
        let mut shown = shown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if shown.0 == pack && (shown.1 == percent || !percent.is_multiple_of(10)) {
            return;
        }
        if shown.0 != pack {
            println!("{pack}: {:.0} MiB", total as f64 / MIB);
        }
        *shown = (pack.to_string(), percent);
        println!("  {percent}%");
    };
    block_on(async {
        let installed = install(
            &reqwest::Client::new(),
            GITHUB_API,
            paths.data_dir(),
            &packs,
            &hardware,
            &progress,
        )
        .await
        .with_context(|| format!("could not install {}", packs.join(" or ")))?;
        for skipped in &installed.skipped {
            println!("passed over {skipped}");
        }
        println!(
            "installed {} ({})",
            installed.pack,
            installed.server.display()
        );
        // A running daemon picks the new runtime up when asked for its status.
        if let Ok((mut client, _)) = connect().await
            && let Ok(status) = client.call::<RuntimeStatus>(RuntimeStatusParams {}).await
        {
            println!();
            print_runtime(&status);
        }
        Ok(())
    })
}
