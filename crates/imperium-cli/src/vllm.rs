//! `imperium runtime vllm setup|status|remove`: the managed vLLM environment
//! ([`imperium_local::vllm_env`]). Nothing here runs unless the user types the command, and
//! setup and removal ask first.

use std::io::{IsTerminal, Write};

use anyhow::{Context, bail};
use imperium_config::Paths;
use imperium_local::hardware::probe;
use imperium_local::vllm_env::{
    self, Event, Launch, Mode, Plan, SystemRunner, pinned_image, pinned_version,
};

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Asks `question` at a terminal; `--yes` answers it, and without a terminal there is no way to.
fn confirm(question: &str, yes: bool) -> anyhow::Result<bool> {
    if yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        bail!(
            "this needs your approval: run it in a terminal, or pass --yes to approve what is listed above"
        );
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn print_event(event: Event) {
    match event {
        Event::Step(text) => println!("==> {text}"),
        Event::Output(line) => println!("    {line}"),
        Event::Warning(text) => println!("warning: {text}"),
    }
}

pub fn setup(docker: bool, yes: bool, version: Option<String>) -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let settings = imperium_config::load(&paths, None, std::env::vars())?.settings()?;
    if !cfg!(target_os = "linux") {
        bail!(
            "vLLM runs on Linux, including WSL2; run this inside WSL2 (or on a Linux machine, or \
             use a remote vLLM endpoint)"
        );
    }
    let mode = if docker { Mode::Docker } else { Mode::Venv };
    let plan =
        Plan::new(mode, &settings.runtime.vllm, version.as_deref()).map_err(anyhow::Error::msg)?;
    if let Some(existing) = vllm_env::managed(paths.data_dir())
        && (existing.mode != plan.mode || existing.version != plan.version)
    {
        bail!(
            "vLLM {} ({}) is set up; run `imperium runtime vllm remove` first to change it",
            existing.version,
            existing.mode.name()
        );
    }
    println!(
        "Managed vLLM ({}), pinned {} / {}",
        mode.name(),
        pinned_version(),
        pinned_image()
    );
    for line in plan.requirements() {
        println!("  - {line}");
    }
    let hardware = probe(&settings.hardware);
    if hardware.gpus.is_empty() {
        println!("  - no GPU was detected on this machine; vLLM needs one to serve a model");
    }
    for gpu in &hardware.gpus {
        println!(
            "  - detected {} ({}), {:.0} MiB",
            gpu.name,
            gpu.vendor,
            mib(gpu.vram_total_bytes)
        );
    }
    if mode == Mode::Venv && !hardware.gpus.iter().any(|gpu| gpu.vendor == "nvidia") {
        println!(
            "  - the PyPI build of vLLM targets NVIDIA CUDA; on AMD or Intel use --docker with \
             runtime.vllm.docker_image set to a build for your GPU"
        );
    }
    println!("  - where: {}", vllm_env::root(paths.data_dir()).display());
    if !confirm("Download and set this up now?", yes)? {
        println!("Nothing was changed.");
        return Ok(());
    }
    let runner = SystemRunner;
    match vllm_env::setup(&runner, paths.data_dir(), &plan, &mut print_event) {
        Ok(record) => {
            println!(
                "\nvLLM {} is set up ({}). Restart the Imperium daemon to use it for `vllm:` \
                 models; remove it with `imperium runtime vllm remove`.",
                record.version,
                record.mode.name()
            );
            Ok(())
        }
        Err(error) => {
            eprintln!("\nSetup stopped at {}: {}", error.step, error.message);
            for line in &error.tail {
                eprintln!("    {line}");
            }
            bail!("vLLM was not set up")
        }
    }
}

pub fn status() -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let settings = imperium_config::load(&paths, None, std::env::vars())?.settings()?;
    let status = vllm_env::status(paths.data_dir());
    match &status.managed {
        None => {
            println!("No managed vLLM environment. Set one up with `imperium runtime vllm setup`");
            println!("(or `--docker`); nothing is downloaded until you confirm.");
        }
        Some(record) => {
            println!("Managed vLLM {} ({})", record.version, record.mode.name());
            if let Some(path) = &status.path {
                println!("  environment: {}", path.display());
            }
            if let Some(image) = &record.image {
                println!("  image: {image}");
            }
            if let Some(size) = status.size_bytes {
                println!("  size on disk: {:.0} MiB", mib(size));
            }
            if !status.present {
                println!(
                    "  its files are missing; run `imperium runtime vllm remove`, then set it up again"
                );
            }
        }
    }
    let used = match vllm_env::resolve(paths.data_dir(), &settings.runtime.vllm.command) {
        Launch::Command(program) => format!("the command {}", program.display()),
        Launch::Docker { image, .. } => format!("Docker image {image}"),
    };
    println!("The daemon starts vLLM models with {used} (read when it starts).");
    Ok(())
}

pub fn remove(yes: bool) -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let Some(record) = vllm_env::managed(paths.data_dir()) else {
        println!("No managed vLLM environment to remove.");
        return Ok(());
    };
    println!(
        "This removes the managed vLLM {} ({}) {}.",
        record.version,
        record.mode.name(),
        match (&record.mode, &record.image) {
            (Mode::Docker, Some(image)) => format!("by removing the Docker image {image}"),
            _ => format!("under {}", vllm_env::root(paths.data_dir()).display()),
        }
    );
    if !confirm("Remove it?", yes)? {
        println!("Nothing was changed.");
        return Ok(());
    }
    vllm_env::remove(&SystemRunner, paths.data_dir(), &mut print_event)
        .map_err(anyhow::Error::msg)
        .context("could not remove the managed vLLM environment")?;
    println!("Removed. Restart the Imperium daemon so it stops using it.");
    Ok(())
}
