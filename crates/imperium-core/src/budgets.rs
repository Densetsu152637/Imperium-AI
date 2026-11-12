//! Budgets and alerts. Every model call a session makes is recorded by local day, environment,
//! and model spec in `<data>/usage.json`, so windows survive restarts. Budgets from the user
//! config (`[budgets]`) count those records over daily, weekly, or monthly windows in local
//! time, alert once per threshold per window (`budgets.alert`), and, once used up, can make new
//! turns in their scope ask first or stop ([`DaemonState::budget_gate`]). Turns already running
//! finish.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{Datelike, Days, Local, Months, NaiveDate, Weekday};
use imperium_config::{
    Budget, BudgetAction, BudgetScope, BudgetSettings, BudgetWindow, DEFAULT_THRESHOLDS, WeekStart,
};
use imperium_protocol::budgets as wire;
use imperium_protocol::jsonrpc::{ErrorObject, codes};
use imperium_protocol::session::{AgentEvent, SessionEvent};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::state::DaemonState;

/// The source of calls that come through the OpenAI-compatible API server.
pub const SOURCE_API: &str = "api";

/// How many days of usage are kept.
const KEEP_DAYS: u64 = 400;
const DATE: &str = "%Y-%m-%d";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One model call, as a budget counts it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CallUsage {
    pub environment_id: Option<String>,
    /// The entry point the call came through when it was not a session's (`api`).
    pub source: Option<String>,
    /// `<endpoint>:<model>`.
    pub spec: String,
    /// Prompt plus completion tokens.
    pub tokens: u64,
    /// `None` when the model has no prices.
    pub cost_usd: Option<f64>,
}

/// Usage in one day for one environment and model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Row {
    /// Empty for sessions outside an environment.
    #[serde(default)]
    environment: String,
    /// Empty for a session's calls.
    #[serde(default)]
    source: String,
    spec: String,
    tokens: u64,
    cost_usd: f64,
    calls: u64,
}

/// The thresholds a budget has alerted in its current window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Alerted {
    window_start: String,
    thresholds: Vec<u32>,
}

/// What `usage.json` holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Ledger {
    /// Rows by local day (`YYYY-MM-DD`).
    #[serde(default)]
    days: BTreeMap<String, Vec<Row>>,
    /// By budget id.
    #[serde(default)]
    alerted: BTreeMap<String, Alerted>,
}

/// An alert to send: a budget crossed `threshold`.
#[derive(Debug, Clone, PartialEq)]
pub struct Fired {
    pub budget: Budget,
    pub threshold: u32,
    pub share: f64,
    pub window_end: NaiveDate,
}

/// A used-up budget that stops or questions a new turn.
#[derive(Debug, Clone, PartialEq)]
pub struct Stop {
    pub budget: Budget,
    pub window_end: NaiveDate,
}

/// Where a budget stands.
#[derive(Debug, Clone, PartialEq)]
pub struct Standing {
    pub budget: Budget,
    pub window_start: NaiveDate,
    pub window_end: NaiveDate,
    pub tokens: u64,
    pub cost_usd: f64,
    pub share: f64,
    pub alerted: Vec<u32>,
}

/// The budgets, the usage ledger, and where the ledger is saved.
#[derive(Debug)]
pub struct BudgetBook {
    path: Option<PathBuf>,
    inner: Mutex<(BudgetSettings, Ledger)>,
    /// Calls that were admitted but have not been recorded yet, with what they are expected to
    /// use, so calls that arrive together cannot all pass a budget that has room for one.
    reserved: Mutex<Vec<Reserved>>,
    next_reservation: std::sync::atomic::AtomicU64,
}

/// What an admitted call is expected to use until it is recorded.
#[derive(Debug)]
struct Reserved {
    id: u64,
    environment: Option<String>,
    spec: String,
    source: Option<String>,
    tokens: u64,
    cost_usd: f64,
}

impl Reserved {
    fn origin(&self) -> Origin<'_> {
        Origin {
            environment: self.environment.as_deref(),
            spec: Some(&self.spec),
            source: self.source.as_deref(),
        }
    }
}

impl BudgetBook {
    /// Reads the ledger at `path` (an unreadable one starts empty, with a warning).
    pub fn load(path: PathBuf, settings: BudgetSettings) -> Self {
        let ledger = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|error| {
                tracing::warn!(path = %path.display(), %error, "the usage ledger is unreadable; starting a new one");
                Ledger::default()
            }),
            Err(_) => Ledger::default(),
        };
        BudgetBook {
            path: Some(path),
            inner: Mutex::new((settings, ledger)),
            reserved: Mutex::new(Vec::new()),
            next_reservation: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Admits a call of `origin` expected to use `tokens` and `cost_usd`, unless a used-up
    /// budget that asks or blocks covers it. The check counts the calls admitted before and not
    /// yet recorded, and the call is entered among them atomically, so calls that arrive
    /// together cannot all pass a budget with room for one. Drop the reservation with
    /// [`BudgetBook::release`] once the call is recorded or abandoned.
    pub fn reserve(
        &self,
        today: NaiveDate,
        origin: Origin<'_>,
        tokens: u64,
        cost_usd: f64,
    ) -> Result<u64, Stop> {
        let mut reserved = lock(&self.reserved);
        let mut stop: Option<Stop> = None;
        for standing in self.standings(today) {
            let budget = &standing.budget;
            if budget.action == BudgetAction::Warn || !applies(budget, origin) {
                continue;
            }
            let (held_tokens, held_cost) = reserved
                .iter()
                .filter(|held| applies(budget, held.origin()))
                .fold((0u64, 0.0), |(t, c), held| {
                    (t.saturating_add(held.tokens), c + held.cost_usd)
                });
            let share = share(
                budget,
                standing.tokens.saturating_add(held_tokens),
                standing.cost_usd + held_cost,
            );
            if share < 1.0 {
                continue;
            }
            let stronger = match &stop {
                None => true,
                Some(current) => {
                    current.budget.action == BudgetAction::Ask
                        && budget.action == BudgetAction::Block
                }
            };
            if stronger {
                stop = Some(Stop {
                    budget: budget.clone(),
                    window_end: standing.window_end,
                });
            }
        }
        if let Some(stop) = stop {
            return Err(stop);
        }
        let id = self
            .next_reservation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        reserved.push(Reserved {
            id,
            environment: origin.environment.map(str::to_string),
            spec: origin.spec.unwrap_or_default().to_string(),
            source: origin.source.map(str::to_string),
            tokens,
            cost_usd,
        });
        Ok(id)
    }

    /// Forgets a reservation made by [`BudgetBook::reserve`].
    pub fn release(&self, id: u64) {
        lock(&self.reserved).retain(|held| held.id != id);
    }

    pub fn settings(&self) -> BudgetSettings {
        lock(&self.inner).0.clone()
    }

    pub fn set_settings(&self, settings: BudgetSettings) {
        let mut inner = lock(&self.inner);
        // Forget alerts of budgets that are gone.
        let ids: Vec<&str> = settings.limits.iter().map(|b| b.id.as_str()).collect();
        inner.1.alerted.retain(|id, _| ids.contains(&id.as_str()));
        inner.0 = settings;
        self.save(&inner.1);
    }

    /// Adds a call on `today` and returns the alerts it sets off.
    pub fn record(&self, today: NaiveDate, call: &CallUsage) -> Vec<Fired> {
        let mut inner = lock(&self.inner);
        let (settings, ledger) = &mut *inner;
        let environment = call.environment_id.clone().unwrap_or_default();
        let source = call.source.clone().unwrap_or_default();
        let rows = ledger.days.entry(day(today)).or_default();
        let index = match rows.iter().position(|row| {
            row.environment == environment && row.source == source && row.spec == call.spec
        }) {
            Some(index) => index,
            None => {
                rows.push(Row {
                    environment,
                    source,
                    spec: call.spec.clone(),
                    ..Row::default()
                });
                rows.len() - 1
            }
        };
        let row = &mut rows[index];
        row.tokens = row.tokens.saturating_add(call.tokens);
        row.cost_usd += call.cost_usd.unwrap_or(0.0).max(0.0);
        row.calls += 1;
        if let Some(oldest) = today.checked_sub_days(Days::new(KEEP_DAYS)) {
            let oldest = day(oldest);
            ledger.days.retain(|date, _| *date >= oldest);
        }

        let mut fired = Vec::new();
        for budget in &settings.limits {
            let origin = Origin {
                environment: call.environment_id.as_deref(),
                spec: Some(&call.spec),
                source: call.source.as_deref(),
            };
            if !applies(budget, origin) {
                continue;
            }
            let (start, end) = window(budget.window, today, settings);
            let (tokens, cost) = used(ledger, budget, start, end);
            let share = share(budget, tokens, cost);
            let alerted = ledger.alerted.entry(budget.id.clone()).or_default();
            if alerted.window_start != day(start) {
                *alerted = Alerted {
                    window_start: day(start),
                    thresholds: Vec::new(),
                };
            }
            let crossed: Vec<u32> = thresholds(budget, settings)
                .into_iter()
                .filter(|t| share * 100.0 >= f64::from(*t) && !alerted.thresholds.contains(t))
                .collect();
            // One alert per call, for the highest threshold crossed; the others count as sent.
            if let Some(highest) = crossed.iter().max().copied() {
                alerted.thresholds.extend(crossed);
                alerted.thresholds.sort_unstable();
                fired.push(Fired {
                    budget: budget.clone(),
                    threshold: highest,
                    share,
                    window_end: end,
                });
            }
        }
        self.save(ledger);
        fired
    }

    /// Every budget's standing on `today`.
    pub fn standings(&self, today: NaiveDate) -> Vec<Standing> {
        let inner = lock(&self.inner);
        let (settings, ledger) = &*inner;
        settings
            .limits
            .iter()
            .map(|budget| {
                let (start, end) = window(budget.window, today, settings);
                let (tokens, cost) = used(ledger, budget, start, end);
                let alerted = ledger
                    .alerted
                    .get(&budget.id)
                    .filter(|alerted| alerted.window_start == day(start))
                    .map(|alerted| alerted.thresholds.clone())
                    .unwrap_or_default();
                Standing {
                    budget: budget.clone(),
                    window_start: start,
                    window_end: end,
                    tokens,
                    cost_usd: cost,
                    share: share(budget, tokens, cost),
                    alerted,
                }
            })
            .collect()
    }

    /// The used-up budget, among those over a call from `origin`, that has the strongest action
    /// (block over ask); `None` when new turns may start.
    pub fn gate(&self, today: NaiveDate, origin: Origin<'_>) -> Option<Stop> {
        let mut stop: Option<Stop> = None;
        for standing in self.standings(today) {
            let budget = &standing.budget;
            if budget.action == BudgetAction::Warn
                || standing.share < 1.0
                || !applies(budget, origin)
            {
                continue;
            }
            let stronger = match &stop {
                None => true,
                Some(current) => {
                    current.budget.action == BudgetAction::Ask
                        && budget.action == BudgetAction::Block
                }
            };
            if stronger {
                stop = Some(Stop {
                    budget: budget.clone(),
                    window_end: standing.window_end,
                });
            }
        }
        stop
    }

    /// Usage over the `days` days up to `today`, limited to `budget`'s scope when given:
    /// `(by day, by environment, by endpoint, by model)` as `(key, tokens, cost, calls)`.
    #[allow(clippy::type_complexity)]
    pub fn usage(
        &self,
        today: NaiveDate,
        days: u32,
        budget: Option<&Budget>,
    ) -> (Vec<Total>, Vec<Total>, Vec<Total>, Vec<Total>) {
        let inner = lock(&self.inner);
        let ledger = &inner.1;
        let first = today
            .checked_sub_days(Days::new(u64::from(days.max(1) - 1)))
            .unwrap_or(today);
        let mut by_day = Vec::new();
        let mut environments: HashMap<String, Total> = HashMap::new();
        let mut endpoints: HashMap<String, Total> = HashMap::new();
        let mut models: HashMap<String, Total> = HashMap::new();
        let mut date = first;
        while date <= today {
            let mut total = Total::new(day(date));
            for row in ledger.days.get(&day(date)).into_iter().flatten() {
                if let Some(budget) = budget
                    && !applies(budget, row.origin())
                {
                    continue;
                }
                total.add(row);
                if !row.environment.is_empty() {
                    environments
                        .entry(row.environment.clone())
                        .or_insert_with(|| Total::new(row.environment.clone()))
                        .add(row);
                }
                let endpoint = endpoint(&row.spec).to_string();
                endpoints
                    .entry(endpoint.clone())
                    .or_insert_with(|| Total::new(endpoint))
                    .add(row);
                models
                    .entry(row.spec.clone())
                    .or_insert_with(|| Total::new(row.spec.clone()))
                    .add(row);
            }
            by_day.push(total);
            date = date.succ_opt().unwrap_or(today + Days::new(1));
        }
        (
            by_day,
            ranked(environments),
            ranked(endpoints),
            ranked(models),
        )
    }

    fn save(&self, ledger: &Ledger) {
        let Some(path) = &self.path else { return };
        let text = match serde_json::to_string(ledger) {
            Ok(text) => text,
            Err(error) => {
                tracing::warn!(%error, "could not serialise the usage ledger");
                return;
            }
        };
        let temporary = path.with_extension("json.tmp");
        let written =
            std::fs::write(&temporary, text).and_then(|()| std::fs::rename(&temporary, path));
        if let Err(error) = written {
            tracing::warn!(path = %path.display(), %error, "could not save the usage ledger");
        }
    }
}

/// Tokens, cost, and calls for one key.
#[derive(Debug, Clone, PartialEq)]
pub struct Total {
    pub key: String,
    pub tokens: u64,
    pub cost_usd: f64,
    pub calls: u64,
}

impl Total {
    fn new(key: String) -> Self {
        Total {
            key,
            tokens: 0,
            cost_usd: 0.0,
            calls: 0,
        }
    }

    fn add(&mut self, row: &Row) {
        self.tokens = self.tokens.saturating_add(row.tokens);
        self.cost_usd += row.cost_usd;
        self.calls += row.calls;
    }
}

fn ranked(totals: HashMap<String, Total>) -> Vec<Total> {
    let mut totals: Vec<Total> = totals.into_values().collect();
    totals.sort_by(|a, b| {
        b.cost_usd
            .total_cmp(&a.cost_usd)
            .then(b.tokens.cmp(&a.tokens))
            .then(a.key.cmp(&b.key))
    });
    totals
}

fn day(date: NaiveDate) -> String {
    date.format(DATE).to_string()
}

fn endpoint(spec: &str) -> &str {
    spec.split_once(':').map_or(spec, |(endpoint, _)| endpoint)
}

/// Where a model call, or a session about to make them, comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Origin<'a> {
    /// The environment of the session.
    pub environment: Option<&'a str>,
    /// The model it calls, `<endpoint>:<model>`.
    pub spec: Option<&'a str>,
    /// The entry point, for calls that are not a session's (`api`).
    pub source: Option<&'a str>,
}

impl Row {
    fn origin(&self) -> Origin<'_> {
        Origin {
            environment: Some(self.environment.as_str()).filter(|e| !e.is_empty()),
            spec: Some(&self.spec),
            source: Some(self.source.as_str()).filter(|s| !s.is_empty()),
        }
    }
}

/// Whether a budget counts a call (or covers a session) from `origin`.
fn applies(budget: &Budget, origin: Origin<'_>) -> bool {
    let target = budget.target.trim();
    match budget.scope {
        BudgetScope::Global => true,
        BudgetScope::Environment => origin.environment == Some(target),
        BudgetScope::Endpoint => origin.spec.is_some_and(|spec| endpoint(spec) == target),
        BudgetScope::Model => origin.spec == Some(target),
        BudgetScope::Source => origin.source == Some(target),
    }
}

fn thresholds(budget: &Budget, settings: &BudgetSettings) -> Vec<u32> {
    let mut list = budget
        .thresholds
        .clone()
        .unwrap_or_else(|| settings.thresholds.clone());
    if list.is_empty() {
        list = DEFAULT_THRESHOLDS.to_vec();
    }
    list.sort_unstable();
    list.dedup();
    list
}

/// Tokens and cost a budget counts in `[start, end)`.
fn used(ledger: &Ledger, budget: &Budget, start: NaiveDate, end: NaiveDate) -> (u64, f64) {
    let (start, end) = (day(start), day(end));
    let mut tokens = 0u64;
    let mut cost = 0.0;
    for (_, rows) in ledger.days.range(start..end) {
        for row in rows {
            if applies(budget, row.origin()) {
                tokens = tokens.saturating_add(row.tokens);
                cost += row.cost_usd;
            }
        }
    }
    (tokens, cost)
}

/// How far along a budget is: the larger of its token and cost shares.
fn share(budget: &Budget, tokens: u64, cost: f64) -> f64 {
    let by_tokens = budget
        .tokens
        .filter(|limit| *limit > 0)
        .map_or(0.0, |limit| tokens as f64 / limit as f64);
    let by_cost = budget
        .cost_usd
        .filter(|limit| *limit > 0.0)
        .map_or(0.0, |limit| cost / limit);
    by_tokens.max(by_cost)
}

fn weekday(start: WeekStart) -> Weekday {
    match start {
        WeekStart::Monday => Weekday::Mon,
        WeekStart::Tuesday => Weekday::Tue,
        WeekStart::Wednesday => Weekday::Wed,
        WeekStart::Thursday => Weekday::Thu,
        WeekStart::Friday => Weekday::Fri,
        WeekStart::Saturday => Weekday::Sat,
        WeekStart::Sunday => Weekday::Sun,
    }
}

/// The window holding `today`: its first day and the day after its last.
pub fn window(
    kind: BudgetWindow,
    today: NaiveDate,
    settings: &BudgetSettings,
) -> (NaiveDate, NaiveDate) {
    match kind {
        BudgetWindow::Daily => (today, today + Days::new(1)),
        BudgetWindow::Weekly => {
            let back = (today.weekday().num_days_from_monday() + 7
                - weekday(settings.week_start).num_days_from_monday())
                % 7;
            let start = today - Days::new(u64::from(back));
            (start, start + Days::new(7))
        }
        BudgetWindow::Monthly => {
            let reset = settings.month_start_day.clamp(1, 28);
            let this_month = today.with_day(reset).unwrap_or(today);
            let start = if today.day() >= reset {
                this_month
            } else {
                this_month - Months::new(1)
            };
            (start, start + Months::new(1))
        }
    }
}

// ---- the daemon side ----

/// Records a session's model calls and sends the alerts they set off.
pub fn watch_session(
    state: &Arc<DaemonState>,
    session_id: String,
    environment_id: Option<String>,
    mut events: broadcast::Receiver<AgentEvent>,
) {
    let daemon = Arc::downgrade(state);
    tokio::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            };
            let SessionEvent::ModelCallCompleted {
                model,
                usage,
                cost_usd,
                ..
            } = event.event
            else {
                continue;
            };
            let Some(state) = daemon.upgrade() else {
                return;
            };
            let call = CallUsage {
                environment_id: environment_id.clone(),
                source: None,
                spec: model,
                tokens: usage.prompt_tokens.saturating_add(usage.completion_tokens),
                cost_usd,
            };
            record(&state, &call, Some(&session_id));
        }
    });
}

/// Records a call today, sends its alerts, and tells clients the new standings.
pub fn record(state: &DaemonState, call: &CallUsage, session_id: Option<&str>) {
    let today = Local::now().date_naive();
    let fired = state.budgets().record(today, call);
    for alert in fired {
        let label = label(state, &alert.budget);
        let message = alert_message(&label, &alert);
        tracing::info!(budget = %alert.budget.id, threshold = alert.threshold, "budget alert");
        state.announce(
            wire::BUDGETS_ALERT,
            wire::BudgetAlert {
                budget_id: alert.budget.id.clone(),
                label,
                threshold: alert.threshold,
                share: alert.share,
                action: action_to_wire(alert.budget.action),
                window_end: day(alert.window_end),
                message,
                session_id: session_id.map(str::to_string),
            },
        );
    }
    if !state.budgets().settings().limits.is_empty() {
        state.announce(wire::BUDGETS_CHANGED, result(state));
    }
}

fn alert_message(label: &str, alert: &Fired) -> String {
    let percent = (alert.share * 100.0).round();
    let reset = day(alert.window_end);
    if alert.share >= 1.0 {
        let then = match alert.budget.action {
            BudgetAction::Warn => "",
            BudgetAction::Ask => " New turns ask before they start.",
            BudgetAction::Block => " New turns are blocked.",
        };
        format!("{label} is used up ({percent}%) until {reset}.{then}")
    } else {
        format!(
            "{label} is at {percent}% (alert at {}%); it resets on {reset}.",
            alert.threshold
        )
    }
}

/// A readable name for a budget: its own, else its scope and window.
fn label(state: &DaemonState, budget: &Budget) -> String {
    if !budget.name.trim().is_empty() {
        return budget.name.trim().to_string();
    }
    let window = match budget.window {
        BudgetWindow::Daily => "daily",
        BudgetWindow::Weekly => "weekly",
        BudgetWindow::Monthly => "monthly",
    };
    let scope = match budget.scope {
        BudgetScope::Global => "All usage".to_string(),
        BudgetScope::Environment => state
            .environments()
            .find(&budget.target)
            .map_or_else(|_| budget.target.clone(), |env| env.name),
        BudgetScope::Endpoint | BudgetScope::Model => budget.target.clone(),
        BudgetScope::Source if budget.target == SOURCE_API => "API calls".to_string(),
        BudgetScope::Source => budget.target.clone(),
    };
    format!("{scope}, {window}")
}

fn environment_label(state: &DaemonState, id: &str) -> String {
    state
        .environments()
        .find(id)
        .map_or_else(|_| id.to_string(), |env| env.name)
}

/// The budgets and their standings, for `budgets.get` and `budgets.changed`.
pub fn result(state: &DaemonState) -> wire::BudgetsResult {
    let book = state.budgets();
    let today = Local::now().date_naive();
    wire::BudgetsResult {
        settings: settings_to_wire(&book.settings()),
        status: book
            .standings(today)
            .into_iter()
            .map(|standing| wire::BudgetStatus {
                label: label(state, &standing.budget),
                window_start: day(standing.window_start),
                window_end: day(standing.window_end),
                tokens_used: standing.tokens,
                cost_usd_used: standing.cost_usd,
                share: standing.share,
                alerted: standing.alerted,
                exceeded: standing.share >= 1.0,
                budget: budget_to_wire(&standing.budget),
            })
            .collect(),
    }
}

fn invalid(message: impl Into<String>) -> ErrorObject {
    ErrorObject::new(codes::INVALID_PARAMS, message.into())
}

/// `budgets.get`.
pub fn get(state: &DaemonState) -> Result<wire::BudgetsResult, ErrorObject> {
    Ok(result(state))
}

/// `budgets.set`: gives new budgets ids, saves them, and applies them at once.
pub fn set(
    state: &DaemonState,
    params: wire::BudgetSettings,
) -> Result<wire::BudgetsResult, ErrorObject> {
    let mut settings = settings_from_wire(&params);
    for index in 0..settings.limits.len() {
        if settings.limits[index].id.trim().is_empty() {
            settings.limits[index].id = new_id(&settings.limits);
        }
    }
    let saved = imperium_config::save_budgets(state.paths(), &settings, std::env::vars())
        .map_err(|error| invalid(error.to_string()))?;
    state.budgets().set_settings(saved.clone());
    tracing::info!(budgets = saved.limits.len(), "a client changed the budgets");
    let result = result(state);
    state.announce(wire::BUDGETS_CHANGED, result.clone());
    Ok(result)
}

fn new_id(limits: &[Budget]) -> String {
    (1..)
        .map(|n| format!("budget-{n}"))
        .find(|id| !limits.iter().any(|budget| budget.id == *id))
        .unwrap_or_default()
}

/// `budgets.usage`.
pub fn usage(
    state: &DaemonState,
    params: wire::BudgetsUsageParams,
) -> Result<wire::UsageReport, ErrorObject> {
    let days = params.days.unwrap_or(30).clamp(1, 400);
    let book = state.budgets();
    let budget = match &params.budget_id {
        Some(id) => Some(
            book.settings()
                .limits
                .into_iter()
                .find(|budget| budget.id == *id)
                .ok_or_else(|| invalid(format!("no budget {id:?}")))?,
        ),
        None => None,
    };
    let (by_day, environments, endpoints, models) =
        book.usage(Local::now().date_naive(), days, budget.as_ref());
    let amount = |total: Total, label: String| wire::UsageAmount {
        label,
        key: total.key,
        tokens: total.tokens,
        cost_usd: total.cost_usd,
        calls: total.calls,
    };
    Ok(wire::UsageReport {
        days: by_day
            .into_iter()
            .map(|t| {
                let key = t.key.clone();
                amount(t, key)
            })
            .collect(),
        environments: environments
            .into_iter()
            .map(|t| {
                let name = environment_label(state, &t.key);
                amount(t, name)
            })
            .collect(),
        endpoints: endpoints
            .into_iter()
            .map(|t| {
                let key = t.key.clone();
                amount(t, key)
            })
            .collect(),
        models: models
            .into_iter()
            .map(|t| {
                let key = t.key.clone();
                amount(t, key)
            })
            .collect(),
    })
}

/// The `-32008` error for a turn a used-up budget stops, unless it asks and was confirmed.
pub fn check(state: &DaemonState, session_id: &str, confirmed: bool) -> Result<(), ErrorObject> {
    let Some(session) = state.session(session_id) else {
        return Ok(());
    };
    let summary = state.summary(&session);
    let spec = summary
        .model
        .as_deref()
        .map(|model| resolve_spec(state, model));
    let today = Local::now().date_naive();
    let Some(stop) = state.budgets().gate(
        today,
        Origin {
            environment: summary.environment_id.as_deref(),
            spec: spec.as_deref(),
            source: None,
        },
    ) else {
        return Ok(());
    };
    if stop.budget.action == BudgetAction::Ask && confirmed {
        return Ok(());
    }
    Err(stop_error(state, &stop))
}

/// An admitted API call's claim on the budgets: what it is expected to use counts against them,
/// for calls that arrive meanwhile, until it is dropped (when the call has been recorded or
/// abandoned).
#[derive(Debug)]
pub(crate) struct ApiReservation {
    state: Arc<DaemonState>,
    id: u64,
}

impl Drop for ApiReservation {
    fn drop(&mut self) {
        self.state.budgets().release(self.id);
    }
}

/// Admits an API call to `spec` expected to use `tokens` and `cost_usd`, or says why a used-up
/// budget refuses it. An API call cannot be asked, so a budget that asks refuses it as one that
/// blocks does.
pub(crate) fn reserve_api_call(
    state: &Arc<DaemonState>,
    spec: &str,
    tokens: u64,
    cost_usd: f64,
) -> Result<ApiReservation, String> {
    let today = Local::now().date_naive();
    let origin = Origin {
        environment: None,
        spec: Some(spec),
        source: Some(SOURCE_API),
    };
    match state.budgets().reserve(today, origin, tokens, cost_usd) {
        Ok(id) => Ok(ApiReservation {
            state: Arc::clone(state),
            id,
        }),
        Err(stop) => {
            let label = label(state, &stop.budget);
            let reset = day(stop.window_end);
            Err(match stop.budget.action {
                BudgetAction::Block => {
                    format!("{label} is used up until {reset}; calls through the API are blocked.")
                }
                _ => format!(
                    "{label} is used up until {reset}; it asks before new work starts, and a call \
                     through the API cannot be asked, so it is refused."
                ),
            })
        }
    }
}

/// A model as a spec: catalog ids are looked up; specs pass through.
fn resolve_spec(state: &DaemonState, model: &str) -> String {
    if model.contains(':') {
        return model.to_string();
    }
    state
        .settings()
        .models
        .catalog
        .iter()
        .find(|entry| entry.id == model)
        .map_or_else(|| model.to_string(), |entry| entry.spec.clone())
}

fn stop_error(state: &DaemonState, stop: &Stop) -> ErrorObject {
    let label = label(state, &stop.budget);
    let reset = day(stop.window_end);
    let message = match stop.budget.action {
        BudgetAction::Block => {
            format!("{label} is used up until {reset}; new turns are blocked.")
        }
        _ => format!("{label} is used up until {reset}. Start the turn anyway?"),
    };
    let data = wire::BudgetStop {
        budget_id: stop.budget.id.clone(),
        label,
        action: action_to_wire(stop.budget.action),
        window_end: reset,
        message: message.clone(),
    };
    let mut error = ErrorObject::new(codes::BUDGET_EXCEEDED, message);
    error.data = serde_json::to_value(data).ok();
    error
}

// ---- conversions between the wire and the config's shapes (the same fields) ----

fn action_to_wire(action: BudgetAction) -> wire::BudgetAction {
    match action {
        BudgetAction::Warn => wire::BudgetAction::Warn,
        BudgetAction::Ask => wire::BudgetAction::Ask,
        BudgetAction::Block => wire::BudgetAction::Block,
    }
}

fn convert<T: Serialize, U: serde::de::DeserializeOwned>(value: &T) -> U {
    serde_json::to_value(value)
        .and_then(serde_json::from_value)
        .expect("the wire and config budget shapes have the same fields")
}

fn budget_to_wire(budget: &Budget) -> wire::Budget {
    convert(budget)
}

fn settings_to_wire(settings: &BudgetSettings) -> wire::BudgetSettings {
    convert(settings)
}

fn settings_from_wire(settings: &wire::BudgetSettings) -> BudgetSettings {
    let mut settings: BudgetSettings = convert(settings);
    for budget in &mut settings.limits {
        budget.id = budget.id.trim().to_string();
        budget.name = budget.name.trim().to_string();
        budget.target = budget.target.trim().to_string();
    }
    settings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, DATE).unwrap()
    }

    #[test]
    fn windows_follow_the_week_start_and_month_reset_day() {
        let mut settings = BudgetSettings::default();
        // 2026-11-11 is a Wednesday.
        let today = date("2026-11-11");
        assert_eq!(
            window(BudgetWindow::Daily, today, &settings),
            (today, date("2026-11-12"))
        );
        assert_eq!(
            window(BudgetWindow::Weekly, today, &settings),
            (date("2026-11-09"), date("2026-11-16"))
        );
        settings.week_start = WeekStart::Wednesday;
        assert_eq!(
            window(BudgetWindow::Weekly, today, &settings),
            (today, date("2026-11-18"))
        );
        settings.week_start = WeekStart::Sunday;
        assert_eq!(
            window(BudgetWindow::Weekly, today, &settings).0,
            date("2026-11-08")
        );
        assert_eq!(
            window(BudgetWindow::Monthly, today, &settings),
            (date("2026-11-01"), date("2026-12-01"))
        );
        settings.month_start_day = 15;
        assert_eq!(
            window(BudgetWindow::Monthly, today, &settings),
            (date("2026-10-15"), date("2026-11-15"))
        );
        assert_eq!(
            window(BudgetWindow::Monthly, date("2026-11-15"), &settings),
            (date("2026-11-15"), date("2026-12-15"))
        );
        // Across a year.
        assert_eq!(
            window(BudgetWindow::Monthly, date("2027-01-03"), &settings),
            (date("2026-12-15"), date("2027-01-15"))
        );
    }

    fn call(environment: Option<&str>, spec: &str, tokens: u64, cost: Option<f64>) -> CallUsage {
        CallUsage {
            environment_id: environment.map(str::to_string),
            source: None,
            spec: spec.into(),
            tokens,
            cost_usd: cost,
        }
    }

    fn origin<'a>(environment: Option<&'a str>, spec: Option<&'a str>) -> Origin<'a> {
        Origin {
            environment,
            spec,
            source: None,
        }
    }

    fn settings(limits: Vec<Budget>) -> BudgetSettings {
        BudgetSettings {
            limits,
            ..BudgetSettings::default()
        }
    }

    fn weekly_env(tokens: u64, action: BudgetAction) -> Budget {
        Budget {
            id: "work".into(),
            scope: BudgetScope::Environment,
            target: "e1".into(),
            window: BudgetWindow::Weekly,
            tokens: Some(tokens),
            action,
            ..Budget::default()
        }
    }

    fn thresholds_of(fired: &[Fired]) -> Vec<u32> {
        fired.iter().map(|alert| alert.threshold).collect()
    }

    #[test]
    fn each_threshold_alerts_once_per_window_and_again_after_rollover() {
        let dir = tempfile::tempdir().unwrap();
        let book = BudgetBook::load(
            dir.path().join("usage.json"),
            settings(vec![weekly_env(1000, BudgetAction::Warn)]),
        );
        let monday = date("2026-11-09");
        assert!(
            book.record(monday, &call(Some("e1"), "a:m", 400, None))
                .is_empty()
        );
        // Another environment does not count.
        assert!(
            book.record(monday, &call(Some("e2"), "a:m", 900, None))
                .is_empty()
        );
        assert_eq!(
            thresholds_of(&book.record(monday, &call(Some("e1"), "a:m", 200, None))),
            [50]
        );
        // Still over 50, under 80: nothing new.
        assert!(
            book.record(date("2026-11-10"), &call(Some("e1"), "a:m", 50, None))
                .is_empty()
        );
        // One call past both 80 and 100 sends one alert, for 100, and 80 counts as sent.
        let fired = book.record(date("2026-11-11"), &call(Some("e1"), "a:m", 500, None));
        assert_eq!(thresholds_of(&fired), [100]);
        assert!(fired[0].share >= 1.0);
        assert_eq!(fired[0].window_end, date("2026-11-16"));
        assert!(
            book.record(date("2026-11-15"), &call(Some("e1"), "a:m", 500, None))
                .is_empty()
        );
        let standing = &book.standings(date("2026-11-15"))[0];
        assert_eq!(standing.tokens, 1650);
        assert_eq!(standing.alerted, [50, 80, 100]);

        // The next week starts empty and alerts again.
        let next = date("2026-11-16");
        let standing = &book.standings(next)[0];
        assert_eq!((standing.tokens, standing.alerted.len()), (0, 0));
        assert_eq!(
            thresholds_of(&book.record(next, &call(Some("e1"), "a:m", 600, None))),
            [50]
        );
    }

    #[test]
    fn cost_budgets_count_priced_calls_and_the_further_limit_wins() {
        let book = BudgetBook {
            path: None,
            reserved: Mutex::new(Vec::new()),
            next_reservation: std::sync::atomic::AtomicU64::new(1),
            inner: Mutex::new((
                settings(vec![Budget {
                    id: "router".into(),
                    scope: BudgetScope::Endpoint,
                    target: "router".into(),
                    window: BudgetWindow::Daily,
                    tokens: Some(1_000_000),
                    cost_usd: Some(1.0),
                    thresholds: Some(vec![90]),
                    ..Budget::default()
                }]),
                Ledger::default(),
            )),
        };
        let today = date("2026-11-10");
        // A local model without prices counts tokens only, and another endpoint not at all.
        book.record(today, &call(None, "llamacpp:q", 5_000_000, None));
        assert!(
            book.record(today, &call(None, "router:big", 1000, Some(0.5)))
                .is_empty()
        );
        assert_eq!(
            thresholds_of(&book.record(today, &call(None, "router:big", 1000, Some(0.45)))),
            [90]
        );
        let standing = &book.standings(today)[0];
        assert_eq!(standing.tokens, 2000);
        assert!((standing.share - 0.95).abs() < 1e-9, "{}", standing.share);
    }

    #[test]
    fn used_up_budgets_ask_or_block_new_turns_in_their_scope() {
        let book = BudgetBook {
            path: None,
            inner: Mutex::new((
                settings(vec![
                    weekly_env(100, BudgetAction::Ask),
                    Budget {
                        id: "model".into(),
                        scope: BudgetScope::Model,
                        target: "a:big".into(),
                        window: BudgetWindow::Monthly,
                        tokens: Some(100),
                        action: BudgetAction::Block,
                        ..Budget::default()
                    },
                    Budget {
                        id: "all".into(),
                        window: BudgetWindow::Daily,
                        tokens: Some(10),
                        action: BudgetAction::Warn,
                        ..Budget::default()
                    },
                ]),
                Ledger::default(),
            )),
        };
        let today = date("2026-11-10");
        assert!(
            book.gate(today, origin(Some("e1"), Some("a:small")))
                .is_none()
        );
        book.record(today, &call(Some("e1"), "a:small", 150, None));
        // The global budget is used up too, but only warns.
        let stop = book
            .gate(today, origin(Some("e1"), Some("a:small")))
            .unwrap();
        assert_eq!(
            (stop.budget.id.as_str(), stop.budget.action),
            ("work", BudgetAction::Ask)
        );
        assert!(
            book.gate(today, origin(Some("e2"), Some("a:small")))
                .is_none()
        );
        book.record(today, &call(Some("e2"), "a:big", 150, None));
        // Block wins over ask.
        let stop = book.gate(today, origin(Some("e1"), Some("a:big"))).unwrap();
        assert_eq!(stop.budget.action, BudgetAction::Block);
        assert!(
            book.gate(today, origin(Some("e2"), Some("a:small")))
                .is_none()
        );
        // After the week the environment budget lets turns start again.
        assert!(
            book.gate(date("2026-11-16"), origin(Some("e1"), Some("a:small")))
                .is_none()
        );
    }

    #[test]
    fn usage_and_alerts_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        let budgets = settings(vec![weekly_env(1000, BudgetAction::Block)]);
        let today = date("2026-11-10");
        {
            let book = BudgetBook::load(path.clone(), budgets.clone());
            assert_eq!(
                thresholds_of(&book.record(today, &call(Some("e1"), "a:m", 600, Some(0.25)))),
                [50]
            );
        }
        let book = BudgetBook::load(path, budgets);
        let standing = &book.standings(today)[0];
        assert_eq!((standing.tokens, standing.alerted.clone()), (600, vec![50]));
        // 50 does not alert again after the restart.
        assert!(
            book.record(today, &call(Some("e1"), "a:m", 10, None))
                .is_empty()
        );
        let (days, environments, endpoints, models) = book.usage(today, 7, None);
        assert_eq!(days.len(), 7);
        assert_eq!(days[6].tokens, 610);
        assert_eq!(days[0].tokens, 0);
        assert_eq!(environments[0].key, "e1");
        assert_eq!(endpoints[0].key, "a");
        assert_eq!((models[0].key.as_str(), models[0].calls), ("a:m", 2));
        assert!((models[0].cost_usd - 0.25).abs() < 1e-9);
    }

    #[test]
    fn scopes_match_environments_endpoints_and_models() {
        let budget = |scope, target: &str| Budget {
            id: "b".into(),
            scope,
            target: target.into(),
            tokens: Some(1),
            ..Budget::default()
        };
        assert!(applies(
            &budget(BudgetScope::Global, ""),
            origin(None, None)
        ));
        let env = budget(BudgetScope::Environment, "e1");
        assert!(applies(&env, origin(Some("e1"), Some("x:y"))));
        assert!(!applies(&env, origin(Some("e2"), Some("x:y"))));
        assert!(!applies(&env, origin(None, Some("x:y"))));
        let endpoint = budget(BudgetScope::Endpoint, "openrouter");
        assert!(applies(
            &endpoint,
            origin(None, Some("openrouter:qwen/qwen3"))
        ));
        assert!(!applies(&endpoint, origin(None, Some("llamacpp:qwen"))));
        assert!(!applies(&endpoint, origin(None, None)));
        let model = budget(BudgetScope::Model, "openrouter:qwen/qwen3");
        assert!(applies(&model, origin(None, Some("openrouter:qwen/qwen3"))));
        assert!(!applies(&model, origin(None, Some("openrouter:other"))));
        let api = budget(BudgetScope::Source, "api");
        let from_api = Origin {
            source: Some("api"),
            ..origin(None, Some("llamacpp:qwen"))
        };
        assert!(applies(&api, from_api));
        assert!(!applies(&api, origin(None, Some("llamacpp:qwen"))));
        // A global budget counts API calls too.
        assert!(applies(&budget(BudgetScope::Global, ""), from_api));
    }

    #[test]
    fn a_source_budget_counts_only_calls_from_that_source() {
        let today = date("2026-11-11");
        let book = BudgetBook {
            path: None,
            inner: Mutex::new((
                settings(vec![Budget {
                    id: "api-cap".into(),
                    scope: BudgetScope::Source,
                    target: "api".into(),
                    window: BudgetWindow::Daily,
                    tokens: Some(1000),
                    action: BudgetAction::Block,
                    ..Budget::default()
                }]),
                Ledger::default(),
            )),
        };
        let from_api = |tokens| CallUsage {
            source: Some("api".into()),
            ..call(None, "llamacpp:m", tokens, None)
        };
        book.record(today, &call(None, "llamacpp:m", 5000, None));
        let api = Origin {
            source: Some("api"),
            ..origin(None, Some("llamacpp:m"))
        };
        assert!(book.gate(today, api).is_none(), "sessions do not count");
        book.record(today, &from_api(1200));
        let stop = book.gate(today, api).expect("the API budget is used up");
        assert_eq!(stop.budget.id, "api-cap");
        assert!(book.gate(today, origin(None, Some("llamacpp:m"))).is_none());
    }
}
