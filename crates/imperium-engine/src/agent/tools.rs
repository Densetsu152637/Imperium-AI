use super::*;

impl Agent {
    /// The tool providers in `group` this agent is offered, in the order they were given:
    /// subagents leave out the ones kept to the root agent.
    pub(super) fn providers(
        &self,
        group: ToolGroup,
    ) -> impl Iterator<Item = &Arc<dyn ToolProvider>> {
        self.shared.tools.iter().filter(move |provider| {
            provider.group() == group
                && (self.can_delegate || provider.for_subagents())
                && (!self.writer || provider.for_writers())
        })
    }

    /// The provider that runs the tool called `name`: outside servers' tools first, as they
    /// are offered first.
    pub(super) fn provider_for(&self, name: &str) -> Option<Arc<dyn ToolProvider>> {
        let plugins = &self.shared.extensions.plugins;
        if plugins.handles(name) {
            return Some(Arc::clone(plugins) as Arc<dyn ToolProvider>);
        }
        self.providers(ToolGroup::External)
            .chain(self.providers(ToolGroup::Builtin))
            .find(|provider| provider.handles(name))
            .cloned()
    }

    /// The tools offered to the model, and their definitions by breakdown category. Activated
    /// plugins' tools join from the call after their activation.
    pub(super) fn offered_tools(&self) -> (Vec<ToolSpec>, Vec<(Category, String)>) {
        self.enforce_lock();
        let (specs, groups) = self.tool_specs();
        let texts = tool_texts(&specs, &groups);
        (specs, texts)
    }

    /// The tools offered to the model, and where the outside tools (MCP servers', activated
    /// plugins') sit among them.
    pub(super) fn tool_specs(&self) -> (Vec<ToolSpec>, Vec<(Category, Range<usize>)>) {
        if self.single_call {
            return (Vec::new(), Vec::new());
        }
        // The agent's permission profile only hides tools here (and refuses them again when
        // they are called): it never adds one.
        let profile = self.shared.workspace.profile();
        let mut tools = specs();
        if !profile.allows_shell() {
            tools.retain(|tool| !SHELL_TOOLS.contains(&tool.name.as_str()));
        }
        let mcp_start = tools.len();
        for provider in self.providers(ToolGroup::External) {
            tools.extend(
                provider
                    .specs()
                    .into_iter()
                    .filter(|tool| profile.allows_mcp(provider.server_of(&tool.name).as_deref())),
            );
        }
        let mcp = mcp_start..tools.len();
        let lazy = &self.shared.extensions.plugins;
        tools.extend(lazy.specs().into_iter().filter(|tool| {
            lazy.plugin_of(&tool.name)
                .is_some_and(|plugin| profile.allows_plugin(&plugin))
        }));
        let plugins = mcp.end..tools.len();
        tools.push(update_plan_spec());
        if !self.shared.extensions.skills.is_empty() {
            tools.push(load_skill_spec());
        }
        if lazy
            .plugins()
            .iter()
            .any(|plugin| !lazy.is_active(&plugin.name) && profile.allows_plugin(&plugin.name))
        {
            tools.push(activate_plugin_spec());
        }
        if self.shared.context.is_some() {
            tools.push(context_load_spec());
        }
        if self.can_delegate && self.shared.launcher.is_some() {
            tools.push(start_session_spec());
        }
        if profile.allows_provider_tools() {
            for provider in self.providers(ToolGroup::Builtin) {
                tools.extend(provider.specs());
            }
        }
        if self.shared.memory.is_some() {
            tools.extend([memory_search_spec(), memory_read_spec()]);
            if self.can_delegate {
                tools.push(memory_remember_spec());
            }
        }
        if self.can_delegate {
            tools.extend([
                spawn_agent_spec(),
                await_tasks_spec(),
                steer_agent_spec(),
                queue_agent_spec(),
                list_agents_spec(),
                resume_agent_spec(),
            ]);
            if self.shared.knowledge.enabled() {
                tools.push(keep_as_knowledge_spec());
            }
            tools.extend([delegate_chunks_spec(), ask_user_spec()]);
            if self.harness_on(self.shared.harness.clarify) {
                tools.push(request_clarification_spec());
            }
            if self.harness_on(self.shared.harness.state) {
                tools.push(update_state_spec());
            }
        }
        // Questions are offered while the session has a knowledge agent to ask.
        if self.can_ask
            && self.fork.is_none()
            && self.shared.knowledge.enabled()
            && !self.shared.agents.knowledge_cards().is_empty()
        {
            tools.extend([list_knowledge_agents_spec(), ask_agent_spec()]);
        }
        let groups = vec![(Category::ToolsMcp, mcp), (Category::ToolsPlugins, plugins)];
        // A named agent's file narrows what it is offered, never widens it.
        match self.named.as_ref().and_then(|named| named.tools.as_deref()) {
            Some(allowed) => narrow_tools(tools, groups, allowed),
            None => (tools, groups),
        }
    }

    /// Why a call to `name` is refused when this agent runs as a named agent whose file lists
    /// tools and `name` is not one of them. Checked where calls run, as the offered list is.
    pub(super) fn named_refusal(&self, name: &str) -> Option<String> {
        let named = self.named.as_ref()?;
        let allowed = named.tools.as_deref()?;
        (!allowed.iter().any(|tool| tool == name)).then(|| {
            format!(
                "{name} is not one of the tools of the agent {} ({}); it was not run",
                named.name,
                if allowed.is_empty() {
                    "none".to_string()
                } else {
                    allowed.join(", ")
                }
            )
        })
    }

    /// Puts the instruction files of the directories the round's file tools touched into the
    /// conversation, after the round's tool results (so every call keeps its result next to
    /// it). Does nothing when the workflow is turned off.
    pub(super) fn load_instruction_files(&mut self, touched: &[PathBuf]) {
        let Some(context) = self.shared.context.clone() else {
            return;
        };
        if touched.is_empty() || self.single_call {
            return;
        }
        let tracker = self
            .instructions
            .get_or_insert_with(|| instructions::Tracker::from_messages(&self.messages));
        let loaded = instructions::discover(&self.workspace, &context, touched, tracker);
        if loaded.is_empty() {
            return;
        }
        let blocks: Vec<String> = loaded.iter().map(|file| file.block.clone()).collect();
        let content = instructions::wrap(&blocks);
        self.spans.insert(
            self.messages.len(),
            vec![(Category::Manifest, 0..content.len())],
        );
        self.messages.push(ChatMessage::user(content));
        self.emit(SessionEvent::ContextLoaded {
            references: loaded
                .into_iter()
                .map(|file| format!("{}{}", instructions::REFERENCE_PREFIX, file.path))
                .collect(),
        });
    }

    /// Runs `tool.before` hooks: the call may run, be blocked (with the message for the model),
    /// or be rewritten. A call a hook already rewrote is not rewritten again, so hooks cannot
    /// loop; they can still block it.
    pub(super) async fn before_tool(&self, tool: &str, arguments: &str) -> Hooked {
        let original = tool_input(arguments);
        let payload = json!({"agent_id": self.id, "tool_input": original});
        let rewritten = lock(&self.rewritten_from).is_some();
        if rewritten {
            return match self
                .shared
                .hook_in(&self.workspace, HookEvent::ToolBefore, Some(tool), payload)
                .await
            {
                Verdict::Allow => Hooked::Run,
                Verdict::Block(reason) => {
                    Hooked::Blocked(format!("A hook blocked this call: {reason}"))
                }
            };
        }
        let outcome = self
            .shared
            .hook_rewriting_in(&self.workspace, HookEvent::ToolBefore, Some(tool), payload)
            .await;
        match outcome.verdict {
            Verdict::Block(reason) => {
                Hooked::Blocked(format!("A hook blocked this call: {reason}"))
            }
            Verdict::Allow => match outcome.updated_input {
                Some(new) if new != original => Hooked::Rewrite(new.to_string()),
                _ => Hooked::Run,
            },
        }
    }

    /// The summary of a call, with what the model asked when a hook rewrote the call.
    pub(super) fn annotate(&self, summary: String) -> String {
        match lock(&self.rewritten_from).as_deref() {
            Some(original) => {
                format!("{summary} [rewritten by a hook; the model asked with {original}]")
            }
            None => summary,
        }
    }

    pub(super) async fn after_tool(&self, tool: &str, arguments: &str, ok: bool, output: &str) {
        let payload = json!({
            "agent_id": self.id,
            "tool_input": tool_input(arguments),
            "ok": ok,
            "tool_output": truncate(output, REPORT_CHARS),
        });
        self.shared
            .hook_in(&self.workspace, HookEvent::ToolAfter, Some(tool), payload)
            .await;
    }

    /// Under `required-before-action`, the message refusing a write or command while required
    /// context is unloaded.
    pub(super) fn missing_context(&self) -> Option<String> {
        if self.shared.enforce != Enforce::RequiredBeforeAction {
            return None;
        }
        let unloaded = lock(&self.unloaded);
        (!unloaded.is_empty()).then(|| {
            format!(
                "error: load the required context first with context_load: {}",
                unloaded.join(", ")
            )
        })
    }

    pub(super) fn context_load(&self, arguments: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Args {
            references: Vec<String>,
        }
        let args: Args = parse(arguments)?;
        let index = self
            .shared
            .context
            .as_ref()
            .ok_or("no workflow is configured")?;
        let mut out = String::new();
        let mut loaded = Vec::new();
        for reference in &args.references {
            match index.load(reference) {
                Ok(text) => {
                    out.push_str(&format!(
                        "<loaded reference=\"{reference}\">\n{text}\n</loaded>\n"
                    ));
                    loaded.push(reference.clone());
                }
                Err(error) => out.push_str(&format!("could not load {reference}: {error}\n")),
            }
        }
        lock(&self.unloaded).retain(|reference| !loaded.contains(reference));
        if !loaded.is_empty() {
            self.emit(SessionEvent::ContextLoaded { references: loaded });
        }
        Ok(out)
    }

    /// Hands a brief to the daemon as a new session (project agents only).
    pub(super) async fn start_session(&self, arguments: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Args {
            brief: String,
            #[serde(default)]
            model: Option<String>,
            #[serde(default)]
            reason: Option<String>,
            #[serde(default)]
            source: Option<String>,
        }
        let args: Args = parse(arguments)?;
        if args.brief.trim().is_empty() {
            return Err("brief must not be empty".into());
        }
        let launcher = self
            .shared
            .launcher
            .as_ref()
            .ok_or("this session cannot start sessions")?;
        let id = launcher
            .launch_request(crate::LaunchRequest {
                brief: &args.brief,
                model: args.model.as_deref(),
                reason: args
                    .reason
                    .as_deref()
                    .filter(|text| !text.trim().is_empty()),
                source: args
                    .source
                    .as_deref()
                    .filter(|text| !text.trim().is_empty()),
            })
            .await?;
        Ok(format!(
            "started session {id}; its report will arrive as a message when it finishes"
        ))
    }

    pub(super) fn load_skill(&self, arguments: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Args {
            name: String,
            #[serde(default)]
            file: Option<String>,
        }
        let args: Args = parse(arguments)?;
        let skill = self
            .shared
            .extensions
            .skills
            .iter()
            .find(|skill| skill.name == args.name)
            .ok_or_else(|| format!("no skill named {:?}", args.name))?;
        let file = args.file.as_deref().filter(|file| !file.is_empty());
        if file.is_some() {
            return imperium_extensions::skills::load(skill, file);
        }
        // Each skill's instructions enter a conversation once.
        let key = (self.id.clone(), skill.name.clone());
        if skill.eager || lock(&self.shared.loaded_skills).contains(&key) {
            return Ok(format!(
                "The {} skill is already loaded: its instructions are earlier in this \
                 conversation. Follow them.",
                skill.name
            ));
        }
        let text = imperium_extensions::skills::load(skill, None)?;
        lock(&self.shared.loaded_skills).insert(key);
        self.emit(SessionEvent::ExtensionLoaded {
            kind: ExtensionKind::Skill,
            name: skill.name.clone(),
            tools: Vec::new(),
        });
        Ok(text)
    }

    /// Starts a plugin's MCP servers for the session; their tools are offered from the next
    /// model call.
    pub(super) async fn activate_plugin(&self, arguments: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Args {
            name: String,
        }
        let args: Args = parse(arguments)?;
        if let Some(why) = self
            .shared
            .workspace
            .profile()
            .refusal(&ToolSource::Plugin { plugin: &args.name })
        {
            return Err(format!(
                "This agent's permission profile does not allow this: {why}."
            ));
        }
        match self.shared.extensions.plugins.activate(&args.name).await? {
            Activation::AlreadyActive { tools } => Ok(format!(
                "Plugin {} is already active. Its tools: {}",
                args.name,
                tools.join(", ")
            )),
            Activation::Started { tools, failed } => {
                self.emit(SessionEvent::ExtensionLoaded {
                    kind: ExtensionKind::Plugin,
                    name: args.name.clone(),
                    tools: tools.clone(),
                });
                let mut text = format!(
                    "Plugin {} is active. Its tools, offered from now on: {}",
                    args.name,
                    if tools.is_empty() {
                        "none".to_string()
                    } else {
                        tools.join(", ")
                    }
                );
                if !failed.is_empty() {
                    text.push_str(&format!(
                        "\nThese servers did not start: {}",
                        failed.join("; ")
                    ));
                }
                Ok(text)
            }
        }
    }

    /// Asks the user to approve a call to `tool`, unless they allowed it for the session (and
    /// the session is not in ask-every-time mode).
    pub(super) async fn approve(&self, tool: &str, summary: &str) -> bool {
        let sticky = self.workspace.permissions().mode() != PermissionMode::Ask;
        self.approve_with(tool, summary, None, sticky, false).await
    }

    /// Like [`Agent::approve`], offering `suggested` as the rule "always allow" would add. With
    /// `sticky` false (ask-every-time mode, ask rules, the safety floor) a tool allowed for the
    /// session still asks. Tools an unattended session was started with pre-approved run
    /// without asking, except a call the safety floor covers (`floored`): that one waits for
    /// the user's approval like any other.
    pub(super) async fn approve_with(
        &self,
        tool: &str,
        summary: &str,
        suggested: Option<PermissionRule>,
        sticky: bool,
        floored: bool,
    ) -> bool {
        if (!floored && self.shared.pre_approved.contains(tool))
            || (sticky && lock(&self.shared.allowed_tools).contains(tool))
        {
            return true;
        }
        let (request_id, answer) = self.shared.broker.request();
        // A bridged call's request is marked: it is the user's to answer, not the harness's.
        let summary = if self.bridged.load(Ordering::SeqCst) {
            format!("{}{summary}", crate::bridge::SUMMARY_PREFIX)
        } else {
            summary.to_string()
        };
        let summary = summary.as_str();
        self.emit(SessionEvent::PermissionRequested {
            request_id,
            tool: tool.to_string(),
            summary: summary.to_string(),
            suggested_rule: suggested.map(wire_rule),
        });
        // Observers only, started detached: a hook cannot answer the request, so it cannot widen
        // what is allowed, and it never delays the prompt.
        self.shared.observe_in(
            &self.workspace,
            HookEvent::PermissionRequested,
            Some(tool),
            json!({"agent_id": self.id, "tool": tool, "summary": summary}),
        );
        // Asleep until the person answers: the local runtime may swap the model out meanwhile.
        let waiting = self
            .local_models()
            .map(|local| local.waiting(&self.model.model, &self.holder()));
        let decision = answer.await.unwrap_or(PermissionDecision::Deny);
        drop(waiting);
        match decision {
            PermissionDecision::Allow => true,
            PermissionDecision::AllowForSession => {
                lock(&self.shared.allowed_tools).insert(tool.to_string());
                true
            }
            PermissionDecision::Deny => false,
        }
    }

    /// Runs a provider's tool: its check, then `tool.before` hooks, the required-context rule,
    /// and approval, exactly as for the built-in tools.
    pub(super) async fn run_provider(
        &self,
        provider: &dyn ToolProvider,
        call: &ToolCall,
    ) -> String {
        let name = call.function.name.as_str();
        let arguments = call.function.arguments.as_str();
        let finish = |ok: bool, output: String| self.tool_finished(call, name, ok, output, None);
        let check = match provider.check(name, arguments) {
            Ok(check) => check,
            Err(error) => return finish(false, format!("error: {error}")),
        };
        let summary = self.annotate(check.summary.clone());
        // A call that runs a command is judged as `run_command` on that command.
        let (rule_tool, target, kind, floor_override) = match &check.command {
            Some(command) => (
                "run_command",
                Target::Command(command.clone()),
                CallKind::Execute,
                None,
            ),
            // A call approved afresh each time is treated like the safety floor: it asks in every
            // mode, whatever allow rules, session approvals or pre-approval say.
            None => (
                name,
                Target::Other,
                CallKind::Other { asks: check.ask },
                Some(provider.always_asks(name)),
            ),
        };
        let Judgement {
            decision, floored, ..
        } = self
            .workspace
            .permissions()
            .judge(rule_tool, &target, kind, floor_override);
        if let Decision::Refuse(why) = decision {
            return finish(false, refused(why, &summary));
        }
        if matches!(decision, Decision::Ask { .. }) && self.fork.is_some() {
            return finish(false, fork::ask_refusal(&summary));
        }
        match self.before_tool(name, arguments).await {
            Hooked::Run => {}
            Hooked::Blocked(blocked) => return finish(false, blocked),
            Hooked::Rewrite(new) => {
                *lock(&self.pending_rewrite) = Some(new);
                return String::new();
            }
        }
        if check.needs_context
            && let Some(missing) = self.missing_context()
        {
            return finish(false, missing);
        }
        if let Decision::Ask { sticky } = decision
            && !self
                .approve_with(
                    rule_tool,
                    &summary,
                    suggest(rule_tool, &target).filter(|_| !(floored && check.command.is_none())),
                    sticky,
                    floored,
                )
                .await
        {
            return finish(false, "The user denied this action.".into());
        }
        let started = self.tool_started(call, name, summary);
        let emit = |event: ToolEvent| {
            if let Some(event) = tool_event(event) {
                self.emit(event);
            }
        };
        let sampler = McpSampler { agent: self };
        let context = ToolContext {
            root: self.workspace.root(),
            emit: &emit,
            sampler: Some(&sampler),
        };
        let (ok, output) = match provider.call(&context, name, arguments).await {
            Ok(output) => (true, output),
            Err(error) => (false, format!("error: {error}")),
        };
        self.after_tool(name, arguments, ok, &output).await;
        self.tool_finished(call, name, ok, output, Some(started))
    }

    /// Announces a tool call that is about to run; returns when it started.
    pub(super) fn tool_started(&self, call: &ToolCall, tool: &str, summary: String) -> Instant {
        self.emit(SessionEvent::ToolStarted {
            call_id: call.id.clone(),
            tool: tool.to_string(),
            summary,
            arguments: Some(truncate(&call.function.arguments, EVENT_TOOL_CHARS)),
        });
        Instant::now()
    }

    /// Announces a tool call's end and passes its output through; `started` is absent for a
    /// call that never ran (denied, invalid or blocked).
    pub(super) fn tool_finished(
        &self,
        call: &ToolCall,
        tool: &str,
        ok: bool,
        output: String,
        started: Option<Instant>,
    ) -> String {
        self.last_tool_ok.store(ok, Ordering::SeqCst);
        if ok && let Some(fork) = &self.fork {
            fork.note_tool(tool);
        }
        self.emit(SessionEvent::ToolFinished {
            call_id: call.id.clone(),
            ok,
            output: truncate(&output, EVENT_TOOL_CHARS),
            tool: Some(tool.to_string()),
            duration_ms: started.map(|started| millis(started.elapsed())),
        });
        output
    }

    /// A session limited to local files runs no commands: the background commands it started
    /// before the lock are stopped (`command_stop` is refused with the other shell tools).
    pub(super) fn enforce_lock(&self) {
        let workspace = &self.shared.workspace;
        if workspace.profile().local_files_only() && workspace.processes().running() > 0 {
            workspace.processes().stop_all();
        }
    }

    /// Why the agent's permission profile refuses a call to the tool `name`, if it does. The
    /// tool list hides these tools, but a conversation can hold a stale list (the profile
    /// changed, or the model remembers a tool), so every call is checked again here.
    pub(super) fn profile_refusal(&self, name: &str) -> Option<String> {
        let profile = self.shared.workspace.profile();
        if profile.is_unrestricted() {
            return None;
        }
        if SHELL_TOOLS.contains(&name) {
            return profile.refusal(&ToolSource::Shell);
        }
        if name == ACTIVATE_PLUGIN {
            return profile.refusal(&ToolSource::PluginActivation);
        }
        if let Some(plugin) = self.shared.extensions.plugins.plugin_of(name) {
            return profile.refusal(&ToolSource::Plugin { plugin: &plugin });
        }
        let provider = self.provider_for(name)?;
        match provider.group() {
            ToolGroup::External => {
                let server = provider.server_of(name);
                profile.refusal(&ToolSource::Mcp {
                    server: server.as_deref(),
                })
            }
            ToolGroup::Builtin => profile.refusal(&ToolSource::Provider),
        }
    }

    /// Runs one tool call and returns the text the model sees.
    /// Runs one tool call. When a `tool.before` hook rewrites it, the rewritten call is run
    /// again from the start, through every check the model's own call goes through (the
    /// launcher's tool limits, the agent's permission profile, the tool's own validation, the
    /// permission rules and the safety floor, then approval), and the user is shown both
    /// versions. It stays the same tool: a hook cannot turn a call into another tool's.
    pub(super) async fn run_tool(&mut self, call: &ToolCall) -> String {
        *lock(&self.pending_rewrite) = None;
        *lock(&self.ran_rewritten) = None;
        let output = self.run_tool_once(call).await;
        let Some(arguments) = lock(&self.pending_rewrite).take() else {
            return output;
        };
        let original = shown(&call.function.arguments);
        self.emit(SessionEvent::Notice {
            message: format!(
                "A hook rewrote the {} call.\nBefore: {original}\nAfter: {}",
                call.function.name,
                shown(&arguments)
            ),
        });
        let mut rewritten = call.clone();
        rewritten.function.arguments = arguments;
        *lock(&self.rewritten_from) = Some(original);
        *lock(&self.ran_rewritten) = Some(rewritten.function.arguments.clone());
        let output = self.run_tool_once(&rewritten).await;
        *lock(&self.rewritten_from) = None;
        *lock(&self.pending_rewrite) = None;
        format!("(A hook rewrote this call's arguments before it ran.)\n{output}")
    }

    pub(super) async fn run_tool_once(&mut self, call: &ToolCall) -> String {
        let name = call.function.name.as_str();
        let arguments = call.function.arguments.as_str();
        // A question's fork may read and search, and nothing else: checked here, where a call
        // is dispatched, so the tool list it was given (and its cached prefix) stays as it was.
        if self.fork.is_some()
            && let Some(why) = fork::refusal(name)
        {
            return self.tool_finished(call, name, false, why, None);
        }
        // A turn the daemon limits (an initiative review) may use only the tools it lists.
        if let Some(why) = self
            .shared
            .launcher
            .as_ref()
            .and_then(|launcher| launcher.restricted(name))
        {
            return self.tool_finished(call, name, false, format!("error: {why}"), None);
        }
        if let Some(why) = self.named_refusal(name) {
            return self.tool_finished(call, name, false, format!("error: {why}"), None);
        }
        self.enforce_lock();
        if let Some(why) = self.profile_refusal(name) {
            return self.tool_finished(
                call,
                name,
                false,
                format!(
                    "This agent's permission profile does not allow this: {why}. {name} was not run."
                ),
                None,
            );
        }
        let tool = match name {
            "update_plan" => AgentTool::UpdatePlan,
            "spawn_agent" if self.can_delegate => AgentTool::SpawnAgent,
            "await_tasks" if self.can_delegate => AgentTool::AwaitTasks,
            "steer_agent" if self.can_delegate => AgentTool::SteerAgent,
            "queue_agent" if self.can_delegate => AgentTool::QueueAgent,
            "list_agents" if self.can_delegate => AgentTool::ListAgents,
            "resume_agent" if self.can_delegate => AgentTool::ResumeAgent,
            "keep_as_knowledge" if self.can_delegate && self.shared.knowledge.enabled() => {
                AgentTool::KeepAsKnowledge
            }
            "ask_agent" if self.shared.knowledge.enabled() => AgentTool::AskAgent,
            "list_knowledge_agents" if self.shared.knowledge.enabled() => {
                AgentTool::ListKnowledgeAgents
            }
            "delegate_chunks" if self.can_delegate => AgentTool::DelegateChunks,
            "ask_user" if self.can_delegate => AgentTool::AskUser,
            "request_clarification"
                if self.can_delegate && self.harness_on(self.shared.harness.clarify) =>
            {
                AgentTool::RequestClarification
            }
            "update_state" if self.can_delegate && self.harness_on(self.shared.harness.state) => {
                AgentTool::UpdateState
            }
            LOAD_SKILL if !self.shared.extensions.skills.is_empty() => AgentTool::LoadSkill,
            ACTIVATE_PLUGIN if !self.shared.extensions.plugins.is_empty() => {
                AgentTool::ActivatePlugin
            }
            "context_load" if self.shared.context.is_some() => AgentTool::ContextLoad,
            "start_session" if self.can_delegate && self.shared.launcher.is_some() => {
                AgentTool::StartSession
            }
            "memory_search" if self.shared.memory.is_some() => AgentTool::MemorySearch,
            "memory_read" if self.shared.memory.is_some() => AgentTool::MemoryRead,
            "memory_remember" if self.shared.memory.is_some() && self.can_delegate => {
                AgentTool::MemoryRemember
            }
            _ => {
                return match self.provider_for(name) {
                    Some(provider) => self.run_provider(provider.as_ref(), call).await,
                    None => self.run_builtin(call).await,
                };
            }
        };
        let summary = match tool {
            AgentTool::UpdatePlan => "update the plan",
            AgentTool::SpawnAgent => "delegate a task",
            AgentTool::AwaitTasks => "wait for subagents",
            AgentTool::SteerAgent => "steer a subagent",
            AgentTool::QueueAgent => "queue a message for a subagent",
            AgentTool::ListAgents => "list subagents",
            AgentTool::ResumeAgent => "resume a subagent",
            AgentTool::KeepAsKnowledge => "keep a subagent as a knowledge agent",
            AgentTool::AskAgent => "ask a knowledge agent",
            AgentTool::ListKnowledgeAgents => "list knowledge agents",
            AgentTool::DelegateChunks => "split an artefact across subagents",
            AgentTool::AskUser => "ask the user",
            AgentTool::RequestClarification => "ask the user to clarify",
            AgentTool::UpdateState => "update the session state",
            AgentTool::LoadSkill => "load a skill",
            AgentTool::ActivatePlugin => "activate a plugin",
            AgentTool::ContextLoad => "load workflow context",
            AgentTool::StartSession => "start a session",
            AgentTool::MemorySearch => "search memory",
            AgentTool::MemoryRead => "read a memory",
            AgentTool::MemoryRemember => "save a memory",
        };
        // Plan mode only reads: starting a session (which would not be in plan mode) or saving
        // a memory changes things.
        let start_allowed = matches!(tool, AgentTool::StartSession)
            && self
                .shared
                .launcher
                .as_ref()
                .is_some_and(|launcher| launcher.allows_start_in_plan_mode());
        if matches!(tool, AgentTool::StartSession | AgentTool::MemoryRemember)
            && !start_allowed
            && self.workspace.permissions().mode() == PermissionMode::Plan
        {
            return self.tool_finished(call, name, false, refused(Refusal::Plan, summary), None);
        }
        let started = self.tool_started(call, name, summary.to_string());
        let result = match tool {
            AgentTool::UpdatePlan => self.update_plan(arguments),
            AgentTool::SpawnAgent => self.spawn_agent(arguments).await,
            AgentTool::AwaitTasks => self.await_tasks(arguments).await,
            AgentTool::SteerAgent => self.message_agent(arguments, true),
            AgentTool::QueueAgent => self.message_agent(arguments, false),
            AgentTool::ListAgents => Ok(self.list_agents()),
            AgentTool::ResumeAgent => self.resume_agent(arguments),
            AgentTool::KeepAsKnowledge => self.keep_as_knowledge(arguments),
            AgentTool::AskAgent => self.ask_agent(arguments).await,
            AgentTool::ListKnowledgeAgents => self.list_knowledge_agents(arguments),
            AgentTool::DelegateChunks => self.delegate_chunks(arguments),
            AgentTool::AskUser => self.ask_user(arguments).await,
            AgentTool::RequestClarification => self.request_clarification(arguments).await,
            AgentTool::UpdateState => self.update_state(arguments),
            AgentTool::LoadSkill => self.load_skill(arguments),
            AgentTool::ActivatePlugin => self.activate_plugin(arguments).await,
            AgentTool::ContextLoad => self.context_load(arguments),
            AgentTool::StartSession => self.start_session(arguments).await,
            AgentTool::MemorySearch => self.memory_search(arguments),
            AgentTool::MemoryRead => self.memory_read(arguments),
            AgentTool::MemoryRemember => self.memory_remember(arguments).await,
        };
        let (ok, output) = match result {
            Ok(output) => (true, output),
            Err(error) => (false, format!("error: {error}")),
        };
        self.tool_finished(call, name, ok, output, Some(started))
    }

    pub(super) async fn run_builtin(&self, call: &ToolCall) -> String {
        let finish = |ok: bool, output: String| {
            self.tool_finished(call, &call.function.name, ok, output, None)
        };
        let invocation = match Invocation::parse(&call.function.name, &call.function.arguments) {
            Ok(invocation) => invocation,
            Err(error) => return finish(false, format!("error: {error}")),
        };
        let tool = invocation.tool_name();
        let summary = self.annotate(invocation.summary());
        // Hooks see the arguments under the schema's names (`path`), whatever the model wrote.
        let hook_arguments = invocation
            .hook_arguments()
            .unwrap_or_else(|| call.function.arguments.clone());
        let target = match (&invocation, invocation.path()) {
            (Invocation::RunCommand { command, .. }, _) => Target::Command(command.clone()),
            (_, Some(path)) => self.workspace.rule_target(path),
            (_, None) => Target::Other,
        };
        let kind = match invocation.access() {
            Access::Read => CallKind::Read,
            Access::Write => CallKind::Edit,
            Access::Execute => CallKind::Execute,
        };
        // A write into a read-only mount fails whatever the user would answer, so it is not
        // asked about.
        if invocation.access() == Access::Write
            && let Some(path) = invocation.path()
            && let Err(error @ ToolError::ReadOnly(_)) = self.workspace.resolve_writable(path)
        {
            return finish(false, format!("error: {error}"));
        }
        let Judgement {
            decision, floored, ..
        } = self
            .workspace
            .permissions()
            .judge(tool, &target, kind, None);
        if let Decision::Refuse(why) = decision {
            return finish(false, refused(why, &summary));
        }
        if matches!(decision, Decision::Ask { .. }) && self.fork.is_some() {
            return finish(false, fork::ask_refusal(&summary));
        }
        match self.before_tool(tool, &hook_arguments).await {
            Hooked::Run => {}
            Hooked::Blocked(blocked) => return finish(false, blocked),
            Hooked::Rewrite(new) => {
                *lock(&self.pending_rewrite) = Some(new);
                return String::new();
            }
        }
        if invocation.access() != Access::Read
            && let Some(missing) = self.missing_context()
        {
            return finish(false, missing);
        }
        if let Decision::Ask { sticky } = decision
            && !self
                .approve_with(tool, &summary, suggest(tool, &target), sticky, floored)
                .await
        {
            return finish(false, "The user denied this action.".into());
        }
        let started = self.tool_started(call, tool, summary);
        let (ok, output) = match invocation.execute(&self.workspace).await {
            Ok(output) => (true, output),
            Err(error) => (false, format!("error: {error}")),
        };
        self.after_tool(tool, &hook_arguments, ok, &output).await;
        self.tool_finished(call, tool, ok, output, Some(started))
    }

    pub(super) fn update_plan(&self, arguments: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Args {
            #[serde(default)]
            goal: String,
            steps: Vec<PlanStep>,
            #[serde(default)]
            assumptions: Vec<String>,
            #[serde(default)]
            open_questions: Vec<String>,
        }
        let args: Args = parse(arguments)?;
        let plan = Plan {
            goal: args.goal,
            steps: args.steps,
            assumptions: args.assumptions,
            open_questions: args.open_questions,
        };
        let done = plan
            .steps
            .iter()
            .filter(|step| step.status == StepStatus::Done)
            .count();
        let summary = format!("plan updated: {done} of {} steps done", plan.steps.len());
        self.emit(SessionEvent::PlanUpdated { plan });
        Ok(summary)
    }
}

/// Tools the agent loop handles itself rather than `imperium-tools`.
pub(super) enum AgentTool {
    UpdatePlan,
    SpawnAgent,
    AwaitTasks,
    SteerAgent,
    QueueAgent,
    DelegateChunks,
    AskUser,
    RequestClarification,
    UpdateState,
    LoadSkill,
    ActivatePlugin,
    MemorySearch,
    MemoryRead,
    MemoryRemember,
    ContextLoad,
    StartSession,
    ListAgents,
    ResumeAgent,
    KeepAsKnowledge,
    AskAgent,
    ListKnowledgeAgents,
}

/// What a `tool.before` hook decided about a call.
pub(super) enum Hooked {
    Run,
    /// Blocked; the message for the model.
    Blocked(String),
    /// Rewritten: the new arguments, to be checked as a new call.
    Rewrite(String),
}

/// Arguments or a message as shown to the user when a hook rewrote it.
pub(crate) fn shown(text: &str) -> String {
    truncate(text, 1_000)
}

pub(super) fn tool_input(arguments: &str) -> serde_json::Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| arguments.into())
}

pub(super) fn manifest_event(manifest: &LoadManifest) -> SessionEvent {
    SessionEvent::ContextManifest {
        role: manifest.role.clone(),
        items: manifest
            .items
            .iter()
            .map(|item| ContextItem {
                reference: item.reference.clone(),
                reason: item.reason.clone(),
                required: item.required,
                tokens: item.tokens,
            })
            .collect(),
        documentation: manifest
            .documentation
            .iter()
            .map(|pointer| ContextPointer {
                reference: pointer.reference.clone(),
                reason: pointer.reason.clone(),
                summary: pointer.summary.clone(),
            })
            .collect(),
    }
}

/// What the model is told when a permission rule refuses a call.
pub(super) fn denied_by_rule(summary: &str) -> String {
    format!(
        "A permission rule denies this action ({summary}). Do not retry it; find another way or \
         ask the user to change the rule."
    )
}

/// What the model is told about a call refused without asking.
pub(super) fn refused(why: Refusal, summary: &str) -> String {
    match why {
        Refusal::Rule => denied_by_rule(summary),
        Refusal::Plan => format!(
            "The session is in plan mode, which only reads: {summary} was not run. Read and \
             search what you need, then present your plan; the user switches the mode when \
             they want it carried out."
        ),
    }
}

/// A rule as the protocol carries it.
pub(super) fn wire_rule(rule: PermissionRule) -> imperium_protocol::permissions::PermissionRule {
    use imperium_protocol::permissions::RuleAction as Wire;
    imperium_protocol::permissions::PermissionRule {
        tool: rule.tool,
        pattern: rule.pattern,
        action: match rule.action {
            RuleAction::Allow => Wire::Allow,
            RuleAction::Ask => Wire::Ask,
            RuleAction::Deny => Wire::Deny,
        },
    }
}

/// A provider's event as the session reports it; `None` for kinds it does not report.
pub(super) fn tool_event(event: ToolEvent) -> Option<SessionEvent> {
    Some(match event {
        ToolEvent::MediaCreated { path, kind, prompt } => {
            SessionEvent::MediaCreated { path, kind, prompt }
        }
        ToolEvent::BrowserView {
            url,
            title,
            screenshot,
            tab,
            tabs,
        } => SessionEvent::BrowserView {
            url,
            title,
            screenshot,
            tab: Some(tab),
            tabs: tabs
                .into_iter()
                .map(|tab| imperium_protocol::methods::BrowserTab {
                    id: tab.id,
                    url: tab.url,
                    title: tab.title,
                    active: tab.active,
                })
                .collect(),
        },
        ToolEvent::PreviewChanged { url, command } => SessionEvent::PreviewChanged { url, command },
        _ => return None,
    })
}

/// Keeps the tools `allowed` names, and moves the ranges of `groups` to match what is left.
fn narrow_tools(
    tools: Vec<ToolSpec>,
    groups: Vec<(Category, Range<usize>)>,
    allowed: &[String],
) -> (Vec<ToolSpec>, Vec<(Category, Range<usize>)>) {
    let keep: Vec<bool> = tools
        .iter()
        .map(|tool| allowed.contains(&tool.name))
        .collect();
    // before[i]: how many of the first i tools are kept.
    let mut before = Vec::with_capacity(keep.len() + 1);
    let mut kept = 0;
    before.push(0);
    for keeps in &keep {
        if *keeps {
            kept += 1;
        }
        before.push(kept);
    }
    let groups = groups
        .into_iter()
        .map(|(category, range)| (category, before[range.start]..before[range.end]))
        .collect();
    let tools = tools
        .into_iter()
        .zip(keep)
        .filter_map(|(tool, keeps)| keeps.then_some(tool))
        .collect();
    (tools, groups)
}
