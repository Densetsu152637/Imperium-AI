use super::*;

impl Agent {
    /// Adds the steering messages that arrived since the last model call to the conversation,
    /// as user messages marked as steering. Called only between complete tool rounds, so tool
    /// calls and their results stay together. For the root, the subagents it was running pause
    /// for the message first; the first message carries their status, so the root decides
    /// whether to resume, re-brief, or cancel them.
    pub(super) async fn take_steering(&mut self) {
        let steering = self.inbox.take_steering();
        if steering.is_empty() {
            return;
        }
        let stopped = if self.can_delegate {
            self.collect_paused().await
        } else {
            Vec::new()
        };
        if self.can_delegate {
            self.ensure_resident().await;
        }
        let paused: Vec<String> = stopped
            .iter()
            .filter(|report| report.paused)
            .map(|report| report.task_id.clone())
            .collect();
        let status = self.stopped_status(&stopped);
        for (index, message) in steering.into_iter().enumerate() {
            let mut text = steering_text(&message.text);
            let mut listed = Vec::new();
            if index == 0 && !stopped.is_empty() {
                text.push_str(&status);
                listed = paused.clone();
            }
            self.note_user_request(&message.text);
            let rendered = self.render_attachments(&message.attachments).await;
            self.emit(SessionEvent::Steered {
                id: message.id,
                text: message.text,
                paused: listed,
                attachments: message.attachments,
            });
            let mut parts = Vec::new();
            if !rendered.text.is_empty() {
                parts.push((Category::Attachments, rendered.text));
            }
            parts.push((Category::User, text));
            let (content, spans) = join_parts(parts);
            self.spans.insert(self.messages.len(), spans);
            self.messages.push(ChatMessage::User {
                content,
                images: rendered.images,
            });
        }
    }

    /// Waits for the subagents this agent spawned or resumed to pause for a steering message
    /// (each at its next safe point: an in-flight tool call finishes, an in-flight model
    /// request is cancelled) and collects them. One that finished meanwhile is collected with
    /// its report, so nothing is lost.
    pub(super) async fn collect_paused(&mut self) -> Vec<TaskReport> {
        let mut ids: Vec<String> = self
            .tasks
            .keys()
            .filter(|id| self.pausable_tasks.contains(*id))
            .cloned()
            .collect();
        if ids.is_empty() {
            return Vec::new();
        }
        ids.sort_by_key(|id| task_number(id));
        // Asleep while they stop: the local runtime may swap this agent's model out meanwhile.
        let _waiting = self
            .local_models()
            .map(|local| local.waiting(&self.model.model, &self.holder()));
        let mut reports = Vec::new();
        for id in ids {
            self.pausable_tasks.remove(&id);
            let Some(handle) = self.tasks.remove(&id) else {
                continue;
            };
            reports.push(match handle.await {
                Ok(report) => report,
                Err(error) => TaskReport {
                    task_id: id,
                    model: String::new(),
                    ok: false,
                    paused: false,
                    report: format!("the subagent crashed: {error}"),
                },
            });
        }
        reports
    }

    /// Makes sure the agent is ready to work again after waiting (on subagents, or a stopped
    /// session): when the local runtime saved its context and freed its cells meanwhile, its
    /// model is loaded and its context restored now, as the user's work, ahead of idle
    /// subagents, rather than at its next call. A hosted model keeps no state between calls.
    pub(super) async fn ensure_resident(&self) {
        // A fork's slot is restored by its next call, at the session's own priority (a
        // background session's fork does not jump the user's calls); resuming it here would
        // be the user's.
        if self.fork.is_some() {
            return;
        }
        let Some(local) = self.local_models() else {
            return;
        };
        let holder = self.holder();
        if !local.suspended(&holder) {
            return;
        }
        // The runtime loads the model if needed and restores the context into the slot this
        // agent gets, reporting it as a resume, and leaves the slot this agent's.
        if let Err(error) = local.resume(&self.model.model, &holder).await {
            tracing::debug!(%holder, %error, "could not restore the agent's context ahead of its call");
        }
    }

    /// While the user has the session stopped: waits here, at a safe point, holding no
    /// local-model slot (asleep to the runtime, which may swap the model out and save the slot),
    /// then takes the model back and goes on.
    pub(super) async fn hold_while_stopped(
        &self,
        attached: &mut Option<LocalPresence>,
        holder: &str,
    ) {
        let mut stop = self.shared.controls.stop_signal();
        if !*stop.borrow_and_update() {
            return;
        }
        *attached = None;
        let asleep = self
            .local_models()
            .map(|local| local.waiting(&self.model.model, holder));
        // Stopped for who knows how long: its context is saved and its cells freed at once, so
        // other sessions use the GPU meanwhile.
        if let Some(local) = self.local_models() {
            local.suspend(&self.model.model, holder).await;
        }
        let _ = stop.wait_for(|stopped| !*stopped).await;
        drop(asleep);
        *attached = self
            .local_models()
            .map(|local| local.attach(&self.model.model, holder));
        // Resumed: its context comes back before it goes on.
        self.ensure_resident().await;
    }

    /// Ends a turn early for a pause: the conversation is saved as it stands (every tool call
    /// has its result), and the outcome says how far the turn got.
    pub(super) fn pause_turn(&self, usage: Usage, calls: u32, reply: &str) -> TurnOutcome {
        self.checkpoint();
        let text = if reply.trim().is_empty() {
            format!("Paused after {calls} model calls in its current turn, before it replied.")
        } else {
            format!(
                "Paused after {calls} model calls in its current turn. Its latest words: {}",
                truncate(reply, 600)
            )
        };
        TurnOutcome {
            text,
            ok: false,
            paused: true,
            cost_usd: self.cost(&usage),
            usage,
        }
    }

    /// Runs one turn: model calls and tool calls until the model answers without tools.
    /// `context` (recalled memories) is given to the model before the message but not shown
    /// as part of it.
    pub(crate) async fn run_turn(&mut self, text: String, context: Option<String>) -> TurnOutcome {
        self.run_turn_with(text, Vec::new(), context).await
    }

    /// [`run_turn`](Self::run_turn) for a message with files attached: they are read now and
    /// go before the message's text (images as image parts for a vision model).
    pub(crate) async fn run_turn_with(
        &mut self,
        text: String,
        attachments: Vec<Attachment>,
        context: Option<String>,
    ) -> TurnOutcome {
        let text = if self.can_delegate {
            let window = self.window().await;
            self.guard_input(text, window)
        } else {
            text
        };
        let shown = self.shown.take().unwrap_or_else(|| text.clone());
        self.note_user_request(&text);
        let rendered = self.render_attachments(&attachments).await;
        self.emit(SessionEvent::TurnStarted {
            text: shown,
            attachments,
        });
        // Harness aids for small models (root agent only): the message de-referenced, the
        // session state and the ambiguity hints for it, all at the top of the turn. Earlier
        // turns' messages are left as they are, so the prompt prefix a local model has cached
        // does not change.
        let mut parts = Vec::new();
        let mut user_text = text.clone();
        if self.can_delegate && !self.single_call {
            let found = if self.harness_on(self.shared.harness.dereference) {
                self.dereference(&text).await
            } else {
                harness::Dereferenced::default()
            };
            let keep_state = self.harness_on(self.shared.harness.state);
            let hints = self.harness_on(self.shared.harness.hints);
            let max_state = self.shared.harness.state_max_chars as usize;
            if keep_state && let Some(update) = found.update {
                self.state.apply(update, max_state);
            }
            if let Some(resolved) = found.prompt {
                self.emit(SessionEvent::Notice {
                    message: format!("understood as: {resolved}"),
                });
                user_text = harness::resolved_text(&resolved, &text);
            }
            // The block goes in when the state differs from the newest copy in the
            // conversation, so an unchanged state is not repeated every turn.
            // A copy far back is repeated too (`harness.state_reemit_turns`), so a small model
            // reads the state near the end of its context.
            let reemit = self.shared.harness.state_reemit_turns as usize;
            let stale = reemit > 0
                && harness::turns_since_copy(&self.messages).is_some_and(|turns| turns >= reemit);
            if keep_state
                && (stale || harness::latest_state(&self.messages) != self.state)
                && let Some(block) = self.state.block()
            {
                parts.push((Category::Memories, block));
            }
            if hints
                && let Some(block) =
                    patterns::render(&patterns::retrieve(&text, MAX_AMBIGUITY_HINTS))
            {
                parts.push((Category::Memories, block));
            }
        }
        if !self.single_call
            && let Some(index) = &self.shared.context
        {
            let manifest = index.resolve(&self.role, &text);
            if !manifest.is_empty() {
                *lock(&self.unloaded) = manifest.required().map(str::to_string).collect();
                self.emit(manifest_event(&manifest));
                parts.push((Category::Manifest, manifest.render()));
            }
        }
        parts.extend(context.map(|memories| (Category::Memories, memories)));
        if !rendered.text.is_empty() {
            parts.push((Category::Attachments, rendered.text));
        }
        parts.push((Category::User, user_text));
        let (content, spans) = join_parts(parts);
        self.spans.insert(self.messages.len(), spans);
        self.messages.push(ChatMessage::User {
            content,
            images: rendered.images,
        });
        self.checkpoint();
        self.refresh_loader_text();
        let (mut tools, mut tool_texts) = self.offered_tools();
        self.last_tools.clone_from(&tools);
        self.asked_this_turn = 0;
        self.compact_stalled = false;
        let mut total = Usage::default();
        let mut reply = String::new();
        let max_steps = self.shared.max_steps;
        // Names this agent to a local model, which gives it back the slot it used last and
        // keeps the model loaded between its calls.
        let holder = self.holder();
        let mut attached = self
            .local_models()
            .map(|local| local.attach(&self.model.model, &holder));
        for step in 1..=max_steps {
            // A background session gives way here, between model calls, holding no slot.
            self.yield_to_user(&mut attached, &holder).await;
            // The safe point for steering: the last model response's tool calls all have
            // their results. A subagent stops here when the user steered the root.
            // A session the user stopped halts here, holding no slot, until resumed.
            self.hold_while_stopped(&mut attached, &holder).await;
            if self.pause_requested() {
                return self.pause_turn(total, step - 1, &reply);
            }
            self.take_steering().await;
            self.take_background_notices();
            // A compaction is a model call too: a used-up budget stops it, and the turn.
            if let Some(message) = self.budget_stop().await {
                self.emit(SessionEvent::TurnFailed {
                    message: message.clone(),
                });
                return self.outcome(message, false, total);
            }
            self.compact_near_limit(&tool_texts).await;
            let mut waits = 0;
            let mut waited = std::time::Duration::ZERO;
            // A reply cut short by its reservation is retried once without a cap.
            let mut uncapped = false;
            // What the reservation counted for the prompt, to compare with what it was.
            let mut estimate: u32;
            // The reply room the call that finished was sent with (`max_tokens`).
            let mut reserved_output: Option<u32>;
            let (client, queue_wait, started, first_text, result) = loop {
                // A used-up budget stops spend here, before each call, mid-turn included.
                if let Some(message) = self.budget_stop().await {
                    self.emit(SessionEvent::TurnFailed {
                        message: message.clone(),
                    });
                    return self.outcome(message, false, total);
                }
                // A local model's slot is held for this one request, not for the turn: tool
                // calls, `await_tasks`, and usage-limit pauses run without one. The model is
                // loaded here if needed.
                let prompt_tokens = self.prompt_estimate(&tool_texts);
                estimate = prompt_tokens;
                let (client, lease) = match self.client(&holder, prompt_tokens, uncapped).await {
                    Ok(taken) => taken,
                    Err(message) => {
                        self.emit(SessionEvent::TurnFailed {
                            message: message.clone(),
                        });
                        return self.outcome(message, false, total);
                    }
                };
                let queue_wait = lease.as_ref().map(|lease| lease.waited);
                // A variant's edits of the root's tools: what the model reads is renamed and
                // redescribed; the engine's own messages and list keep the engine's names.
                let shown_tools = self.shown_tools(&tools);
                let shown_messages = self.shown_messages();
                let request = ChatRequest {
                    model: &self.model.model,
                    messages: shown_messages.as_deref().unwrap_or(&self.messages),
                    tools: shown_tools.as_deref().unwrap_or(&tools),
                    slot: lease.as_ref().and_then(|lease| lease.slot),
                    max_tokens: lease.as_ref().and_then(|lease| lease.max_tokens),
                    effort: self.call_effort(),
                };
                let capped = request.max_tokens.is_some();
                reserved_output = request.max_tokens;
                let started = Instant::now();
                let mut first_text = None;
                let emitter = &self.shared.emitter;
                let id = &self.id;
                let mut on_text = |text: &str| {
                    first_text.get_or_insert_with(Instant::now);
                    emitter.emit(
                        id,
                        SessionEvent::TextDelta {
                            text: text.to_string(),
                        },
                    );
                };
                // The request is cancelled when the user stops the session (it is sent again on
                // resume) or, for a pausable subagent, steers the root.
                let mut stop = self.shared.controls.stop_signal();
                let streamed = tokio::select! {
                    result = client.stream(request, &mut on_text) => Streamed::Done(result),
                    () = maybe_signalled(self.pause.as_mut()) => Streamed::Paused,
                    Ok(_) = stop.wait_for(|stopped| *stopped) => Streamed::Stopped,
                };
                drop(lease);
                let result = match streamed {
                    Streamed::Done(result) => result,
                    Streamed::Paused => return self.pause_turn(total, step - 1, &reply),
                    Streamed::Stopped => {
                        self.emit(SessionEvent::Notice {
                            message: "the model call was cancelled because the session was                                       stopped; it is sent again on resume"
                                .into(),
                        });
                        self.hold_while_stopped(&mut attached, &holder).await;
                        continue;
                    }
                };
                // A capped local reply that hit its reservation (or overflowed the KV pool) starts
                // again with the whole context; one sent uncapped would only fail the same way.
                if self.model.local && !uncapped && capped {
                    let cut = match &result {
                        Ok(completion) => completion.finish_reason.as_deref() == Some("length"),
                        Err(error) => error.is_context_overflow(),
                    };
                    if cut {
                        uncapped = true;
                        self.emit(SessionEvent::Notice {
                            message: "the local reply was cut off by its token reservation (or the KV pool was full); it starts again below".into(),
                        });
                        continue;
                    }
                }
                // Usage ran out: wait as the session's resume policy says, then try again.
                let Err(error) = &result else {
                    break (client, queue_wait, started, first_text, result);
                };
                if !error.is_usage_limit() {
                    break (client, queue_wait, started, first_text, result);
                }
                let now = chrono::Local::now();
                let policy = self.shared.controls.policy();
                let Some(until) =
                    crate::next_attempt(&policy, error.retry_after_s(), waits, waited, now)
                else {
                    break (client, queue_wait, started, first_text, result);
                };
                self.emit(SessionEvent::Paused {
                    until_unix_ms: u64::try_from(until.timestamp_millis()).unwrap_or(0),
                    reason: error.to_string(),
                    cause: PauseCause::UsageLimit,
                });
                let pause = Instant::now();
                // Asleep until the limit lifts: the model may be swapped out meanwhile.
                let sleeping = self
                    .local_models()
                    .map(|local| local.waiting(&self.model.model, &holder));
                self.shared.controls.pause_until(until).await;
                drop(sleeping);
                waited += pause.elapsed();
                waits += 1;
                self.emit(SessionEvent::Resumed {});
            };
            let mut completion = match result {
                Ok(completion) => completion,
                Err(error) => {
                    let message = error.to_string();
                    self.emit(SessionEvent::TurnFailed {
                        message: message.clone(),
                    });
                    return self.outcome(message, false, total);
                }
            };
            // The model's names for a variant's renamed tools become the engine's again before
            // anything is stored.
            self.engine_calls(&mut completion.tool_calls);
            let usage = completion.usage.unwrap_or_default();
            // A local model's call that the server left in the text becomes a call.
            let offered: Vec<String> = tools.iter().map(|tool| tool.name.clone()).collect();
            if self.model.local
                && !self.single_call
                && completion.tool_calls.is_empty()
                && let Some((calls, text)) = call_repair::from_content(&completion.text, &offered)
            {
                completion.tool_calls = calls;
                completion.text = text;
            }
            // Arguments are repaired before the call is stored, so the history keeps the
            // repaired form (for hosted models as well).
            completion.tool_calls = completion
                .tool_calls
                .into_iter()
                .map(call_repair::vet)
                .collect();
            // The pool admits calls by the estimate: a prompt above it overcommits the pool.
            if self.model.local && usage.prompt_tokens > u64::from(estimate) {
                tracing::warn!(
                    model = %self.model.model,
                    estimate,
                    prompt_tokens = usage.prompt_tokens,
                    "a local prompt exceeded its reservation estimate"
                );
            }
            total.prompt_tokens += usage.prompt_tokens;
            total.completion_tokens += usage.completion_tokens;
            total.cached_tokens += usage.cached_tokens;
            // A thinking model's reasoning streams before any reply text; the client marks it.
            let first_token = match (completion.first_token, first_text) {
                (Some(marked), Some(text)) => Some(marked.min(text)),
                (marked, text) => marked.or(text),
            };
            self.emit(self.call_telemetry(&usage, started, first_token, queue_wait));
            // What this call read: the messages before its reply is appended, and the tools.
            let breakdown = self
                .shared
                .tokens
                .breakdown(
                    Call {
                        client: client.as_ref(),
                        model: &self.model,
                        usage: completion.usage,
                        reserved_output,
                    },
                    &Request {
                        messages: &self.messages,
                        spans: &self.spans,
                        tools: &tool_texts,
                    },
                )
                .await;
            self.emit(breakdown);
            if let Some(usage) = completion.usage.filter(|usage| usage.prompt_tokens > 0) {
                self.last_reported = Some((self.messages.len(), usage.prompt_tokens));
            }
            if !completion.text.is_empty() {
                reply = completion.text.clone();
            }
            // A single-call agent runs no tools, so stray calls are dropped from the transcript
            // rather than left without results.
            let tool_calls = if self.single_call {
                Vec::new()
            } else {
                completion.tool_calls.clone()
            };
            self.messages.push(ChatMessage::Assistant {
                content: Some(completion.text).filter(|text| !text.is_empty()),
                tool_calls,
            });
            if self.model.local && completion.usage.is_some() {
                self.last_call = Some((
                    usage.prompt_tokens + usage.completion_tokens,
                    self.messages.len(),
                ));
            }
            if self.single_call && reply.trim().is_empty() {
                let message = "the model returned no reply to a single-call task".to_string();
                self.emit(SessionEvent::TurnFailed {
                    message: message.clone(),
                });
                return self.outcome(message, false, total);
            }
            // Steering that arrived during the final answer keeps the turn going (a single-call
            // task makes exactly one call, so it does not).
            if !self.single_call && completion.tool_calls.is_empty() && self.inbox.has_steering() {
                continue;
            }
            if completion.tool_calls.is_empty() || self.single_call {
                // The turn ends after the files before it are recorded, so what changes once
                // it has ended (the user's edits) is not mistaken for the turn's.
                let _ = self.shared.snapshot_ready().await;
                self.checkpoint();
                self.emit(SessionEvent::TurnCompleted {
                    usage: token_usage(&total),
                    cost_usd: self.cost(&total),
                    steps: step,
                });
                return self.outcome(reply, true, total);
            }
            // Measured again for each round: the session may have switched the model.
            let window = if self.can_delegate {
                self.window().await
            } else {
                u64::MAX
            };
            let activates = completion
                .tool_calls
                .iter()
                .any(|call| effective_call(call).function.name == ACTIVATE_PLUGIN);
            let mut touched: Vec<PathBuf> = Vec::new();
            let calls = completion.tool_calls;
            // With loading on, other tools are reached through `call_tool`: no names to point at.
            let close: &[String] = if self.reactive() { &[] } else { &offered };
            // Reads in a row run at the same time (`harness.parallel_reads`), up to the call
            // this index reaches; each call below still goes through `run_tool` in call order.
            let mut ahead_until = 0;
            for (index, call) in calls.iter().enumerate() {
                if index >= ahead_until {
                    ahead_until = index + self.prefetch_reads(&calls[index..]).await;
                }
                self.last_tool_ok.store(false, Ordering::SeqCst);
                let output = self.run_tool(call).await;
                self.forget_prefetched(&call.id);
                let output = call_repair::with_close_names(
                    &output,
                    &effective_call(call).function.name,
                    close,
                )
                .unwrap_or(output);
                let output = self.guard_result(call, output, window);
                // Only a call that worked counts as opening the directory: not one that was
                // denied, refused by a hook, or failed.
                // The arguments the call ran with: a hook may have rewritten them.
                // A call through `call_tool` is the tool it ran.
                let (dir, searched) = {
                    let effective = effective_call(call);
                    let ran_with = lock(&self.ran_rewritten)
                        .clone()
                        .unwrap_or_else(|| effective.function.arguments.clone());
                    let dir = if self.last_tool_ok.load(Ordering::SeqCst) {
                        instructions::touched_dir(
                            &self.workspace,
                            &effective.function.name,
                            &ran_with,
                        )
                    } else {
                        None
                    };
                    (dir, effective.function.name == TOOL_SEARCH)
                };
                if let Some(dir) = dir
                    && !touched.contains(&dir)
                {
                    touched.push(dir);
                }
                // The schemas in a `tool_search` result are counted by their tools' sources.
                if searched && let Some(parts) = self.search_result_spans(&output) {
                    self.spans.insert(self.messages.len(), parts);
                }
                self.messages.push(ChatMessage::Tool {
                    tool_call_id: call.id.clone(),
                    content: output,
                });
            }
            self.clear_prefetched();
            self.load_instruction_files(&touched);
            // A plugin activated in this round offers its tools from the next call.
            if activates {
                (tools, tool_texts) = self.offered_tools();
                self.last_tools.clone_from(&tools);
            }
            // A safe point: every tool call has its result.
            self.checkpoint();
        }
        let message = format!("stopped after {max_steps} model calls (agents.max_steps)");
        self.emit(SessionEvent::TurnFailed {
            message: message.clone(),
        });
        self.outcome(message, false, total)
    }

    /// For a background session: waits, before the next model call, while the machine is busy
    /// or saturated. The call in flight has finished and the next one has not asked for a
    /// slot, so the wait holds none and spends no tokens; it ends when the machine is quiet or
    /// a person runs the session now. While it waits it is not attached to its model, so a
    /// model the user asks for can swap that one out without waiting for this agent.
    pub(super) async fn yield_to_user(&self, attached: &mut Option<LocalPresence>, holder: &str) {
        let Some(background) = &self.shared.background else {
            return;
        };
        let run_now = self.shared.controls.run_now();
        if run_now.released() || background.load.level() == Load::Quiet {
            return;
        }
        self.emit(SessionEvent::Paused {
            until_unix_ms: 0,
            reason: format!(
                "the machine is {}; waiting for it to be quiet",
                background.load.level().label()
            ),
            cause: PauseCause::Load,
        });
        *attached = None;
        crate::wait_for_quiet(&*background.load, run_now, None).await;
        *attached = self
            .local_models()
            .map(|local| local.attach(&self.model.model, holder));
        self.emit(SessionEvent::Resumed {});
    }

    /// Tokens the next call's prompt is expected to take, for a local model's reservation: the
    /// last call's exact count plus the messages added since, or on the first call every
    /// message and the tool specs; estimates carry a 1.25 margin, each message 8 tokens of chat
    /// template and the whole prompt 64.
    pub(super) fn prompt_estimate(&self, tools: &[(Category, String)]) -> u32 {
        let message = |message: &ChatMessage| {
            let text = serde_json::to_string(message).unwrap_or_default();
            8.0 + estimate_tokens(&text) as f64 * 1.25
        };
        let total = match self.last_call {
            Some((exact, covered)) if covered <= self.messages.len() => {
                exact as f64 + self.messages[covered..].iter().map(message).sum::<f64>()
            }
            _ => {
                let tools: u64 = tools.iter().map(|(_, text)| estimate_tokens(text)).sum();
                64.0 + self.messages.iter().map(message).sum::<f64>() + tools as f64 * 1.25
            }
        };
        total.ceil().min(f64::from(u32::MAX)) as u32
    }

    /// The client for one model call, with the slot held on a local model until the call ends.
    pub(super) async fn client(
        &self,
        holder: &str,
        prompt_tokens: u32,
        uncapped: bool,
    ) -> Result<(Arc<dyn ChatClient>, Option<LocalLease>), String> {
        self.client_for(&self.model, holder, Some(prompt_tokens), uncapped)
            .await
    }

    /// A client for one call to `model`, with the slot held on a local model until the lease
    /// is dropped. `prompt_tokens` is the call's estimated prompt; `None` reserves the whole
    /// context (a summary or a measurement outside the turn loop).
    pub(super) async fn client_for(
        &self,
        model: &ResolvedModel,
        holder: &str,
        prompt_tokens: Option<u32>,
        uncapped: bool,
    ) -> Result<(Arc<dyn ChatClient>, Option<LocalLease>), String> {
        let (endpoint, lease) = self
            .endpoint(model, holder, prompt_tokens, uncapped)
            .await?;
        let kind = &model.kind;
        let client = self
            .shared
            .clients
            .build(kind, &self.shared.http, endpoint)
            .ok_or_else(|| format!("no model client for endpoint kind {kind:?}"))?;
        Ok((client, lease))
    }

    /// Where to send one call to `model`, with the slot held on a local model. Waits for a slot
    /// when every one is busy, and loads the model first if it is not loaded.
    pub(super) async fn endpoint(
        &self,
        model: &ResolvedModel,
        holder: &str,
        prompt_tokens: Option<u32>,
        uncapped: bool,
    ) -> Result<(Endpoint, Option<LocalLease>), String> {
        if !model.local {
            return Ok((model.endpoint.clone(), None));
        }
        let local = self
            .shared
            .local
            .as_deref()
            .ok_or("local models are not available: no llama.cpp runtime is configured")?;
        // A session a person ran now, or that waited out its deferral, ranks as the user's.
        let priority =
            if self.shared.background.is_some() && !self.shared.controls.run_now().released() {
                Priority::Background
            } else {
                Priority::Interactive
            };
        let acquire = local.acquire_with(
            &model.model,
            holder,
            LocalRequest {
                priority,
                prompt_tokens,
                uncapped,
            },
        );
        tokio::pin!(acquire);
        // Says why the call waits, each time the reason changes, so clients can show it.
        let mut told = None;
        let lease = loop {
            tokio::select! {
                lease = &mut acquire => break lease,
                () = tokio::time::sleep(WAIT_REASON_POLL) => {
                    let reason = local.wait_reason(holder);
                    if reason != told {
                        if let Some(reason) = reason {
                            self.emit(SessionEvent::LocalModelWaiting {
                                agent_id: self.id.clone(),
                                model: model.model.clone(),
                                reason: wait_reason(reason),
                            });
                        }
                        told = reason;
                    }
                }
            }
        }
        .map_err(|error| format!("could not load local model {}: {error}", model.model))?;
        let endpoint = Endpoint {
            base_url: lease.base_url.clone(),
            api_key: None,
        };
        Ok((endpoint, Some(lease)))
    }

    /// Why the next call to this agent's model must not happen.
    pub(super) async fn budget_stop(&self) -> Option<String> {
        self.budget_stop_for(&self.model.spec).await
    }

    /// Why a call to the model `spec` must not happen: a used-up budget that blocks, or one
    /// that asks and the person declined. A budget that asks waits here for approval, once for
    /// the rest of the session's window; agents running in parallel raise one prompt, not one
    /// each (the check is repeated once the prompt lock is held).
    pub(super) async fn budget_stop_for(&self, spec: &str) -> Option<String> {
        let gate = self.shared.budget_gate.as_ref()?;
        let mut turn = None;
        loop {
            match gate.check(spec) {
                crate::BudgetVerdict::Go => return None,
                crate::BudgetVerdict::Blocked(message) => return Some(message),
                crate::BudgetVerdict::Ask { message, key } => {
                    if turn.is_none() {
                        turn = Some(self.shared.budget_ask.lock().await);
                        continue;
                    }
                    if !self
                        .approve_with("budget", &message, None, false, true)
                        .await
                    {
                        return Some(format!("{message} The call was not approved."));
                    }
                    gate.confirm(&key);
                }
            }
        }
    }

    pub(super) fn outcome(&self, text: String, ok: bool, usage: Usage) -> TurnOutcome {
        TurnOutcome {
            text,
            ok,
            paused: false,
            cost_usd: self.cost(&usage),
            usage,
        }
    }

    pub(super) fn cost(&self, usage: &Usage) -> Option<f64> {
        self.model.pricing.map(|pricing| pricing.cost_usd(usage))
    }

    pub(super) fn call_telemetry(
        &self,
        usage: &Usage,
        started: Instant,
        first_token: Option<Instant>,
        queue_wait: Option<std::time::Duration>,
    ) -> SessionEvent {
        let duration = started.elapsed();
        let (tokens_per_sec, measured) = generation_speed(
            usage.completion_tokens,
            duration,
            first_token.map(|first| first.elapsed()),
        );
        // The local runtime keeps the measured speed per model, which routing and the model
        // listing report; only a real generation window goes into it.
        if let Some(generating) = measured
            && let Some(local) = self.local_models()
        {
            local.record_throughput(&self.model.model, usage.completion_tokens, generating);
        }
        SessionEvent::ModelCallCompleted {
            model: self.model.spec.clone(),
            usage: token_usage(usage),
            duration_ms: millis(duration),
            queue_wait_ms: queue_wait.map(millis),
            time_to_first_token_ms: first_token.map(|first| millis(first - started)),
            tokens_per_sec,
            cost_usd: self.cost(usage),
        }
    }

    /// `update_state`: changes the session state. The result carries the new state, which is
    /// how the conversation records it: appended, so the cached prompt prefix is untouched.
    pub(super) fn update_state(&mut self, arguments: &str) -> Result<String, String> {
        let update: StateUpdate = parse(arguments)?;
        let max = self.shared.harness.state_max_chars as usize;
        self.state.apply(update, max);
        Ok(harness::state_result(&self.state))
    }

    /// Rewrites the harness paragraph of the system prompt for the model the agent has now. The
    /// `auto` settings follow the model (local or hosted), and the tools offered do at every
    /// turn; the prompt is rewritten here so it never describes tools that are not offered. The
    /// paragraph is in the tail of the system message, so the text before it is untouched.
    pub(super) fn refresh_harness_text(&mut self) {
        if !self.can_delegate || self.single_call {
            return;
        }
        let text = harness_prompt(
            &self.shared.harness,
            self.model.local,
            is_attended(&self.shared),
        );
        if text == self.harness_text {
            return;
        }
        self.harness_text = text;
        self.set_tail();
    }

    /// Rewrites the loader sentence of the system prompt: present while reactive tool loading
    /// is on for this agent, whatever its attendance or role (an unattended root and a subagent
    /// get it too), and taken out when the mode changes with the model. It is in the tail of
    /// the system message, after the harness paragraph.
    pub(super) fn refresh_loader_text(&mut self) {
        let text = self.loader_prompt();
        if text == self.loader_text {
            return;
        }
        self.loader_text = text;
        self.set_tail();
    }

    /// Takes the swappable tail off the system message: back to its fixed part.
    pub(super) fn cut_tail(&mut self) {
        let fixed = self.fixed_len;
        if let (Some(ChatMessage::System { content }), Some(spans)) =
            (self.messages.first_mut(), self.spans.get_mut(&0))
        {
            retail(content, spans, fixed, &[]);
        }
    }

    /// Puts the swappable tail at the end of the system message: the harness paragraph and the
    /// loader sentence, whichever are present. Both depend on the model, so they come last: a
    /// swap changes only the end of the message, and every byte before it (the base prompt, the
    /// indexes, the project's instructions) stays what a provider's prompt cache or a local
    /// runtime's slot already holds.
    pub(super) fn set_tail(&mut self) {
        let fixed = self.fixed_len;
        if let (Some(ChatMessage::System { content }), Some(spans)) =
            (self.messages.first_mut(), self.spans.get_mut(&0))
        {
            retail(
                content,
                spans,
                fixed,
                &[&self.harness_text, &self.loader_text],
            );
        }
    }

    /// The de-reference step: one small call that rewrites `text` into a standalone prompt and
    /// names the entities it mentions. Skipped when the conversation and the state hold nothing
    /// to resolve against. Any failure (no model, a budget that would ask, a stopped session, a
    /// timeout, an unreadable reply) leaves the message as it is; the turn goes on with the
    /// original. The call is bounded in tokens, seconds and history, whatever the settings say,
    /// and it is not pinned to the root's slot, so it does not evict the root's cached prompt.
    pub(super) async fn dereference(&self, text: &str) -> harness::Dereferenced {
        let settings = &self.shared.harness;
        let history = (settings.dereference_history_chars as usize).min(MAX_REWRITE_HISTORY_CHARS);
        let excerpt = harness::digest(&self.messages, history);
        if excerpt.is_empty() && self.state.is_empty() {
            return harness::Dereferenced::default();
        }
        let model = self.model.clone();
        if !self.budget_allows(&model.spec) {
            return harness::Dereferenced::default();
        }
        let request = harness::dereference_request(&self.state, &excerpt, text);
        let limit = std::time::Duration::from_secs(
            settings.dereference_timeout_s.clamp(1, MAX_REWRITE_SECONDS),
        );
        let budget = settings.dereference_max_tokens.clamp(1, MAX_REWRITE_TOKENS);
        // What the call reads, so a local runtime reserves room for it.
        let estimate: u64 = request
            .iter()
            .map(|message| {
                estimate_tokens(&serde_json::to_string(message).unwrap_or_default())
                    + compact::MESSAGE_FORMATTING
            })
            .sum();
        let prompt_tokens = u32::try_from(estimate).ok();
        let holder = self.holder();
        let started = Instant::now();
        let call = async {
            let (client, lease) = self
                .client_for(&model, &holder, prompt_tokens, false)
                .await?;
            let mut ignore = |_: &str| {};
            let result = client
                .stream(
                    ChatRequest {
                        model: &model.model,
                        messages: &request,
                        tools: &[],
                        slot: None,
                        max_tokens: Some(
                            lease
                                .as_ref()
                                .and_then(|lease| lease.max_tokens)
                                .map_or(budget, |reserved| reserved.min(budget)),
                        ),
                        // A rewrite needs no deliberation: no reasoning control is sent.
                        effort: None,
                    },
                    &mut ignore,
                )
                .await
                .map_err(|error| error.to_string());
            drop(lease);
            result
        };
        let mut stop = self.shared.controls.stop_signal();
        let outcome = tokio::select! {
            result = tokio::time::timeout(limit, call) => Some(result),
            Ok(_) = stop.wait_for(|stopped| *stopped) => None,
        };
        let completion = match outcome {
            None => return harness::Dereferenced::default(),
            Some(Ok(Ok(completion))) => completion,
            Some(Ok(Err(error))) => {
                tracing::debug!(%error, "the de-reference step failed; using the original message");
                return harness::Dereferenced::default();
            }
            Some(Err(_)) => {
                self.emit(SessionEvent::Notice {
                    message: format!(
                        "the message was not rewritten: the rewrite took longer than {} s",
                        limit.as_secs()
                    ),
                });
                return harness::Dereferenced::default();
            }
        };
        // The rewrite was a model call: it counts toward budgets like any other. (A call that
        // failed or timed out reported no usage, so it is not counted.)
        if let Some(usage) = &completion.usage {
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
        harness::parse_dereference(&completion.text, text).unwrap_or_default()
    }
}

/// A local runtime's wait reason as the protocol says it.
pub(super) fn wait_reason(
    reason: imperium_api::LocalWait,
) -> imperium_protocol::session::LocalWaitReason {
    use imperium_api::LocalWait as Wait;
    use imperium_protocol::session::LocalWaitReason as Reason;
    match reason {
        Wait::Loading => Reason::Loading,
        Wait::GrowingPool => Reason::GrowingPool,
        Wait::KvRoom => Reason::KvRoom,
        Wait::Slot => Reason::Slot,
        Wait::OtherModel => Reason::OtherModel,
        Wait::MakingRoom => Reason::MakingRoom,
        Wait::SavingContext => Reason::SavingContext,
        Wait::RestoringContext => Reason::RestoringContext,
    }
}

/// The window to plan with: a hosted model's table value; a local model's planned value; the
/// default when neither is known.
pub(crate) fn effective_window(table: Option<u64>, local: bool, reported: Option<u64>) -> u64 {
    match table {
        Some(window) if !local => window,
        _ => reported.or(table).unwrap_or(DEFAULT_WINDOW_TOKENS),
    }
}

/// How a model request ended.
pub(super) enum Streamed<T> {
    Done(T),
    /// The user steered the root: a pausable subagent stops its turn.
    Paused,
    /// The user stopped the session: the request is sent again on resume.
    Stopped,
}

/// [`signalled`] for an agent that may have no pause signal (then never).
pub(super) async fn maybe_signalled(signal: Option<&mut watch::Receiver<u64>>) {
    match signal {
        Some(signal) => signalled(signal).await,
        None => std::future::pending().await,
    }
}

/// Resolves when `signal` changes (the user steered the root agent); never when its sender
/// is gone.
pub(super) async fn signalled(signal: &mut watch::Receiver<u64>) {
    if signal.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

pub(super) fn add_usage(a: &Usage, b: &Usage) -> Usage {
    Usage {
        prompt_tokens: a.prompt_tokens + b.prompt_tokens,
        completion_tokens: a.completion_tokens + b.completion_tokens,
        cached_tokens: a.cached_tokens + b.cached_tokens,
    }
}

pub(super) fn add_cost(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
    }
}

pub(super) fn token_usage(usage: &Usage) -> TokenUsage {
    TokenUsage {
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        cached_tokens: usage.cached_tokens,
    }
}

/// A call's generation speed in tokens/s for its event, and the generation window to record as
/// the model's measured speed. The window runs from the first generated token (reasoning or
/// text) to the end. When it is under 100 ms, or nothing streamed (a tool call alone), the
/// event's speed falls back to the whole call's duration, prompt reading included; that is
/// shown but not recorded, since a short reply after a long prompt would understate the model.
pub(super) fn generation_speed(
    completion_tokens: u64,
    duration: std::time::Duration,
    after_first: Option<std::time::Duration>,
) -> (Option<f64>, Option<std::time::Duration>) {
    let measured = after_first.filter(|window| window.as_millis() >= 100);
    let generating = measured.unwrap_or(duration);
    let tokens_per_sec = (completion_tokens > 0 && !generating.is_zero())
        .then(|| completion_tokens as f64 / generating.as_secs_f64());
    (
        tokens_per_sec,
        measured.filter(|_| tokens_per_sec.is_some()),
    )
}

pub(super) fn millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
