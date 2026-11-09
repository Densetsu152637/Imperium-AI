//! Path grants and permission rules: the global ones in the user config (`[permissions]`), each
//! environment's in `environments.json`, the rule handles running sessions check calls against
//! (so a change applies from their next call), the `permissions.*` methods, and one-click
//! presets for common package caches and toolchains, resolved for this machine.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError, RwLock};

use imperium_config::PermissionSettings;
use imperium_protocol::jsonrpc::{ErrorObject, codes};
use imperium_protocol::methods::EnvironmentUpdateParams;
use imperium_protocol::permissions as wire;
use imperium_tools::permissions::{
    PathGrant, PathMode, PermissionRule, Permissions, RuleAction, SharedRules, home_dir,
    validate_grant, validate_rule,
};

use crate::state::DaemonState;

/// The rules sessions hold, kept current as the user changes them.
#[derive(Debug, Default)]
pub struct PermissionStore {
    global_rules: SharedRules,
    global_grants: RwLock<Vec<PathGrant>>,
    environments: Mutex<HashMap<String, SharedRules>>,
}

impl PermissionStore {
    pub fn new(settings: &PermissionSettings) -> Self {
        PermissionStore {
            global_rules: SharedRules::new(settings.rules.clone()),
            global_grants: RwLock::new(settings.grants.clone()),
            environments: Mutex::default(),
        }
    }

    /// The global grants as last saved.
    pub fn global_grants(&self) -> Vec<PathGrant> {
        self.global_grants
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_global(&self, settings: &PermissionSettings) {
        self.global_rules.set(settings.rules.clone());
        *self
            .global_grants
            .write()
            .unwrap_or_else(PoisonError::into_inner) = settings.grants.clone();
    }

    /// The rules for a session in environment `id` (whose saved rules are `rules`), or the global
    /// rules alone without an environment.
    pub fn for_session(&self, environment: Option<(&str, &[wire::PermissionRule])>) -> Permissions {
        let local = match environment {
            Some((id, rules)) => self
                .environments
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(id.to_string())
                .or_insert_with(|| SharedRules::new(rules.iter().map(rule_from_wire).collect()))
                .clone(),
            None => SharedRules::default(),
        };
        Permissions::new(self.global_rules.clone(), local)
    }

    /// Applies an environment's saved rules to its running sessions.
    pub fn set_environment(&self, id: &str, rules: &[wire::PermissionRule]) {
        let rules: Vec<PermissionRule> = rules.iter().map(rule_from_wire).collect();
        self.environments
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(id.to_string())
            .or_default()
            .set(rules);
    }
}

// ---- conversions between the wire and the tools' shapes ----

pub fn grant_from_wire(grant: &wire::PathGrant) -> PathGrant {
    PathGrant {
        path: grant.path.trim().to_string(),
        mode: match grant.mode {
            wire::PathMode::Read => PathMode::Read,
            wire::PathMode::ReadWrite => PathMode::ReadWrite,
        },
    }
}

fn grant_to_wire(grant: &PathGrant) -> wire::PathGrant {
    wire::PathGrant {
        path: grant.path.clone(),
        mode: match grant.mode {
            PathMode::Read => wire::PathMode::Read,
            PathMode::ReadWrite => wire::PathMode::ReadWrite,
        },
    }
}

pub fn rule_from_wire(rule: &wire::PermissionRule) -> PermissionRule {
    PermissionRule {
        tool: rule.tool.trim().to_string(),
        pattern: rule.pattern.trim().to_string(),
        action: match rule.action {
            wire::RuleAction::Allow => RuleAction::Allow,
            wire::RuleAction::Ask => RuleAction::Ask,
            wire::RuleAction::Deny => RuleAction::Deny,
        },
    }
}

fn rule_to_wire(rule: &PermissionRule) -> wire::PermissionRule {
    wire::PermissionRule {
        tool: rule.tool.clone(),
        pattern: rule.pattern.clone(),
        action: match rule.action {
            RuleAction::Allow => wire::RuleAction::Allow,
            RuleAction::Ask => wire::RuleAction::Ask,
            RuleAction::Deny => wire::RuleAction::Deny,
        },
    }
}

/// A set as sent, trimmed and checked; an invalid grant or rule is refused with the reason.
pub fn checked(set: &wire::PermissionSet) -> Result<wire::PermissionSet, String> {
    let mut grants: Vec<wire::PathGrant> = Vec::new();
    for grant in &set.grants {
        validate_grant(&grant_from_wire(grant))?;
        let grant = grant_to_wire(&grant_from_wire(grant));
        if !grants.iter().any(|known| known.path == grant.path) {
            grants.push(grant);
        }
    }
    let mut rules: Vec<wire::PermissionRule> = Vec::new();
    for rule in &set.rules {
        validate_rule(&rule_from_wire(rule))?;
        let rule = rule_to_wire(&rule_from_wire(rule));
        // A later rule for the same tool and pattern replaces an earlier one.
        rules.retain(|known| !(known.tool == rule.tool && known.pattern == rule.pattern));
        rules.push(rule);
    }
    Ok(wire::PermissionSet { grants, rules })
}

fn invalid(message: impl Into<String>) -> ErrorObject {
    ErrorObject::new(codes::INVALID_PARAMS, message.into())
}

fn saved(state: &DaemonState) -> Result<PermissionSettings, ErrorObject> {
    imperium_config::load(state.paths(), None, std::env::vars())
        .and_then(|config| config.settings())
        .map(|settings| settings.permissions)
        .map_err(|error| ErrorObject::new(codes::INTERNAL_ERROR, error.to_string()))
}

fn result(settings: &PermissionSettings) -> wire::PermissionsResult {
    wire::PermissionsResult {
        global: wire::PermissionSet {
            grants: settings.grants.iter().map(grant_to_wire).collect(),
            rules: settings.rules.iter().map(rule_to_wire).collect(),
        },
        presets: presets(),
    }
}

/// `permissions.get`.
pub fn get(state: &DaemonState) -> Result<wire::PermissionsResult, ErrorObject> {
    Ok(result(&saved(state)?))
}

/// `permissions.set`: saves the global grants and rules and applies the rules at once.
pub fn set(
    state: &DaemonState,
    params: wire::PermissionSet,
) -> Result<wire::PermissionsResult, ErrorObject> {
    let set = checked(&params).map_err(invalid)?;
    let settings = PermissionSettings {
        grants: set.grants.iter().map(grant_from_wire).collect(),
        rules: set.rules.iter().map(rule_from_wire).collect(),
    };
    let saved = imperium_config::save_permissions(state.paths(), &settings, std::env::vars())
        .map_err(|error| invalid(error.to_string()))?;
    state.permissions().set_global(&saved);
    tracing::info!(
        grants = saved.grants.len(),
        rules = saved.rules.len(),
        "a client changed the global permissions"
    );
    Ok(result(&saved))
}

/// `permissions.add_rule`: one rule into an environment (given, or the session's) or the global
/// rules, replacing a rule for the same tool and pattern.
pub fn add_rule(
    state: &DaemonState,
    params: wire::PermissionAddRuleParams,
) -> Result<wire::PermissionAddRuleResult, ErrorObject> {
    validate_rule(&rule_from_wire(&params.rule)).map_err(invalid)?;
    let environment_id = match (&params.environment_id, &params.session_id) {
        (Some(id), _) => Some(id.clone()),
        (None, Some(session_id)) => {
            let session = state
                .session(session_id)
                .ok_or_else(|| invalid(format!("no session {session_id:?}")))?;
            state.summary(&session).environment_id
        }
        (None, None) => None,
    };
    match environment_id {
        Some(id) => {
            let env = state
                .environments()
                .find(&id)
                .map_err(|error| invalid(error.to_string()))?;
            let mut permissions = env.permissions.clone();
            permissions.rules.push(params.rule);
            update_environment(state, &env.id, permissions)?;
            Ok(wire::PermissionAddRuleResult {
                environment_id: Some(env.id),
            })
        }
        None => {
            let mut current = result(&saved(state)?).global;
            current.rules.push(params.rule);
            set(state, current)?;
            Ok(wire::PermissionAddRuleResult {
                environment_id: None,
            })
        }
    }
}

/// Saves an environment's grants and rules, applies the rules to its running sessions, and
/// tells every client.
fn update_environment(
    state: &DaemonState,
    id: &str,
    permissions: wire::PermissionSet,
) -> Result<(), ErrorObject> {
    state
        .environments()
        .update(EnvironmentUpdateParams {
            id: id.to_string(),
            permissions: Some(permissions),
            ..Default::default()
        })
        .map_err(|error| invalid(error.to_string()))?;
    state.environment_permissions_changed(id);
    Ok(())
}

// ---- presets ----

/// Common caches and toolchains, resolved for this machine: writable for caches, read-only for
/// toolchains. Paths in the home folder are written with `~` so the configuration travels.
pub fn presets() -> Vec<wire::PathPreset> {
    let home = home_dir().map(PathBuf::from);
    let env = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
    };
    let in_home = |rest: &str| home.as_ref().map(|home| home.join(rest));
    let local_app_data = env("LOCALAPPDATA");
    let cache_root = if cfg!(target_os = "macos") {
        in_home("Library/Caches")
    } else {
        env("XDG_CACHE_HOME").or_else(|| in_home(".cache"))
    };
    let windows = cfg!(windows);
    let os_cache = |windows_rest: &str, unix_rest: &str| -> Option<PathBuf> {
        if windows {
            local_app_data.as_ref().map(|base| base.join(windows_rest))
        } else {
            cache_root.as_ref().map(|base| base.join(unix_rest))
        }
    };
    let go_cache = env("GOMODCACHE")
        .or_else(|| {
            env("GOPATH").map(|gopath| {
                // GOPATH may list several folders; the module cache is in the first.
                let first = std::env::split_paths(&gopath).next().unwrap_or(gopath);
                first.join("pkg").join("mod")
            })
        })
        .or_else(|| in_home("go/pkg/mod"));
    let pnpm_store = if windows {
        local_app_data.as_ref().map(|base| base.join("pnpm/store"))
    } else if cfg!(target_os = "macos") {
        in_home("Library/pnpm/store")
    } else {
        env("XDG_DATA_HOME")
            .map(|base| base.join("pnpm/store"))
            .or_else(|| in_home(".local/share/pnpm/store"))
    };
    let candidates: Vec<(&str, &str, Option<PathBuf>, wire::PathMode)> = vec![
        (
            "cargo",
            "Cargo registry and tools (~/.cargo)",
            env("CARGO_HOME").or_else(|| in_home(".cargo")),
            wire::PathMode::ReadWrite,
        ),
        (
            "rustup",
            "Rust toolchains (rustup)",
            env("RUSTUP_HOME").or_else(|| in_home(".rustup")),
            wire::PathMode::Read,
        ),
        (
            "npm",
            "npm cache",
            env("npm_config_cache").or_else(|| {
                if windows {
                    local_app_data.as_ref().map(|base| base.join("npm-cache"))
                } else {
                    in_home(".npm")
                }
            }),
            wire::PathMode::ReadWrite,
        ),
        ("pnpm", "pnpm store", pnpm_store, wire::PathMode::ReadWrite),
        (
            "yarn",
            "Yarn cache",
            os_cache(
                "Yarn/Cache",
                if cfg!(target_os = "macos") {
                    "Yarn"
                } else {
                    "yarn"
                },
            ),
            wire::PathMode::ReadWrite,
        ),
        (
            "pip",
            "pip cache",
            env("PIP_CACHE_DIR").or_else(|| os_cache("pip/Cache", "pip")),
            wire::PathMode::ReadWrite,
        ),
        (
            "uv",
            "uv cache",
            env("UV_CACHE_DIR").or_else(|| {
                if windows {
                    local_app_data.as_ref().map(|base| base.join("uv/cache"))
                } else {
                    // uv uses the XDG cache folder on macOS too.
                    env("XDG_CACHE_HOME")
                        .or_else(|| in_home(".cache"))
                        .map(|base| base.join("uv"))
                }
            }),
            wire::PathMode::ReadWrite,
        ),
        ("go", "Go module cache", go_cache, wire::PathMode::ReadWrite),
        (
            "maven",
            "Maven repository (~/.m2)",
            in_home(".m2"),
            wire::PathMode::ReadWrite,
        ),
        (
            "gradle",
            "Gradle caches (~/.gradle)",
            env("GRADLE_USER_HOME").or_else(|| in_home(".gradle")),
            wire::PathMode::ReadWrite,
        ),
        (
            "nuget",
            "NuGet packages",
            env("NUGET_PACKAGES").or_else(|| in_home(".nuget/packages")),
            wire::PathMode::ReadWrite,
        ),
    ];
    candidates
        .into_iter()
        .filter_map(|(id, label, path, mode)| {
            let path = path?;
            Some(wire::PathPreset {
                id: id.into(),
                label: label.into(),
                exists: path.exists(),
                path: shown(&path, home.as_deref()),
                mode,
            })
        })
        .collect()
}

/// A path as written in a grant: `~/…` inside the home folder, with `/` separators there.
fn shown(path: &Path, home: Option<&Path>) -> String {
    if let Some(rest) = home.and_then(|home| path.strip_prefix(home).ok()) {
        let rest = rest.to_string_lossy().replace('\\', "/");
        return if rest.is_empty() {
            "~".into()
        } else {
            format!("~/{rest}")
        };
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_resolve_for_this_machine_and_validate() {
        let presets = presets();
        let ids: Vec<&str> = presets.iter().map(|preset| preset.id.as_str()).collect();
        for id in ["cargo", "rustup", "npm", "pip", "uv", "go"] {
            assert!(ids.contains(&id), "{id} missing from {ids:?}");
        }
        for preset in &presets {
            validate_grant(&PathGrant {
                path: preset.path.clone(),
                mode: PathMode::Read,
            })
            .unwrap_or_else(|error| panic!("{}: {error}", preset.id));
        }
        let rustup = presets.iter().find(|preset| preset.id == "rustup").unwrap();
        assert_eq!(rustup.mode, wire::PathMode::Read);
    }

    #[test]
    fn sets_are_checked_and_deduplicated() {
        let rule = |pattern: &str, action| wire::PermissionRule {
            tool: "run_command".into(),
            pattern: pattern.into(),
            action,
        };
        let set = wire::PermissionSet {
            grants: vec![
                wire::PathGrant {
                    path: " ~/.cargo ".into(),
                    mode: wire::PathMode::ReadWrite,
                },
                wire::PathGrant {
                    path: "~/.cargo".into(),
                    mode: wire::PathMode::Read,
                },
            ],
            rules: vec![
                rule("cargo test", wire::RuleAction::Allow),
                rule(" cargo test ", wire::RuleAction::Deny),
            ],
        };
        let clean = checked(&set).unwrap();
        assert_eq!(clean.grants.len(), 1);
        assert_eq!(clean.grants[0].path, "~/.cargo");
        assert_eq!(
            clean.rules,
            vec![rule("cargo test", wire::RuleAction::Deny)]
        );
        let bad = wire::PermissionSet {
            grants: vec![wire::PathGrant {
                path: "relative".into(),
                mode: wire::PathMode::Read,
            }],
            rules: Vec::new(),
        };
        assert!(checked(&bad).is_err());
    }

    #[test]
    fn environment_rule_changes_reach_running_sessions() {
        let store = PermissionStore::new(&PermissionSettings::default());
        let saved = [wire::PermissionRule {
            tool: "run_command".into(),
            pattern: "make".into(),
            action: wire::RuleAction::Deny,
        }];
        let session = store.for_session(Some(("env1", &saved)));
        let target = imperium_tools::permissions::Target::Command("make all".into());
        assert_eq!(
            session.evaluate("run_command", &target),
            Some(RuleAction::Deny)
        );
        store.set_environment(
            "env1",
            &[wire::PermissionRule {
                tool: "run_command".into(),
                pattern: "make".into(),
                action: wire::RuleAction::Allow,
            }],
        );
        assert_eq!(
            session.evaluate("run_command", &target),
            Some(RuleAction::Allow)
        );
        store.set_global(&PermissionSettings {
            grants: Vec::new(),
            rules: vec![PermissionRule {
                tool: "run_command".into(),
                pattern: String::new(),
                action: RuleAction::Deny,
            }],
        });
        assert_eq!(
            session.evaluate("run_command", &target),
            Some(RuleAction::Deny)
        );
    }
}
