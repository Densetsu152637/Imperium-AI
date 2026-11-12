mod api;
mod approvals;
mod autostart;
mod bench;
mod changes;
mod chat;
mod env;
mod local;
mod memory;
mod phone;
mod project;
mod pull;
mod remote;
mod resources;
mod schedule;
mod session;
mod settings;
mod workflow;
mod workspaces;

use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use imperium_cli::client::{Client, read_endpoint};
use imperium_config::{Paths, SecretName};
use imperium_protocol::methods::{
    ModelDownloadParams, Peer, Ping, PingParams, Shutdown, ShutdownParams,
};

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
    /// Check that imperium-core is running and measure a round trip.
    Ping,
    /// Chat with an agent in an environment or a project folder.
    Chat {
        /// Environment (name or id) the agent works in: all its folders are open to it. With
        /// neither this nor --project, the session starts in the built-in Home environment.
        #[arg(long, conflicts_with = "project")]
        env: Option<String>,
        /// Project folder the agent works in (its environment, made if needed); `--project .`
        /// is the current directory.
        #[arg(long)]
        project: Option<PathBuf>,
        /// Model as `<endpoint>:<model>` or a model table id; defaults to `models.default`.
        #[arg(long)]
        model: Option<String>,
        /// After each turn, print one line on how full the model's context is (type /context
        /// for the full breakdown at any time).
        #[arg(long)]
        context: bool,
        /// Start on git worktrees: each folder in a git repository gets a new worktree on a new
        /// branch (`imperium/<session id>`, or the branch given).
        #[arg(long, value_name = "BRANCH", num_args = 0..=1, default_missing_value = "")]
        worktree: Option<String>,
        /// Narrow the session to one folder inside the environment's folders (absolute, or
        /// relative to its first folder): the only folder the agent can use.
        #[arg(long, value_name = "FOLDER", conflicts_with = "worktree")]
        subfolder: Option<String>,
        /// Mount the session's folders read-only: the agent reads but never writes there, and
        /// commands run only under isolation.tier = os or container.
        #[arg(long, conflicts_with = "worktree")]
        read_only: bool,
    },
    /// Manage sessions (also `sessions`).
    #[command(alias = "sessions")]
    Session {
        #[command(subcommand)]
        command: SessionCommand,
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
    /// The OpenAI-compatible API other tools use your models through.
    Api {
        #[command(subcommand)]
        command: ApiCommand,
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
    /// Environments: named sets of folders sessions work in.
    Env {
        #[command(subcommand)]
        command: EnvCommand,
    },
    /// Environments' working folders: each one's `Memory.md` and its sessions' scratch folders.
    Workspaces {
        #[command(subcommand)]
        command: WorkspacesCommand,
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
    /// CPU, GPU, RAM, and VRAM usage, the local models loaded, and the endpoints sessions use.
    Resources {
        /// Keep printing a fresh snapshot every two seconds until Ctrl+C.
        #[arg(long)]
        watch: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ModelsCommand {
    /// List GGUF models in models.folders, whether each fits in memory, and what is loaded.
    Local,
    /// Unload an idle local model now, freeing its memory.
    Unload {
        /// The local model id, as `imperium resources` lists it.
        model: String,
    },
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
    /// List, add, or remove model folders (models.folders); changes are scanned at once.
    Folders {
        #[command(subcommand)]
        command: Option<FoldersCommand>,
    },
    /// Make a model the default for new sessions (models.default).
    Use {
        /// A local model id (from `imperium models local`), a model-table id, or
        /// <endpoint>:<model>.
        model: String,
    },
    /// Load a model and send it one short prompt, showing the reply and its speed.
    Test {
        /// As for `use`.
        model: String,
        /// The prompt to send instead of a short greeting.
        #[arg(long)]
        prompt: Option<String>,
    },
    /// Benchmark local models: load time, time to first token, prompt and generation speed,
    /// peak memory. Loads each model like any call does, waiting while the machine is saturated; saves
    /// the numbers per model and machine. Ctrl+C cancels.
    Bench {
        /// A local model id (from `imperium models local`); every model that can run when
        /// left out.
        model: Option<String>,
        /// List the saved results instead of running a benchmark.
        #[arg(long)]
        results: bool,
        /// Do not unload an idle loaded model first (its load time is then not measured).
        #[arg(long, conflicts_with = "results")]
        warm: bool,
    },
}

#[derive(Debug, Subcommand)]
enum FoldersCommand {
    /// List the model folders (the default).
    List,
    /// Add a folder and scan it.
    Add { path: PathBuf },
    /// Stop scanning a folder; nothing in it is touched.
    Remove { path: PathBuf },
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
    /// Start imperium-core when you log in (enable, disable, or status).
    Autostart {
        #[arg(value_parser = ["enable", "disable", "status"])]
        action: String,
    },
    /// Start imperium-core in the background.
    Start,
    /// Stop the running imperium-core gracefully.
    Stop,
    /// Print the running daemon's endpoint as JSON (used by `imperium remote connect`).
    Endpoint,
}

#[derive(Debug, Subcommand)]
enum ApiCommand {
    /// Show whether the API is on, where it listens, and the models it serves.
    Status,
    /// Turn the API on (the first time it also picks a port) and show how to use it.
    Enable,
    /// Turn the API off.
    Disable,
    /// Show the API token, or make a new one that replaces it.
    Token {
        #[command(subcommand)]
        command: Option<TokenCommand>,
    },
}

#[derive(Debug, Subcommand)]
enum TokenCommand {
    /// Replace the token; the old one stops working at once.
    Rotate,
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
    /// Forward a host's imperium-core over SSH (starting it there if needed) until Ctrl+C.
    Connect {
        /// An SSH destination: user@host, or a Host from ~/.ssh/config.
        destination: String,
        /// Name used with --remote; defaults to the destination.
        #[arg(long)]
        name: Option<String>,
        /// Local port for the forward; a free one by default.
        #[arg(long)]
        local_port: Option<u16>,
        /// Forward the daemon's local socket (needs `daemon.socket` on the host and a Unix
        /// machine here) instead of its TCP port.
        #[arg(long)]
        socket: bool,
    },
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    /// List running sessions (with --archived, the archived ones).
    List {
        /// Show archived sessions instead.
        #[arg(long)]
        archived: bool,
    },
    /// Archive a session: it stops, keeps its history, and does not resume after a restart.
    Archive { session_id: String },
    /// Resume an archived session.
    Unarchive { session_id: String },
    /// Export a session's transcript (messages, tool calls, subagents, files), also an
    /// archived session's: Markdown or JSON, to a file or standard output.
    Export {
        session_id: String,
        /// `md` (default) or `json`.
        #[arg(long, value_name = "FORMAT", default_value = "md")]
        format: String,
        /// Leave out each subagent's own log (their tasks and reports stay).
        #[arg(long)]
        no_subagents: bool,
        /// Write to this file instead of standard output.
        #[arg(short = 'o', long, value_name = "FILE")]
        output: Option<PathBuf>,
    },
    /// Stop and forget a session so it does not continue after a restart.
    Delete {
        session_id: String,
        /// Also remove the session's git worktrees (their branches are kept).
        #[arg(long)]
        remove_worktrees: bool,
    },
}

#[derive(Debug, Subcommand)]
enum AgentCommand {
    /// List the environment's persistent agents.
    List {
        /// Environment name or id.
        env: String,
    },
    /// Add a persistent agent; it starts at once.
    Add {
        /// Environment name or id.
        env: String,
        /// The agent's name, unique in the environment.
        name: String,
        /// What the agent is for, added to its instructions.
        #[arg(long)]
        role: Option<String>,
    },
    /// Rename an agent or change its role.
    Set {
        /// Environment name or id.
        env: String,
        /// The agent's name or id.
        agent: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        role: Option<String>,
    },
    /// Delete an agent (its session is archived).
    Rm {
        /// Environment name or id.
        env: String,
        /// The agent's name or id.
        agent: String,
    },
}

#[derive(Debug, Subcommand)]
enum WorkspacesCommand {
    /// Show where the working folders are, and each environment's.
    Where,
    /// Move the working folders to another folder (it must be empty or new, and outside the
    /// current one) and make it `workspaces.root`. An empty path goes back to the default.
    Move { path: PathBuf },
}

#[derive(Debug, Subcommand)]
enum EnvCommand {
    /// List environments and their folders.
    List,
    /// Create an environment, optionally with folders (the first is the working directory).
    Create { name: String, folders: Vec<PathBuf> },
    /// Add a folder to an environment.
    AddFolder {
        /// Environment name or id.
        env: String,
        folder: PathBuf,
    },
    /// Remove a folder from an environment (the folder itself is untouched).
    RemoveFolder {
        /// Environment name or id.
        env: String,
        folder: PathBuf,
    },
    /// Rename an environment.
    Rename {
        /// Environment name or id.
        env: String,
        name: String,
    },
    /// Delete an environment (its folders are untouched).
    Delete {
        /// Environment name or id.
        env: String,
    },
    /// Choose where an environment's tools run: local, docker, wsl, or ssh. For a remote
    /// runtime, its folders are paths inside it.
    SetRuntime {
        /// Environment name or id.
        env: String,
        /// `local`, `docker`, `wsl`, or `ssh`.
        kind: String,
        /// docker: the image (each command runs in a fresh container with the folders mounted).
        #[arg(long)]
        image: Option<String>,
        /// docker: a running container to `docker exec` into instead.
        #[arg(long)]
        container: Option<String>,
        /// wsl: the distribution.
        #[arg(long)]
        distro: Option<String>,
        /// ssh: a host alias from your SSH config, or user@host[:port].
        #[arg(long)]
        host: Option<String>,
    },
    /// Check that an environment's runtime can be reached.
    Test {
        /// Environment name or id.
        env: String,
    },
    /// Show what the environment's own `.imperium/config.toml` changes for its sessions, and
    /// what it asked for that was refused (it can only narrow your settings).
    Config {
        /// Environment name or id.
        env: String,
    },
    /// Manage an environment's persistent agents (none exist until added).
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
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
    /// Download llama.cpp's own build for this machine from its GitHub release, verified
    /// against the release's SHA-256 digests; falls back to the next build (CUDA 13, CUDA 12,
    /// Vulkan, CPU) when one does not run here.
    Install {
        /// Build to install, such as `windows-x64-vulkan`; defaults to the configured backend
        /// (`runtime.llamacpp.backend`), or the best that runs on this machine.
        #[arg(long)]
        pack: Option<String>,
    },
    /// Show whether llama-server runs and sees the GPU, and what to do when it does not.
    Status,
    /// List the llama-server executables found on this machine, including unpacked downloads
    /// in the home and Downloads folders. None is run until you adopt one with
    /// `imperium runtime use <path>` or `imperium config set runtime.llamacpp.custom_path <path>`.
    Candidates,
    /// Use an existing llama-server (runtime.llamacpp.custom_path); without a path, go back
    /// to the installed runtime.
    Use { path: Option<PathBuf> },
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
    /// Set a setting in your config file, such as `imperium config set policy.personality
    /// luxion`. Keys are those in the desktop Settings window (docs/10-configuration.md). With a
    /// daemon running the change goes through it, so settings it applies at once take effect.
    Set {
        /// Dotted key, for example `policy.personality`.
        key: String,
        /// The value; several for a list setting.
        #[arg(required = true, num_args = 1..)]
        value: Vec<String>,
    },
    /// Remove a setting from your config file, so its default applies.
    Unset {
        /// Dotted key, for example `policy.personality`.
        key: String,
    },
    /// List the changes agents made to the app config (model cards, installed and removed
    /// mods, plugins, and skills), newest first.
    Changes,
    /// Undo a change listed by `imperium config changes`.
    Undo { id: u64 },
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
            env,
            project,
            model,
            context,
            worktree,
            subfolder,
            read_only,
        } => chat::run(
            env,
            project,
            model,
            context,
            worktree.map(|branch| (!branch.is_empty()).then_some(branch)),
            subfolder,
            read_only,
        ),
        Command::Session { command } => match command {
            SessionCommand::List { archived } => session::list(archived),
            SessionCommand::Archive { session_id } => session::set_archived(session_id, true),
            SessionCommand::Unarchive { session_id } => session::set_archived(session_id, false),
            SessionCommand::Export {
                session_id,
                format,
                no_subagents,
                output,
            } => session::export(session_id, &format, no_subagents, output),
            SessionCommand::Delete {
                session_id,
                remove_worktrees,
            } => env::delete_session(session_id, remove_worktrees),
        },
        Command::Workspaces { command } => match command {
            WorkspacesCommand::Where => workspaces::show(),
            WorkspacesCommand::Move { path } => workspaces::move_to(&path),
        },
        Command::Env { command } => match command {
            EnvCommand::List => env::list(),
            EnvCommand::Create { name, folders } => env::create(name, folders),
            EnvCommand::AddFolder { env: id, folder } => env::add_folder(id, folder),
            EnvCommand::RemoveFolder { env: id, folder } => env::remove_folder(id, folder),
            EnvCommand::Rename { env: id, name } => env::rename(id, name),
            EnvCommand::Delete { env: id } => env::delete(id),
            EnvCommand::SetRuntime {
                env: id,
                kind,
                image,
                container,
                distro,
                host,
            } => env::set_runtime(id, env::runtime(&kind, image, container, distro, host)?),
            EnvCommand::Test { env: id } => env::test(id),
            EnvCommand::Config { env: id } => env::config(id),
            EnvCommand::Agent { command } => match command {
                AgentCommand::List { env: id } => env::agent_list(id),
                AgentCommand::Add {
                    env: id,
                    name,
                    role,
                } => env::agent_add(id, name, role),
                AgentCommand::Set {
                    env: id,
                    agent,
                    name,
                    role,
                } => env::agent_set(id, agent, name, role),
                AgentCommand::Rm { env: id, agent } => env::agent_remove(id, agent),
            },
        },
        Command::Config {
            command: ConfigCommand::Explain { key, project },
        } => explain(&key, project),
        Command::Config {
            command: ConfigCommand::Set { key, value },
        } => settings::set(&key, value),
        Command::Config {
            command: ConfigCommand::Unset { key },
        } => settings::unset(&key),
        Command::Config {
            command: ConfigCommand::Changes,
        } => changes::list(),
        Command::Config {
            command: ConfigCommand::Undo { id },
        } => changes::undo(id),
        Command::Secret {
            command: SecretCommand::Set { name },
        } => set_secret(&name),
        Command::Secret {
            command: SecretCommand::Delete { name },
        } => delete_secret(&name),
        Command::Models { command } => match command {
            ModelsCommand::Local => local::list(),
            ModelsCommand::Unload { model } => resources::unload(model),
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
            ModelsCommand::Folders { command } => match command {
                None | Some(FoldersCommand::List) => local::folders(),
                Some(FoldersCommand::Add { path }) => local::add_folder(&path),
                Some(FoldersCommand::Remove { path }) => local::remove_folder(&path),
            },
            ModelsCommand::Use { model } => local::use_model(model),
            ModelsCommand::Test { model, prompt } => local::test_model(model, prompt),
            ModelsCommand::Bench {
                model,
                results,
                warm,
            } => {
                if results {
                    bench::results(model)
                } else {
                    bench::run(model, warm)
                }
            }
        },
        Command::Runtime {
            command: RuntimeCommand::Install { pack },
        } => local::install_runtime(pack),
        Command::Runtime {
            command: RuntimeCommand::Status,
        } => local::runtime_status(),
        Command::Runtime {
            command: RuntimeCommand::Candidates,
        } => local::runtime_candidates(),
        Command::Runtime {
            command: RuntimeCommand::Use { path },
        } => local::use_server(path),
        Command::Resources { watch } => resources::show(watch),
        Command::Api { command } => match command {
            ApiCommand::Status => api::status(),
            ApiCommand::Enable => api::set_enabled(true),
            ApiCommand::Disable => api::set_enabled(false),
            ApiCommand::Token { command } => api::token(command.is_some()),
        },
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
                    socket,
                },
        } => remote::connect(&destination, name, local_port, socket),
        Command::Daemon {
            command: DaemonCommand::Start,
        } => remote::start_daemon(),
        Command::Daemon {
            command: DaemonCommand::Stop,
        } => stop_daemon(),
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

fn stop_daemon() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, url) = connect().await?;
        let stopped = client.call::<Shutdown>(ShutdownParams {}).await?;
        println!("imperium-core (pid {}) at {url} is stopping", stopped.pid);
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
            "imperium-core at {} did not answer; it may have stopped without removing {}",
            endpoint.url,
            file.display()
        )
    })?;
    Ok((client, endpoint.url))
}

fn explain(key: &str, project: Option<PathBuf>) -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let config = imperium_config::load(&paths, project.as_deref(), std::env::vars())?;
    for warning in config.warnings() {
        eprintln!("note: {warning}");
    }
    if let Some(project) = &project {
        // The project's own file is not a layer; this is what it narrows.
        let narrowed = imperium_config::EnvironmentConfig::load(project, "project")
            .apply(&config.settings()?);
        for line in &narrowed.applied {
            println!("project file applies: {line}");
        }
        for line in &narrowed.refused {
            println!("project file refused: {line}");
        }
    }
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
