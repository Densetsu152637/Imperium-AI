use super::*;

/// Tool arguments as JSON for hooks, or the raw text when they do not parse.
/// Switching models and compacting the conversation; see [`compact`](crate::compact).
impl Agent {
    /// Tokens the next model call is expected to read: the latest reported prompt size plus an
    /// estimate of the messages added since, else the whole request estimated.
    pub(super) fn context_tokens(&self, tools: &[(Category, String)]) -> u64 {
        match self.last_reported {
            Some((covered, prompt)) if covered <= self.messages.len() => {
                prompt
                    + self.messages[covered..]
                        .iter()
                        .map(|message| {
                            let text = serde_json::to_string(message).unwrap_or_default();
                            estimate_tokens(&text) + compact::MESSAGE_FORMATTING
                        })
                        .sum::<u64>()
            }
            _ => Request {
                messages: &self.messages,
                spans: &self.spans,
                tools,
            }
            .estimate(),
        }
    }

    /// Mid-turn compaction: before a model call, when the conversation has reached
    /// `context.auto_compact_at` of this model's context window, older messages are replaced by
    /// a summary (the same compaction a model switch runs) and the turn goes on. Needs a known
    /// window. Never twice without progress: another attempt waits for two more messages, and
    /// a compaction that did not shrink the conversation stops them for the turn. A failure is
    /// a notice; the turn goes on with the conversation as it is. When it is skipped because the
    /// system prompt, tools, and carried context alone take most of the trigger, one notice says
    /// so, once for the agent's lifetime (`compact_skip_noticed`), not once per turn.
    pub(super) async fn compact_near_limit(&mut self, tools: &[(Category, String)]) {
        let Some(at) = self.shared.auto_compact_at.filter(|at| *at > 0.0) else {
            return;
        };
        if self.single_call || self.compact_stalled || self.messages.len() <= 3 {
            return;
        }
        if self
            .compact_mark
            .is_some_and(|mark| self.messages.len() < mark + 2)
        {
            return;
        }
        let Some(window) = self.known_window(&self.model) else {
            return;
        };
        let trigger = compact::trigger_tokens(window, at);
        let tokens = self.context_tokens(tools);
        if tokens < trigger {
            return;
        }
        // What compaction cannot shrink: the system prompt, the tools, and the carried context.
        let fixed = estimate_tokens(&serde_json::to_string(&self.messages[0]).unwrap_or_default())
            + tools
                .iter()
                .map(|(_, text)| estimate_tokens(text))
                .sum::<u64>()
            + estimate_tokens(&compact::pinned_context(
                &self.messages,
                &self.spans,
                self.messages.len(),
            ));
        if !compact::worth_compacting(fixed, trigger) {
            // Summarising would not get the conversation under its trigger; ask again later.
            self.compact_mark = Some(self.messages.len() + 4);
            if !self.compact_skip_noticed {
                self.compact_skip_noticed = true;
                self.emit(SessionEvent::Notice {
                    message: "the conversation is near its context limit, but compacting it would \
                              not help: the system prompt, tools, and carried context alone take \
                              most of the window"
                        .into(),
                });
            }
            return;
        }
        self.compact_mark = Some(self.messages.len());
        let model = self.model.clone();
        match self.fit_with(&model, false, Some(trigger)).await {
            Ok(fit) => {
                if let Some(compaction) = fit.compaction {
                    self.compact_mark = Some(self.messages.len());
                    // Only a compaction that ran and did not shrink the conversation stops
                    // the attempts for the turn.
                    self.compact_stalled = compaction.after_tokens >= compaction.before_tokens;
                    self.checkpoint();
                }
            }
            Err(error) => {
                // A failed summary is not tried again until the conversation has grown.
                self.compact_mark = Some(self.messages.len() + 4);
                self.emit(SessionEvent::Notice {
                    message: format!("the conversation could not be compacted: {error}"),
                });
            }
        }
    }

    /// Switches this agent to `target` from its next turn, without loading or calling it: the
    /// switch is journalled now, and the conversation is measured against the target (and
    /// compacted if it does not fit) when the next turn starts ([`Agent::settle_model`]).
    /// Returns an estimate of the conversation against the window the target is expected to
    /// have.
    pub(crate) fn select_model(&mut self, target: ResolvedModel) -> ContextFit {
        let from = std::mem::replace(&mut self.model, target);
        self.shared
            .agents
            .register(&self.id, &self.role, &self.model);
        self.refresh_harness_text();
        self.refresh_loader_text();
        // The calibration related the heuristic to the old model's counts, and the exact count
        // the next local reservation starts from was the old model's.
        self.last_reported = None;
        self.last_call = None;
        match self.previous.take() {
            // Switched again before a turn: the model that last read the conversation stays
            // the one that summarises it, and going back to it leaves nothing to fit.
            Some(previous) if self.fit_pending => {
                if previous.spec == self.model.spec {
                    self.fit_pending = false;
                } else {
                    self.previous = Some(previous);
                }
            }
            _ => {
                self.previous = Some(from.clone());
                self.fit_pending = true;
            }
        }
        self.emit(SessionEvent::ModelChanged {
            from: from.spec,
            to: self.model.spec.clone(),
            compacted: false,
        });
        self.estimate_fit()
    }

    /// Takes up the model a session without one was just given, without loading it: journals
    /// the change from no model; the conversation is fitted when the first turn starts.
    pub(crate) fn adopt_first_model(&mut self) -> ContextFit {
        self.fit_pending = true;
        self.previous = None;
        self.emit(SessionEvent::ModelChanged {
            from: String::new(),
            to: self.model.spec.clone(),
            compacted: false,
        });
        self.estimate_fit()
    }

    /// Marks the conversation as not yet measured against this agent's model, so the next turn
    /// fits it first (a session resumed after a switch that no turn followed).
    pub(crate) fn mark_unfitted(&mut self) {
        self.fit_pending = true;
    }

    /// Whether the conversation still has to be fitted to a model switched to.
    pub(crate) fn fit_pending(&self) -> bool {
        self.fit_pending
    }

    /// Fits the conversation to the model it was switched to, when a fit is pending: measures
    /// it with the model's own tokenizer and window (loading a local model) and compacts it if
    /// it does not fit. Called when a turn starts. On an error the fit stays pending, so the
    /// next turn tries again, or another model can be picked.
    pub(crate) async fn settle_model(&mut self) -> Result<Option<ContextFit>, SwitchError> {
        if !self.fit_pending {
            return Ok(None);
        }
        let model = self.model.clone();
        let fit = self.fit_to(&model, false).await?;
        self.fit_pending = false;
        self.previous = None;
        Ok(Some(fit))
    }

    /// The context window this agent's model is expected to have: the model table's, else (for a
    /// local model) what the runtime would start it with.
    pub(super) fn expected_window(&self) -> Option<u64> {
        self.model.context_window.or_else(|| {
            self.local_models()
                .and_then(|local| local.planned_context(&self.model.model))
        })
    }

    /// The conversation estimated against the window this agent's model is expected to have,
    /// without asking its server anything: the model table's window, else (for a local model)
    /// what the runtime would start it with.
    pub(super) fn estimate_fit(&self) -> ContextFit {
        let (_, tools) = self.offered_tools();
        let window = self.model.context_window.or_else(|| {
            self.local_models()
                .and_then(|local| local.planned_context(&self.model.model))
        });
        let request = Request {
            messages: &self.messages,
            spans: &self.spans,
            tools: &tools,
        };
        let measured = TokenCounter::estimate(&request, window);
        ContextFit {
            tokens: measured.total(),
            context_window: window,
            budget: window.map(compact::budget),
            method: measured.method,
            compaction: None,
        }
    }

    /// Compacts the conversation now for this agent's own model, keeping the last
    /// [`KEEP_TURNS`](compact::KEEP_TURNS) turns verbatim (fewer if they do not fit).
    pub(crate) async fn compact_now(&mut self) -> Result<ContextFit, SwitchError> {
        let model = self.model.clone();
        let fit = self.fit_to(&model, true).await?;
        self.fit_pending = false;
        self.previous = None;
        Ok(fit)
    }

    /// Compacts the conversation only when it no longer fits this agent's own model's budget
    /// (nothing happens when the model's context window is unknown).
    pub(crate) async fn fit_current(&mut self) -> Result<ContextFit, SwitchError> {
        let model = self.model.clone();
        let fit = self.fit_to(&model, false).await?;
        self.fit_pending = false;
        self.previous = None;
        Ok(fit)
    }

    /// Measures the conversation for `target` and, when it does not fit the target's budget
    /// (or `force` is set), replaces older messages with a summary until it does.
    pub(super) async fn fit_to(
        &mut self,
        target: &ResolvedModel,
        force: bool,
    ) -> Result<ContextFit, SwitchError> {
        self.fit_with(target, force, None).await
    }

    /// [`fit_to`](Self::fit_to) that also compacts when the conversation reaches `trigger`
    /// tokens, whether or not it is over the budget (mid-turn compaction).
    pub(super) async fn fit_with(
        &mut self,
        target: &ResolvedModel,
        force: bool,
        trigger: Option<u64>,
    ) -> Result<ContextFit, SwitchError> {
        let (_, tools) = self.offered_tools();
        let scale = self.calibration(&tools);
        let before = self
            .measure_for(target, &self.messages, &self.spans, &tools, scale)
            .await?;
        let before_tokens = before.total();
        let Some(window) = before.window else {
            if force {
                return Err(SwitchError::UnknownWindow(target.spec.clone()));
            }
            return Ok(ContextFit {
                tokens: before_tokens,
                context_window: None,
                budget: None,
                method: before.method,
                compaction: None,
            });
        };
        let budget = compact::budget(window);
        let unchanged = ContextFit {
            tokens: before_tokens,
            context_window: Some(window),
            budget: Some(budget),
            method: before.method,
            compaction: None,
        };
        let triggered = trigger.is_some_and(|trigger| before_tokens >= trigger);
        if before_tokens <= budget && !force && !triggered {
            return Ok(unchanged);
        }
        // The system prompt and the tools stay whatever happens, with at least a short summary.
        // Workflow context loaded so far is carried over verbatim, so it is part of the base.
        let pinned_all = compact::pinned_context(&self.messages, &self.spans, self.messages.len());
        let pinned_tokens = estimate_tokens(&pinned_all);
        let base = before.fixed + before.messages[0] + compact::MESSAGE_FORMATTING + pinned_tokens;
        if base + compact::MIN_SUMMARY_TOKENS > budget {
            return Err(SwitchError::TooLarge {
                model: target.spec.clone(),
                needed: before.fixed + before.messages[0] + pinned_tokens,
                budget,
            });
        }
        let keep_from = if force {
            match compact::recent_turns_start(&self.messages, compact::KEEP_TURNS) {
                Some(start) => Some(start),
                // Nothing is older than the turns a manual compaction keeps.
                None => return Ok(unchanged),
            }
        } else {
            None
        };
        // Aim well under the budget so the conversation can go on; a third of that room goes
        // to the summary and the rest to the most recent messages.
        let mut aim = budget * compact::TARGET_PERCENT / 100;
        if let Some(trigger) = trigger {
            // Well under the trigger, or the next call would trigger it again.
            aim = aim.min(trigger * compact::BELOW_TRIGGER_PERCENT / 100);
        }
        let free = aim.saturating_sub(base);
        let allowance = (free / 3).clamp(compact::MIN_SUMMARY_TOKENS, compact::MAX_SUMMARY_TOKENS);
        // The tools looked up so far, as names, for the model to search again.
        let loaded = self.loaded_tools_block();
        let mut cut = compact::choose_cut(
            &self.messages,
            &before,
            free.saturating_sub(allowance),
            keep_from,
        );
        self.emit(SessionEvent::CompactionStarted {
            model: target.spec.clone(),
        });
        let (mut summary, mut summary_model) = self.summarise(target, cut, allowance).await?;
        for _ in 0..compact::MAX_ATTEMPTS {
            let block = compact::summary_block(&summary, &loaded);
            let pinned = compact::pinned_context(&self.messages, &self.spans, cut);
            let (mut messages, mut spans) =
                compact::rebuild(&self.messages, &self.spans, cut, &block, &pinned);
            if self.can_delegate {
                reattach_state(&mut messages, &mut spans, &self.state);
            }
            let after = self
                .measure_for(target, &messages, &spans, &tools, scale)
                .await?;
            let after_tokens = after.total();
            if after_tokens <= budget {
                let summarised_messages = u32::try_from(cut - 1).unwrap_or(u32::MAX);
                self.messages = messages;
                self.spans = spans;
                self.last_reported = None;
                self.last_call = None;
                // The earlier `tool_search` results are gone: parameters are advised again.
                self.forget_schemas();
                self.emit(SessionEvent::ContextCompacted {
                    model: target.spec.clone(),
                    before_tokens,
                    after_tokens,
                    summarised_messages,
                    budget,
                    context_window: Some(window),
                    method: after.method,
                    summary_model: summary_model.clone(),
                    kept_context: true,
                });
                return Ok(ContextFit {
                    tokens: after_tokens,
                    context_window: Some(window),
                    budget: Some(budget),
                    method: after.method,
                    compaction: Some(Compaction {
                        before_tokens,
                        after_tokens,
                        summarised_messages,
                        summary_model,
                    }),
                });
            }
            // What the summary took: its message, less the kept message it went into.
            let host = if compact::merges(&self.messages, cut) {
                before.messages[cut]
            } else {
                compact::MESSAGE_FORMATTING
            };
            let summary_tokens = after.messages[1]
                .saturating_sub(host)
                .saturating_sub(estimate_tokens(&pinned));
            let others = after_tokens.saturating_sub(summary_tokens);
            if others + compact::MIN_SUMMARY_TOKENS <= budget {
                // A shorter summary is enough.
                summary = compact::shorten(&summary, budget - others, summary_tokens);
                continue;
            }
            // Keep fewer recent messages, summarising the ones no longer kept.
            let tail: u64 = before.messages[cut..].iter().sum();
            let excess = others + compact::MIN_SUMMARY_TOKENS - budget;
            let next = compact::choose_cut(
                &self.messages,
                &before,
                tail.saturating_sub(excess),
                keep_from,
            );
            let next = if next > cut {
                next
            } else {
                match compact::next_cut(&self.messages, cut) {
                    Some(next) => next,
                    None => break,
                }
            };
            cut = next;
            (summary, summary_model) = self.summarise(target, cut, allowance).await?;
        }
        Err(SwitchError::NoFit {
            model: target.spec.clone(),
            budget,
        })
    }

    /// Summarises the messages before `cut` (after the system prompt) in about `allowance`
    /// tokens. The model that last read the whole conversation writes it (the one switched from,
    /// while a switch's fit is pending); if it cannot be reached, the target model does. Returns the summary and the model's spec.
    pub(super) async fn summarise(
        &self,
        target: &ResolvedModel,
        cut: usize,
        allowance: u64,
    ) -> Result<(String, String), SwitchError> {
        let request = compact::summary_request(&self.messages[1..cut], allowance * 3 / 4);
        // After a switch, the model the conversation was last read by.
        let first = self.previous.as_ref().unwrap_or(&self.model);
        let mut candidates = vec![first];
        if target.spec != first.spec {
            candidates.push(target);
        }
        let mut failures = Vec::new();
        let holder = self.holder();
        for model in candidates {
            if let Some(message) = self.budget_stop_for(&model.spec).await {
                failures.push(format!("{}: {message}", model.spec));
                continue;
            }
            let (client, lease) = match self
                .until_stopped(self.client_for(model, &holder, None, false))
                .await?
            {
                Ok(client) => client,
                Err(error) => {
                    failures.push(format!("{}: {error}", model.spec));
                    continue;
                }
            };
            let mut ignore = |_: &str| {};
            let started = Instant::now();
            let completion = self
                .until_stopped(client.stream(
                    ChatRequest {
                        model: &model.model,
                        messages: &request,
                        tools: &[],
                        slot: lease.as_ref().and_then(|lease| lease.slot),
                        max_tokens: lease.as_ref().and_then(|lease| lease.max_tokens),
                        // A summary needs no deliberation: no reasoning control is sent.
                        effort: None,
                    },
                    &mut ignore,
                ))
                .await;
            drop(lease);
            let completion = completion?;
            // The summary was a paid call: it counts toward budgets like any other.
            if let Ok(done) = &completion
                && let Some(usage) = &done.usage
            {
                self.emit(SessionEvent::ModelCallCompleted {
                    model: model.spec.clone(),
                    usage: token_usage(usage),
                    duration_ms: millis(started.elapsed()),
                    queue_wait_ms: None,
                    time_to_first_token_ms: None,
                    tokens_per_sec: None,
                    cost_usd: model.pricing.map(|pricing| pricing.cost_usd(usage)),
                });
            }
            match completion {
                Ok(completion) if !completion.text.trim().is_empty() => {
                    return Ok((completion.text.trim().to_string(), model.spec.clone()));
                }
                Ok(_) => failures.push(format!("{} wrote no summary", model.spec)),
                Err(error) => failures.push(format!("{}: {error}", model.spec)),
            }
        }
        Err(SwitchError::Summary(failures.join("; ")))
    }

    /// Measures `messages` as `model` would read them.
    pub(super) async fn measure_for(
        &self,
        model: &ResolvedModel,
        messages: &[ChatMessage],
        spans: &HashMap<usize, Spans>,
        tools: &[(Category, String)],
        scale: Option<f64>,
    ) -> Result<Measured, SwitchError> {
        let (client, _lease) = self
            .until_stopped(self.client_for(model, &self.holder(), None, false))
            .await?
            .map_err(SwitchError::Unavailable)?;
        let request = Request {
            messages,
            spans,
            tools,
        };
        self.until_stopped(
            self.shared
                .tokens
                .measure(client.as_ref(), model, &request, scale),
        )
        .await
    }

    /// Runs `work` (a model call or load that may never return) until the user stops the
    /// session, which ends it with [`SwitchError::Summary`] ("stopped"): the compaction or fit
    /// fails, the switch stays pending, and the turn is dropped cleanly.
    async fn until_stopped<T>(
        &self,
        work: impl std::future::Future<Output = T>,
    ) -> Result<T, SwitchError> {
        let mut stop = self.shared.controls.stop_signal();
        tokio::select! {
            output = work => Ok(output),
            Ok(_) = stop.wait_for(|stopped| *stopped) => Err(SwitchError::Summary("stopped".into())),
        }
    }

    /// How far the heuristic was off on the latest call that reported its prompt size: the
    /// reported size over the estimate, within reason.
    pub(super) fn calibration(&self, tools: &[(Category, String)]) -> Option<f64> {
        let (count, prompt_tokens) = self.last_reported?;
        let request = Request {
            messages: self.messages.get(..count)?,
            spans: &self.spans,
            tools,
        };
        let estimate = request.estimate();
        (estimate > 0).then(|| (prompt_tokens as f64 / estimate as f64).clamp(0.5, 3.0))
    }
}
