use super::*;

impl Agent {
    pub(super) async fn spawn_agent(&mut self, arguments: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Args {
            brief: String,
            #[serde(default)]
            tier: Option<String>,
            #[serde(default)]
            model: Option<String>,
            #[serde(default)]
            location: Option<String>,
            #[serde(default)]
            tags: Vec<String>,
            #[serde(default)]
            role: Option<String>,
            #[serde(default)]
            single_call: bool,
            #[serde(default)]
            title: Option<String>,
            #[serde(default)]
            effort: Option<String>,
            #[serde(default)]
            access: Option<String>,
            #[serde(default)]
            knowledge: Option<String>,
            #[serde(default)]
            agent: Option<String>,
        }
        let args: Args = parse(arguments)?;
        if args.brief.trim().is_empty() {
            return Err("brief must not be empty".into());
        }
        // A named agent (an agent file): found before anything starts, so a wrong name costs
        // nothing. Its file may narrow the tools and pick the model; it never widens either.
        let named_def = match args
            .agent
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            Some(name) => {
                if args.single_call {
                    return Err(
                        "agent cannot be combined with single_call: a single_call subagent gets \
                         no tools and one reply, so there is nothing for a named agent to narrow"
                            .into(),
                    );
                }
                let extensions = &self.shared.extensions;
                match extensions.agent(name) {
                    Some(def) => Some(def.clone()),
                    None if extensions.agents.is_empty() => {
                        return Err(format!(
                            "there is no named agent {name:?}: no agent files are defined"
                        ));
                    }
                    None => {
                        let known: Vec<&str> = extensions
                            .agents
                            .iter()
                            .map(|agent| agent.name.as_str())
                            .collect();
                        return Err(format!(
                            "there is no named agent {name:?}; the named agents are: {}",
                            known.join(", ")
                        ));
                    }
                }
            }
            None => None,
        };
        let (named, named_notes) = match &named_def {
            Some(def) => {
                // The tools the delegator has now: a file narrows these, so it can never give a
                // subagent one that the delegator lacks.
                let parent: Vec<String> = self
                    .tool_specs()
                    .0
                    .into_iter()
                    .map(|tool| tool.name)
                    .collect();
                let (named, notes) = named_agent(def, &parent);
                (Some(named), notes)
            }
            None => (None, Vec::new()),
        };
        // A subagent kept as knowledge when its first turn succeeds, and routed to Emperor
        // (or `routing.knowledge_model`) by the `knowledge` tag.
        let knowledge = args
            .knowledge
            .as_deref()
            .map(str::trim)
            .filter(|topic| !topic.is_empty());
        if let Some(topic) = knowledge {
            if !self.shared.knowledge.enabled() {
                return Err("knowledge agents are turned off (knowledge.enabled)".into());
            }
            if topic.chars().count() > crate::knowledge::MAX_TOPIC_CHARS {
                return Err(format!(
                    "knowledge must be a topic of at most {} characters",
                    crate::knowledge::MAX_TOPIC_CHARS
                ));
            }
            if args.single_call || args.access.as_deref() == Some("write") {
                return Err(
                    "knowledge needs a read-only subagent with tools: a writer's worktree outlives \
                     a question and a single_call subagent's context is one reply"
                        .into(),
                );
            }
        }
        let mut tags = args.tags.clone();
        if knowledge.is_some() && !tags.iter().any(|tag| tag == "knowledge") {
            tags.push("knowledge".into());
        }
        let writes = match args.access.as_deref() {
            None | Some("read") => false,
            Some("write") => true,
            Some(other) => {
                return Err(format!(
                    "access must be \"read\" or \"write\", not {other:?}"
                ));
            }
        };
        if writes && self.workspace.permissions().mode() == PermissionMode::Plan {
            return Err(
                "the session is in plan mode, which only reads: a writer subagent edits files, \
                 so none was started. Start a read-only subagent, or ask the user to leave plan \
                 mode"
                    .into(),
            );
        }
        if writes && self.workspace.is_read_only() {
            return Err(
                "this session's folders are a read-only mount: a writer subagent edits files, \
                 so none was started. Start a read-only subagent instead"
                    .into(),
            );
        }
        let effort = args
            .effort
            .as_deref()
            .map(str::parse::<Effort>)
            .transpose()
            .map_err(|error| error.to_string())?;
        let tier = match args.tier.as_deref() {
            Some(tier) => tier.parse::<Tier>()?,
            None => Tier::default(),
        };
        let location = args
            .location
            .as_deref()
            .map(str::parse::<Location>)
            .transpose()?;
        // The model the file asks for, unless the call names one: `inherit` is the delegating
        // agent's own; anything else goes through the same routing as `model`.
        let file_model = match (
            &args.model,
            named_def.as_ref().and_then(|def| def.model.as_deref()),
        ) {
            (None, Some(model)) => Some(model),
            _ => None,
        };
        let inherit_model = file_model == Some("inherit");
        let file_model = file_model.filter(|_| !inherit_model);
        let routed = route_noted(
            &self.shared.catalog,
            &self.shared.routing,
            // Only the root delegates, so this is the session's current model.
            &self.model,
            TaskRequest {
                named: args.model.as_deref().or(file_model),
                tier,
                location,
                tags: &tags,
                session: Some(&self.shared.session_id),
                needs_tools: !args.single_call,
            },
            self.shared.local.as_deref(),
        );
        let mut route_notes = routed.notes;
        let mut model = routed.model;
        if inherit_model {
            model = self.model.clone();
        } else if let (Some(def), Some(wanted)) = (&named_def, file_model) {
            // The file's model must be one routing found and can use now; otherwise the agent
            // runs on the delegating agent's model, and the user is told.
            let found = self.shared.catalog.iter().any(|row| {
                (row.id == wanted || row.model.spec == wanted) && row.model.spec == model.spec
            });
            if !found {
                model = self.model.clone();
                route_notes.push(format!(
                    "the agent {} asks for the model {wanted}, which is not available, so it \
                     runs on {}",
                    def.name, model.spec
                ));
            }
        }
        route_notes.extend(named_notes);
        // A model whose budget is used up and blocks is not started; one that asks is
        // approved at the subagent's first call.
        if let Some(gate) = &self.shared.budget_gate
            && let crate::BudgetVerdict::Blocked(message) = gate.check(&model.spec)
        {
            return Err(message);
        }
        let task_id = self.shared.next_task_id();
        // A writer gets worktrees of its own, so writers running together never edit the same
        // files; a read-only subagent (and a writer outside a repository) shares this agent's
        // folders.
        let mut workspace = self.workspace.clone();
        let mut writer_note = None;
        let mut notice = None;
        if writes && !args.single_call {
            match self.workspace.writer_trees() {
                Some(trees) => {
                    let folders: Vec<PathBuf> = self
                        .workspace
                        .roots()
                        .into_iter()
                        .map(Path::to_path_buf)
                        .collect();
                    // Making the worktrees runs git: off the async threads.
                    let task = task_id.clone();
                    let started = tokio::task::spawn_blocking(move || trees.start(&task, &folders))
                        .await
                        .map_err(|error| {
                            format!("could not make the writer's worktree: {error}")
                        })?;
                    match started {
                        Ok(Some(start)) => {
                            workspace = self
                                .workspace
                                .with_writer_folders(&start)
                                .map_err(|error| format!("the writer's worktree: {error}"))?;
                            writer_note = Some(writer_prompt(&start));
                            if !start.notice.is_empty() {
                                notice = Some(start.notice);
                            }
                        }
                        Ok(None) => {
                            notice = Some(
                                "none of the project's folders can have a worktree (not in a \
                                 repository, or no commit yet), so the writer shares them; \
                                 writers running together can overwrite each other"
                                    .into(),
                            );
                        }
                        Err(error) => {
                            return Err(format!("could not give the writer a worktree: {error}"));
                        }
                    }
                }
                None => {
                    notice = Some(
                        "this session cannot make worktrees, so the writer shares the \
                         project's folders"
                            .into(),
                    );
                }
            }
        }
        let isolated = writer_note.is_some();
        self.emit(SessionEvent::TaskSpawned {
            task_id: task_id.clone(),
            brief: args.brief.clone(),
            model: model.spec.clone(),
            title: crate::title::task_title(args.title.as_deref(), &args.brief),
            at_ms: Some(imperium_api::time::unix_ms()),
        });
        // Why routing chose as it did, or passed models over: shown where the task starts.
        for note in route_notes {
            self.emit(SessionEvent::Notice {
                message: format!("{task_id}: {note}"),
            });
        }
        let role = args.role.unwrap_or_else(|| "leaf".into());
        // A failed task is retried on a stronger route, except a writer: its half-finished
        // edits stay in its worktree, and a second model would start over on top of them.
        let retry = (!isolated && !writes).then(|| Retry {
            tier,
            // Where the task was confined (the call's location, else a rule's) stays so.
            location: crate::model::effective_location(
                &self.shared.routing,
                TaskRequest {
                    tier,
                    location,
                    tags: &tags,
                    ..TaskRequest::default()
                },
            ),
            tags: tags.clone(),
            role: role.clone(),
            workspace: workspace.clone(),
            effort,
            single_call: args.single_call,
            named: named.clone(),
        });
        let mut agent = Agent::new_in(
            task_id.clone(),
            Arc::clone(&self.shared),
            model,
            false,
            role,
            workspace,
            writer_note.as_deref(),
        )
        .with_effort(effort);
        if let Some(named) = &named {
            // Kept with the subagent's record, so a resumed one is the same agent.
            self.shared.agents.set_named(&task_id, named.clone());
            agent = agent.with_named(named.clone());
        }
        if isolated {
            agent = agent.writing();
        }
        if writes && !args.single_call {
            agent.kind = AgentKind::Writer;
        }
        if args.single_call {
            agent = agent.single_call();
        }
        if let Some(topic) = knowledge {
            self.shared.knowledge.expect(&task_id, topic.to_string());
        }
        self.launch_subagent(agent.pausable(), args.brief, &task_id, retry);
        let mut started = format!("started {task_id}; call await_tasks to collect its report");
        if let Some(named) = &named {
            started.push_str(&format!("; it runs as the agent {}", named.name));
        }
        if knowledge.is_some() {
            started.push_str("; it is kept as a knowledge agent when it finishes");
        }
        if let Some(notice) = notice {
            self.emit(SessionEvent::Notice {
                message: format!("{task_id}: {notice}"),
            });
            started.push_str(&format!("\nNote: {notice}."));
        }
        Ok(started)
    }

    /// Starts `agent` on `brief` as its own task. (A plain function, so the async tool functions
    /// that call it do not contain the subagent's own future.)
    pub(super) fn launch_subagent(
        &mut self,
        agent: Agent,
        brief: String,
        task_id: &str,
        retry: Option<Retry>,
    ) {
        let spawned = json!({"task_id": task_id, "brief": brief, "model": agent.model.spec});
        // Each subagent is its own task, so subagents run in parallel across the runtime's threads.
        let handle = tokio::spawn(run_escalating(agent, brief, spawned, retry));
        self.tasks.insert(task_id.to_string(), handle);
        self.pausable_tasks.insert(task_id.to_string());
    }

    /// Starts a chunk or reduce subagent as its own task. `brief` resolves to the text it
    /// works from (a reduce step waits for its chunk reports first); `shown` is what events and
    /// hooks say instead. With a `gate`, the subagent waits for a permit before it starts.
    pub(super) fn start_task(
        &self,
        brief: impl Future<Output = String> + Send + 'static,
        shown: String,
        model: ResolvedModel,
        gate: Option<Arc<Semaphore>>,
    ) -> (String, JoinHandle<TaskReport>) {
        let task_id = self.shared.next_task_id();
        self.emit(SessionEvent::TaskSpawned {
            task_id: task_id.clone(),
            brief: shown.clone(),
            model: model.spec.clone(),
            title: crate::title::task_title(None, &shown),
            at_ms: Some(imperium_api::time::unix_ms()),
        });
        let mut agent = Agent::new(
            task_id.clone(),
            Arc::clone(&self.shared),
            model,
            false,
            "leaf".into(),
        );
        let spawned = json!({"task_id": task_id, "brief": &shown, "model": agent.model.spec});
        agent.shown = Some(shown);
        agent.can_ask = false;
        agent.kind = AgentKind::Chunk;
        let handle = tokio::spawn(async move {
            let _permit = match gate {
                Some(gate) => gate.acquire_owned().await.ok(),
                None => None,
            };
            let brief = brief.await;
            run_subagent(agent, brief, Some(spawned), false).await
        });
        (task_id, handle)
    }

    /// `model`'s context window without loading it: the table's, or for a local model the
    /// one its launch settings plan.
    pub(super) fn known_window(&self, model: &ResolvedModel) -> Option<u64> {
        model.context_window.or_else(|| {
            self.shared
                .local
                .as_deref()
                .filter(|_| model.local)
                .and_then(|local| local.planned_context(&model.model))
        })
    }

    /// The window of this agent's current model to guard against: the table's for a hosted
    /// model; for a local one the window its launch settings plan (no slot is taken and nothing
    /// is loaded); a hosted model without a table value is asked; else a conservative default.
    pub(super) async fn window(&self) -> u64 {
        let table = self.model.context_window;
        let reported = if self.model.local {
            self.local_models()
                .and_then(|local| local.planned_context(&self.model.model))
        } else if table.is_none() {
            // A hosted endpoint holds no lease, so asking it takes nothing from a call.
            match self
                .client_for(&self.model, &self.holder(), None, false)
                .await
            {
                Ok((client, _)) => self.shared.tokens.server_window(client.as_ref()).await,
                Err(_) => None,
            }
        } else {
            None
        };
        effective_window(table, self.model.local, reported)
    }

    /// Keeps an input that would fill the window out of the conversation: past the threshold
    /// the agent gets an artefact handle and an outline instead of the text. Only the root,
    /// which can chunk, is guarded.
    pub(super) fn guard_input(&self, text: String, window: u64) -> String {
        if !self.can_delegate
            || artefacts::estimate_tokens(&text)
                <= window.saturating_mul(ARTEFACT_THRESHOLD_PERCENT) / 100
        {
            return text;
        }
        let id = self.shared.artefacts.store(&text);
        handle_message(&id, &text)
    }

    /// Splits an artefact and gives each chunk to its own subagent, within the session's limit
    /// on parallel chunk agents. With `reduce`, one more subagent merges their reports and is
    /// the only task the caller collects.
    pub(super) fn delegate_chunks(&mut self, arguments: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Args {
            artefact_id: String,
            chunk_tokens: u64,
            brief: String,
            #[serde(default)]
            reduce: Option<String>,
        }
        let args: Args = parse(arguments)?;
        if args.brief.trim().is_empty() {
            return Err("brief must not be empty".into());
        }
        if args.chunk_tokens == 0 {
            return Err("chunk_tokens must be at least 1".into());
        }
        let Some(text) = self.shared.artefacts.get(&args.artefact_id) else {
            return Err(format!(
                "artefact {:?} does not exist (artefacts are lost when the daemon restarts)",
                args.artefact_id
            ));
        };
        let model = route(
            &self.shared.catalog,
            &self.shared.routing,
            // Only the root delegates, so this is the session's current model.
            &self.model,
            TaskRequest {
                named: None,
                tier: Tier::default(),
                location: None,
                tags: &[],
                session: Some(&self.shared.session_id),
                needs_tools: false,
            },
            self.shared.local.as_deref(),
        );
        let window = self.known_window(&model);
        // A chunk must leave the chunk agent room for its brief and its answer.
        let chunk_tokens = window.map_or(args.chunk_tokens, |window| {
            args.chunk_tokens
                .min((window * CHUNK_WINDOW_PERCENT / 100).max(1))
        });
        let note = if chunk_tokens < args.chunk_tokens {
            format!(" (chunk_tokens lowered to {chunk_tokens} to fit the chunk model's window)")
        } else {
            String::new()
        };
        let chunks = artefacts::split(&text, chunk_tokens);
        if chunks.len() > MAX_CHUNKS {
            return Err(format!(
                "that would make {} chunks and one call allows {MAX_CHUNKS}; raise chunk_tokens",
                chunks.len()
            ));
        }
        let used = self.shared.chunk_agents.load(Ordering::Relaxed);
        if used + chunks.len() > MAX_CHUNK_AGENTS {
            return Err(format!(
                "this session has started {used} chunk agents and may start {MAX_CHUNK_AGENTS} in \
                 all; this call would add {}",
                chunks.len()
            ));
        }
        self.shared
            .chunk_agents
            .fetch_add(chunks.len(), Ordering::Relaxed);
        let total = chunks.len();
        let artefact = args.artefact_id;
        let mut started = Vec::new();
        for (index, chunk) in chunks.into_iter().enumerate() {
            let number = index + 1;
            let shown = format!("{} (chunk {number} of {total} of {artefact})", args.brief);
            let brief = format!(
                "{}\n\nYou have chunk {number} of {total} of artefact {artefact}; report on this \
                 chunk only.\n\n--- chunk {number} of {total} ---\n{chunk}\n--- end of chunk ---",
                args.brief
            );
            started.push(self.start_task(
                std::future::ready(brief),
                shown,
                model.clone(),
                Some(Arc::clone(&self.shared.chunk_gate)),
            ));
        }
        let list = started
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let Some(reduce) = args.reduce.filter(|reduce| !reduce.trim().is_empty()) else {
            self.tasks.extend(started);
            return Ok(format!(
                "started {list}, one per chunk of {artefact} ({total} chunks){note}; call \
                 await_tasks to collect the reports"
            ));
        };
        let shown = format!("{reduce} (merge the reports on {artefact})");
        let intro = format!(
            "{reduce}\n\nThe reports below come from {total} subagents that each read one chunk \
             of artefact {artefact}."
        );
        // The merge agent reads every report, so each gets an equal share of sixty percent of
        // its window (four characters a token).
        let share = window.unwrap_or(DEFAULT_WINDOW_TOKENS) * 12 / 5;
        let cap = usize::try_from(share / u64::try_from(total).unwrap_or(1))
            .unwrap_or(usize::MAX)
            .max(200);
        let brief = async move {
            let mut reports = vec![intro];
            for (id, handle) in started {
                reports.push(match handle.await {
                    Ok(task) => format!(
                        "## {} ({})\n{}",
                        task.task_id,
                        if task.ok { "finished" } else { "failed" },
                        clip(&task.report, cap)
                    ),
                    Err(error) => format!("## {id}\nthe subagent crashed: {error}"),
                });
            }
            reports.join("\n\n")
        };
        let (reduce_id, handle) = self.start_task(brief, shown, model, None);
        self.tasks.insert(reduce_id.clone(), handle);
        Ok(format!(
            "started {list}, one per chunk of {artefact} ({total} chunks), and {reduce_id}, which \
             merges their reports{note}; call await_tasks with task_ids [\"{reduce_id}\"]"
        ))
    }

    /// Pauses the agent, with no model calls, until the user answers the batch from any client.
    pub(super) async fn ask_user(&self, arguments: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Args {
            questions: Vec<Question>,
        }
        let args: Args = parse(arguments)?;
        if args.questions.is_empty() {
            return Err("ask at least one question".into());
        }
        let (answers, timed_out) = self.pause_for_answers(&args.questions).await?;
        let mut lines = Vec::new();
        for (index, question) in args.questions.iter().enumerate() {
            let number = index + 1;
            let given = answers.get(index).map_or("", |answer| answer.trim());
            let answer = if given.is_empty() {
                format!(
                    "no answer; go with your recommendation: {}",
                    question.recommended
                )
            } else {
                given.to_string()
            };
            lines.push(format!(
                "Q{number}: {}\nA{number}: {answer}",
                question.question
            ));
        }
        let lines = lines.join("\n\n");
        Ok(if timed_out {
            format!(
                "Nobody answered within {} minutes, so your recommended answers were used.\n\n{lines}",
                ASK_TIMEOUT.as_secs() / 60
            )
        } else {
            lines
        })
    }

    /// Pauses the agent, with no model calls, until `questions` are answered from any client
    /// or [`ASK_TIMEOUT`] passes. The answers are one per question in order (blank for none);
    /// the flag says nobody answered in time.
    pub(super) async fn pause_for_answers(
        &self,
        questions: &[Question],
    ) -> Result<(Vec<String>, bool), String> {
        // Asleep until answered: the local runtime may swap this agent's model out meanwhile.
        let _waiting = self
            .local_models()
            .map(|local| local.waiting(&self.model.model, &self.holder()));
        let (request_id, answer) = self.shared.questions.request();
        self.emit(SessionEvent::QuestionsAsked {
            request_id,
            questions: questions.to_vec(),
        });
        let (answers, timed_out) = match tokio::time::timeout(ASK_TIMEOUT, answer).await {
            Ok(Ok(answers)) => (answers, false),
            Ok(Err(_)) => return Err("nobody can answer: the session is closing".into()),
            Err(_) => {
                self.shared.questions.cancel(request_id);
                (Vec::new(), true)
            }
        };
        self.emit(SessionEvent::QuestionsAnswered { request_id });
        Ok((answers, timed_out))
    }

    /// `request_clarification`: pauses for the user's answer about one missing parameter. The
    /// question goes through the same prompt as `ask_user`; unlike there, an empty answer or a
    /// timeout is not a recommendation to take, so the agent is told not to guess.
    pub(super) async fn request_clarification(&self, arguments: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Args {
            missing_parameter: String,
            #[serde(default)]
            options: Vec<String>,
            #[serde(default)]
            question: Option<String>,
            #[serde(default)]
            confidence: Option<f64>,
        }
        let args: Args = parse(arguments)?;
        let missing = args.missing_parameter.trim().to_string();
        if missing.is_empty() {
            return Err("name the missing parameter".into());
        }
        let options: Vec<String> = args
            .options
            .iter()
            .map(|option| option.trim().to_string())
            .filter(|option| !option.is_empty())
            .take(MAX_CLARIFICATION_OPTIONS)
            .collect();
        let question = args
            .question
            .map(|question| question.trim().to_string())
            .filter(|question| !question.is_empty())
            .unwrap_or_else(|| format!("I need one more detail before I go on: {missing}?"));
        let confidence = args
            .confidence
            .filter(|confidence| confidence.is_finite())
            .map(|confidence| confidence.clamp(0.0, 1.0));
        let prompt = Question {
            question,
            // Nothing is recommended: an answer taken from the options would be a guess, and a
            // runner that answers with recommendations (none is attended here) gets a blank.
            recommended: String::new(),
            options,
        };
        let (answers, timed_out) = self
            .pause_for_answers(std::slice::from_ref(&prompt))
            .await?;
        let given = answers.first().map_or("", |answer| answer.trim());
        let confidence_note = confidence
            .map(|confidence| format!(" (your confidence was {confidence:.2})"))
            .unwrap_or_default();
        Ok(if given.is_empty() {
            let why = if timed_out {
                format!(
                    "Nobody answered within {} minutes",
                    ASK_TIMEOUT.as_secs() / 60
                )
            } else {
                "The user gave no answer".to_string()
            };
            format!(
                "{why}, so {missing} is still unknown{confidence_note}. Do not guess it: finish \
                 what does not depend on it, then tell the user plainly what is waiting for it."
            )
        } else {
            format!(
                "Clarified{confidence_note}: {missing} = {given}\nTreat this as the user's answer; \
                 record it with update_state when later messages may refer to it."
            )
        })
    }

    /// `list_agents`: the session's subagents with their status and latest report.
    pub(super) fn list_agents(&self) -> String {
        let agents: Vec<_> = self
            .shared
            .agents
            .list()
            .into_iter()
            .filter(|agent| agent.agent_id != ROOT_AGENT)
            .collect();
        if agents.is_empty() {
            return "No subagents yet.".into();
        }
        agents
            .iter()
            .map(|agent| {
                let status = match agent.status {
                    AgentStatus::Running => "running",
                    AgentStatus::Idle => "idle",
                    AgentStatus::Failed => "failed",
                    AgentStatus::Interrupted => "interrupted",
                    AgentStatus::Paused => "paused",
                };
                let resumable = if agent.resumable && agent.status != AgentStatus::Running {
                    ", resumable"
                } else {
                    ""
                };
                let mut line = format!(
                    "- {} ({status}{resumable}; {}; {} turns, {} messages): {}",
                    agent.agent_id, agent.model, agent.turns, agent.messages, agent.title
                );
                if let Some(report) = &agent.last_report {
                    let report = truncate(report.lines().next().unwrap_or_default(), 200);
                    line.push_str(&format!("\n  last report: {report}"));
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// `resume_agent`: a follow-up for a finished or paused subagent, which continues with its
    /// earlier conversation. Collected with `await_tasks` like a new task. With `cancel`, ends
    /// a paused subagent instead.
    pub(super) fn resume_agent(&mut self, arguments: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Args {
            agent_id: String,
            #[serde(default)]
            text: String,
            #[serde(default)]
            cancel: bool,
        }
        let args: Args = parse(arguments)?;
        if self.tasks.contains_key(&args.agent_id) {
            return Err(format!(
                "{} has not been collected yet; call await_tasks first, or steer_agent / \
                 queue_agent while it works",
                args.agent_id
            ));
        }
        if args.cancel {
            cancel_subagent(&self.shared, &args.agent_id)?;
            return Ok(format!(
                "cancelled {}; its conversation is kept, so it can still be resumed later",
                args.agent_id
            ));
        }
        let (handle, model) = resume_subagent(&self.shared, &args.agent_id, args.text)?;
        self.tasks.insert(args.agent_id.clone(), handle);
        self.pausable_tasks.insert(args.agent_id.clone());
        Ok(format!(
            "resumed {} on {model} with its earlier conversation; call await_tasks to collect \
             its report",
            args.agent_id
        ))
    }

    /// `steer_agent` and `queue_agent`: a message for one of this agent's running subagents.
    pub(super) fn message_agent(&self, arguments: &str, steer: bool) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Args {
            agent_id: String,
            text: String,
        }
        let args: Args = parse(arguments)?;
        if !self.tasks.contains_key(&args.agent_id) {
            return Err(format!(
                "{:?} is not a subagent you started and have not collected",
                args.agent_id
            ));
        }
        let id = self.shared.inboxes.next_id();
        let message = self
            .shared
            .inboxes
            .send(&args.agent_id, id, args.text, steer, Vec::new())
            .map_err(|error| match error {
                QueueError::Empty => error.to_string(),
                _ => format!(
                    "{} has finished; collect its report with await_tasks",
                    args.agent_id
                ),
            })?;
        Ok(if steer {
            format!(
                "{} reaches {} after its current step, in the same turn",
                message.id, args.agent_id
            )
        } else {
            format!(
                "{} is queued for {}; it runs as a new turn after the current one, and the \
                 report covers it",
                message.id, args.agent_id
            )
        })
    }

    /// Waits for subagents without calling the model, then returns their reports.
    pub(super) async fn await_tasks(&mut self, arguments: &str) -> Result<String, String> {
        #[derive(Deserialize, Default)]
        struct Args {
            #[serde(default)]
            task_ids: Option<Vec<String>>,
        }
        let args: Args = parse(arguments)?;
        // Asleep until the reports arrive: the local runtime may swap this agent's model out
        // for its subagents' and restore it afterwards.
        let _waiting = self
            .local_models()
            .map(|local| local.waiting(&self.model.model, &self.holder()));
        let mut ids = args
            .task_ids
            .unwrap_or_else(|| self.tasks.keys().cloned().collect());
        ids.sort();
        if ids.is_empty() {
            return Ok("no tasks are running".into());
        }
        let mut reports = Vec::new();
        // A steering message from the user ends the wait: the root reads it next, with the
        // status of the subagents that paused for it.
        let mut steering = self.inbox.steering_signal();
        let mut not_collected = Vec::new();
        for id in ids {
            if !not_collected.is_empty() || self.inbox.has_steering() {
                if self.tasks.contains_key(&id) {
                    not_collected.push(id);
                }
                continue;
            }
            let Some(mut handle) = self.tasks.remove(&id) else {
                reports.push(format!("## {id}\nunknown or already collected"));
                continue;
            };
            let joined = tokio::select! {
                joined = &mut handle => Some(joined),
                () = signalled(&mut steering) => None,
            };
            let Some(joined) = joined else {
                self.tasks.insert(id.clone(), handle);
                not_collected.push(id);
                continue;
            };
            if let Ok(task) = &joined
                && task.paused
            {
                self.pausable_tasks.remove(&id);
            }
            reports.push(match joined {
                Ok(task) => {
                    let mut text = task.render(usize::MAX);
                    // A subagent that read a lot may be worth keeping for the tasks after it.
                    if task.ok
                        && !task.paused
                        && let Some(nudge) =
                            crate::knowledge::nudge(&self.shared, &task.task_id, task.report.len())
                    {
                        text.push_str(&format!("\n\n{nudge}"));
                    }
                    text
                }
                Err(error) => format!("## {id}\nthe subagent crashed: {error}"),
            });
        }
        if !not_collected.is_empty() {
            reports.push(format!(
                "Stopped waiting: the user sent a message, which follows. Not collected yet: {}.",
                not_collected.join(", ")
            ));
        }
        Ok(reports.join("\n\n"))
    }
}

/// What a writer subagent is told about its worktrees.
pub(super) fn writer_prompt(start: &imperium_tools::WriterStart) -> String {
    let mut text = format!(
        "You are a writer with worktrees of your own on branch {}: your folders are copies of \
         the project made from its last commit, other agents' edits do not reach them, and \
         yours do not reach the project's checkout. Edit freely there; do not switch branches \
         or touch other worktrees. When you stop, your changes are committed on the branch \
         for the delegator to merge, so you need not commit them.",
        start.branch
    );
    if !start.notice.is_empty() {
        text.push(' ');
        text.push_str(&start.notice);
        text.push('.');
    }
    text
}

/// `text` cut to `max` characters, with a marker when anything was dropped.
pub(super) fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}\n[report truncated to fit the merge window]")
}

/// Runs a subagent's turn on `text`, then any messages queued for it, and reports. A new task
/// runs `agent.spawn` hooks first (`spawned` is their payload); a resumed one is fitted to its
/// model's context window first, compacting a long conversation.
/// Whether a report calls for a stronger route: the task failed, or the subagent said it is not
/// confident (its report's first line is exactly `CONFIDENCE: LOW`). A paused task is neither.
pub(super) fn needs_escalation(report: &TaskReport) -> bool {
    if report.paused {
        return false;
    }
    !report.ok
        || report
            .report
            .lines()
            .next()
            .is_some_and(|line| line.trim() == "CONFIDENCE: LOW")
}

/// What a subagent needs to be retried on a stronger route when it fails.
pub(super) struct Retry {
    pub(super) tier: Tier,
    pub(super) location: Option<Location>,
    pub(super) tags: Vec<String>,
    pub(super) role: String,
    pub(super) workspace: Workspace,
    pub(super) effort: Option<Effort>,
    pub(super) single_call: bool,
    /// The agent file the task runs as, which the retry runs as too.
    pub(super) named: Option<crate::agents::NamedAgent>,
}

/// What a named agent runs with: its instructions, and its tools: those its file lists that
/// `parent` (the delegator's tools) also has. Claude Code's tool names are mapped to Imperium's.
/// Names that match nothing are left out, with a note for the user.
fn named_agent(
    def: &imperium_extensions::AgentDef,
    parent: &[String],
) -> (crate::agents::NamedAgent, Vec<String>) {
    let mut notes = Vec::new();
    let tools = def.tools.as_ref().map(|listed| {
        let mut allowed: Vec<String> = Vec::new();
        for item in listed {
            let found = imperium_extensions::agents::resolve_tool(item, parent);
            if found.is_empty() {
                notes.push(format!(
                    "the agent {}: the tool {item} is not one of the tools available here, so it \
                     was left out",
                    def.name
                ));
            }
            for name in found {
                if !allowed.iter().any(|tool| tool == name) {
                    allowed.push(name.to_string());
                }
            }
        }
        allowed
    });
    if tools.as_ref().is_some_and(Vec::is_empty) {
        notes.push(format!(
            "the agent {} has none of the tools its file lists, so it runs with no tools",
            def.name
        ));
    }
    let named = crate::agents::NamedAgent {
        name: def.name.clone(),
        prompt: def.prompt.clone(),
        tools,
    };
    (named, notes)
}

/// Runs a subagent and, when it fails, runs the task again on a stronger route: up to
/// `routing.escalation_attempts` times, each on the model [`escalate`] picks (a rule's
/// `escalate` list, else the tier above the failed model's), never the model that just failed.
/// The retry's brief carries the failed attempt's report. A model whose budget blocks it is not
/// tried; one whose budget asks is approved at its first call, like any subagent's. A paused
/// task is not a failure and is not retried.
pub(super) async fn run_escalating(
    agent: Agent,
    brief: String,
    spawned: serde_json::Value,
    retry: Option<Retry>,
) -> TaskReport {
    let shared = Arc::clone(&agent.shared);
    let id = agent.id.clone();
    // What routing falls back to when nothing fits. It is an escalation target only when it is
    // also a real table row; the fallback itself is never accepted as a route.
    let session_model = shared
        .agents
        .model(ROOT_AGENT)
        .unwrap_or_else(|| agent.model.clone());
    let mut failed_spec = agent.model.spec.clone();
    let mut failed_local = agent.model.local;
    let mut report = run_subagent(agent, brief.clone(), Some(spawned), false).await;
    let Some(retry) = retry else {
        return report;
    };
    let max = shared.routing.escalation_attempts;
    let mut attempt = 0;
    while needs_escalation(&report) && attempt < max {
        attempt += 1;
        let notice = |message: String| {
            shared.emitter.emit(&id, SessionEvent::Notice { message });
        };
        // On an initiative turn the daemon chooses the models, not the agent: no escalation.
        if shared
            .launcher
            .as_ref()
            .is_some_and(|launcher| launcher.model_choice_ignored())
        {
            break;
        }
        // A budget the user has not approved, or declined, at the model that just ran: stop
        // rather than spend on a stronger one.
        if let Some(gate) = &shared.budget_gate
            && let crate::BudgetVerdict::Ask { .. } = gate.check(&failed_spec)
        {
            notice(format!(
                "{id} on {failed_spec} needs a stronger route but its budget is waiting for approval, so it is not retried"
            ));
            break;
        }
        // The subagent answered but said it is unsure, as opposed to failing.
        let unsure = report.ok;
        let request = TaskRequest {
            named: None,
            tier: retry.tier,
            location: retry.location,
            tags: &retry.tags,
            session: Some(&shared.session_id),
            needs_tools: !retry.single_call,
        };
        let Some(routed) = escalate(
            &shared.catalog,
            &shared.routing,
            &session_model,
            request,
            &failed_spec,
            attempt,
            shared.local.as_deref(),
        ) else {
            notice(format!(
                "{id} on {failed_spec} needs a stronger route and none is available, so it is not retried"
            ));
            break;
        };
        let spec = routed.model.spec.clone();
        // An unsure answer from a local model never sends the task off the machine.
        if unsure && failed_local && !routed.model.local {
            notice(format!(
                "{id} on {failed_spec} was unsure, but the stronger route {spec} is hosted, so it is not retried"
            ));
            break;
        }
        if let Some(gate) = &shared.budget_gate
            && let crate::BudgetVerdict::Blocked(message) = gate.check(&spec)
        {
            notice(format!(
                "{id} on {failed_spec} needs a stronger route but was not retried on {spec}: {message}"
            ));
            break;
        }
        notice(format!(
            "{id} did not succeed on {failed_spec}; retrying on {spec} (escalation {attempt} of {max})"
        ));
        for note in routed.notes {
            notice(format!("{id}: {note}"));
        }
        let mut next = Agent::new_in(
            id.clone(),
            Arc::clone(&shared),
            routed.model,
            false,
            retry.role.clone(),
            retry.workspace.clone(),
            None,
        )
        .with_effort(retry.effort);
        if let Some(named) = &retry.named {
            next = next.with_named(named.clone());
        }
        if retry.single_call {
            next = next.single_call();
        }
        let text = format!(
            "{brief}\n\nAn earlier attempt at this task, by {failed_spec}, did not succeed or was not confident. Its report:\n{}",
            truncate(&report.report, REPORT_CHARS)
        );
        failed_spec = spec;
        failed_local = next.model.local;
        report = run_subagent(next.pausable(), text, None, false).await;
    }
    report
}

pub(super) async fn run_subagent(
    mut agent: Agent,
    text: String,
    spawned: Option<serde_json::Value>,
    resumed: bool,
) -> TaskReport {
    let shared = Arc::clone(&agent.shared);
    let id = agent.id.clone();
    let spec = agent.model.spec.clone();
    if let Some(payload) = spawned {
        shared
            .hook_in(&agent.workspace, HookEvent::AgentSpawn, None, payload)
            .await;
    }
    if resumed {
        match agent.fit_current().await {
            Ok(fit) if fit.compaction.is_some() => agent.checkpoint(),
            Ok(_) => {}
            Err(error) => agent.emit(SessionEvent::Notice {
                message: format!("{id}'s conversation could not be compacted: {error}"),
            }),
        }
    }
    let mut outcome = agent.run_turn(text, None).await;
    // Messages queued for the subagent run as further turns before it reports; the
    // report is the last turn's reply, with every turn's usage.
    while !outcome.paused
        && let Some(message) = agent.inbox().pop_or_close()
    {
        let next = agent
            .run_turn_with(message.text, message.attachments, None)
            .await;
        outcome = TurnOutcome {
            text: next.text,
            ok: next.ok,
            paused: next.paused,
            usage: add_usage(&outcome.usage, &next.usage),
            cost_usd: add_cost(outcome.cost_usd, next.cost_usd),
        };
    }
    if outcome.paused {
        // Paused for the user's steering message to the root: what still waited for it goes
        // back to the root in its status, and its state is kept to resume from.
        let waiting = agent.inbox().close_and_drain();
        if !waiting.is_empty() {
            outcome
                .text
                .push_str("\nMessages for it that were not delivered:");
            for message in waiting {
                outcome
                    .text
                    .push_str(&format!("\n- {}", truncate(&message.text, 300)));
            }
        }
        shared.inboxes.close(&id);
        agent.checkpoint();
        shared.emitter.emit(
            &id,
            SessionEvent::TaskPaused {
                task_id: id.clone(),
                progress: truncate(&outcome.text, EVENT_OUTPUT_CHARS),
                at_ms: Some(imperium_api::time::unix_ms()),
            },
        );
        return TaskReport {
            task_id: id,
            model: spec,
            ok: false,
            paused: true,
            report: truncate(&outcome.text, REPORT_CHARS),
        };
    }
    shared.inboxes.close(&id);
    agent.checkpoint();
    if agent.writer
        && let Some(trees) = shared.workspace.writer_trees()
    {
        // Whatever the writer changed is committed on its branch, and the report says how it
        // integrates; this runs git, so off the async threads.
        // A rule denying `git_commit`, or plan mode, keeps the changes in the worktree.
        let permissions = agent.workspace.permissions();
        let commit = permissions.mode() != PermissionMode::Plan
            && permissions.evaluate("git_commit", &Target::Other) != Some(RuleAction::Deny);
        let task = id.clone();
        let note = tokio::task::spawn_blocking(move || trees.finish(&task, commit))
            .await
            .unwrap_or_default();
        if !note.is_empty() {
            let room = REPORT_CHARS.saturating_sub(note.chars().count() + 2);
            outcome.text = format!("{note}\n\n{}", truncate(&outcome.text, room));
        }
    }
    shared
        .hook_in(
            &agent.workspace,
            HookEvent::AgentComplete,
            None,
            json!({"task_id": id, "ok": outcome.ok, "report": truncate(&outcome.text, REPORT_CHARS)}),
        )
        .await;
    // Finishing makes the subagent idle (or failed): its state stays, so it can be resumed.
    shared.emitter.emit(
        &id,
        SessionEvent::TaskFinished {
            task_id: id.clone(),
            ok: outcome.ok,
            report: truncate(&outcome.text, EVENT_OUTPUT_CHARS),
            usage: token_usage(&outcome.usage),
            cost_usd: outcome.cost_usd,
            at_ms: Some(imperium_api::time::unix_ms()),
        },
    );
    // A knowledge agent resumed for more work takes its new prefix, or is retired; a subagent
    // spawned with `knowledge` is kept now that its first turn succeeded.
    crate::knowledge::finished(&shared, &id, outcome.ok);
    if outcome.ok {
        crate::knowledge::promote_pending(&shared, &id);
    }
    TaskReport {
        task_id: id,
        model: spec,
        ok: outcome.ok,
        paused: false,
        report: truncate(&outcome.text, REPORT_CHARS),
    }
}

/// Ends a paused (or otherwise finished) subagent without resuming it: it reports as cancelled
/// and is no longer listed as paused. Its conversation stays, so it can still be resumed.
pub(crate) fn cancel_subagent(shared: &Arc<Shared>, agent_id: &str) -> Result<(), String> {
    shared.agents.cancel(agent_id)?;
    shared.emitter.emit(
        agent_id,
        SessionEvent::TaskFinished {
            task_id: agent_id.to_string(),
            ok: false,
            report: "Cancelled before it finished.".into(),
            usage: TokenUsage::default(),
            cost_usd: None,
            at_ms: Some(imperium_api::time::unix_ms()),
        },
    );
    Ok(())
}

/// Resumes a finished subagent with `text` as a new turn after its kept conversation, on the
/// model it last ran on (found by spec after a restart; the session's model when it is gone).
/// Returns its task and the model's spec.
pub(crate) fn resume_subagent(
    shared: &Arc<Shared>,
    agent_id: &str,
    text: String,
) -> Result<(JoinHandle<TaskReport>, String), String> {
    if text.trim().is_empty() {
        return Err("the follow-up must not be empty".into());
    }
    let before = shared.agents.claim(agent_id)?;
    // A writer goes back to its worktrees (kept for the session, and recorded for a restart);
    // one whose worktrees are gone is not resumed in the delegator's folders instead.
    let mut tree = None;
    if let Some(trees) = shared.workspace.writer_trees()
        && trees.is_writer(agent_id)
    {
        let found = trees.resume(agent_id).ok_or_else(|| {
            format!(
                "{agent_id} is a writer whose worktrees are gone, so it cannot be resumed (its \
                 branch keeps its committed work); start a new writer instead"
            )
        });
        let made = found.and_then(|start| {
            shared
                .workspace
                .with_writer_folders(&start)
                .map(|workspace| (workspace, start))
                .map_err(|error| format!("{agent_id}'s worktree: {error}"))
        });
        match made {
            Ok(made) => tree = Some(made),
            Err(error) => {
                shared.agents.release(agent_id, before.status);
                return Err(error);
            }
        }
    }
    let model = shared
        .agents
        .model(agent_id)
        .or_else(|| {
            shared
                .catalog
                .iter()
                .find(|entry| entry.model.spec == before.model)
                .map(|entry| entry.model.clone())
        })
        .or_else(|| shared.agents.model(ROOT_AGENT));
    let Some(model) = model else {
        shared.agents.release(agent_id, before.status);
        return Err("the session has no model to run the subagent on".into());
    };
    let spec = model.spec.clone();
    let note = tree.as_ref().map(|(_, start)| writer_prompt(start));
    let mut agent = Agent::new_in(
        agent_id.to_string(),
        Arc::clone(shared),
        model,
        false,
        before.role.clone(),
        tree.as_ref().map_or_else(
            || shared.workspace.clone(),
            |(workspace, _)| workspace.clone(),
        ),
        note.as_deref(),
    );
    // A named agent resumes as itself: its instructions and its narrowed tools.
    if let Some(named) = shared.agents.named(agent_id) {
        agent = agent.with_named(named);
    }
    if tree.is_some() {
        agent = agent.writing();
    }
    agent.restore(shared.agents.conversation(agent_id));
    agent.kind = shared.agents.kind(agent_id);
    if before.knowledge.is_some() {
        agent.can_ask = false;
        crate::knowledge::resumed(shared, agent_id);
    }
    let agent = agent.pausable();
    shared.emitter.emit(
        ROOT_AGENT,
        SessionEvent::TaskResumed {
            task_id: agent_id.to_string(),
            text: text.clone(),
            model: spec.clone(),
            at_ms: Some(imperium_api::time::unix_ms()),
        },
    );
    let text = match before.status {
        AgentStatus::Interrupted => format!(
            "<note>The daemon stopped while you were working on the previous message, so that \
             turn did not finish. Check what was already done, finish it, and then handle this \
             message.</note>\n\n{text}"
        ),
        AgentStatus::Paused => format!(
            "<note>You were paused while working on the previous message because the user \
             redirected the work. Check what was already done, then carry on as this message \
             says.</note>\n\n{text}"
        ),
        _ => text,
    };
    Ok((tokio::spawn(run_subagent(agent, text, None, true)), spec))
}
