//! `imperium plugin`: plugin marketplaces and the plugins installed from them.
//!
//! The daemon does the work (`plugins.*`). Installing and updating print the trust prompt the
//! daemon's inspection gives (what the plugin would run, where it came from, its content hash)
//! and ask before going ahead. There is **no approving command** here: a plugin with hooks or
//! servers lands withheld, and only the desktop app's own dialog can approve it. A command line
//! could be driven by an agent, so nothing it says counts as the user's approval.

use std::io::{IsTerminal, Read, Write};

use anyhow::{Context, bail};
use clap::Subcommand;
use imperium_cli::client::Client;
use imperium_protocol::methods::{
    InstalledPlugin, PluginInstalled, PluginPreview, PluginsBrowse, PluginsBrowseParams,
    PluginsDeleteSecret, PluginsDeleteSecretParams, PluginsInspect, PluginsInspectParams,
    PluginsInstall, PluginsInstallParams, PluginsInstallResult, PluginsList, PluginsListParams,
    PluginsListResult, PluginsMarketplaceAdd, PluginsMarketplaceAddParams, PluginsMarketplaceList,
    PluginsMarketplaceListParams, PluginsMarketplaceRefresh, PluginsMarketplaceRefreshParams,
    PluginsMarketplaceRemove, PluginsMarketplaceRemoveParams, PluginsSetEnabled,
    PluginsSetEnabledParams, PluginsSetSecret, PluginsSetSecretParams, PluginsSuggestions,
    PluginsSuggestionsParams, PluginsSuggestionsResult, PluginsUninstall, PluginsUninstallParams,
    PluginsUpdate, PluginsUpdateParams,
};

use crate::{block_on, connect};

/// The longest secret value the daemon takes (64 KiB).
const MAX_SECRET_BYTES: usize = 64 * 1024;

#[derive(Debug, Subcommand)]
pub(crate) enum PluginCommand {
    /// Marketplaces: catalogues of plugins (Anthropic's official one is built in).
    Marketplace {
        #[command(subcommand)]
        command: MarketplaceCommand,
    },
    /// List the plugins the marketplaces offer, optionally narrowed by words matched against
    /// name, description, keywords, tags and category.
    Browse {
        /// Words to look for.
        query: Option<String>,
        /// Only this marketplace.
        #[arg(long)]
        marketplace: Option<String>,
    },
    /// Show what a plugin would run, where it comes from and its content hash, without
    /// installing it. Nothing in the plugin runs.
    Inspect {
        /// `name@marketplace`.
        id: String,
    },
    /// Install a plugin from a marketplace. Prints what it would run and its content hash and
    /// asks before installing. Installing never approves: a plugin with hooks or servers is
    /// withheld until you approve it in the desktop app.
    Install {
        /// `name@marketplace`.
        id: String,
        /// Plugins are not yet turned on per environment, so this is only noted.
        #[arg(long = "env")]
        environment: Option<String>,
        /// Install only if the plugin's content has this SHA-256 (as shown by a previous run).
        #[arg(long)]
        sha256: Option<String>,
        /// Answer yes to the question without a terminal (for scripts). It does not approve
        /// the plugin to run.
        #[arg(short, long)]
        yes: bool,
    },
    /// Update an installed plugin from the marketplace it came from. Prints what the new
    /// version would run and asks before replacing it. An update is withheld again until it is
    /// approved in the desktop app.
    Update {
        /// The plugin's name, or `name@marketplace`.
        name: String,
        /// Update only if the new content has this SHA-256 (as shown by a previous run).
        #[arg(long)]
        sha256: Option<String>,
        /// Answer yes to the question without a terminal (for scripts).
        #[arg(short, long)]
        yes: bool,
    },
    /// Turn a plugin on. An unapproved plugin with hooks or servers cannot be turned on.
    Enable {
        /// The plugin's name.
        name: String,
        /// Only for this environment (its id or name).
        #[arg(long = "env")]
        environment: Option<String>,
    },
    /// Turn a plugin off. Its approval is kept.
    Disable {
        /// The plugin's name.
        name: String,
        /// Only for this environment (its id or name).
        #[arg(long = "env")]
        environment: Option<String>,
    },
    /// Remove a plugin. Its folder is kept in the daemon's config-trash, and
    /// `imperium config undo` restores it.
    Uninstall {
        /// The plugin's name, or `name@marketplace`.
        name: String,
        /// Also delete the plugin's saved secrets.
        #[arg(long)]
        delete_secrets: bool,
    },
    /// List the installed plugins, whether each is on, and why a withheld one is withheld.
    List {
        /// Show which plugins are on for this environment (its id or name).
        #[arg(long = "env")]
        environment: Option<String>,
    },
    /// Show what a trusted project's own `.imperium/config.toml` offers: marketplaces and
    /// plugins it suggests. An offer adds, fetches, installs and enables nothing. The commands
    /// that would act on an offer are printed only when the output is a terminal.
    Suggestions {
        /// The environment (its id or name).
        #[arg(long = "env")]
        environment: String,
    },
    /// Secrets a plugin's servers need (`${VAR}` in their environment or headers).
    Secret {
        #[command(subcommand)]
        command: PluginSecretCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum MarketplaceCommand {
    /// Add a marketplace from a git URL, `owner/repo` (GitHub) or a folder. Nothing in it runs.
    Add {
        source: String,
        /// A branch, tag, or commit (git only).
        #[arg(long)]
        revision: Option<String>,
    },
    /// List the marketplaces.
    List,
    /// Fetch marketplaces again and say which plugins were added, removed or changed.
    Refresh {
        /// Only this marketplace.
        name: Option<String>,
    },
    /// Remove a marketplace. The built-in one is turned off instead.
    Remove { name: String },
}

#[derive(Debug, Subcommand)]
pub(crate) enum PluginSecretCommand {
    /// Save a secret for a plugin. The value is read from standard input, or prompted for
    /// without echo; it is never taken as an argument and never printed. Saving a secret does
    /// not approve the plugin.
    Set {
        /// The plugin's name.
        name: String,
        /// The variable, as the plugin's server names it (`${VAR}`).
        variable: String,
    },
    /// Delete a plugin's saved secret.
    Delete {
        /// The plugin's name.
        name: String,
        /// The variable.
        variable: String,
    },
}

/// Where the answers come from: a person at a terminal, or a script.
pub(crate) trait Prompter {
    /// Whether a person can answer here.
    fn interactive(&self) -> bool;
    /// Whether standard output is a terminal (a person reading it, not a pipe or a file).
    fn output_is_terminal(&self) -> bool;
    /// Asks `question [y/N]`; true for a yes.
    fn confirm(&mut self, question: &str) -> anyhow::Result<bool>;
    /// Reads a secret value without echoing it: from a prompt at a terminal, else from
    /// standard input.
    fn secret(&mut self, prompt: &str) -> anyhow::Result<String>;
}

struct Terminal;

impl Prompter for Terminal {
    fn interactive(&self) -> bool {
        std::io::stdin().is_terminal()
    }

    fn output_is_terminal(&self) -> bool {
        std::io::stdout().is_terminal()
    }

    fn confirm(&mut self, question: &str) -> anyhow::Result<bool> {
        print!("{question} [y/N] ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        Ok(matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        ))
    }

    fn secret(&mut self, prompt: &str) -> anyhow::Result<String> {
        if self.interactive() {
            rpassword::prompt_password(prompt).context("could not read the value")
        } else {
            read_piped(std::io::stdin())
        }
    }
}

/// A value piped in: everything up to the end, without the final line break, at most 64 KiB.
fn read_piped(input: impl Read) -> anyhow::Result<String> {
    let mut value = String::new();
    input
        .take(MAX_SECRET_BYTES as u64 + 1)
        .read_to_string(&mut value)
        .context("could not read the value from standard input")?;
    if value.len() > MAX_SECRET_BYTES {
        bail!("the value is longer than 64 KiB");
    }
    Ok(value.trim_end_matches(['\r', '\n']).to_string())
}

pub(crate) fn run(command: PluginCommand) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        dispatch(&mut client, &mut std::io::stdout(), &mut Terminal, command).await
    })
}

async fn dispatch(
    client: &mut Client,
    out: &mut dyn Write,
    prompter: &mut dyn Prompter,
    command: PluginCommand,
) -> anyhow::Result<()> {
    match command {
        PluginCommand::Marketplace { command } => marketplace(client, out, command).await,
        PluginCommand::Browse { query, marketplace } => {
            browse(client, out, query, marketplace).await
        }
        PluginCommand::Inspect { id } => {
            let preview = inspect(client, &id).await?;
            write!(out, "{}", render(&preview))?;
            Ok(())
        }
        PluginCommand::Install {
            id,
            environment,
            sha256,
            yes,
        } => install(client, out, prompter, id, environment, sha256, yes).await,
        PluginCommand::Update { name, sha256, yes } => {
            update(client, out, prompter, name, sha256, yes).await
        }
        PluginCommand::Enable { name, environment } => {
            set_enabled(client, out, name, true, environment).await
        }
        PluginCommand::Disable { name, environment } => {
            set_enabled(client, out, name, false, environment).await
        }
        PluginCommand::Uninstall {
            name,
            delete_secrets,
        } => uninstall(client, out, name, delete_secrets).await,
        PluginCommand::List { environment } => {
            let listing = client
                .call::<PluginsList>(PluginsListParams { environment })
                .await?;
            write!(out, "{}", render_list(&listing))?;
            Ok(())
        }
        PluginCommand::Suggestions { environment } => {
            let offers = client
                .call::<PluginsSuggestions>(PluginsSuggestionsParams { environment })
                .await?;
            write!(
                out,
                "{}",
                render_suggestions(&offers, prompter.output_is_terminal())
            )?;
            Ok(())
        }
        PluginCommand::Secret { command } => match command {
            PluginSecretCommand::Set { name, variable } => {
                set_secret(client, out, prompter, name, variable).await
            }
            PluginSecretCommand::Delete { name, variable } => {
                let done = client
                    .call::<PluginsDeleteSecret>(PluginsDeleteSecretParams {
                        name: name.clone(),
                        variable: variable.clone(),
                    })
                    .await?;
                if done.changed {
                    writeln!(out, "Deleted the saved secret {variable} of {name}.")?;
                } else {
                    writeln!(out, "{name} had no saved secret {variable}.")?;
                }
                Ok(())
            }
        },
    }
}

async fn marketplace(
    client: &mut Client,
    out: &mut dyn Write,
    command: MarketplaceCommand,
) -> anyhow::Result<()> {
    match command {
        MarketplaceCommand::Add { source, revision } => {
            let info = client
                .call::<PluginsMarketplaceAdd>(PluginsMarketplaceAddParams {
                    source: crate::mods::absolute_folder(source),
                    revision,
                })
                .await?;
            let count = info
                .plugin_count
                .map_or_else(String::new, |count| format!(" It lists {count} plugins."));
            writeln!(
                out,
                "Added marketplace {} ({}).{count}",
                info.name, info.source
            )?;
            for problem in &info.problems {
                writeln!(out, "  problem: {problem}")?;
            }
        }
        MarketplaceCommand::List => {
            let listing = client
                .call::<PluginsMarketplaceList>(PluginsMarketplaceListParams {})
                .await?;
            if listing.marketplaces.is_empty() {
                writeln!(
                    out,
                    "No marketplaces. Add one with: imperium plugin marketplace add <source>"
                )?;
            }
            for info in &listing.marketplaces {
                let mut flags = String::new();
                if info.built_in {
                    flags.push_str(" [built in]");
                }
                if !info.enabled {
                    flags.push_str(" [off]");
                }
                let revision = info
                    .revision
                    .as_deref()
                    .map(|revision| format!(" at {revision}"))
                    .unwrap_or_default();
                let commit = info
                    .commit
                    .as_deref()
                    .map(|commit| format!(", commit {}", short(commit)))
                    .unwrap_or_default();
                let count = info.plugin_count.map_or_else(
                    || "not read yet".to_string(),
                    |count| format!("{count} plugins"),
                );
                writeln!(
                    out,
                    "{}{flags}: {}{revision}{commit}; {count}",
                    info.name, info.source
                )?;
                for problem in &info.problems {
                    writeln!(out, "  problem: {problem}")?;
                }
            }
        }
        MarketplaceCommand::Refresh { name } => {
            let done = client
                .call::<PluginsMarketplaceRefresh>(PluginsMarketplaceRefreshParams { name })
                .await?;
            if done.refreshed.is_empty() {
                writeln!(out, "No marketplace to refresh.")?;
            }
            for one in &done.refreshed {
                if let Some(error) = &one.error {
                    writeln!(
                        out,
                        "{}: could not be refreshed ({error}); the last copy is kept.",
                        one.name
                    )?;
                    continue;
                }
                let commit = |commit: &Option<String>| {
                    commit.as_deref().map_or_else(|| "none".to_string(), short)
                };
                writeln!(
                    out,
                    "{}: {} -> {}; {} added, {} removed, {} changed.",
                    one.name,
                    commit(&one.commit_before),
                    commit(&one.commit_after),
                    one.added.len(),
                    one.removed.len(),
                    one.changed.len()
                )?;
                for (label, names) in [
                    ("added", &one.added),
                    ("removed", &one.removed),
                    ("changed", &one.changed),
                ] {
                    if !names.is_empty() {
                        writeln!(out, "  {label}: {}", names.join(", "))?;
                    }
                }
                for problem in &one.problems {
                    writeln!(out, "  problem: {problem}")?;
                }
            }
        }
        MarketplaceCommand::Remove { name } => {
            let done = client
                .call::<PluginsMarketplaceRemove>(PluginsMarketplaceRemoveParams { name })
                .await?;
            if done.disabled {
                writeln!(
                    out,
                    "{} is built in, so it was turned off instead of removed.",
                    done.name
                )?;
            } else {
                writeln!(out, "Removed marketplace {}.", done.name)?;
            }
        }
    }
    Ok(())
}

async fn browse(
    client: &mut Client,
    out: &mut dyn Write,
    query: Option<String>,
    marketplace: Option<String>,
) -> anyhow::Result<()> {
    let found = client
        .call::<PluginsBrowse>(PluginsBrowseParams { marketplace, query })
        .await?;
    if found.plugins.is_empty() {
        writeln!(out, "No plugins found.")?;
    }
    for plugin in &found.plugins {
        let version = plugin
            .version
            .as_deref()
            .map(|version| format!(" {version}"))
            .unwrap_or_default();
        writeln!(out, "{}{version}: {}", plugin.id, plugin.description)?;
        if plugin.source_kind == "unsupported" {
            writeln!(out, "  not supported by Imperium ({})", plugin.source)?;
        } else if let Some(components) = &plugin.components {
            let mut holds = Vec::new();
            if !components.skills.is_empty() {
                holds.push(format!("skills {}", components.skills.join(", ")));
            }
            if !components.commands.is_empty() {
                holds.push(format!("commands {}", components.commands.join(", ")));
            }
            if !components.agents.is_empty() {
                holds.push(format!("agents {}", components.agents.join(", ")));
            }
            if components.hooks > 0 {
                holds.push(format!("{} hooks", components.hooks));
            }
            if !components.mcp_servers.is_empty() {
                holds.push(format!("servers {}", components.mcp_servers.join(", ")));
            }
            if !holds.is_empty() {
                writeln!(out, "  holds: {}", holds.join("; "))?;
            }
        }
    }
    for problem in &found.problems {
        writeln!(out, "problem: {problem}")?;
    }
    Ok(())
}

async fn inspect(client: &mut Client, id: &str) -> anyhow::Result<PluginPreview> {
    Ok(client
        .call::<PluginsInspect>(PluginsInspectParams {
            id: id.to_string(),
            revision: None,
        })
        .await?)
}

/// Asks the question unless `--yes` answers it; without a terminal `--yes` is required.
fn approve(prompter: &mut dyn Prompter, question: &str, yes: bool) -> anyhow::Result<bool> {
    if yes {
        return Ok(true);
    }
    if !prompter.interactive() {
        bail!(
            "this needs your answer: run it in a terminal, or pass --yes to go ahead with what is listed above"
        );
    }
    prompter.confirm(question)
}

fn check_pin(preview: &PluginPreview, pin: &str) -> anyhow::Result<()> {
    let pin = pin.trim().trim_start_matches("sha256:");
    if !pin.eq_ignore_ascii_case(&preview.sha256) {
        bail!(
            "the plugin hashes to {}, not the {pin} you pinned; nothing was changed",
            preview.sha256
        );
    }
    Ok(())
}

async fn install(
    client: &mut Client,
    out: &mut dyn Write,
    prompter: &mut dyn Prompter,
    id: String,
    environment: Option<String>,
    sha256: Option<String>,
    yes: bool,
) -> anyhow::Result<()> {
    let preview = inspect(client, &id).await?;
    if let Some(pin) = &sha256 {
        check_pin(&preview, pin)?;
    }
    if let Some(installed) = &preview.installed {
        bail!(
            "a plugin named {} is already installed at {}; update it with `imperium plugin update {}` or remove it with `imperium plugin uninstall {}`",
            preview.name,
            installed.path,
            preview.name,
            preview.name
        );
    }
    write!(out, "{}", render(&preview))?;
    if !approve(prompter, &format!("Install {}?", preview.id), yes)? {
        writeln!(out, "Nothing was installed.")?;
        return Ok(());
    }
    // The hash is the one shown above: content that changed since is refused.
    let done = client
        .call::<PluginsInstall>(PluginsInstallParams {
            id: preview.id.clone(),
            sha256: preview.sha256.clone(),
            revision: None,
            environment,
            approve: None,
        })
        .await?;
    write!(out, "{}", render_installed(&done))?;
    Ok(())
}

async fn update(
    client: &mut Client,
    out: &mut dyn Write,
    prompter: &mut dyn Prompter,
    name: String,
    sha256: Option<String>,
    yes: bool,
) -> anyhow::Result<()> {
    let listing = client
        .call::<PluginsList>(PluginsListParams { environment: None })
        .await?;
    let current =
        find_plugin(&listing, &name).with_context(|| format!("no installed plugin {name}"))?;
    let Some(id) = current.id.clone() else {
        bail!(
            "{} was not installed from a marketplace, so there is nothing to update it from",
            current.name
        );
    };
    let preview = inspect(client, &id).await?;
    if let Some(pin) = &sha256 {
        check_pin(&preview, pin)?;
    }
    // The daemon alone decides "up to date" (it also checks the record's hash). When the
    // folder already holds the inspected content there is nothing new to trust, so it is asked
    // without a question; otherwise the trust prompt comes first.
    if !nothing_to_trust(&preview) {
        write!(out, "{}", render(&preview))?;
        if !approve(prompter, &format!("Update {id}?"), yes)? {
            writeln!(out, "Not updated.")?;
            return Ok(());
        }
    }
    let done = client
        .call::<PluginsUpdate>(PluginsUpdateParams {
            name: id.clone(),
            sha256: preview.sha256.clone(),
        })
        .await?;
    for note in &done.notes {
        writeln!(out, "Note: {note}")?;
    }
    if done.up_to_date {
        writeln!(out, "{id} is up to date.")?;
        return Ok(());
    }
    writeln!(out, "Updated {} in {}.", done.name, done.path)?;
    if let Some(kept) = &done.kept_at {
        writeln!(out, "The previous folder was kept at {kept}.")?;
    }
    if done.withheld {
        writeln!(
            out,
            "It has code, so it is withheld until you approve this version in the desktop app; an earlier approval does not carry over.{}",
            done.reason
                .as_deref()
                .map(|reason| format!(" ({reason})"))
                .unwrap_or_default()
        )?;
    }
    Ok(())
}

/// The folder holds the inspected content and the record's hash is that content's too, so an
/// update changes nothing and there is nothing new to trust. A record that disagrees with the
/// folder is for the daemon to settle, after the user has seen the prompt.
fn nothing_to_trust(preview: &PluginPreview) -> bool {
    preview.installed.as_ref().is_some_and(|installed| {
        installed.same_content
            && installed.recorded_sha256.as_deref() == Some(preview.sha256.as_str())
    })
}

fn find_plugin<'a>(listing: &'a PluginsListResult, name: &str) -> Option<&'a InstalledPlugin> {
    listing.plugins.iter().find(|plugin| {
        plugin.name.eq_ignore_ascii_case(name)
            || plugin
                .id
                .as_deref()
                .is_some_and(|id| id.eq_ignore_ascii_case(name))
    })
}

async fn set_enabled(
    client: &mut Client,
    out: &mut dyn Write,
    name: String,
    enabled: bool,
    environment: Option<String>,
) -> anyhow::Result<()> {
    let done = client
        .call::<PluginsSetEnabled>(PluginsSetEnabledParams {
            name,
            enabled,
            environment,
        })
        .await?;
    writeln!(
        out,
        "{} is {} (decided by: {}).",
        done.name,
        if done.enabled { "on" } else { "off" },
        done.decided_by
    )?;
    for note in &done.notes {
        writeln!(out, "Note: {note}")?;
    }
    Ok(())
}

async fn uninstall(
    client: &mut Client,
    out: &mut dyn Write,
    name: String,
    delete_secrets: bool,
) -> anyhow::Result<()> {
    let done = client
        .call::<PluginsUninstall>(PluginsUninstallParams {
            name,
            delete_secrets,
        })
        .await?;
    writeln!(
        out,
        "Uninstalled {}. Its folder is kept at {}; undo with `imperium config undo {}`.",
        done.name, done.kept_at, done.change_id
    )?;
    for note in &done.notes {
        writeln!(out, "Note: {note}")?;
    }
    Ok(())
}

async fn set_secret(
    client: &mut Client,
    out: &mut dyn Write,
    prompter: &mut dyn Prompter,
    name: String,
    variable: String,
) -> anyhow::Result<()> {
    let value = prompter.secret(&format!("Value for {variable} of {name}: "))?;
    if value.is_empty() {
        bail!("no value was given; nothing was saved");
    }
    let done = client
        .call::<PluginsSetSecret>(PluginsSetSecretParams {
            name: name.clone(),
            variable: variable.clone(),
            value,
        })
        .await?;
    writeln!(
        out,
        "Saved {variable} for {name} as {}. Saving a secret does not approve the plugin.",
        done.secret
    )?;
    Ok(())
}

/// Text from a repository, made safe to print: control characters (escape sequences that could
/// rewrite a terminal, carriage returns, line breaks) and characters that reorder text are shown
/// as `\u{..}` instead of being sent to the terminal.
fn printable(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let hidden = c.is_control()
            || matches!(
                c,
                '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
            );
        if hidden {
            out.push_str(&format!("\\u{{{:x}}}", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    out
}

/// A word for a shell, in single quotes unless it is plain, after [`printable`].
fn shell_word(text: &str) -> String {
    let text = printable(text);
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '@' | ':'));
    if plain {
        text
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

/// The offers of a project's file, as text. Everything the repository wrote is printed through
/// [`printable`]; the commands that would act on an offer are added only for a terminal, where
/// a person reads them (they are never run here).
fn render_suggestions(offers: &PluginsSuggestionsResult, commands: bool) -> String {
    let mut text = String::new();
    let Some(folder) = &offers.source_folder else {
        text.push_str("This environment has no project folder, so nothing is offered.\n");
        return text;
    };
    let folder = printable(folder);
    if !offers.trusted {
        text.push_str(&format!(
            "{folder} is not in mcp.trusted_projects, so its suggestions are not offered. Add it to mcp.trusted_projects in your own config to see them.\n"
        ));
        return text;
    }
    if offers.muted {
        text.push_str(&format!("Suggestions from {folder} are muted.\n"));
        return text;
    }
    if offers.marketplaces.is_empty() && offers.plugins.is_empty() {
        text.push_str(&format!("{folder} offers nothing.\n"));
        return text;
    }
    text.push_str(&format!(
        "Offered by {folder} (the repository's own words; nothing is added or installed by an offer):\n"
    ));
    for offer in &offers.marketplaces {
        let revision = offer
            .revision
            .as_deref()
            .map(|revision| format!(" at {}", printable(revision)))
            .unwrap_or_default();
        let state = if offer.added { " [already added]" } else { "" };
        text.push_str(&format!(
            "  marketplace {}{revision}{state} (from {})\n",
            printable(&offer.source),
            printable(&offer.file)
        ));
        if commands && !offer.added {
            let flag = offer
                .revision
                .as_deref()
                .map(|revision| format!(" --revision {}", shell_word(revision)))
                .unwrap_or_default();
            text.push_str(&format!(
                "    imperium plugin marketplace add {}{flag}\n",
                shell_word(&offer.source)
            ));
        }
    }
    for offer in &offers.plugins {
        let mut state = String::new();
        if offer.installed {
            state.push_str(" [already installed]");
        }
        if !offer.marketplace_added {
            state.push_str(" [its marketplace is not added]");
        }
        text.push_str(&format!(
            "  plugin {}{state} (from {})\n",
            printable(&offer.id),
            printable(&offer.file)
        ));
        if commands && !offer.installed && offer.marketplace_added {
            text.push_str(&format!(
                "    imperium plugin install {}\n",
                shell_word(&offer.id)
            ));
        }
    }
    if !commands {
        text.push_str("Run this in a terminal to see the commands that would act on an offer.\n");
    }
    text
}

fn short(commit: &str) -> String {
    commit.chars().take(12).collect()
}

/// What the user is asked to trust: where the plugin comes from, every hook and server it would
/// run, what else it holds, and the content hash the answer is for.
fn render(preview: &PluginPreview) -> String {
    let mut text = String::new();
    let version = preview
        .version
        .as_deref()
        .map(|version| format!(" {version}"))
        .unwrap_or_default();
    text.push_str(&format!("{} ({}){version}\n", preview.name, preview.id));
    if !preview.description.is_empty() {
        text.push_str(&format!("  {}\n", preview.description));
    }
    let commit = |commit: &Option<String>| {
        commit
            .as_deref()
            .map(|commit| format!(" at {}", short(commit)))
            .unwrap_or_default()
    };
    text.push_str(&format!(
        "From marketplace {} ({}{})\n",
        preview.marketplace,
        preview.marketplace_source,
        commit(&preview.marketplace_commit)
    ));
    text.push_str(&format!(
        "Plugin source: {} {}{}\n",
        preview.source_kind,
        preview.source,
        commit(&preview.commit)
    ));
    let claims: Vec<String> = [
        ("author", &preview.author),
        ("homepage", &preview.homepage),
        ("license", &preview.license),
    ]
    .into_iter()
    .filter_map(|(label, value)| value.as_ref().map(|value| format!("{label} {value}")))
    .collect();
    if !claims.is_empty() {
        text.push_str(&format!(
            "Its own claims, not verified: {}\n",
            claims.join("; ")
        ));
    }
    let has_code = !preview.hooks.is_empty() || !preview.servers.is_empty();
    if has_code {
        text.push_str("It would run:\n");
        for hook in &preview.hooks {
            let what = match &hook.url {
                Some(url) if hook.command.is_empty() => format!("POST {url}"),
                _ => hook.command.clone(),
            };
            let matcher = if hook.matcher.is_empty() {
                String::new()
            } else {
                format!(" [{}]", hook.matcher)
            };
            text.push_str(&format!("  hook {}{matcher}: {what}\n", hook.event));
        }
        for server in &preview.servers {
            let what = match &server.url {
                Some(url) => format!("{} {url}", server.transport),
                None => {
                    let mut line = server.command.clone().unwrap_or_default();
                    for arg in &server.args {
                        line.push(' ');
                        line.push_str(arg);
                    }
                    format!("{} {line}", server.transport)
                }
            };
            text.push_str(&format!("  server {}: {}\n", server.name, what.trim_end()));
            if !server.env_names.is_empty() {
                text.push_str(&format!("    sets: {}\n", server.env_names.join(", ")));
            }
            if !server.header_names.is_empty() {
                text.push_str(&format!(
                    "    headers: {}\n",
                    server.header_names.join(", ")
                ));
            }
            for variable in &server.variables {
                text.push_str(&format!(
                    "    needs a saved secret: {variable} (`imperium plugin secret set {} {variable}`)\n",
                    preview.name
                ));
            }
        }
        if preview.launcher {
            text.push_str(
                "WARNING: something it runs starts a launcher that fetches more code (npx, uvx, @latest), which this listing cannot show. Even without this warning, a script can download and run code.\n",
            );
        }
    } else {
        text.push_str("It has no hooks or servers, so nothing in it runs.\n");
    }
    for (label, names) in [
        ("skills", &preview.skills),
        ("commands", &preview.commands),
        ("agents", &preview.agents),
    ] {
        if !names.is_empty() {
            text.push_str(&format!("It adds {label}: {}\n", names.join(", ")));
        }
    }
    if !preview.commands_with_shell.is_empty() {
        text.push_str(&format!(
            "Commands with shell lines or allowed tools (Imperium does not run these itself): {}\n",
            preview.commands_with_shell.join(", ")
        ));
    }
    text.push_str(&format!("Content sha256:{}\n", preview.sha256));
    if let Some(PluginInstalled {
        path,
        recorded_sha256,
        same_content,
    }) = &preview.installed
    {
        let state = if *same_content {
            "the same content"
        } else {
            "different content"
        };
        let recorded = recorded_sha256
            .as_deref()
            .map(|hash| format!(" (recorded sha256:{hash})"))
            .unwrap_or_default();
        text.push_str(&format!("Installed now at {path}: {state}{recorded}\n"));
    }
    for note in &preview.notes {
        text.push_str(&format!("Note: {note}\n"));
    }
    if has_code {
        text.push_str(
            "Installing it does not let it run: a plugin with hooks or servers is withheld until you approve it in the desktop app.\n",
        );
    }
    text
}

fn render_installed(done: &PluginsInstallResult) -> String {
    let mut text = String::new();
    for note in &done.notes {
        text.push_str(&format!("Note: {note}\n"));
    }
    text.push_str(&format!("Installed {} into {}.\n", done.name, done.path));
    if done.withheld {
        text.push_str(&format!(
            "It has code (hooks or servers), so it is withheld: it does not run until you approve it in the desktop app (Plugins window).{}\n",
            done.reason
                .as_deref()
                .map(|reason| format!(" ({reason})"))
                .unwrap_or_default()
        ));
    } else {
        text.push_str("It holds no hooks or servers, so it is available to new sessions.\n");
    }
    text
}

fn render_list(listing: &PluginsListResult) -> String {
    let mut text = String::new();
    if listing.plugins.is_empty() {
        text.push_str("No plugins installed. Find some with: imperium plugin browse\n");
    }
    for plugin in &listing.plugins {
        let state = if plugin.withheld {
            "withheld"
        } else if plugin.enabled {
            "on"
        } else {
            "off"
        };
        let id = plugin
            .id
            .as_deref()
            .map(|id| format!(" ({id})"))
            .unwrap_or_default();
        text.push_str(&format!("{}{id}: {state}\n", plugin.name));
        if let Some(reason) = &plugin.reason {
            text.push_str(&format!("  why: {reason}\n"));
        }
        let origin = plugin.origin.as_deref().unwrap_or("not recorded");
        let approval = plugin.approval.as_deref().unwrap_or("not approved");
        text.push_str(&format!("  origin: {origin}; approval: {approval}\n"));
        if let Some(source) = &plugin.source {
            let commit = plugin
                .commit
                .as_deref()
                .map(|commit| format!(" at {}", short(commit)))
                .unwrap_or_default();
            text.push_str(&format!("  source: {source}{commit}\n"));
        }
        if plugin.hooks > 0 || !plugin.servers.is_empty() {
            text.push_str(&format!(
                "  runs: {} hooks; servers: {}\n",
                plugin.hooks,
                if plugin.servers.is_empty() {
                    "none".to_string()
                } else {
                    plugin.servers.join(", ")
                }
            ));
        }
        if !plugin.missing_secrets.is_empty() {
            text.push_str(&format!(
                "  missing secrets: {} (`imperium plugin secret set {} <VAR>`)\n",
                plugin.missing_secrets.join(", "),
                plugin.name
            ));
        }
    }
    for problem in &listing.problems {
        text.push_str(&format!("problem: {problem}\n"));
    }
    text
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use clap::CommandFactory;
    use imperium_cli::client::Client;
    use imperium_config::{Config, Layer, MemorySecrets, Paths};
    use imperium_core::{AccessToken, DaemonState};
    use imperium_protocol::PROTOCOL_VERSION;
    use imperium_protocol::discovery::DaemonEndpoint;
    use imperium_protocol::methods::Peer;
    use tokio::net::TcpListener;

    use super::*;

    const VALUE: &str = "tok-9f3a-very-secret-value";

    /// Answers a script would: whether there is a terminal, the answer to a question, and a
    /// secret value.
    struct Scripted {
        terminal: bool,
        output: bool,
        answer: bool,
        asked: Vec<String>,
    }

    impl Scripted {
        fn script() -> Self {
            Self {
                terminal: false,
                output: false,
                answer: false,
                asked: Vec::new(),
            }
        }

        fn terminal(answer: bool) -> Self {
            Self {
                terminal: true,
                output: true,
                answer,
                asked: Vec::new(),
            }
        }
    }

    impl Prompter for Scripted {
        fn interactive(&self) -> bool {
            self.terminal
        }

        fn output_is_terminal(&self) -> bool {
            self.output
        }

        fn confirm(&mut self, question: &str) -> anyhow::Result<bool> {
            self.asked.push(question.to_string());
            Ok(self.answer)
        }

        fn secret(&mut self, _prompt: &str) -> anyhow::Result<String> {
            Ok(VALUE.to_string())
        }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A folder marketplace `mk` with a plugin that has a hook and a server, a skills-only one,
    /// and one whose server needs a secret.
    fn write_market(dir: &Path) {
        write(
            &dir.join(".claude-plugin/marketplace.json"),
            r#"{"name": "mk", "owner": {"name": "Owner"}, "plugins": [
                {"name": "hooked", "source": "./hooked", "description": "Hooks"},
                {"name": "plain", "source": "./plain"},
                {"name": "keyed", "source": "./keyed"}
            ]}"#,
        );
        write(
            &dir.join("hooked/.claude-plugin/plugin.json"),
            r#"{"name": "hooked", "version": "1.0.0"}"#,
        );
        write(
            &dir.join("hooked/hooks/hooks.json"),
            r#"{"hooks": {"PostToolUse": [{"hooks": [{"type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/hook.sh"}]}]}}"#,
        );
        write(&dir.join("hooked/hook.sh"), "touch ran.marker\n");
        write(
            &dir.join("plain/.claude-plugin/plugin.json"),
            r#"{"name": "plain"}"#,
        );
        write(
            &dir.join("plain/skills/pdf/SKILL.md"),
            "---\nname: pdf\ndescription: PDFs\n---\nbody\n",
        );
        write(
            &dir.join("keyed/.claude-plugin/plugin.json"),
            r#"{"name": "keyed"}"#,
        );
        write(
            &dir.join("keyed/.mcp.json"),
            r#"{"mcpServers": {"api": {"command": "echo", "env": {"TOKEN": "${API_KEY}"}}}}"#,
        );
    }

    struct Setup {
        // Held so the folders outlive the test.
        _home: tempfile::TempDir,
        _market: tempfile::TempDir,
        client: Client,
        market_path: String,
    }

    async fn setup() -> Setup {
        let home = tempfile::tempdir().unwrap();
        let market = tempfile::tempdir().unwrap();
        write_market(market.path());
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        let state = DaemonState::with_secrets(
            Paths::under(home.path()),
            settings,
            None,
            Arc::new(MemorySecrets::new()),
        );
        state.migrate_plugins();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let token = AccessToken::generate().unwrap();
        let endpoint = DaemonEndpoint {
            url: format!("ws://{}/rpc", listener.local_addr().unwrap()),
            token: token.as_str().to_string(),
            pid: std::process::id(),
            socket: None,
            protocol: PROTOCOL_VERSION,
        };
        tokio::spawn(imperium_core::serve(
            listener,
            token,
            Arc::clone(&state),
            std::future::pending(),
        ));
        let client = Client::connect(
            &endpoint,
            Peer {
                name: "test".into(),
                version: "0".into(),
            },
        )
        .await
        .unwrap();
        let market_path = market.path().display().to_string();
        Setup {
            _home: home,
            _market: market,
            client,
            market_path,
        }
    }

    async fn go(
        setup: &mut Setup,
        prompter: &mut Scripted,
        command: PluginCommand,
    ) -> (anyhow::Result<()>, String) {
        let mut out = Vec::new();
        let result = dispatch(&mut setup.client, &mut out, prompter, command).await;
        (result, String::from_utf8(out).unwrap())
    }

    async fn add_market(setup: &mut Setup) {
        let source = setup.market_path.clone();
        let (result, _) = go(
            setup,
            &mut Scripted::script(),
            PluginCommand::Marketplace {
                command: MarketplaceCommand::Add {
                    source,
                    revision: None,
                },
            },
        )
        .await;
        result.unwrap();
    }

    fn install_command(id: &str, yes: bool) -> PluginCommand {
        PluginCommand::Install {
            id: id.into(),
            environment: None,
            sha256: None,
            yes,
        }
    }

    #[tokio::test]
    async fn a_scripted_install_with_yes_lands_withheld_and_says_so() {
        let mut setup = setup().await;
        add_market(&mut setup).await;
        let mut script = Scripted::script();
        let (result, out) = go(&mut setup, &mut script, install_command("hooked@mk", true)).await;
        result.unwrap();
        // The trust prompt is printed with what would run, and the answer needed no terminal.
        assert!(
            out.contains("It would run:") && out.contains("hook tool.after"),
            "{out}"
        );
        assert!(out.contains("Content sha256:"), "{out}");
        assert!(out.contains("Installed hooked"), "{out}");
        assert!(
            out.contains("withheld") && out.contains("desktop app"),
            "{out}"
        );
        assert!(script.asked.is_empty());

        // It is listed, withheld, and turning it on is refused.
        let (result, out) = go(
            &mut setup,
            &mut Scripted::script(),
            PluginCommand::List { environment: None },
        )
        .await;
        result.unwrap();
        assert!(out.contains("hooked (hooked@mk): withheld"), "{out}");
        let (result, _) = go(
            &mut setup,
            &mut Scripted::script(),
            PluginCommand::Enable {
                name: "hooked".into(),
                environment: None,
            },
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn an_update_of_unchanged_content_is_answered_by_the_daemon_without_a_question() {
        let mut setup = setup().await;
        add_market(&mut setup).await;
        let (result, _) = go(
            &mut setup,
            &mut Scripted::script(),
            install_command("plain@mk", true),
        )
        .await;
        result.unwrap();
        // Not even `--yes` is needed: nothing would change, so nothing is asked.
        let mut script = Scripted::script();
        let (result, out) = go(
            &mut setup,
            &mut script,
            PluginCommand::Update {
                name: "plain".into(),
                sha256: None,
                yes: false,
            },
        )
        .await;
        result.unwrap();
        assert!(out.contains("plain@mk is up to date."), "{out}");
        assert!(script.asked.is_empty());
    }

    #[tokio::test]
    async fn a_plugin_without_code_is_installed_and_available() {
        let mut setup = setup().await;
        add_market(&mut setup).await;
        let (result, out) = go(
            &mut setup,
            &mut Scripted::script(),
            install_command("plain@mk", true),
        )
        .await;
        result.unwrap();
        assert!(out.contains("nothing in it runs"), "{out}");
        assert!(out.contains("available to new sessions"), "{out}");
        assert!(!out.contains("withheld"), "{out}");
    }

    #[tokio::test]
    async fn without_a_terminal_or_yes_nothing_is_installed() {
        let mut setup = setup().await;
        add_market(&mut setup).await;
        let (result, _) = go(
            &mut setup,
            &mut Scripted::script(),
            install_command("plain@mk", false),
        )
        .await;
        assert!(result.unwrap_err().to_string().contains("--yes"));
        let (result, out) = go(
            &mut setup,
            &mut Scripted::script(),
            PluginCommand::List { environment: None },
        )
        .await;
        result.unwrap();
        assert!(out.contains("No plugins installed"), "{out}");
    }

    #[tokio::test]
    async fn at_a_terminal_the_question_names_the_plugin_and_no_installs() {
        let mut setup = setup().await;
        add_market(&mut setup).await;
        let mut person = Scripted::terminal(false);
        let (result, out) = go(&mut setup, &mut person, install_command("plain@mk", false)).await;
        result.unwrap();
        assert_eq!(person.asked, ["Install plain@mk?"]);
        assert!(out.contains("Nothing was installed."), "{out}");
        let mut person = Scripted::terminal(true);
        let (result, out) = go(&mut setup, &mut person, install_command("plain@mk", false)).await;
        result.unwrap();
        assert!(out.contains("Installed plain"), "{out}");
    }

    #[tokio::test]
    async fn a_pin_that_does_not_match_stops_the_install() {
        let mut setup = setup().await;
        add_market(&mut setup).await;
        let (result, _) = go(
            &mut setup,
            &mut Scripted::script(),
            PluginCommand::Install {
                id: "plain@mk".into(),
                environment: None,
                sha256: Some("00".repeat(32)),
                yes: true,
            },
        )
        .await;
        assert!(result.unwrap_err().to_string().contains("you pinned"));
    }

    #[tokio::test]
    async fn a_secret_is_saved_without_being_printed() {
        let mut setup = setup().await;
        add_market(&mut setup).await;
        let (result, _) = go(
            &mut setup,
            &mut Scripted::script(),
            install_command("keyed@mk", true),
        )
        .await;
        result.unwrap();
        let (result, out) = go(
            &mut setup,
            &mut Scripted::script(),
            PluginCommand::List { environment: None },
        )
        .await;
        result.unwrap();
        assert!(out.contains("missing secrets: API_KEY"), "{out}");
        let (result, out) = go(
            &mut setup,
            &mut Scripted::script(),
            PluginCommand::Secret {
                command: PluginSecretCommand::Set {
                    name: "keyed".into(),
                    variable: "API_KEY".into(),
                },
            },
        )
        .await;
        result.unwrap();
        assert!(out.contains("plugin.keyed.api_key"), "{out}");
        assert!(!out.contains(VALUE), "{out}");
        let (result, out) = go(
            &mut setup,
            &mut Scripted::script(),
            PluginCommand::List { environment: None },
        )
        .await;
        result.unwrap();
        assert!(!out.contains("missing secrets"), "{out}");
        assert!(!out.contains(VALUE), "{out}");
    }

    fn preview_with(installed: Option<PluginInstalled>) -> PluginPreview {
        PluginPreview {
            id: "plain@mk".into(),
            name: "plain".into(),
            marketplace: "mk".into(),
            marketplace_source: "/mk".into(),
            marketplace_commit: None,
            source_kind: "path".into(),
            source: "plain".into(),
            commit: None,
            description: String::new(),
            version: None,
            author: None,
            homepage: None,
            license: None,
            hooks: Vec::new(),
            servers: Vec::new(),
            launcher: false,
            skills: Vec::new(),
            commands: Vec::new(),
            agents: Vec::new(),
            commands_with_shell: Vec::new(),
            sha256: "aa".repeat(32),
            runs_digest: String::new(),
            installed,
            notes: Vec::new(),
        }
    }

    #[test]
    fn an_update_is_unchanged_only_when_folder_and_record_both_match() {
        let installed = |same_content, recorded: Option<String>| PluginInstalled {
            path: "/p".into(),
            recorded_sha256: recorded,
            same_content,
        };
        let hash = "aa".repeat(32);
        assert!(nothing_to_trust(&preview_with(Some(installed(
            true,
            Some(hash.clone())
        )))));
        // The folder matches but the record holds another hash, or none: ask as usual.
        assert!(!nothing_to_trust(&preview_with(Some(installed(
            true,
            Some("bb".repeat(32))
        )))));
        assert!(!nothing_to_trust(&preview_with(Some(installed(
            true, None
        )))));
        // The content differs, or nothing is installed.
        assert!(!nothing_to_trust(&preview_with(Some(installed(
            false,
            Some(hash)
        )))));
        assert!(!nothing_to_trust(&preview_with(None)));
    }

    fn offers() -> PluginsSuggestionsResult {
        use imperium_protocol::methods::{SuggestedMarketplace, SuggestedPlugin};
        PluginsSuggestionsResult {
            source_folder: Some("/work/app".into()),
            trusted: true,
            muted: false,
            marketplaces: vec![SuggestedMarketplace {
                key: "marketplace:acme/plugins".into(),
                source: "acme/plugins; rm -rf ~ \u{1b}[2J\r".into(),
                revision: Some("v1".into()),
                file: "/work/app/.imperium/config.toml".into(),
                added: false,
            }],
            plugins: vec![SuggestedPlugin {
                key: "plugin:lint@acme".into(),
                id: "lint@acme".into(),
                file: "/work/app/.imperium/config.toml".into(),
                marketplace_added: true,
                installed: false,
            }],
        }
    }

    #[test]
    fn the_commands_are_printed_only_for_a_terminal_and_repository_text_is_escaped() {
        let piped = render_suggestions(&offers(), false);
        assert!(!piped.contains("imperium plugin"), "{piped}");
        assert!(piped.contains("lint@acme"), "{piped}");
        let shown = render_suggestions(&offers(), true);
        assert!(
            shown.contains("imperium plugin install lint@acme"),
            "{shown}"
        );
        assert!(
            shown.contains("imperium plugin marketplace add '"),
            "{shown}"
        );
        for text in [&piped, &shown] {
            assert!(!text.contains('\u{1b}') && !text.contains('\r'), "{text:?}");
            assert!(text.contains("\\u{1b}"), "{text}");
        }
        // The hostile text is one quoted word, not several commands.
        assert!(
            shown.contains("'acme/plugins; rm -rf ~ \\u{1b}[2J\\u{d}'"),
            "{shown}"
        );
    }

    #[test]
    fn nothing_is_offered_from_an_untrusted_or_muted_project() {
        let mut result = offers();
        result.trusted = false;
        let text = render_suggestions(&result, true);
        assert!(
            text.contains("mcp.trusted_projects") && !text.contains("acme"),
            "{text}"
        );
        result.trusted = true;
        result.muted = true;
        let text = render_suggestions(&result, true);
        assert!(text.contains("muted") && !text.contains("acme"), "{text}");
    }

    #[test]
    fn bidirectional_overrides_are_escaped_too() {
        assert_eq!(printable("a\u{202e}b"), "a\\u{202e}b");
        assert_eq!(printable("tab\there"), "tab\\u{9}here");
        assert_eq!(printable("plain text é"), "plain text é");
    }

    #[test]
    fn a_piped_value_loses_only_its_final_line_break_and_is_capped() {
        assert_eq!(read_piped("tok\r\n".as_bytes()).unwrap(), "tok");
        assert_eq!(read_piped("a\nb".as_bytes()).unwrap(), "a\nb");
        assert_eq!(read_piped("".as_bytes()).unwrap(), "");
        let big = "x".repeat(MAX_SECRET_BYTES);
        assert_eq!(read_piped(big.as_bytes()).unwrap().len(), MAX_SECRET_BYTES);
        let too_big = "x".repeat(MAX_SECRET_BYTES + 1);
        assert!(read_piped(too_big.as_bytes()).is_err());
    }

    #[test]
    fn the_command_line_has_no_way_to_approve_and_takes_no_secret_argument() {
        fn walk(command: &clap::Command, found: &mut Vec<String>) {
            found.push(command.get_name().to_string());
            for argument in command.get_arguments() {
                found.push(argument.get_id().to_string());
            }
            for sub in command.get_subcommands() {
                walk(sub, found);
            }
        }
        let root = crate::Cli::command();
        let plugin = root
            .find_subcommand("plugin")
            .expect("the plugin command exists");
        let mut names = Vec::new();
        walk(plugin, &mut names);
        for forbidden in ["approve", "approval", "trust", "value", "secret_value"] {
            assert!(
                !names.iter().any(|name| name == forbidden),
                "{forbidden} in {names:?}"
            );
        }
        // `secret set` takes the plugin and the variable only.
        let set = plugin
            .find_subcommand("secret")
            .and_then(|secret| secret.find_subcommand("set"))
            .unwrap();
        let positional: Vec<_> = set
            .get_arguments()
            .filter(|argument| argument.is_positional())
            .map(|argument| argument.get_id().to_string())
            .collect();
        assert_eq!(positional, ["name", "variable"]);
    }
}
