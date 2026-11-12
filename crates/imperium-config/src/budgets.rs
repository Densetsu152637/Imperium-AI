//! The `[budgets]` table: token and cost budgets over daily, weekly, or monthly windows, for
//! everything, one environment, one endpoint, or one model, with alert thresholds and what
//! happens when a budget runs out (see docs/10-configuration.md#budgets). Like
//! [`crate::save_permissions`], saving keeps the file's comments, formatting, and unknown keys,
//! and a change that leaves the configuration invalid is undone.
//!
//! ```toml
//! [budgets]
//! week_start = "monday"
//! month_start_day = 1
//! thresholds = [50, 80, 100]
//! limits = [
//!   { id = "all-month", scope = "global", window = "monthly", cost_usd = 50.0, action = "ask" },
//!   { id = "work-week", scope = "environment", target = "env-1", window = "weekly", tokens = 5000000 },
//! ]
//! ```

use serde::{Deserialize, Serialize};
use toml_edit::{Array, InlineTable, Item, Table, Value};

use crate::registry::edit_user_file;
use crate::{Paths, SetError};

/// What a budget counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetScope {
    /// Every model call.
    #[default]
    Global,
    /// Calls from sessions in one environment (`target` is its id).
    Environment,
    /// Calls to one endpoint (`target` is its name, such as `openrouter` or `llamacpp`).
    Endpoint,
    /// Calls to one model (`target` is its spec, `<endpoint>:<model>`).
    Model,
    /// Calls that arrive through one entry point rather than from a session (`target` is its
    /// name; `api` is the OpenAI-compatible local API server).
    Source,
}

/// The period a budget resets over, in local time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetWindow {
    Daily,
    Weekly,
    #[default]
    Monthly,
}

/// What happens to new turns in the budget's scope once it is used up. Turns already running
/// always finish.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetAction {
    /// Alert only.
    #[default]
    Warn,
    /// Ask before each new turn.
    Ask,
    /// Refuse new turns until the window resets.
    Block,
}

/// The day weekly budgets start on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WeekStart {
    #[default]
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

/// One budget. At least one of `tokens` (prompt plus completion) and `cost_usd` is set; with
/// both, whichever is further along counts.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct Budget {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default)]
    pub scope: BudgetScope,
    /// The environment id, endpoint name, or model spec; empty for a global budget.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub target: String,
    #[serde(default)]
    pub window: BudgetWindow,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    /// In US dollars; only calls to models with prices count (local models have none unless
    /// one is configured).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Percentages that alert, replacing the table's `thresholds`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thresholds: Option<Vec<u32>>,
    #[serde(default)]
    pub action: BudgetAction,
}

/// The `[budgets]` table.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct BudgetSettings {
    pub week_start: WeekStart,
    /// The day of the month monthly budgets reset on, 1 to 28.
    pub month_start_day: u32,
    /// Percentages of a budget that alert, by default.
    pub thresholds: Vec<u32>,
    pub limits: Vec<Budget>,
}

/// The names a [`BudgetScope::Source`] budget may target.
pub const SOURCES: [&str; 1] = ["api"];

/// The thresholds a budget alerts at when none are configured.
pub const DEFAULT_THRESHOLDS: [u32; 3] = [50, 80, 100];

impl Default for BudgetSettings {
    fn default() -> Self {
        BudgetSettings {
            week_start: WeekStart::Monday,
            month_start_day: 1,
            thresholds: DEFAULT_THRESHOLDS.to_vec(),
            limits: Vec::new(),
        }
    }
}

impl BudgetSettings {
    /// Checks the table: valid days and thresholds, unique ids, a target where the scope needs
    /// one, and a positive limit on every budget.
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=28).contains(&self.month_start_day) {
            return Err(format!(
                "budgets.month_start_day must be 1 to 28, not {}",
                self.month_start_day
            ));
        }
        check_thresholds(&self.thresholds, "budgets.thresholds")?;
        let mut ids = std::collections::HashSet::new();
        for budget in &self.limits {
            let id = budget.id.trim();
            if id.is_empty() {
                return Err("every budget needs an id".into());
            }
            if !ids.insert(id) {
                return Err(format!("two budgets have the id {id:?}"));
            }
            let target = budget.target.trim();
            match (budget.scope, target.is_empty()) {
                (BudgetScope::Global, false) => {
                    return Err(format!("budget {id:?} is global and takes no target"));
                }
                (
                    BudgetScope::Environment
                    | BudgetScope::Endpoint
                    | BudgetScope::Model
                    | BudgetScope::Source,
                    true,
                ) => {
                    return Err(format!("budget {id:?} needs a target for its scope"));
                }
                _ => {}
            }
            if budget.scope == BudgetScope::Source && !SOURCES.contains(&target) {
                return Err(format!(
                    "budget {id:?}: a source target is one of {}",
                    SOURCES.join(", ")
                ));
            }
            if budget.scope == BudgetScope::Model && !target.contains(':') {
                return Err(format!(
                    "budget {id:?}: a model target is a spec, <endpoint>:<model>"
                ));
            }
            let tokens = budget.tokens.filter(|tokens| *tokens > 0);
            let cost = budget
                .cost_usd
                .filter(|cost| cost.is_finite() && *cost > 0.0);
            if budget.tokens.is_some() && tokens.is_none() {
                return Err(format!("budget {id:?}: tokens must be above 0"));
            }
            if budget.cost_usd.is_some() && cost.is_none() {
                return Err(format!("budget {id:?}: cost_usd must be above 0"));
            }
            if tokens.is_none() && cost.is_none() {
                return Err(format!("budget {id:?} needs tokens, cost_usd, or both"));
            }
            if let Some(thresholds) = &budget.thresholds {
                check_thresholds(thresholds, &format!("budget {id:?} thresholds"))?;
            }
        }
        Ok(())
    }
}

fn check_thresholds(thresholds: &[u32], what: &str) -> Result<(), String> {
    if let Some(bad) = thresholds.iter().find(|t| !(1..=1000).contains(*t)) {
        return Err(format!("{what}: {bad} is not a percentage from 1 to 1000"));
    }
    Ok(())
}

/// Replaces the user file's `[budgets]` table (the defaults remove their keys). Returns the
/// budgets as loaded back.
pub fn save_budgets(
    paths: &Paths,
    budgets: &BudgetSettings,
    env: impl IntoIterator<Item = (String, String)>,
) -> Result<BudgetSettings, SetError> {
    budgets
        .validate()
        .map_err(|message| SetError::InvalidValue {
            key: "budgets".into(),
            message,
        })?;
    let defaults = BudgetSettings::default();
    let config = edit_user_file(paths, env, |document| {
        if document.get("budgets").is_none() {
            document.insert("budgets", Item::Table(Table::new()));
        }
        let table = document
            .get_mut("budgets")
            .and_then(Item::as_table_like_mut)
            .ok_or("budgets in the user config is not a table")?;
        if budgets.week_start == defaults.week_start {
            table.remove("week_start");
        } else {
            table.insert(
                "week_start",
                Item::Value(week_start(budgets.week_start).into()),
            );
        }
        if budgets.month_start_day == defaults.month_start_day {
            table.remove("month_start_day");
        } else {
            table.insert(
                "month_start_day",
                Item::Value(i64::from(budgets.month_start_day).into()),
            );
        }
        if budgets.thresholds == defaults.thresholds {
            table.remove("thresholds");
        } else {
            table.insert(
                "thresholds",
                Item::Value(Value::Array(numbers(&budgets.thresholds))),
            );
        }
        if budgets.limits.is_empty() {
            table.remove("limits");
        } else {
            let mut list: Array = budgets.limits.iter().map(budget_value).collect();
            for item in list.iter_mut() {
                item.decor_mut().set_prefix("\n  ");
            }
            list.set_trailing("\n");
            list.set_trailing_comma(true);
            table.insert("limits", Item::Value(Value::Array(list)));
        }
        if table.is_empty() {
            document.remove("budgets");
        }
        Ok(())
    })?;
    Ok(config.settings().map_err(SetError::Rejected)?.budgets)
}

fn numbers(values: &[u32]) -> Array {
    values.iter().map(|value| i64::from(*value)).collect()
}

fn week_start(day: WeekStart) -> &'static str {
    match day {
        WeekStart::Monday => "monday",
        WeekStart::Tuesday => "tuesday",
        WeekStart::Wednesday => "wednesday",
        WeekStart::Thursday => "thursday",
        WeekStart::Friday => "friday",
        WeekStart::Saturday => "saturday",
        WeekStart::Sunday => "sunday",
    }
}

fn budget_value(budget: &Budget) -> Value {
    let mut table = InlineTable::new();
    table.insert("id", budget.id.trim().into());
    if !budget.name.trim().is_empty() {
        table.insert("name", budget.name.trim().into());
    }
    let scope = match budget.scope {
        BudgetScope::Global => "global",
        BudgetScope::Environment => "environment",
        BudgetScope::Endpoint => "endpoint",
        BudgetScope::Model => "model",
        BudgetScope::Source => "source",
    };
    table.insert("scope", scope.into());
    if !budget.target.trim().is_empty() {
        table.insert("target", budget.target.trim().into());
    }
    let window = match budget.window {
        BudgetWindow::Daily => "daily",
        BudgetWindow::Weekly => "weekly",
        BudgetWindow::Monthly => "monthly",
    };
    table.insert("window", window.into());
    if let Some(tokens) = budget.tokens {
        table.insert("tokens", i64::try_from(tokens).unwrap_or(i64::MAX).into());
    }
    if let Some(cost) = budget.cost_usd {
        table.insert("cost_usd", cost.into());
    }
    if let Some(thresholds) = &budget.thresholds {
        table.insert("thresholds", Value::Array(numbers(thresholds)));
    }
    let action = match budget.action {
        BudgetAction::Warn => "warn",
        BudgetAction::Ask => "ask",
        BudgetAction::Block => "block",
    };
    table.insert("action", action.into());
    Value::InlineTable(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        (dir, paths)
    }

    #[test]
    fn budgets_round_trip_through_the_user_file_keeping_comments() {
        let (_dir, paths) = paths();
        std::fs::write(
            paths.user_config_file(),
            "# my settings\n[agents]\nmax_steps = 12\n",
        )
        .unwrap();
        let wanted = BudgetSettings {
            week_start: WeekStart::Sunday,
            month_start_day: 15,
            thresholds: vec![25, 90],
            limits: vec![
                Budget {
                    id: "all".into(),
                    window: BudgetWindow::Monthly,
                    cost_usd: Some(40.0),
                    action: BudgetAction::Ask,
                    ..Budget::default()
                },
                Budget {
                    id: "work".into(),
                    name: "Work week".into(),
                    scope: BudgetScope::Environment,
                    target: "env-1".into(),
                    window: BudgetWindow::Weekly,
                    tokens: Some(5_000_000),
                    thresholds: Some(vec![100]),
                    action: BudgetAction::Block,
                    ..Budget::default()
                },
            ],
        };
        let saved = save_budgets(&paths, &wanted, Vec::new()).unwrap();
        assert_eq!(saved, wanted);
        let text = std::fs::read_to_string(paths.user_config_file()).unwrap();
        assert!(text.contains("# my settings"), "{text}");
        assert!(text.contains(r#"week_start = "sunday""#), "{text}");
        assert!(text.contains(r#"action = "block""#), "{text}");

        let cleared = save_budgets(&paths, &BudgetSettings::default(), Vec::new()).unwrap();
        assert_eq!(cleared, BudgetSettings::default());
        let text = std::fs::read_to_string(paths.user_config_file()).unwrap();
        assert!(!text.contains("[budgets]"), "{text}");
    }

    #[test]
    fn invalid_budgets_are_refused() {
        let (_dir, paths) = paths();
        let bad = [
            BudgetSettings {
                month_start_day: 31,
                ..BudgetSettings::default()
            },
            BudgetSettings {
                limits: vec![Budget {
                    id: "x".into(),
                    ..Budget::default()
                }],
                ..BudgetSettings::default()
            },
            BudgetSettings {
                limits: vec![Budget {
                    id: "x".into(),
                    scope: BudgetScope::Environment,
                    tokens: Some(1),
                    ..Budget::default()
                }],
                ..BudgetSettings::default()
            },
            BudgetSettings {
                limits: vec![Budget {
                    id: "x".into(),
                    scope: BudgetScope::Model,
                    target: "no-endpoint".into(),
                    tokens: Some(1),
                    ..Budget::default()
                }],
                ..BudgetSettings::default()
            },
        ];
        for settings in bad {
            assert!(save_budgets(&paths, &settings, Vec::new()).is_err());
        }
        assert!(!paths.user_config_file().exists());
    }
}
