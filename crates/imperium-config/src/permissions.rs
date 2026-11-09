//! The global `[permissions]` table: path grants outside environments and allow / ask / deny
//! rules on tool calls (see docs/05-tools-and-isolation.md#permissions). Like
//! [`crate::set_user_setting`], saving keeps the file's comments, formatting, and unknown keys,
//! and a change that leaves the configuration invalid is undone.
//!
//! ```toml
//! [permissions]
//! grants = [{ path = "~/.cargo", mode = "read_write" }, { path = "~/.rustup", mode = "read" }]
//! rules = [
//!   { tool = "run_command", pattern = "cargo test", action = "allow" },
//!   { tool = "*", pattern = "**/.env", action = "deny" },
//! ]
//! ```

use imperium_api::{PathGrant, PathMode, PermissionRule, RuleAction};
use serde::{Deserialize, Serialize};
use toml_edit::{Array, InlineTable, Item, Table, Value};

use crate::registry::edit_user_file;
use crate::{Paths, SetError};

/// Grants and rules that apply to every session, beside its environment's own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct PermissionSettings {
    pub grants: Vec<PathGrant>,
    pub rules: Vec<PermissionRule>,
}

/// Replaces the user file's `[permissions]` grants and rules (an empty list removes its key).
/// Returns them as loaded back.
pub fn save_permissions(
    paths: &Paths,
    permissions: &PermissionSettings,
    env: impl IntoIterator<Item = (String, String)>,
) -> Result<PermissionSettings, SetError> {
    let config = edit_user_file(paths, env, |document| {
        if document.get("permissions").is_none() {
            document.insert("permissions", Item::Table(Table::new()));
        }
        let table = document
            .get_mut("permissions")
            .and_then(Item::as_table_like_mut)
            .ok_or("permissions in the user config is not a table")?;
        let grants: Array = permissions.grants.iter().map(grant_value).collect();
        let rules: Array = permissions.rules.iter().map(rule_value).collect();
        for (key, list) in [("grants", grants), ("rules", rules)] {
            if list.is_empty() {
                table.remove(key);
            } else {
                table.insert(key, Item::Value(Value::Array(multiline(list))));
            }
        }
        if table.is_empty() {
            document.remove("permissions");
        }
        Ok(())
    })?;
    Ok(config.settings().map_err(SetError::Rejected)?.permissions)
}

/// One item per line, so the file stays readable as lists grow.
fn multiline(mut list: Array) -> Array {
    for item in list.iter_mut() {
        item.decor_mut().set_prefix("\n  ");
    }
    list.set_trailing("\n");
    list.set_trailing_comma(true);
    list
}

fn grant_value(grant: &PathGrant) -> Value {
    let mut table = InlineTable::new();
    table.insert("path", grant.path.trim().into());
    let mode = match grant.mode {
        PathMode::Read => "read",
        PathMode::ReadWrite => "read_write",
    };
    table.insert("mode", mode.into());
    Value::InlineTable(table)
}

fn rule_value(rule: &PermissionRule) -> Value {
    let mut table = InlineTable::new();
    table.insert("tool", rule.tool.trim().into());
    if !rule.pattern.trim().is_empty() {
        table.insert("pattern", rule.pattern.trim().into());
    }
    let action = match rule.action {
        RuleAction::Allow => "allow",
        RuleAction::Ask => "ask",
        RuleAction::Deny => "deny",
    };
    table.insert("action", action.into());
    Value::InlineTable(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_round_trip_through_the_user_file_keeping_comments() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(
            paths.user_config_file(),
            "# my settings\n[agents]\nmax_steps = 12\n",
        )
        .unwrap();
        let wanted = PermissionSettings {
            grants: vec![PathGrant {
                path: "~/.cargo".into(),
                mode: PathMode::ReadWrite,
            }],
            rules: vec![
                PermissionRule {
                    tool: "run_command".into(),
                    pattern: "cargo test".into(),
                    action: RuleAction::Allow,
                },
                PermissionRule {
                    tool: "*".into(),
                    pattern: String::new(),
                    action: RuleAction::Ask,
                },
            ],
        };
        let saved = save_permissions(&paths, &wanted, Vec::new()).unwrap();
        assert_eq!(saved, wanted);
        let text = std::fs::read_to_string(paths.user_config_file()).unwrap();
        assert!(text.contains("# my settings"), "{text}");
        assert!(text.contains("max_steps = 12"), "{text}");
        assert!(text.contains(r#"mode = "read_write""#), "{text}");

        let cleared = save_permissions(&paths, &PermissionSettings::default(), Vec::new()).unwrap();
        assert_eq!(cleared, PermissionSettings::default());
        let text = std::fs::read_to_string(paths.user_config_file()).unwrap();
        assert!(!text.contains("[permissions]"), "{text}");
    }
}
