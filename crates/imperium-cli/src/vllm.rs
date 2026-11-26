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
use imperium_local::wsl;

use crate::block_on;

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Whether `question` is approved: by `--yes`, or by `yes` typed at a terminal. Without a
/// terminal there is no way to ask, which is an error rather than a silent no. `ask` shows the
/// question and reads the answer.
fn approve(
    question: &str,
    yes: bool,
    interactive: bool,
    ask: &mut dyn FnMut(&str) -> std::io::Result<String>,
) -> anyhow::Result<bool> {
    if yes {
        return Ok(true);
    }
    if !interactive {
        bail!(
            "this needs your approval: run it in a terminal, or pass --yes to approve what is \
             listed above"
        );
    }
    let answer = ask(question)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn confirm(question: &str, yes: bool) -> anyhow::Result<bool> {
    approve(
        question,
        yes,
        std::io::stdin().is_terminal(),
        &mut |question| {
            print!("{question} [y/N] ");
            std::io::stdout().flush()?;
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer)?;
            Ok(answer)
        },
    )
}

fn print_event(event: Event) {
    match event {
        Event::Step(text) => println!("==> {text}"),
        Event::Output(line) => println!("    {line}"),
        Event::Warning(text) => println!("warning: {text}"),
    }
}

/// Names (never values) of the `UV_*` and `PIP_*` variables set here: they change where `uv`
/// installs from and apply to the setup.
fn installer_variables(vars: impl Iterator<Item = (String, String)>) -> Vec<String> {
    let mut names: Vec<String> = vars
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("UV_") || name.starts_with("PIP_"))
        .collect();
    names.sort();
    names.dedup();
    names
}

pub fn setup(docker: bool, in_wsl: bool, yes: bool, version: Option<String>) -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let settings = imperium_config::load(&paths, None, std::env::vars())?.settings()?;
    if in_wsl && !cfg!(windows) {
        bail!(
            "--wsl is for Imperium running on Windows; on Linux, or inside WSL2, run this without \
             it"
        );
    }
    if !in_wsl && !cfg!(target_os = "linux") {
        bail!(
            "vLLM runs on Linux, including WSL2; on Windows run `imperium runtime vllm setup \
             --wsl` to set it up inside a WSL2 distribution (or run this inside WSL2, on a Linux \
             machine, or use a remote vLLM endpoint)"
        );
    }
    let mode = if docker {
        Mode::Docker
    } else if in_wsl {
        Mode::Wsl
    } else {
        Mode::Venv
    };
    mode.allowed(settings.daemon.server)
        .map_err(anyhow::Error::msg)?;
    if docker && version.is_some() {
        println!(
            "note: --version applies to the virtual environment; with --docker the image \
             (runtime.vllm.docker_image) names the release"
        );
    }
    let mut plan =
        Plan::new(mode, &settings.runtime.vllm, version.as_deref()).map_err(anyhow::Error::msg)?;
    if mode == Mode::Wsl {
        // `wsl.exe -l -v`: the configured distribution, else the default one; it must be WSL2.
        let distros = wsl::list_distros(std::time::Duration::from_secs(20))
            .map_err(anyhow::Error::msg)
            .context("could not list the WSL distributions")?;
        let chosen = wsl::choose_distro(&distros, plan.distro.as_deref().unwrap_or(""))
            .map_err(anyhow::Error::msg)?;
        plan.distro = Some(chosen.name);
    }
    vllm_env::check_existing(paths.data_dir(), &plan)?;
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
    if mode == Mode::Wsl {
        if !hardware.gpus.iter().any(|gpu| gpu.vendor == "nvidia") {
            println!(
                "  - no NVIDIA GPU was detected; vLLM in WSL2 needs one (setup checks that \
                 nvidia-smi works inside the distribution)"
            );
        }
        if let Some(problem) = wsl::forwarding_problem_here() {
            println!("  - warning: {problem}");
        }
    }
    if mode == Mode::Venv && !hardware.gpus.iter().any(|gpu| gpu.vendor == "nvidia") {
        println!(
            "  - the PyPI build of vLLM targets NVIDIA CUDA; on AMD or Intel use --docker with \
             runtime.vllm.docker_image set to a build for your GPU"
        );
    }
    let variables = installer_variables(std::env::vars());
    if mode == Mode::Venv && !variables.is_empty() {
        println!(
            "  - these variables are set here and apply to the install: {}",
            variables.join(", ")
        );
    }
    match &plan.distro {
        Some(distro) => println!(
            "  - where: inside the WSL2 distribution {distro}, under ~/{}",
            wsl::HOME_SUBDIR
        ),
        None => println!("  - where: {}", vllm_env::root(paths.data_dir()).display()),
    }
    if !confirm("Download and set this up now?", yes)? {
        println!("Nothing was changed.");
        return Ok(());
    }
    let runner = SystemRunner::default();
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
    let runner = SystemRunner {
        timeout: std::time::Duration::from_secs(30),
    };
    let status = vllm_env::status_checked(&runner, paths.data_dir());
    match &status.managed {
        None => {
            println!("No managed vLLM environment. Set one up with `imperium runtime vllm setup`");
            println!("(or `--docker`); nothing is downloaded until you confirm.");
        }
        Some(record) => {
            println!("Managed vLLM {} ({})", record.version, record.mode.name());
            if let Some(distro) = &record.distro {
                println!("  WSL2 distribution: {distro}");
            }
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
    let used = match vllm_env::resolve_for(
        paths.data_dir(),
        &settings.runtime.vllm,
        cfg!(windows),
        &wsl::default_distro_name,
    ) {
        Launch::Command(program) if cfg!(windows) => format!(
            "the command {}, which cannot run on Windows (run `imperium runtime vllm setup \
             --wsl`); vLLM models are unavailable",
            program.display()
        ),
        Launch::Command(program) => format!("the command {}", program.display()),
        Launch::Docker { image, .. } => format!("Docker image {image}"),
        Launch::Wsl {
            distro, program, ..
        } => format!("{program} inside the WSL2 distribution {distro}"),
    };
    println!("The daemon starts vLLM models with {used} (read when it starts).");
    Ok(())
}

pub fn remove(yes: bool) -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let Some(record) = vllm_env::managed(paths.data_dir()) else {
        // A record that is not setup's is reported by `remove` itself.
        let runner = SystemRunner::default();
        return match vllm_env::remove(&runner, paths.data_dir(), &mut print_event) {
            Ok(_) => {
                println!("No managed vLLM environment to remove.");
                Ok(())
            }
            Err(error) => bail!("{error}"),
        };
    };
    println!(
        "This removes the managed vLLM {} ({}) {}.",
        record.version,
        record.mode.name(),
        match (&record.mode, &record.image, &record.distro) {
            (Mode::Docker, Some(image), _) => format!("by removing the Docker image {image}"),
            (Mode::Wsl, _, Some(distro)) => format!(
                "from inside the WSL2 distribution {distro} (~/{}/{})",
                wsl::HOME_SUBDIR,
                record.version
            ),
            _ => format!("under {}", vllm_env::root(paths.data_dir()).display()),
        }
    );
    if block_on(async { Ok(crate::local_daemon_running().await) }).unwrap_or(false) {
        println!(
            "warning: the Imperium daemon is running and may have vLLM models loaded; they fail \
             once their files or image are gone. Unload them (or stop the daemon) first, and \
             restart the daemon afterwards."
        );
    }
    if !confirm("Remove it?", yes)? {
        println!("Nothing was changed.");
        return Ok(());
    }
    vllm_env::remove(&SystemRunner::default(), paths.data_dir(), &mut print_event)
        .map_err(anyhow::Error::msg)
        .context("could not remove the managed vLLM environment")?;
    println!("Removed. Restart the Imperium daemon so it stops using it.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked(answer: &str) -> impl FnMut(&str) -> std::io::Result<String> + '_ {
        move |_| Ok(answer.to_string())
    }

    #[test]
    fn yes_approves_without_asking() {
        let mut never = |_: &str| -> std::io::Result<String> { panic!("must not ask") };
        assert!(approve("Go?", true, false, &mut never).unwrap());
        assert!(approve("Go?", true, true, &mut never).unwrap());
    }

    #[test]
    fn without_a_terminal_and_without_yes_it_is_an_error_not_a_no() {
        let mut never = |_: &str| -> std::io::Result<String> { panic!("must not ask") };
        let error = approve("Go?", false, false, &mut never).unwrap_err();
        assert!(error.to_string().contains("--yes"), "{error}");
    }

    #[test]
    fn at_a_terminal_only_yes_approves() {
        for answer in ["y", "Y", "yes", " YES \n"] {
            assert!(
                approve("Go?", false, true, &mut asked(answer)).unwrap(),
                "{answer:?}"
            );
        }
        for answer in ["", "n", "no", "yep", "sure", "\n"] {
            assert!(
                !approve("Go?", false, true, &mut asked(answer)).unwrap(),
                "{answer:?}"
            );
        }
    }

    #[test]
    fn only_the_names_of_installer_variables_are_listed() {
        let vars = [
            ("UV_INDEX_URL", "https://user:secret@example/simple"),
            ("PIP_EXTRA_INDEX_URL", "x"),
            ("PATH", "/bin"),
            ("UV_INDEX_URL", "dup"),
            ("HF_TOKEN", "hf_secret"),
        ]
        .map(|(name, value)| (name.to_string(), value.to_string()));
        let names = installer_variables(vars.into_iter());
        assert_eq!(names, vec!["PIP_EXTRA_INDEX_URL", "UV_INDEX_URL"]);
        assert!(!names.join(",").contains("secret"));
    }
}
