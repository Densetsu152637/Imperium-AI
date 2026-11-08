mod approvals;
mod autostart;
mod chat;
mod local;
mod memory;
mod phone;
mod project;
mod pull;
mod remote;
mod schedule;
mod workflow;

use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use imperium_cli::client::{Client, read_endpoint};
use imperium_config::{Paths, SecretName};
use imperium_protocol::methods::{ModelDownloadParams, Peer, Ping, PingParams};

/// Command-line client for the Imperium daemon.
#[derive(Debug, Parser)]
#[command(name = "imperium", version)]
struct Cli {
    /// Talk to a remote daemon connected with `imperium remote connect`.
    #[arg(long, global = true)]
    remote: Option<String>,
    #[command(subcommand)]
    command: Command,
}

static REMOTE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

#[derive(Debug, Subcommand)]
enum Command {
    /// Check that imperiumd is running and measure a round trip.
    Ping,
    /// Chat with an agent in a project folder.
    Chat {
        /// Project folder the agent works in; defaults to the current directory.
        #[arg(long)]
        project: Option<PathBuf>,
        /// Model as `<endpoint>:<model>`; defaults to `models.default`.
        #[arg(long)]
        model: Option<String>,
        /// After each turn, print one line on how full the model's context is (type /context
        /// for the full breakdown at any time).
        #[arg(long)]
        context: bool,
    },
    /// Inspect configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Manage secrets in the OS keychain.
    Secret {
        #[command(subcommand)]
        command: SecretCommand,
    },
    /// Local models: the configured model folders, and downloads from Hugging Face.
    Models {
        #[command(subcommand)]
        command: ModelsCommand,
    },
    /// Saved memories for the current project (or --project) and global ones.
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
        /// Project folder; defaults to the current directory.
        #[arg(long, global = true)]
        project: Option<PathBuf>,
    },
    /// Paired phones.
    Phone {
        #[command(subcommand)]
        command: PhoneCommand,
    },
    /// Remote hosts over SSH.
    Remote {
        #[command(subcommand)]
        command: RemoteCommand,
    },
    /// The daemon itself.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Project agents.
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Requests from unattended work (such as auto-fix pushes) waiting for a yes or no.
    Approvals {
        #[command(subcommand)]
        command: ApprovalsCommand,
    },
    /// Scheduled tasks the daemon runs.
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },
    /// Workflow packs and context retrieval.
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
    },
    /// Inference runtimes.
    Runtime {
        #[command(subcommand)]
        command: RuntimeCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ModelsCommand {
    /// List GGUF models in models.folders, whether each fits in memory, and what is loaded.
    Local,
    /// List the quantisations a Hugging Face repository offers, with sizes and whether each
    /// fits this machine.
    Files {
        /// The repository, as owner/name.
        repo: String,
        /// A branch, tag, or commit.
        #[arg(long)]
        revision: Option<String>,
    },
    /// Download a model from Hugging Face into models.download_dir, showing progress.
    Pull {
        /// The repository, as owner/name.
        repo: String,
        /// The GGUF quantisation, such as Q4_K_M (the default); any case.
        #[arg(long, conflicts_with = "file")]
        quant: Option<String>,
        /// A file by name or path instead of a quantisation; every part when it is split.
        #[arg(long)]
        file: Option<String>,
        /// A branch, tag, or commit.
        #[arg(long, default_value = "main")]
        revision: String,
        /// Also download the multimodal projector (mmproj).
        #[arg(long)]
        mmproj: bool,
    },
    /// Stop a download; pulling the same model again resumes it.
    Cancel {
        /// The job id `pull` printed, such as dl-1.
        job_id: String,
    },
}

#[derive(Debug, Subcommand)]
enum MemoryCommand {
    /// List memories: global, the project's shared ones, and your private project notes.
    List,
    /// Find memories relevant to a query.
    Search { query: String },
    /// Show one memory.
    Show { name: String },
    /// Delete a memory.
    Forget {
        name: String,
        /// global, project, or project-private; defaults to where the name is found.
        #[arg(long)]
        scope: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    /// Start imperiumd when you log in (enable, disable, or status).
    Autostart {
        #[arg(value_parser = ["enable", "disable", "status"])]
        action: String,
    },
    /// Start imperiumd in the background.
    Start,
    /// Print the running daemon's endpoint as JSON (used by `imperium remote connect`).
    Endpoint,
}

#[derive(Debug, Subcommand)]
enum PhoneCommand {
    /// Show a one-time code for pairing a phone.
    Pair,
    /// List paired phones.
    Devices,
    /// Unpair a phone.
    Revoke { id: String },
}

#[derive(Debug, Subcommand)]
enum RemoteCommand {
    /// Forward a host's imperiumd over SSH (starting it there if needed) until Ctrl+C.
    Connect {
        /// An SSH destination: user@host, or a Host from ~/.ssh/config.
        destination: String,
        /// Name used with --remote; defaults to the destination.
        #[arg(long)]
        name: Option<String>,
        /// Local port for the forward; a free one by default.
        #[arg(long)]
        local_port: Option<u16>,
    },
}

#[derive(Debug, Subcommand)]
enum ProjectCommand {
    /// List projects with a running agent.
    List,
    /// Send a message to a project's agent.
    Tell {
        text: String,
        /// Project folder; defaults to the current directory.
        #[arg(long)]
        project: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum ApprovalsCommand {
    /// List pending requests.
    List,
    /// Approve a request.
    Approve { id: u64 },
    /// Decline a request.
    Deny { id: u64 },
}

#[derive(Debug, Subcommand)]
enum ScheduleCommand {
    /// List schedules with their next and last runs.
    List,
    /// Run a schedule now.
    Run { id: String },
}

#[derive(Debug, Subcommand)]
enum WorkflowCommand {
    /// Show the load manifest an agent in a role would get for a task.
    Resolve {
        /// The task or message.
        task: String,
        /// root, manager, leaf, or a role the pack's manifest defines.
        #[arg(long, default_value = "root")]
        role: String,
        /// Project folder; defaults to the current directory.
        #[arg(long)]
        project: Option<PathBuf>,
        /// Workflow folders to use instead of workflow.packs.
        #[arg(long = "pack")]
        packs: Vec<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum RuntimeCommand {
    /// Download and verify the llama.cpp runtime pack for this machine.
    Install {
        /// Pack to install, such as `windows-x64-vulkan`; defaults to the configured backend
        /// (`runtime.llamacpp.backend`), or the one that suits this machine.
        #[arg(long)]
        pack: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Show a setting's effective value and which layer set it.
    Explain {
        /// Dotted key, for example `daemon.port`.
        key: String,
        /// Include the project layers of this directory.
        #[arg(long)]
        project: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum SecretCommand {
    /// Store a secret. The value is read from standard input, or prompted for without echo.
    Set { name: SecretName },
    /// Remove a secret.
    Delete { name: SecretName },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    let _ = REMOTE.set(cli.remote);
    match cli.command {
        Command::Ping => ping(),
        Command::Chat {
            project,
            model,
            context,
        } => chat::run(project, model, context),
        Command::Config {
            command: ConfigCommand::Explain { key, project },
        } => explain(&key, project),
        Command::Secret {
            command: SecretCommand::Set { name },
        } => set_secret(&name),
        Command::Secret {
            command: SecretCommand::Delete { name },
        } => delete_secret(&name),
        Command::Models { command } => match command {
            ModelsCommand::Local => local::list(),
            ModelsCommand::Files { repo, revision } => pull::files(repo, revision),
            ModelsCommand::Pull {
                repo,
                quant,
                file,
                revision,
                mmproj,
            } => pull::pull(ModelDownloadParams {
                repo,
                quant,
                file,
                revision: Some(revision),
                mmproj,
            }),
            ModelsCommand::Cancel { job_id } => pull::cancel(job_id),
        },
        Command::Runtime {
            command: RuntimeCommand::Install { pack },
        } => local::install_runtime(pack),
        Command::Phone { command } => match command {
            PhoneCommand::Pair => phone::pair(),
            PhoneCommand::Devices => phone::devices(),
            PhoneCommand::Revoke { id } => phone::revoke(id),
        },
        Command::Remote {
            command:
                RemoteCommand::Connect {
                    destination,
                    name,
                    local_port,
                },
        } => remote::connect(&destination, name, local_port),
        Command::Daemon {
            command: DaemonCommand::Start,
        } => remote::start_daemon(),
        Command::Daemon {
            command: DaemonCommand::Endpoint,
        } => remote::print_endpoint(),
        Command::Daemon {
            command: DaemonCommand::Autostart { action },
        } => match action.as_str() {
            "enable" => autostart::enable(),
            "disable" => autostart::disable(),
            _ => autostart::status(),
        },
        Command::Project { command } => match command {
            ProjectCommand::List => project::list(),
            ProjectCommand::Tell { text, project } => project::tell(text, project),
        },
        Command::Approvals { command } => match command {
            ApprovalsCommand::List => approvals::list(),
            ApprovalsCommand::Approve { id } => approvals::respond(id, true),
            ApprovalsCommand::Deny { id } => approvals::respond(id, false),
        },
        Command::Schedule {
            command: ScheduleCommand::List,
        } => schedule::list(),
        Command::Schedule {
            command: ScheduleCommand::Run { id },
        } => schedule::run(id),
        Command::Workflow {
            command:
                WorkflowCommand::Resolve {
                    task,
                    role,
                    project,
                    packs,
                },
        } => workflow::resolve(&role, &task, project, packs),
        Command::Memory { command, project } => match command {
            MemoryCommand::List => memory::list(project),
            MemoryCommand::Search { query } => memory::search(&query, project),
            MemoryCommand::Show { name } => memory::show(&name, project),
            MemoryCommand::Forget { name, scope } => memory::forget(&name, scope, project),
        },
    }
}

fn ping() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, url) = connect().await?;
        let sent = Instant::now();
        let pong = client.call::<Ping>(PingParams {}).await?;
        let round_trip = sent.elapsed();
        let daemon = client.daemon();
        println!(
            "{} {} (protocol {}) at {url}: up {:.1} s, round trip {:.2} ms",
            daemon.daemon.name,
            daemon.daemon.version,
            daemon.protocol,
            pong.uptime_ms as f64 / 1000.0,
            round_trip.as_secs_f64() * 1000.0,
        );
        Ok(())
    })
}

/// Runs a future on a single-threaded runtime; the CLI does one thing at a time.
fn block_on<T>(future: impl std::future::Future<Output = anyhow::Result<T>>) -> anyhow::Result<T> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("could not start the async runtime")?
        .block_on(future)
}

/// Connects to the running daemon and returns the client and the daemon's URL.
async fn connect() -> anyhow::Result<(Client, String)> {
    let paths = Paths::from_env()?;
    let file = match REMOTE.get().and_then(Option::as_deref) {
        Some(name) => remote::endpoint_file(&paths, name),
        None => paths.daemon_endpoint_file(),
    };
    let endpoint = read_endpoint(&file)?;
    let me = Peer {
        name: "imperium-cli".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    };
    let client = Client::connect(&endpoint, me).await.with_context(|| {
        format!(
            "imperiumd at {} did not answer; it may have stopped without removing {}",
            endpoint.url,
            file.display()
        )
    })?;
    Ok((client, endpoint.url))
}

fn explain(key: &str, project: Option<PathBuf>) -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let config = imperium_config::load(&paths, project.as_deref(), std::env::vars())?;
    let Some(explained) = config.explain(key) else {
        bail!("{key} is not set");
    };
    match explained.source {
        Some(source) => println!(
            "{key} = {}
  set by {source}",
            explained.value
        ),
        None => {
            let section =
                toml::to_string(explained.value).context("could not format the section")?;
            println!(
                "[{key}]
{section}(a section; explain a key inside it to see its source)"
            );
        }
    }
    Ok(())
}

fn set_secret(name: &SecretName) -> anyhow::Result<()> {
    let value = if std::io::stdin().is_terminal() {
        rpassword::prompt_password(format!("Value for {name}: "))
            .context("could not read the value")?
    } else {
        let mut value = String::new();
        std::io::stdin()
            .read_to_string(&mut value)
            .context("could not read the value from standard input")?;
        value.trim_end_matches(['\r', '\n']).to_string()
    };
    imperium_config::store_secret(name, &value)?;
    println!("stored {name}");
    Ok(())
}

fn delete_secret(name: &SecretName) -> anyhow::Result<()> {
    if imperium_config::delete_secret(name)? {
        println!("deleted {name}");
    } else {
        println!("{name} was not stored");
    }
    Ok(())
}
