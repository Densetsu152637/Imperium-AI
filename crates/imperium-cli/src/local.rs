//! `imperium models local|folders|use|test` and `imperium runtime install|update|status|use`.
//! Nothing is downloaded without the user's yes: `install` and `update` show the size and
//! source and ask first (or take `--yes`), and refuse while local models are off.

use std::path::Path;

use anyhow::Context;
use imperium_config::Paths;
use imperium_local::hardware::probe;
use imperium_local::runtime::{
    GITHUB_API, fetch_latest, fetch_release, install, llama_cpp_ref, packs_for, use_build,
};
use imperium_local::runtime_release::{SOURCE, plan};
use imperium_protocol::methods::{
    LocalModelList, LocalModelListParams, LocalModelListResult, ModelFolderAdd, ModelFolderParams,
    ModelFolderRemove, ModelRescan, ModelRescanParams, ModelTest, ModelTestParams, ModelUse,
    ModelUseParams, RUNTIME_INSTALL_PROGRESS, RuntimeCheckUpdate, RuntimeCheckUpdateParams,
    RuntimeDownloadOffer, RuntimeInstallJob, RuntimeSetServer, RuntimeSetServerParams,
    RuntimeStatus, RuntimeStatusParams, RuntimeStatusResult, RuntimeUpdate, RuntimeUpdateCheck,
    RuntimeUpdateParams,
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

pub fn rescan() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let result = client.call::<ModelRescan>(ModelRescanParams {}).await?;
        print_folders(&result.listing);
        for id in &result.added {
            println!("new: {id}");
        }
        for id in &result.removed {
            println!("gone: {id}");
        }
        print_local_off_note(&result.listing);
        Ok(())
    })
}

/// What `imperium models` says while `models.local.enabled` is off.
const LOCAL_OFF_NOTE: &str = "Local models are off by default, so none of these is used until you \
turn them on: `imperium config set models.local.enabled true` (or Settings -> Local models in \
the app). Each model also has its own switch (\"Use this model\").";

/// Says that local models are off (the default) and how to turn them on.
fn print_local_off_note(listing: &LocalModelListResult) {
    if !listing.local_enabled {
        println!("\n{LOCAL_OFF_NOTE}");
    }
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
    if let Some(check) = &status.update_check {
        print_update_available(check);
    }
    print_offers(&status.offers);
}

/// "update available: bNNNNN" when the last check found a newer llama.cpp.
fn print_update_available(check: &RuntimeUpdateCheck) {
    if !check.available {
        return;
    }
    let latest = check.latest.as_deref().unwrap_or("a newer build");
    match &check.current {
        Some(current) => println!("\nupdate available: {latest} (you have {current})"),
        None => println!("\nupdate available: {latest}"),
    }
    let size = check.bytes.map_or_else(String::new, |bytes| {
        format!(", {:.0} MiB", bytes as f64 / MIB)
    });
    let target = match (check.target.as_str(), &check.target_dir) {
        ("beside", Some(dir)) => format!(" into a new folder beside yours ({dir})"),
        _ => String::new(),
    };
    println!(
        "  `imperium runtime update` downloads it from {}{size}{target} after asking; nothing is \
         downloaded until you say yes",
        check.source
    );
}

/// The downloads on offer: what, from where, how big, and the command that approves each.
fn print_offers(offers: &[RuntimeDownloadOffer]) {
    for offer in offers {
        println!("\n{}", offer.message);
        let size = offer.bytes.map_or_else(String::new, |bytes| {
            format!(", {:.0} MiB", bytes as f64 / MIB)
        });
        println!(
            "  from {}{size}; approve with `{}`",
            offer.source, offer.command
        );
    }
}

pub fn runtime_status() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        // The daily check (cached; none when it is off or local models are off).
        let _ = client
            .call::<RuntimeCheckUpdate>(RuntimeCheckUpdateParams { force: false })
            .await;
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

pub fn use_server(path: Option<std::path::PathBuf>, build: Option<String>) -> anyhow::Result<()> {
    if let Some(build) = build {
        // A rollback (or going forward again) to a build already installed: nothing is
        // downloaded. The daemon is told to go back to the installed runtime so the build
        // chosen here is the one used.
        let paths = Paths::from_env()?;
        use_build(paths.data_dir(), &build).map_err(anyhow::Error::msg)?;
        println!("Imperium now uses llama.cpp {build}; the build used before it is kept.");
        return block_on(async {
            let (mut client, _) = connect().await?;
            let status = client
                .call::<RuntimeSetServer>(RuntimeSetServerParams { path: None })
                .await?;
            print_runtime(&status);
            Ok(())
        });
    }
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
        print_local_off_note(&listing);
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
            let state = if model.enabled {
                state
            } else if listing.local_enabled {
                format!("off (switched off); {state}")
            } else {
                format!("off (local models are off); {state}")
            };
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

/// Refuses `what` (a download or an install) while local models are off, the same refusal the
/// daemon gives `models.download` and `runtime.install`.
pub fn refuse_while_local_off(
    settings: &imperium_config::Settings,
    what: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        settings.models.local.enabled,
        "{what} is refused while local models are off (the default): turn them on first with `imperium config set models.local.enabled true` or in Settings -> Local models"
    );
    Ok(())
}

/// Says what installing `packs` downloads (the latest release, else the pinned one: what
/// [`install`] fetches) and asks for the yes; false when it was not given.
async fn approve_install(
    http: &reqwest::Client,
    packs: &[String],
    yes: bool,
) -> anyhow::Result<bool> {
    let release = match fetch_latest(http, GITHUB_API).await {
        Ok(release) => release,
        Err(error) => {
            println!(
                "could not read llama.cpp's latest release ({error}); trying the pinned {}",
                llama_cpp_ref()
            );
            fetch_release(http, GITHUB_API, llama_cpp_ref())
                .await
                .context("could not read llama.cpp's release from GitHub")?
        }
    };
    let planned = plan(&release, packs, GITHUB_API)
        .with_context(|| format!("llama.cpp {} cannot be installed here", release.tag_name))?;
    println!(
        "llama.cpp {} from {SOURCE}: the {} build, {:.0} MiB to download, verified against the \
         release's SHA-256 digests (tried in order: {})",
        planned.release,
        planned.pack,
        planned.bytes as f64 / MIB,
        packs.join(", ")
    );
    crate::vllm::confirm("Download and install it?", yes)
}

pub fn install_runtime(pack: Option<String>, yes: bool) -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let settings = imperium_config::load(&paths, None, std::env::vars())?.settings()?;
    refuse_while_local_off(&settings, "installing the llama.cpp runtime")?;
    let hardware = probe(&settings.hardware);
    let packs = match pack {
        Some(pack) => vec![pack],
        None => packs_for(&settings.runtime.llamacpp.backend, &hardware),
    };
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
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()?;
        if !approve_install(&http, &packs, yes).await? {
            println!("Nothing was downloaded.");
            return Ok(());
        }
        let installed = install(
            &http,
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
        // New loads use it from now on; the build used before stays for rollback.
        imperium_local::runtime::activate(paths.data_dir(), &installed.release)
            .context("the build was installed but could not be switched to")?;
        println!(
            "installed {} {} ({}); the build used before it is kept for rollback",
            installed.release,
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

/// `imperium runtime update`: the daemon looks for the latest release, this shows what it
/// would download and where it would go, asks, and follows the update to its end.
pub fn update_runtime(yes: bool) -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let settings = imperium_config::load(&paths, None, std::env::vars())?.settings()?;
    refuse_while_local_off(&settings, "updating the llama.cpp runtime")?;
    block_on(async {
        let (mut client, _) = connect().await?;
        let check = client
            .call::<RuntimeCheckUpdate>(RuntimeCheckUpdateParams { force: true })
            .await?;
        if !check.available {
            println!(
                "{}",
                check
                    .message
                    .as_deref()
                    .unwrap_or("llama.cpp is up to date.")
            );
            return Ok(());
        }
        let latest = check.latest.as_deref().unwrap_or("the latest release");
        let size = check.bytes.map_or_else(String::new, |bytes| {
            format!(", {:.0} MiB", bytes as f64 / MIB)
        });
        println!(
            "llama.cpp {latest} (you have {}) from {}: the {} build{size}, verified against the \
             release's SHA-256 digests",
            check.current.as_deref().unwrap_or("none"),
            check.source,
            check.pack.as_deref().unwrap_or("best"),
        );
        match (check.target.as_str(), &check.target_dir) {
            ("beside", Some(dir)) => println!(
                "It goes in a new folder beside yours ({dir}); your folder is left as it is and \
                 Imperium uses the new one once it starts and sees the GPU"
            ),
            (_, Some(dir)) => println!(
                "It goes in {dir}; the build in use is kept for rollback (`imperium runtime use \
                 --build <old>`) and is used until the new one starts and sees the GPU"
            ),
            _ => {}
        }
        if !crate::vllm::confirm("Download and install it?", yes)? {
            println!("Nothing was downloaded.");
            return Ok(());
        }
        let job = client.call::<RuntimeUpdate>(RuntimeUpdateParams {}).await?;
        follow_job(&mut client, job).await
    })
}

/// Prints an installation's progress (whole tens of percents) until it ends; an error when it
/// failed.
async fn follow_job(
    client: &mut imperium_client::Client,
    mut job: RuntimeInstallJob,
) -> anyhow::Result<()> {
    let mut shown = u64::MAX;
    loop {
        let percent = (job.done_bytes * 100)
            .checked_div(job.total_bytes)
            .unwrap_or(0);
        match job.state.as_str() {
            "done" => {
                println!("{}", job.message.as_deref().unwrap_or("done"));
                return Ok(());
            }
            "failed" => anyhow::bail!(
                "the update failed: {}",
                job.message.as_deref().unwrap_or("no reason given")
            ),
            _ if percent / 10 != shown => {
                shown = percent / 10;
                println!("{}: {percent}%", job.pack);
            }
            _ => {}
        }
        job = loop {
            let notification = client.next_notification().await?;
            if notification.method == RUNTIME_INSTALL_PROGRESS {
                break serde_json::from_value(notification.params)
                    .context("could not read the update's progress")?;
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_and_setups_are_refused_while_local_models_are_off() {
        let mut settings =
            imperium_config::Config::from_layers([imperium_config::Layer::defaults()])
                .settings()
                .unwrap();
        for what in [
            "installing the llama.cpp runtime",
            "updating the llama.cpp runtime",
            "setting up vLLM",
        ] {
            let error = refuse_while_local_off(&settings, what)
                .unwrap_err()
                .to_string();
            assert!(error.starts_with(what), "{error}");
            assert!(error.contains("models.local.enabled true"), "{error}");
        }
        settings.models.local.enabled = true;
        assert!(refuse_while_local_off(&settings, "installing the llama.cpp runtime").is_ok());
    }
}
