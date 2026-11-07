mod chat;
mod local;
mod memory;
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
use imperium_protocol::methods::{Peer, Ping, PingParams};

/// Command-line client for the Imperium daemon.
#[derive(Debug, Parser)]
#[command(name = "imperium", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

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
    /// Local models in the configured model folders.
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
    match cli.command {
        Command::Ping => ping(),
        Command::Chat { project, model } => chat::run(project, model),
        Command::Config {
            command: ConfigCommand::Explain { key, project },
        } => explain(&key, project),
        Command::Secret {
            command: SecretCommand::Set { name },
        } => set_secret(&name),
        Command::Secret {
            command: SecretCommand::Delete { name },
        } => delete_secret(&name),
        Command::Models {
            command: ModelsCommand::Local,
        } => local::list(),
        Command::Runtime {
            command: RuntimeCommand::Install { pack },
        } => local::install_runtime(pack),
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
    let endpoint = read_endpoint(&paths.daemon_endpoint_file())?;
    let me = Peer {
        name: "imperium-cli".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    };
    let client = Client::connect(&endpoint, me).await.with_context(|| {
        format!(
            "imperiumd at {} did not answer; it may have stopped without removing {}",
            endpoint.url,
            paths.daemon_endpoint_file().display()
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
