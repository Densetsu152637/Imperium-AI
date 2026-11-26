mod agents;
mod api;
mod approvals;
mod autostart;
mod bench;
mod changes;
mod chat;
mod env;
mod initiative;
mod lifetime;
mod local;
mod mcp_login;
mod memory;
mod mods;
mod pack;
mod phone;
mod plugins;
mod project;
mod pull;
mod remote;
mod resources;
mod schedule;
mod session;
mod settings;
mod vllm;
mod workflow;
mod workspaces;

use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use imperium_cli::client::launch::{Launch, LeaseRequest, attach, find_daemon};
use imperium_cli::client::{Client, ClientError, read_endpoint};
use imperium_cli::mcp_bridge::BridgeOptions;
use imperium_config::{Paths, SecretName, SecretStore};
use imperium_protocol::methods::{
    LeaseKind, ModelDownloadParams, Peer, Ping, PingParams, Shutdown, ShutdownParams,
};

/// The release tag and short commit this build came from, as `--version` prints them
/// (`0.2.281-experimental (223a0d02)`; `0.2.0-dev (<commit or unknown>)` for a local build).
pub(crate) const BUILD_VERSION: &str = env!("IMPERIUM_BUILD_VERSION_STRING");

/// Command-line client for the Imperium daemon.
#[derive(Debug, Parser)]
#[command(name = "imperium", version = BUILD_VERSION)]
struct Cli {
    /// Talk to a remote daemon connected with `imperium remote connect`.
    #[arg(long, global = true)]
    remote: Option<String>,
    #[command(subcommand)]
    command: Command,
}

/// Set in the environment of every stdio MCP server a session starts (`imperium-mcp`).
const MCP_CHILD_MARK: &str = "IMPERIUM_MCP_CHILD";

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
    /// Install and update UI mods, asking you to approve the permissions they request.
    Mods {
        #[command(subcommand)]
        command: ModsCommand,
    },
    /// Plugin marketplaces and plugins: browse, install, update, and turn them on or off.
    /// Approving a plugin that runs code (hooks or servers) happens only in the desktop app.
    Plugin {
        #[command(subcommand)]
        command: plugins::PluginCommand,
    },
    /// Manage secrets in the OS keychain (an encrypted file where there is none).
    Secret {
        #[command(subcommand)]
        command: SecretCommand,
    },
    /// Local models: the configured model folders, and downloads from Hugging Face.
    Models {
        #[command(subcommand)]
        command: ModelsCommand,
    },
    /// Your global memories. (An environment's notes are its Memory.md.)
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
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
    /// Pull requests bound to sessions: what the desktop's pull-request card shows.
    Pr {
        #[command(subcommand)]
        command: PrCommand,
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
    /// Policy pack updates: fetch a pack from a source you added, read what changed in plain
    /// words, and accept or reject it. Nothing is applied until you accept.
    Pack {
        #[command(subcommand)]
        command: PackCommand,
    },
    /// Inference runtimes.
    Runtime {
        #[command(subcommand)]
        command: RuntimeCommand,
    },
    /// Serve the Claude harness: an MCP server on standard input and output that lets an
    /// external agent such as Claude Code start sessions, follow them and answer their
    /// permission requests through this daemon. Register it in `.mcp.json`. With `--tools` it
    /// serves Imperium's own tools instead (files, search, web, memory), each run in an
    /// Imperium session under the user's permission rules.
    McpServe {
        /// Serve Imperium's own tools to the caller instead of driving sessions.
        #[arg(long)]
        tools: bool,
        /// With --tools: the environment (name or id) the tools work in; default Home.
        #[arg(long, requires = "tools", conflicts_with_all = ["project_dir", "session"])]
        environment: Option<String>,
        /// With --tools: a folder to work in instead of an environment.
        #[arg(long, requires = "tools", conflicts_with = "session")]
        project_dir: Option<PathBuf>,
        /// With --tools: run the calls in this existing session instead of starting one.
        #[arg(long, requires = "tools")]
        session: Option<String>,
        /// With --tools: the model of the session this starts (the tools never call it).
        #[arg(long, requires = "tools")]
        model: Option<String>,
        /// With --tools: also offer the tools that write files, run commands or save memory.
        /// Each still asks under the session's permission mode.
        #[arg(long, requires = "tools")]
        allow_write: bool,
        /// With --tools: seconds a permission request waits for you before it is refused; 0
        /// refuses at once (use it when nobody is watching).
        #[arg(long, requires = "tools", default_value_t = 120)]
        approval_timeout_s: u64,
    },
    /// Remote MCP servers that use OAuth: sign in and out.
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Named agents: subagents defined by Markdown files that the root agent can delegate to.
    Agents {
        #[command(subcommand)]
        command: AgentsCommand,
    },
    /// Run one task through the real agent loop without the daemon or the desktop, against any
    /// OpenAI-compatible model endpoint, and print the result (transcript, plan, token and
    /// cost ledger, exit reason) as JSON: the harness evaluations run through. With
    /// `--print-prompt` it prints the resolved system prompt and tool schemas instead, without
    /// calling a model. Exit code 0 when the agent finished its turn, 3 when the run stopped
    /// early (a ceiling, the step limit, a timeout or a model error), 1 when it could not run.
    Run(Box<imperium_cli::eval::RunArgs>),
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
    /// List your global memories.
    List,
    /// Find memories relevant to a query.
    Search { query: String },
    /// Show one memory.
    Show { name: String },
    /// Delete a memory.
    Forget {
        name: String,
        /// Only global is listed here; environment memory is the environment's Memory.md file.
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
    /// Show what keeps the running imperium-core up: the programs using it, and its own work.
    Status,
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
        /// How much initiative it takes: off (the default), suggest, or act.
        #[arg(long, value_parser = ["off", "suggest", "act"])]
        initiative: Option<String>,
        #[command(flatten)]
        profile: env::ProfileArgs,
    },
    /// Rename an agent or change its role or permission profile.
    Set {
        /// Environment name or id.
        env: String,
        /// The agent's name or id.
        agent: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        role: Option<String>,
        #[arg(long, value_parser = ["off", "suggest", "act"])]
        initiative: Option<String>,
        #[command(flatten)]
        profile: env::ProfileArgs,
    },
    /// Delete an agent (its session is archived).
    Rm {
        /// Environment name or id.
        env: String,
        /// The agent's name or id.
        agent: String,
    },
    /// Show where an agent's initiative stands.
    Initiative {
        /// Environment name or id.
        env: String,
        /// The agent's name or id.
        agent: String,
    },
    /// List the agent's suggested work and its latest reminder of what is left.
    Suggestions {
        /// Environment name or id.
        env: String,
        /// The agent's name or id.
        agent: String,
    },
    /// Start one of the agent's suggestions as a session.
    Start {
        /// Environment name or id.
        env: String,
        /// The agent's name or id.
        agent: String,
        /// The suggestion's id.
        suggestion: String,
    },
    /// Dismiss one of the agent's suggestions.
    Dismiss {
        /// Environment name or id.
        env: String,
        /// The agent's name or id.
        agent: String,
        /// The suggestion's id.
        suggestion: String,
    },
    /// Ask the agent to review its environment now (it needs initiative turned on).
    Review {
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
enum PrCommand {
    /// Show the pull request bound to a session, or to each running session when none is named.
    Status {
        /// The session's id (see `imperium session list`).
        session: Option<String>,
    },
    /// Show or turn off GitHub's auto-merge for a session's pull request. It cannot be turned on
    /// from the command line: only the desktop app does that.
    Automerge {
        #[command(subcommand)]
        command: PrAutomergeCommand,
    },
}

#[derive(Debug, Subcommand)]
enum PrAutomergeCommand {
    /// Show whether auto-merge is on for a session's pull request.
    Status {
        /// The session's id (see `imperium session list`).
        session: String,
    },
    /// Turn auto-merge off for a session's pull request.
    Off {
        /// The session's id (see `imperium session list`).
        session: String,
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
enum PackCommand {
    /// Pack sources, the version pinned for each and any update waiting for a decision.
    List,
    /// Add an update source for a pack: a Git URL (`https://`, `ssh://`, `git@host:path` or
    /// `file://`) with no credentials in it. Sources are kept only in your config folder.
    AddSource {
        /// A short name; it also prefixes the pack's references.
        name: String,
        url: String,
        /// A branch or tag; defaults to the repository's default branch.
        #[arg(long)]
        revision: Option<String>,
    },
    /// Remove a source together with its pin and any pending update.
    RemoveSource { name: String },
    /// Fetch the source(s) and show what changed since the pinned version. Applies nothing.
    Check {
        /// A source name; every source when left out.
        name: Option<String>,
    },
    /// Show the pending update again.
    Diff {
        name: String,
        /// Also list every changed line.
        #[arg(long)]
        full: bool,
    },
    /// Pin the pending update. At a terminal you type the confirmation shown; otherwise pass
    /// `--hash` (at least 12 digits of the new hash, as shown by `check` and `diff`).
    Accept {
        name: String,
        #[arg(long)]
        hash: Option<String>,
        /// Also accept changes marked as possibly loosening what agents must do.
        #[arg(long)]
        allow_loosening: bool,
    },
    /// Discard the pending update; the pinned version is unchanged.
    Reject { name: String },
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
    /// The managed vLLM environment for Hugging Face model folders (`vllm:<id>`). Nothing is
    /// downloaded unless you run `setup` and confirm.
    Vllm {
        #[command(subcommand)]
        command: VllmCommand,
    },
}

#[derive(Debug, Subcommand)]
enum VllmCommand {
    /// Set up vLLM: a `uv` virtual environment with a pinned vLLM (`runtime.vllm.version`), or
    /// with `--docker` the official Docker image (`runtime.vllm.docker_image`). Shows the
    /// disk and GPU requirements and asks before downloading. Linux and WSL2 (from Windows, with `--wsl`).
    Setup {
        /// Pull the Docker image instead of making a virtual environment.
        #[arg(long)]
        docker: bool,
        /// Windows only: make the virtual environment inside a WSL2 distribution (`runtime.vllm.wsl_distro`, else WSL's default) and run vLLM there.
        #[arg(long, conflicts_with = "docker")]
        wsl: bool,
        /// Approve the download without asking.
        #[arg(long)]
        yes: bool,
        /// A vLLM version other than the setting or the pinned one.
        #[arg(long)]
        version: Option<String>,
    },
    /// Show what is set up and which vLLM the daemon starts.
    Status,
    /// Remove the managed environment (its folder, or the Docker image).
    Remove {
        /// Approve without asking.
        #[arg(long)]
        yes: bool,
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
enum ModsCommand {
    /// Install a mod from a git URL or a folder. Shows the permissions it requests and its
    /// content hash and asks before installing; nothing in the mod runs. Mods appear in the
    /// desktop app (Settings → Mods).
    Install {
        /// A git URL (`https://`, `ssh://`, `git@host:path`, `file://`) or a folder.
        source: String,
        /// The folder inside the source that holds the mod (its `mod.json`).
        #[arg(long)]
        path: Option<String>,
        /// A branch, tag, or commit (git only).
        #[arg(long)]
        revision: Option<String>,
        /// Install only if the mod's content has this SHA-256 (as shown by a previous run).
        #[arg(long)]
        sha256: Option<String>,
        /// Approve without asking (for scripts).
        #[arg(short, long)]
        yes: bool,
    },
    /// Update an installed mod from the source it was installed from. Shows how its requested
    /// permissions differ from what you approved, and asks again if it wants more.
    Update {
        /// The mod's id.
        id: String,
        /// Update only if the new content has this SHA-256 (as shown by a previous run).
        #[arg(long)]
        sha256: Option<String>,
        /// Approve new permissions without asking (for scripts).
        #[arg(short, long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
enum McpCommand {
    /// Sign in to a remote MCP server that uses OAuth. Prints an address to open in your own
    /// browser; the sign-in finishes when the browser returns to this machine.
    Login {
        /// The server's name in your connectors.
        server: String,
    },
    /// Sign out of a remote MCP server: revoke its token where the server allows it, and forget
    /// the tokens kept in the keychain.
    Logout {
        /// The server's name in your connectors.
        server: String,
    },
}

#[derive(Debug, Subcommand)]
enum AgentsCommand {
    /// List the named agents from your agents folder and from plugins: name, source and what
    /// each is for. A project's own agent files load per session, for a trusted project only.
    List,
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
                    initiative,
                    profile,
                } => env::agent_add(id, name, role, initiative, profile),
                AgentCommand::Set {
                    env: id,
                    agent,
                    name,
                    role,
                    initiative,
                    profile,
                } => env::agent_set(id, agent, name, role, initiative, profile),
                AgentCommand::Rm { env: id, agent } => env::agent_remove(id, agent),
                AgentCommand::Initiative { env: id, agent } => initiative::show(id, agent),
                AgentCommand::Suggestions { env: id, agent } => initiative::suggestions(id, agent),
                AgentCommand::Start {
                    env: id,
                    agent,
                    suggestion,
                } => initiative::start(id, agent, suggestion),
                AgentCommand::Dismiss {
                    env: id,
                    agent,
                    suggestion,
                } => initiative::dismiss(id, agent, suggestion),
                AgentCommand::Review { env: id, agent } => initiative::review(id, agent),
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
        Command::Mods { command } => match command {
            ModsCommand::Install {
                source,
                path,
                revision,
                sha256,
                yes,
            } => mods::install(source, path, revision, sha256, yes),
            ModsCommand::Update { id, sha256, yes } => mods::update(id, sha256, yes),
        },
        Command::Plugin { command } => plugins::run(command),
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
        Command::Runtime {
            command:
                RuntimeCommand::Vllm {
                    command:
                        VllmCommand::Setup {
                            docker,
                            wsl,
                            yes,
                            version,
                        },
                },
        } => vllm::setup(docker, wsl, yes, version),
        Command::Runtime {
            command:
                RuntimeCommand::Vllm {
                    command: VllmCommand::Status,
                },
        } => vllm::status(),
        Command::Runtime {
            command:
                RuntimeCommand::Vllm {
                    command: VllmCommand::Remove { yes },
                },
        } => vllm::remove(yes),
        Command::Run(args) => {
            let outcome = block_on(async {
                let (mut client, _) = connect().await?;
                imperium_cli::eval::run(&mut client, *args).await
            })?;
            println!("{}", serde_json::to_string_pretty(&outcome.json)?);
            std::io::Write::flush(&mut std::io::stdout())?;
            if outcome.code != 0 {
                std::process::exit(outcome.code);
            }
            Ok(())
        }
        Command::Resources { watch } => resources::show(watch),
        Command::McpServe {
            tools,
            environment,
            project_dir,
            session,
            model,
            allow_write,
            approval_timeout_s,
        } => {
            // A server the daemon started for a session must not serve Imperium back to it. A
            // remote daemon is another daemon, so `--remote` is not a loop.
            if std::env::var_os(MCP_CHILD_MARK).is_some()
                && REMOTE.get().and_then(Option::as_deref).is_none()
            {
                bail!(
                    "imperium mcp-serve was started by an Imperium session as one of its MCP \
                     servers: it is for outside agents, and serving this daemon to its own \
                     sessions would loop"
                );
            }
            if tools {
                mcp_serve_tools(BridgeOptions {
                    environment,
                    project_dir: project_dir.map(|dir| dir.display().to_string()),
                    session,
                    model,
                    allow_write,
                    approval_timeout: Duration::from_secs(approval_timeout_s),
                })
            } else {
                mcp_serve()
            }
        }
        Command::Mcp { command } => match command {
            McpCommand::Login { server } => mcp_login::login(server),
            McpCommand::Logout { server } => mcp_login::logout(server),
        },
        Command::Agents { command } => match command {
            AgentsCommand::List => agents::list(),
        },
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
            command: DaemonCommand::Status,
        } => lifetime::status(),
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
        Command::Pr { command } => block_on(async {
            let (mut client, _) = connect().await?;
            let text = match command {
                PrCommand::Status { session } => {
                    imperium_cli::pr::status(&mut client, session.as_deref()).await?
                }
                PrCommand::Automerge {
                    command: PrAutomergeCommand::Status { session },
                } => imperium_cli::pr::automerge_status(&mut client, &session).await?,
                PrCommand::Automerge {
                    command: PrAutomergeCommand::Off { session },
                } => imperium_cli::pr::automerge_off(&mut client, &session).await?,
            };
            print!("{text}");
            Ok(())
        }),
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
        Command::Pack { command } => match command {
            PackCommand::List => pack::list(),
            PackCommand::AddSource {
                name,
                url,
                revision,
            } => pack::add_source(name, url, revision),
            PackCommand::RemoveSource { name } => pack::remove_source(name),
            PackCommand::Check { name } => pack::check(name),
            PackCommand::Diff { name, full } => pack::diff(name, full),
            PackCommand::Accept {
                name,
                hash,
                allow_loosening,
            } => pack::accept(name, hash, allow_loosening),
            PackCommand::Reject { name } => pack::reject(name),
        },
        Command::Memory { command } => match command {
            MemoryCommand::List => memory::list(),
            MemoryCommand::Search { query } => memory::search(&query),
            MemoryCommand::Show { name } => memory::show(&name),
            MemoryCommand::Forget { name, scope } => memory::forget(&name, scope),
        },
    }
}

/// Serves the Claude harness on standard input and output until the caller closes it. Only
/// protocol messages go to standard output; errors go to standard error.
fn mcp_serve() -> anyhow::Result<()> {
    block_on(async {
        let (client, _) = connect_as(LeaseKind::Tool, true).await?;
        imperium_cli::mcp_serve::serve(
            client,
            tokio::io::BufReader::new(tokio::io::stdin()),
            tokio::io::stdout(),
        )
        .await
    })
}

/// Serves Imperium's own tools on standard input and output (`mcp-serve --tools`).
fn mcp_serve_tools(options: BridgeOptions) -> anyhow::Result<()> {
    block_on(async {
        let (client, _) = connect_as(LeaseKind::Tool, true).await?;
        imperium_cli::mcp_serve::serve_tools(
            client,
            tokio::io::BufReader::new(tokio::io::stdin()),
            tokio::io::stdout(),
            options,
        )
        .await
    })
}

fn ping() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, url) = connect_existing().await?;
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
        let (mut client, url) = connect_existing().await?;
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

/// Set to anything but `0`, `false`, `no` or `off` to stop `imperium` commands from starting
/// `imperium-core` when none runs (for CI and tests).
const NO_SPAWN_ENV: &str = "IMPERIUM_NO_DAEMON_SPAWN";
/// Set in the environment of every command the daemon runs for an agent
/// (`imperium_tools::SPAWNED_MARK`).
const SPAWNED_MARK: &str = "IMPERIUM_SPAWNED_BY_DAEMON";
/// How many seconds a daemon an `imperium` command starts waits with no client and no work
/// before it exits (the daemon's own default is 10).
const IDLE_SECS_ENV: &str = "IMPERIUM_DAEMON_IDLE_SECS";

fn spawn_disabled() -> bool {
    std::env::var(NO_SPAWN_ENV).is_ok_and(|value| {
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
}

/// How to start `imperium-core` for this command, when none runs: the one beside this
/// executable, else on the `PATH`, as a daemon that exits when no client and no work uses it.
fn daemon_launch(paths: &Paths) -> Option<Launch> {
    if spawn_disabled() {
        return None;
    }
    let grace = std::env::var(IDLE_SECS_ENV)
        .ok()
        .and_then(|seconds| seconds.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let mut launch = Launch::shared(find_daemon()?, grace);
    launch.log = Some(paths.data_dir().join("imperium-core.log"));
    Some(launch)
}

/// What this process is doing, for the daemon's list of who holds it open: the subcommand
/// (never its arguments, which may hold a prompt).
fn lease_label() -> String {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--remote" {
            args.next();
        } else if !arg.starts_with('-') {
            return format!("imperium {arg}");
        }
    }
    "imperium".into()
}

/// Connects to the daemon, starting one if none runs, and holds a lease on it for as long as
/// the returned client lives. Returns the client and the daemon's URL.
async fn connect() -> anyhow::Result<(Client, String)> {
    connect_as(LeaseKind::Cli, true).await
}

/// Like [`connect`], but never starts a daemon: for commands that ask about one.
async fn connect_existing() -> anyhow::Result<(Client, String)> {
    connect_as(LeaseKind::Cli, false).await
}

/// Connects as `kind`; with `start`, starts a daemon when none runs (a local one only: a
/// `--remote` daemon is started with `imperium remote connect`).
async fn connect_as(kind: LeaseKind, start: bool) -> anyhow::Result<(Client, String)> {
    let paths = Paths::from_env()?;
    let remote = REMOTE.get().and_then(Option::as_deref);
    let file = match remote {
        Some(name) => remote::endpoint_file(&paths, name),
        None => paths.daemon_endpoint_file(),
    };
    // A command the daemon's own agents run (`run_command`, hooks, MCP servers) is a
    // descendant of the daemon: it takes no lease, which would keep the daemon up for as long
    // as the agent waits on it, and starts no daemon.
    let spawned_by_daemon = std::env::var_os(SPAWNED_MARK).is_some();
    let launch = if remote.is_none() && start && !spawned_by_daemon {
        daemon_launch(&paths)
    } else {
        None
    };
    let me = Peer {
        name: "imperium-cli".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    };
    let lease = LeaseRequest {
        kind,
        label: lease_label(),
        // A forwarded daemon is on another machine, where this process id means nothing.
        pid: remote.is_none().then(std::process::id),
    };
    let lease = (!spawned_by_daemon).then_some(lease);
    match attach(&file, me, lease, launch.as_ref()).await {
        Ok((client, endpoint)) => Ok((client, endpoint.url)),
        Err(error @ ClientError::Connect(_)) => Err(anyhow::Error::new(error).context(format!(
            "imperium-core did not answer; it may have stopped without removing {}",
            file.display()
        ))),
        Err(error) => Err(error.into()),
    }
}

/// Whether the daemon of this machine answers (never a `--remote` one).
async fn local_daemon_running() -> bool {
    let Ok(paths) = Paths::from_env() else {
        return false;
    };
    let Ok(endpoint) = read_endpoint(&paths.daemon_endpoint_file()) else {
        return false;
    };
    let me = Peer {
        name: "imperium-cli".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    };
    Client::connect(&endpoint, me).await.is_ok()
}

fn explain(key: &str, project: Option<PathBuf>) -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let config = imperium_config::load(&paths, project.as_deref(), std::env::vars())?;
    for warning in config.warnings() {
        eprintln!("note: {warning}");
    }
    if let Some(project) = &project {
        // The project's own file is not a layer; this is what it narrows.
        let narrowed =
            imperium_config::EnvironmentConfig::load(project, "project").apply(&config.settings()?);
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

fn open_secrets() -> anyhow::Result<imperium_config::AutoSecrets> {
    Ok(imperium_config::AutoSecrets::open(&Paths::from_env()?)?)
}

/// Says so when secrets went to the encrypted file because the OS has no keychain.
fn warn_if_weaker(secrets: &imperium_config::AutoSecrets) {
    if let Some(notice) = secrets.notice() {
        eprintln!("warning: {notice}");
    }
    if secrets.backend() == imperium_config::SecretBackend::EncryptedFile {
        eprintln!("warning: {}", secrets.fallback_warning());
    }
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
    let secrets = open_secrets()?;
    secrets.store(name, &value)?;
    println!("stored {name}");
    warn_if_weaker(&secrets);
    Ok(())
}

fn delete_secret(name: &SecretName) -> anyhow::Result<()> {
    if open_secrets()?.delete(name)? {
        println!("deleted {name}");
    } else {
        println!("{name} was not stored");
    }
    Ok(())
}

/// Auto-merge lands code without another look, so the command line must have no way to turn it
/// on. These tests read the command tree and the source.
#[cfg(test)]
mod no_enabling_path {
    use clap::CommandFactory;

    use super::Cli;

    /// Every subcommand path under `command`, as `pr automerge off`.
    fn paths(command: &clap::Command, prefix: &str, out: &mut Vec<(String, Vec<String>)>) {
        for sub in command.get_subcommands() {
            let path = format!("{prefix} {}", sub.get_name());
            let args = sub
                .get_arguments()
                .map(|arg| arg.get_id().to_string())
                .collect();
            out.push((path.clone(), args));
            paths(sub, &path, out);
        }
    }

    #[test]
    fn no_subcommand_can_ask_for_auto_merge() {
        let mut root = Cli::command();
        root.build();
        let mut all = Vec::new();
        paths(&root, "", &mut all);
        for (path, args) in &all {
            // `pr automerge` has exactly `status` and `off`, each taking only the session.
            if path.starts_with(" pr automerge ") && !path.contains(" help") {
                assert!(
                    matches!(path.as_str(), " pr automerge status" | " pr automerge off"),
                    "{path}"
                );
                let named: Vec<&str> = args
                    .iter()
                    .map(String::as_str)
                    .filter(|id| !matches!(*id, "help" | "version" | "remote"))
                    .collect();
                assert_eq!(named, ["session"], "{path}");
            }
            // No command or argument anywhere is about turning it on.
            let spelled = format!("{path} {}", args.join(" ")).to_ascii_lowercase();
            for word in ["enable-auto", "merge-method", "merge_method", "challenge"] {
                assert!(!spelled.contains(word), "{path} has {word}");
            }
        }
        assert!(all.iter().any(|(path, _)| path == " pr automerge off"));
    }

    /// The only auto-merge call the command line makes is the one that turns it off, and it
    /// never asks for the challenge that turning it on needs.
    #[test]
    fn the_source_sends_no_enabling_call() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            // This test's own text names them.
            let source = source.split("mod no_enabling_path").next().unwrap();
            assert!(
                !source.contains("AutoMergeChallenge") && !source.contains("auto_merge_challenge"),
                "{} asks for an auto-merge challenge",
                path.display()
            );
            if source.contains("PullRequestsSetAutoMerge") {
                seen += 1;
                assert!(
                    path.ends_with("pr.rs"),
                    "{} sends set_auto_merge",
                    path.display()
                );
                assert!(source.contains("enabled: false"));
                assert!(!source.contains("enabled: true"));
                assert!(!source.contains("enabled,"));
            }
        }
        assert_eq!(seen, 1);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod help_snapshot {
    use clap::CommandFactory;

    use super::Cli;

    /// Appends the long help of `command` and, under it, of each of its subcommands.
    fn collect(command: &clap::Command, out: &mut String) {
        let name = command.get_bin_name().unwrap_or_else(|| command.get_name());
        out.push_str(&format!("===== {name} =====\n"));
        out.push_str(command.clone().render_long_help().to_string().trim_end());
        out.push_str("\n\n");
        for subcommand in command.get_subcommands() {
            collect(subcommand, out);
        }
    }

    /// Writes the long help of every command to `snapshots/help.txt`. `scripts/check-rust.sh`
    /// fails when it differs from what is checked in, so a change to a flag or a help text
    /// shows in review. It runs on Linux only, so platform-specific arguments cannot make the
    /// file differ between machines.
    #[test]
    fn exports_the_command_line_help() {
        let mut root = Cli::command();
        root.build();
        let mut text = String::new();
        collect(&root, &mut text);
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("snapshots");
        std::fs::create_dir_all(&dir).expect("create snapshots");
        let text = format!("{}\n", text.trim_end());
        std::fs::write(dir.join("help.txt"), text).expect("write the help snapshot");
    }
}
