//! Budgets and alerts: the `budgets.*` methods and notifications (see
//! docs/10-configuration.md#budgets). Budgets count tokens and, where prices are known, cost,
//! over daily, weekly, or monthly windows in local time, globally, per environment, per
//! endpoint, or per model.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::methods::Method;

/// What a budget counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum BudgetScope {
    /// Every model call.
    #[default]
    Global,
    /// Calls from sessions in one environment (`target` is its id).
    Environment,
    /// Calls to one endpoint (`target` is its name).
    Endpoint,
    /// Calls to one model (`target` is its spec, `<endpoint>:<model>`).
    Model,
}

/// The period a budget resets over, in local time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum BudgetWindow {
    Daily,
    Weekly,
    #[default]
    Monthly,
}

/// What happens to new turns in the scope once a budget is used up; running turns finish.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum BudgetAction {
    /// Alert only.
    #[default]
    Warn,
    /// `session.send` fails with `-32008` until it is repeated with `confirm_budget`.
    Ask,
    /// `session.send` fails with `-32008` until the window resets.
    Block,
}

/// The day weekly budgets start on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Budget {
    /// Made by the daemon when empty.
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub scope: BudgetScope,
    /// The environment id, endpoint name, or model spec; empty for a global budget.
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub window: BudgetWindow,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub tokens: Option<u64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// Percentages that alert; the defaults when absent.
    #[serde(default)]
    pub thresholds: Option<Vec<u32>>,
    #[serde(default)]
    pub action: BudgetAction,
}

/// Every budget and when windows start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BudgetSettings {
    pub week_start: WeekStart,
    /// The day of the month monthly budgets reset on, 1 to 28.
    pub month_start_day: u32,
    /// Percentages that alert for budgets without their own (default 50, 80, 100).
    pub thresholds: Vec<u32>,
    pub limits: Vec<Budget>,
}

/// Where a budget stands in its current window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BudgetStatus {
    pub budget: Budget,
    /// A readable name: the budget's own, or its scope and window ("Work, weekly").
    pub label: String,
    /// The window's first day and the day after its last (`YYYY-MM-DD`, local time).
    pub window_start: String,
    pub window_end: String,
    #[ts(type = "number")]
    pub tokens_used: u64,
    pub cost_usd_used: f64,
    /// The larger of tokens and cost used over their limits (1.0 is the whole budget).
    pub share: f64,
    /// Thresholds already alerted in this window.
    pub alerted: Vec<u32>,
    /// Used up (share at least 1).
    pub exceeded: bool,
}

/// `budgets.get`: the configured budgets and where each stands.
#[derive(Debug)]
pub struct BudgetsGet;

impl Method for BudgetsGet {
    const NAME: &'static str = "budgets.get";
    type Params = BudgetsGetParams;
    type Result = BudgetsResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BudgetsGetParams {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BudgetsResult {
    pub settings: BudgetSettings,
    pub status: Vec<BudgetStatus>,
}

/// `budgets.set`: replaces the budgets in the user config (`[budgets]`); budgets without an id
/// get one. Applies at once.
#[derive(Debug)]
pub struct BudgetsSet;

impl Method for BudgetsSet {
    const NAME: &'static str = "budgets.set";
    type Params = BudgetSettings;
    type Result = BudgetsResult;
}

/// `budgets.usage`: recorded usage by day for the dashboard, with the top environments,
/// endpoints, and models over the same days.
#[derive(Debug)]
pub struct BudgetsUsage;

impl Method for BudgetsUsage {
    const NAME: &'static str = "budgets.usage";
    type Params = BudgetsUsageParams;
    type Result = UsageReport;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BudgetsUsageParams {
    /// How many days back, today included (default 30, at most 400).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub days: Option<u32>,
    /// Only usage in this budget's scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub budget_id: Option<String>,
}

/// Tokens and cost in one day or for one key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UsageAmount {
    /// A day (`YYYY-MM-DD`), environment id, endpoint name, or model spec.
    pub key: String,
    /// The environment's name, for environments; else the key.
    pub label: String,
    #[ts(type = "number")]
    pub tokens: u64,
    pub cost_usd: f64,
    #[ts(type = "number")]
    pub calls: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UsageReport {
    /// Every day in the range, oldest first, including days without usage.
    pub days: Vec<UsageAmount>,
    /// Largest first (by cost, then tokens).
    pub environments: Vec<UsageAmount>,
    pub endpoints: Vec<UsageAmount>,
    pub models: Vec<UsageAmount>,
}

/// Notification: a budget crossed an alert threshold (once per threshold per window).
pub const BUDGETS_ALERT: &str = "budgets.alert";

/// Notification: usage or budgets changed; params are [`BudgetsResult`].
pub const BUDGETS_CHANGED: &str = "budgets.changed";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BudgetAlert {
    pub budget_id: String,
    pub label: String,
    /// The threshold crossed, in percent.
    pub threshold: u32,
    pub share: f64,
    pub action: BudgetAction,
    /// When the window resets (`YYYY-MM-DD`, local time).
    pub window_end: String,
    /// A sentence for the user.
    pub message: String,
    /// The session whose model call crossed it.
    pub session_id: Option<String>,
}

/// The `data` of a `-32008` error: the budget that stops a new turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BudgetStop {
    pub budget_id: String,
    pub label: String,
    /// `ask` (repeat with `confirm_budget` to go ahead) or `block`.
    pub action: BudgetAction,
    pub window_end: String,
    pub message: String,
}
