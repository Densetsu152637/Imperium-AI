//! Approving a plugin: the one place the desktop app asks the daemon to count an approval
//! (docs/06-extensibility.md#plugin-marketplaces, docs/07-desktop-ui.md#plugin-approval-implemented).
//!
//! The daemon counts an approval only from this app's own process, over the local socket, after a
//! challenge it issued to the same connection. So the whole flow stays in Rust and the webview
//! only triggers it:
//!
//! 1. the command is callable only from the app's own windows (`own_window`, and the access
//!    control list: `build.rs` and `capabilities/default.json`);
//! 2. the app's own environment is checked at start for the loader and WebView2 overrides the
//!    daemon refuses in a peer (it reads no peer environment on Windows); if one is set nothing
//!    is sent;
//! 3. the app connects on the local socket, never loopback TCP (`local_socket`), and asks the
//!    daemon for the plugin's preview (`plugins.inspect`);
//! 4. a native dialog (`tauri-plugin-dialog`) shows what the daemon's preview says the plugin runs
//!    (`dialog_text`), never text the webview supplied;
//! 5. on *Approve* the app installs or updates the plugin when asked, requests a challenge for the
//!    preview's hash and digest and answers it with `plugins.approve`, all on that one connection.
//!
//! The challenge is requested after the click rather than before the dialog: it lives two minutes,
//! and a plugin being installed has no folder to challenge before that. The daemon refuses a
//! challenge, and an approval, whose folder does not have the previewed hash and digest, so what
//! the dialog showed is what is approved.

use std::time::Duration;

use imperium_client::Client;
use imperium_config::Paths;
use imperium_protocol::discovery::{DaemonEndpoint, read_endpoint};
use imperium_protocol::methods::{
    Method, Peer, PluginPreview, PluginsApprovalChallenge, PluginsApprovalChallengeParams,
    PluginsApprove, PluginsApproveParams, PluginsInspect, PluginsInspectParams, PluginsInstall,
    PluginsInstallParams, PluginsList, PluginsListParams, PluginsUpdate, PluginsUpdateParams,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

/// How long connecting and the handshake may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a call that fetches a plugin (inspect, install, update) may take.
const FETCH_TIMEOUT: Duration = Duration::from_secs(180);
/// How long a quick call (list, challenge) may take.
const QUICK_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the approval may take: the daemon may raise an OS prompt first (stage 8c).
const APPROVE_TIMEOUT: Duration = Duration::from_secs(300);
/// The longest name or environment the command takes.
const MAX_TEXT: usize = 256;
/// The most text the dialog shows. A plugin whose run summary does not fit is refused rather than
/// shown cut short: an approval must be for everything the dialog listed.
const MAX_DIALOG: usize = 6000;

/// What the approval is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Install a marketplace plugin (`name@marketplace`), then approve it.
    Install,
    /// Replace the installed plugin with the marketplace's current content, then approve it.
    Update,
    /// Approve the plugin as installed, which must be what the marketplace offers now.
    Approve,
}

/// What the webview is told. It carries nothing the daemon's challenge or the dialog held.
#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct Outcome {
    /// The daemon recorded an approval.
    pub approved: bool,
    /// The recorded kind: `counted`, `dialog-only` or `dev`.
    pub kind: Option<String>,
    /// Why nothing was recorded.
    pub reason: Option<String>,
    /// The user closed the dialog without approving: nothing was installed or recorded.
    pub cancelled: bool,
    /// This call installed or replaced the plugin's folder (whether or not it was then approved).
    pub installed: bool,
}

impl Outcome {
    fn refused(reason: impl Into<String>) -> Self {
        Self {
            reason: Some(reason.into()),
            ..Self::default()
        }
    }
}

/// The app's own environment as it was at start: the first variable the daemon would refuse in
/// a peer (`imperium_ipc::refused_variable`, the daemon's own list), if any.
pub struct Guard {
    refused: Option<String>,
}

impl Guard {
    /// Reads this process's environment.
    pub fn capture() -> Self {
        let names: Vec<String> = std::env::vars_os()
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();
        Self::from_names(names.iter().map(String::as_str))
    }

    fn from_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            refused: imperium_ipc::refused_variable(names),
        }
    }
}

/// The windows that may call `plugin_approve`: the main window and the detached panels. A mod
/// frame is no window of its own, and the access control list gives it no permission.
fn own_window(label: &str) -> bool {
    label == "main"
        || label
            .strip_prefix("panel-")
            .is_some_and(|rest| !rest.is_empty())
}

/// A name or environment the webview named: not empty, short, with no control characters.
fn plain(value: &str, what: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > MAX_TEXT || value.chars().any(char::is_control) {
        return Err(format!(
            "{what} must be 1 to {MAX_TEXT} characters of plain text"
        ));
    }
    Ok(value.to_string())
}

/// The daemon's local socket, or why there is none. Approvals are never sent over loopback TCP,
/// which the daemon would refuse and which any local process could reach.
fn local_socket(endpoint: &DaemonEndpoint) -> Result<&str, String> {
    endpoint
        .socket
        .as_deref()
        .filter(|socket| !socket.is_empty())
        .ok_or_else(|| {
            "the daemon serves no local socket (`daemon.socket` is off), and approvals are taken \
             only over it, never over loopback TCP"
                .to_string()
        })
}

fn inspect_params(id: &str) -> PluginsInspectParams {
    PluginsInspectParams {
        id: id.to_string(),
        revision: None,
    }
}

/// The install of exactly the content the dialog showed: the daemon refuses another hash.
fn install_params(preview: &PluginPreview, environment: Option<&str>) -> PluginsInstallParams {
    PluginsInstallParams {
        id: preview.id.clone(),
        sha256: preview.sha256.clone(),
        revision: None,
        environment: environment.map(str::to_string),
        approve: None,
    }
}

fn update_params(preview: &PluginPreview) -> PluginsUpdateParams {
    PluginsUpdateParams {
        name: preview.id.clone(),
        sha256: preview.sha256.clone(),
    }
}

/// The challenge for the previewed hash and digest, never for values the webview sent.
fn challenge_params(name: &str, preview: &PluginPreview) -> PluginsApprovalChallengeParams {
    PluginsApprovalChallengeParams {
        name: name.to_string(),
        sha256: preview.sha256.clone(),
        runs_digest: preview.runs_digest.clone(),
    }
}

fn approve_params(name: &str, preview: &PluginPreview, challenge: &str) -> PluginsApproveParams {
    PluginsApproveParams {
        name: name.to_string(),
        sha256: preview.sha256.clone(),
        runs_digest: preview.runs_digest.clone(),
        challenge: challenge.to_string(),
    }
}

/// Whether the preview is something this action can approve.
fn check_preview(action: Action, preview: &PluginPreview) -> Result<(), String> {
    if preview.hooks.is_empty() && preview.servers.is_empty() {
        return Err(format!(
            "{} runs no hook or server, so there is nothing to approve",
            preview.id
        ));
    }
    match (action, &preview.installed) {
        (Action::Install, Some(_)) => Err(format!(
            "{} is already installed: update it, or approve it as installed",
            preview.id
        )),
        (Action::Update, None) => Err(format!("{} is not installed", preview.id)),
        (Action::Update, Some(installed)) if installed.same_content => Err(format!(
            "{} is already up to date: approve it as installed",
            preview.id
        )),
        (Action::Approve, None) => Err(format!("{} is not installed", preview.id)),
        (Action::Approve, Some(installed)) if !installed.same_content => Err(format!(
            "the installed copy of {} is not what its marketplace offers now: update it first",
            preview.id
        )),
        _ => Ok(()),
    }
}

/// Shows a value on one line, with control characters and the characters that reorder or hide
/// text written out, so a command line cannot change how the dialog reads.
fn show(value: &str) -> String {
    let mut shown = String::with_capacity(value.len());
    for c in value.chars() {
        let hidden = c.is_control()
            || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}');
        if hidden {
            match c {
                '\n' => shown.push_str("\\n"),
                '\r' => shown.push_str("\\r"),
                '\t' => shown.push_str("\\t"),
                _ => shown.push_str(&format!("\\u{{{:04x}}}", u32::from(c))),
            }
        } else {
            shown.push(c);
        }
    }
    shown
}

/// What the dialog says, from the daemon's preview alone: the plugin, where it came from, every
/// hook and server command line with launchers flagged, and the content hash the approval is
/// bound to. Refused when it would not fit, so nothing the plugin runs is left out.
fn dialog_text(action: Action, preview: &PluginPreview) -> Result<String, String> {
    let mut text = String::new();
    let verb = match action {
        Action::Install => "Install and approve",
        Action::Update => "Update and approve",
        Action::Approve => "Approve",
    };
    text.push_str(&format!("{verb} {}?\n\n", show(&preview.id)));
    text.push_str(
        "Approving lets this plugin run the commands below on your computer, outside Imperium's \
         tool sandbox, with your permissions.\n\n",
    );
    text.push_str(&format!(
        "Source: {} {}\n",
        show(&preview.source_kind),
        show(&preview.source)
    ));
    if let Some(commit) = &preview.commit {
        text.push_str(&format!("Commit: {}\n", show(commit)));
    }
    text.push_str(&format!(
        "Content hash (SHA-256): {}\n",
        show(&preview.sha256)
    ));
    if preview.launcher {
        text.push_str(
            "\nFLAGGED: it starts a launcher (npx, uvx, @latest or similar) that downloads and runs \
             more code when it starts. That code is not in the hash above.\n",
        );
    }
    if !preview.hooks.is_empty() {
        text.push_str(&format!("\nHooks ({}):\n", preview.hooks.len()));
        for hook in &preview.hooks {
            let what = match (&hook.url, hook.command.is_empty()) {
                (Some(url), true) => format!("HTTP {}", show(url)),
                _ => show(&hook.command),
            };
            text.push_str(&format!(
                "  {} [{}]: {}{}\n",
                show(&hook.event),
                show(&hook.matcher),
                what,
                if hook.launcher { "  (LAUNCHER)" } else { "" }
            ));
        }
    }
    if !preview.servers.is_empty() {
        text.push_str(&format!("\nMCP servers ({}):\n", preview.servers.len()));
        for server in &preview.servers {
            let what = match (&server.command, &server.url) {
                (Some(command), _) => std::iter::once(command.as_str())
                    .chain(server.args.iter().map(String::as_str))
                    .map(show)
                    .collect::<Vec<_>>()
                    .join(" "),
                (None, Some(url)) => format!("HTTP {}", show(url)),
                (None, None) => String::new(),
            };
            text.push_str(&format!(
                "  {} ({}): {}{}\n",
                show(&server.name),
                show(&server.transport),
                what,
                if server.launcher { "  (LAUNCHER)" } else { "" }
            ));
        }
    }
    for note in &preview.notes {
        text.push_str(&format!("\nNote: {}\n", show(note)));
    }
    if text.chars().count() > MAX_DIALOG {
        return Err(format!(
            "{} runs too much to show in full here, so it cannot be approved",
            preview.id
        ));
    }
    Ok(text)
}

/// Calls one daemon method, giving up after `limit`.
async fn ask<M: Method>(
    client: &mut Client,
    params: M::Params,
    limit: Duration,
) -> Result<M::Result, String> {
    match tokio::time::timeout(limit, client.call::<M>(params)).await {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(_) => Err(format!("{} did not answer in time", M::NAME)),
    }
}

/// This app's own connection to the daemon, on its local socket.
async fn connect(version: String) -> Result<Client, String> {
    let file = Paths::from_env()
        .map_err(|error| error.to_string())?
        .daemon_endpoint_file();
    let endpoint = read_endpoint(&file).map_err(|error| error.to_string())?;
    let socket = local_socket(&endpoint)?;
    let peer = Peer {
        name: "imperium-desktop".into(),
        version,
    };
    let connecting = Client::connect_socket(socket, &endpoint.token, Some(endpoint.pid), peer);
    match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(_) => Err("imperium-core did not answer on its local socket in time".into()),
    }
}

/// `name@marketplace` for what the webview named: itself when it already is one, else the id
/// of the installed plugin with that plugin or folder name.
async fn resolve_id(client: &mut Client, action: Action, name: &str) -> Result<String, String> {
    if name.contains('@') {
        return Ok(name.to_string());
    }
    if action == Action::Install {
        return Err("an install names the plugin as name@marketplace".into());
    }
    let listed = ask::<PluginsList>(client, PluginsListParams::default(), QUICK_TIMEOUT).await?;
    listed
        .plugins
        .iter()
        .find(|plugin| {
            plugin.name == name
                || std::path::Path::new(&plugin.folder)
                    .file_name()
                    .is_some_and(|folder| folder.to_string_lossy() == name)
        })
        .ok_or_else(|| format!("no plugin {name} is installed"))?
        .id
        .clone()
        .ok_or_else(|| {
            format!(
                "{name} did not come from a marketplace, so the daemon has no preview of it to show"
            )
        })
}

/// Shows the dialog and waits for the answer: `true` only for *Approve*.
async fn confirm<R: Runtime>(app: &AppHandle<R>, window: &WebviewWindow<R>, text: String) -> bool {
    let (answer, answered) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(text)
        .title("Approve plugin")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Approve".into(),
            "Cancel".into(),
        ))
        .parent(window)
        .show(move |approved| {
            let _ = answer.send(approved);
        });
    answered.await.unwrap_or(false)
}

async fn flow<R: Runtime>(
    app: &AppHandle<R>,
    window: &WebviewWindow<R>,
    name: &str,
    action: Action,
    environment: Option<&str>,
) -> Result<Outcome, String> {
    let mut client = connect(app.package_info().version.to_string()).await?;
    let id = resolve_id(&mut client, action, name).await?;
    let preview = ask::<PluginsInspect>(&mut client, inspect_params(&id), FETCH_TIMEOUT).await?;
    if preview.id != id {
        return Err(format!(
            "the daemon previewed {} instead of {id}",
            preview.id
        ));
    }
    check_preview(action, &preview)?;
    let text = dialog_text(action, &preview)?;
    if !confirm(app, window, text).await {
        return Ok(Outcome {
            cancelled: true,
            reason: Some("cancelled: nothing was installed or recorded".into()),
            ..Outcome::default()
        });
    }
    let (plugin, installed) = match action {
        Action::Install => {
            let done = ask::<PluginsInstall>(
                &mut client,
                install_params(&preview, environment),
                FETCH_TIMEOUT,
            )
            .await?;
            (done.name, true)
        }
        Action::Update => {
            let done =
                ask::<PluginsUpdate>(&mut client, update_params(&preview), FETCH_TIMEOUT).await?;
            (done.name, done.replaced)
        }
        Action::Approve => (preview.id.clone(), false),
    };
    let challenge = ask::<PluginsApprovalChallenge>(
        &mut client,
        challenge_params(&plugin, &preview),
        QUICK_TIMEOUT,
    )
    .await;
    let answer = match challenge {
        Ok(challenge) => {
            ask::<PluginsApprove>(
                &mut client,
                approve_params(&plugin, &preview, &challenge.challenge),
                APPROVE_TIMEOUT,
            )
            .await
        }
        Err(reason) => Err(reason),
    };
    Ok(match answer {
        Ok(result) => Outcome {
            approved: result.approved,
            kind: result.kind,
            reason: result.reason,
            cancelled: false,
            installed,
        },
        Err(reason) => Outcome {
            reason: Some(reason),
            installed,
            ..Outcome::default()
        },
    })
}

/// Approves a plugin after the user confirms it in a native dialog. The webview names the plugin
/// and the action; the challenge, the dialog and the approval never pass through it.
#[tauri::command]
pub async fn plugin_approve<R: Runtime>(
    window: WebviewWindow<R>,
    app: AppHandle<R>,
    guard: tauri::State<'_, Guard>,
    name: String,
    action: Action,
    environment: Option<String>,
) -> Result<Outcome, String> {
    if !own_window(window.label()) {
        return Err("plugins are approved only from the app's own windows".into());
    }
    if let Some(variable) = &guard.refused {
        return Ok(Outcome::refused(format!(
            "{variable} is set in this app's environment, which could change what the app loads: \
             start Imperium without it. Nothing was sent."
        )));
    }
    let name = plain(&name, "the plugin name")?;
    let environment = environment
        .map(|environment| plain(&environment, "the environment"))
        .transpose()?;
    flow(&app, &window, &name, action, environment.as_deref()).await
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use imperium_protocol::PROTOCOL_VERSION;
    use imperium_protocol::methods::{PluginHookRun, PluginInstalled, PluginServerRun};

    use super::*;

    fn preview() -> PluginPreview {
        PluginPreview {
            id: "p@m".into(),
            name: "p".into(),
            marketplace: "m".into(),
            marketplace_source: "https://example.test/m.git".into(),
            marketplace_commit: Some("a".repeat(40)),
            source_kind: "github".into(),
            source: "owner/p".into(),
            commit: Some("b".repeat(40)),
            description: "Does nothing, honestly.".into(),
            version: Some("1.0.0".into()),
            author: None,
            homepage: None,
            license: None,
            hooks: vec![PluginHookRun {
                event: "PreToolUse".into(),
                matcher: "Bash".into(),
                command: "/plugins/p/hooks/check.sh --strict".into(),
                url: None,
                launcher: false,
            }],
            servers: vec![PluginServerRun {
                name: "p_search".into(),
                transport: "stdio".into(),
                command: Some("npx".into()),
                args: vec!["-y".into(), "search-server@latest".into()],
                env_names: vec!["API_KEY".into()],
                url: None,
                header_names: vec![],
                variables: vec![],
                launcher: true,
            }],
            launcher: true,
            skills: vec![],
            commands: vec![],
            agents: vec![],
            commands_with_shell: vec![],
            sha256: "c".repeat(64),
            runs_digest: "d".repeat(64),
            installed: None,
            notes: vec![],
        }
    }

    fn installed(same_content: bool) -> Option<PluginInstalled> {
        Some(PluginInstalled {
            path: "/config/plugins/p".into(),
            recorded_sha256: None,
            same_content,
        })
    }

    #[test]
    fn only_the_apps_own_windows_may_call() {
        assert!(own_window("main"));
        assert!(own_window("panel-agents"));
        assert!(!own_window("panel-"));
        assert!(!own_window("mod-frame"));
        assert!(!own_window("main2"));
        assert!(!own_window(""));
    }

    #[test]
    fn a_tcp_only_endpoint_is_refused() {
        let mut endpoint = DaemonEndpoint {
            url: "ws://127.0.0.1:52011/rpc".into(),
            token: "t".into(),
            pid: 1,
            protocol: PROTOCOL_VERSION,
            socket: None,
        };
        let error = local_socket(&endpoint).unwrap_err();
        assert!(error.contains("never over loopback TCP"), "{error}");
        endpoint.socket = Some(String::new());
        assert!(local_socket(&endpoint).is_err());
        endpoint.socket = Some("/run/user/1000/imperium/core.sock".into());
        assert_eq!(
            local_socket(&endpoint).unwrap(),
            "/run/user/1000/imperium/core.sock"
        );
    }

    #[test]
    fn the_requests_carry_the_previewed_hash_and_digest() {
        let preview = preview();
        let install = install_params(&preview, Some("work"));
        assert_eq!(install.id, "p@m");
        assert_eq!(install.sha256, preview.sha256);
        assert_eq!(install.environment.as_deref(), Some("work"));
        assert!(
            install.approve.is_none(),
            "an install never asks to approve"
        );
        let update = update_params(&preview);
        assert_eq!(
            (update.name.as_str(), update.sha256.as_str()),
            ("p@m", preview.sha256.as_str())
        );
        let challenge = challenge_params("p", &preview);
        assert_eq!(challenge.name, "p");
        assert_eq!(challenge.sha256, preview.sha256);
        assert_eq!(challenge.runs_digest, preview.runs_digest);
        let approve = approve_params("p", &preview, "token");
        assert_eq!(approve.challenge, "token");
        assert_eq!(approve.sha256, preview.sha256);
        assert_eq!(approve.runs_digest, preview.runs_digest);
        assert_eq!(inspect_params("p@m").revision, None);
    }

    #[test]
    fn what_each_action_may_approve() {
        let mut found = preview();
        assert!(check_preview(Action::Install, &found).is_ok());
        assert!(check_preview(Action::Update, &found).is_err());
        assert!(check_preview(Action::Approve, &found).is_err());
        found.installed = installed(true);
        assert!(check_preview(Action::Approve, &found).is_ok());
        assert!(check_preview(Action::Install, &found).is_err());
        assert!(check_preview(Action::Update, &found).is_err());
        found.installed = installed(false);
        assert!(check_preview(Action::Update, &found).is_ok());
        let error = check_preview(Action::Approve, &found).unwrap_err();
        assert!(error.contains("update it first"), "{error}");
        // A plugin with no hook or server has nothing to approve.
        found.hooks.clear();
        found.servers.clear();
        assert!(check_preview(Action::Update, &found).is_err());
    }

    #[test]
    fn the_dialog_shows_the_id_the_commands_the_flags_and_the_hash() {
        let preview = preview();
        let text = dialog_text(Action::Install, &preview).unwrap();
        for wanted in [
            "Install and approve p@m?",
            "/plugins/p/hooks/check.sh --strict",
            "PreToolUse [Bash]",
            "npx -y search-server@latest",
            "(LAUNCHER)",
            "FLAGGED",
            &"c".repeat(64),
            "github owner/p",
        ] {
            assert!(text.contains(wanted), "missing {wanted:?} in:\n{text}");
        }
        // What the plugin says about itself is not shown beside what it runs.
        assert!(!text.contains("Does nothing"));
    }

    #[test]
    fn the_dialog_writes_out_characters_that_could_disguise_a_command() {
        let mut preview = preview();
        preview.hooks[0].command = "echo hi\nApprove q@m?\u{202e}evil".into();
        let text = dialog_text(Action::Approve, &preview).unwrap();
        assert!(
            text.contains("echo hi\\nApprove q@m?\\u{202e}evil"),
            "{text}"
        );
        assert_eq!(text.matches("Approve q@m?").count(), 1);
        assert!(!text.contains('\u{202e}'));
    }

    #[test]
    fn a_plugin_that_does_not_fit_the_dialog_is_refused_not_cut_short() {
        let mut preview = preview();
        let hook = preview.hooks[0].clone();
        preview.hooks = vec![hook; 200];
        let error = dialog_text(Action::Approve, &preview).unwrap_err();
        assert!(error.contains("cannot be approved"), "{error}");
    }

    #[test]
    fn a_loader_override_in_the_apps_environment_refuses_with_its_name() {
        assert!(Guard::from_names(["PATH", "HOME"]).refused.is_none());
        assert_eq!(
            Guard::from_names(["PATH", "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS"])
                .refused
                .as_deref(),
            Some("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS")
        );
        let refused = Outcome::refused("LD_PRELOAD is set");
        assert!(!refused.approved);
        assert!(refused.reason.unwrap().contains("LD_PRELOAD"));
    }

    #[test]
    fn names_are_plain_text() {
        assert_eq!(plain("  p@m ", "x").unwrap(), "p@m");
        assert!(plain("", "x").is_err());
        assert!(plain("a\nb", "x").is_err());
        assert!(plain(&"a".repeat(MAX_TEXT + 1), "x").is_err());
    }

    /// The names in `generate_handler![...]` of `main.rs`.
    fn handler_commands() -> BTreeSet<String> {
        let main = include_str!("main.rs");
        let start = main.find("generate_handler![").unwrap() + "generate_handler![".len();
        let list = &main[start..start + main[start..].find(']').unwrap()];
        list.split(',')
            .map(|entry| entry.trim().rsplit("::").next().unwrap().to_string())
            .filter(|entry| !entry.is_empty())
            .collect()
    }

    #[test]
    fn every_command_is_listed_for_the_access_control_list_and_allowed_to_the_apps_windows() {
        let handlers = handler_commands();
        assert!(handlers.contains("plugin_approve"));

        let build = include_str!("../build.rs");
        let start = build.find(".commands(&[").unwrap() + ".commands(&[".len();
        let listed: BTreeSet<String> = build[start..start + build[start..].find(']').unwrap()]
            .split(',')
            .map(|entry| entry.trim().trim_matches('"').to_string())
            .filter(|entry| !entry.is_empty())
            .collect();
        assert_eq!(
            listed, handlers,
            "build.rs lists the commands main.rs registers"
        );

        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
        let allowed: BTreeSet<String> = capability["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|permission| permission.as_str()?.strip_prefix("allow-"))
            .filter(|permission| !permission.contains(':'))
            .map(|permission| permission.replace('-', "_"))
            .collect();
        assert_eq!(
            allowed, handlers,
            "the capability allows every registered command"
        );
    }

    #[test]
    fn no_capability_reaches_plugin_approve_from_another_window_or_a_remote_page() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities");
        let mut seen = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            seen += 1;
            let capability: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let grants = capability["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|permission| {
                    permission
                        .as_str()
                        .is_some_and(|name| name == "allow-plugin-approve")
                });
            if grants {
                let windows: Vec<&str> = capability["windows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|window| window.as_str())
                    .collect();
                assert_eq!(windows, ["main", "panel-*"], "{}", path.display());
                assert!(capability.get("remote").is_none(), "{}", path.display());
                assert!(capability.get("webviews").is_none(), "{}", path.display());
            }
        }
        assert!(seen > 0);
    }

    #[test]
    fn the_webview_never_names_the_approval_methods() {
        fn scan(dir: &Path, found: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    scan(&path, found);
                } else if let Ok(text) = std::fs::read_to_string(&path) {
                    for method in ["plugins.approve", "plugins.approval_challenge"] {
                        if text.contains(method) {
                            found.push(format!("{} names {method}", path.display()));
                        }
                    }
                }
            }
        }
        let mut found = Vec::new();
        scan(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../src"),
            &mut found,
        );
        assert!(found.is_empty(), "{found:?}");
    }
}
