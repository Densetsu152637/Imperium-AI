//! A variant's tool edits on the root agent (docs/plans/reactive-tools.md, R7): drops leave the
//! universe, renames and descriptions are applied where a request is built and reversed where a
//! reply arrives (`crate::tool_edits`), and an evaluation's watcher is told what the root was
//! offered and reached.

use super::*;
use crate::tool_edits::{Resolved, ToolEditing, ToolWatch};

impl Agent {
    /// The variant's edits, for the root agent only (a subagent's tools are never edited).
    fn editing(&self) -> Option<&ToolEditing> {
        if self.id == ROOT_AGENT {
            self.shared.tool_edits.as_ref()
        } else {
            None
        }
    }

    /// The watcher an evaluation gave the session, for the root agent.
    fn watcher(&self) -> Option<&Arc<dyn ToolWatch>> {
        self.editing().and_then(|editing| editing.watch.as_ref())
    }

    /// The renames and descriptions in force (resolved against the root's tools the first time
    /// they are asked for), or `None` when there are none.
    pub(super) fn tool_renames(&self) -> Option<&Resolved> {
        let editing = self.editing()?;
        if let Some(resolved) = editing.resolved_now() {
            return Some(resolved).filter(|resolved| !resolved.is_empty());
        }
        let mut known: Vec<String> = self
            .universe_before_edits(!self.reactive())
            .0
            .into_iter()
            .map(|spec| spec.name)
            .collect();
        // Tools a profile hides from the list can still be named by a rename.
        known.extend(SHELL_TOOLS.iter().map(|name| (*name).to_string()));
        known.extend([TOOL_SEARCH.to_string(), CALL_TOOL.to_string()]);
        Some(editing.resolved(&known)).filter(|resolved| !resolved.is_empty())
    }

    /// The tools the model is sent for `tools` (the engine's list): after the renames and
    /// descriptions, or `None` when they are the same.
    pub(super) fn shown_tools(&self, tools: &[ToolSpec]) -> Option<Vec<ToolSpec>> {
        let renames = self.tool_renames()?;
        Some(tools.iter().map(|tool| renames.show_spec(tool)).collect())
    }

    /// The conversation the model is sent, or `None` when it is the engine's own.
    pub(super) fn shown_messages(&self) -> Option<Vec<ChatMessage>> {
        self.tool_renames()?.show_messages(&self.messages)
    }

    /// Puts the engine's names back into a reply's tool calls, before anything is stored.
    pub(super) fn engine_calls(&self, calls: &mut [ToolCall]) {
        if let Some(renames) = self.tool_renames() {
            renames.engine_calls(calls);
        }
    }

    /// The engine's name for a name a call carries (a script may use the edited one), when it
    /// differs.
    pub(super) fn engine_name(&self, name: &str) -> Option<String> {
        let renames = self.tool_renames()?;
        let engine = renames.engine(name);
        (engine != name).then(|| engine.to_string())
    }

    /// The name the model sees for the engine's `name`.
    pub(super) fn shown_name(&self, name: &str) -> String {
        match self.tool_renames() {
            Some(renames) => renames.show(name).to_string(),
            None => name.to_string(),
        }
    }

    /// Why a call to `name` is refused when the variant dropped the tool. The tool is out of
    /// the list and the universe, and is refused here whether loading is on or off; the
    /// refusal is reported to the watcher.
    pub(super) fn dropped_refusal(&self, name: &str) -> Option<String> {
        let editing = self.editing().filter(|editing| editing.dropped(name))?;
        if let Some(watch) = &editing.watch {
            watch.warn(&format!("dropped tool {name} was called and refused"));
        }
        Some(format!("{name} is not one of your tools; it was not run"))
    }

    /// Whether the variant dropped the tool.
    pub(super) fn is_dropped(&self, name: &str) -> bool {
        self.editing().is_some_and(|editing| editing.dropped(name))
    }

    /// The tools offered directly with loading on: the variant's core when it sets one, the
    /// default core otherwise.
    pub(super) fn core_list(&self) -> Vec<String> {
        match self
            .editing()
            .map(ToolEditing::core)
            .filter(|core| !core.is_empty())
        {
            Some(core) => core.to_vec(),
            None => core_names(),
        }
    }

    /// Whether the tool is offered directly while loading is on, for this agent.
    pub(super) fn core_here(&self, name: &str) -> bool {
        match self
            .editing()
            .map(ToolEditing::core)
            .filter(|core| !core.is_empty())
        {
            Some(core) => core.iter().any(|tool| tool == name),
            None => is_core(name),
        }
    }

    /// The positions in `specs` of the tools that are deferred while loading is on.
    pub(super) fn deferred_of(&self, specs: &[ToolSpec]) -> Vec<usize> {
        specs
            .iter()
            .enumerate()
            .filter(|(_, spec)| !self.core_here(&spec.name))
            .map(|(index, _)| index)
            .collect()
    }

    /// Tells the watcher what the root is offered and what it reaches on demand, as the model
    /// sees them.
    pub(super) fn report_catalog(&self, offered: &[ToolSpec]) {
        let Some(watch) = self.watcher() else {
            return;
        };
        let show = |specs: &[ToolSpec]| -> Vec<ToolSpec> {
            match self.tool_renames() {
                Some(renames) => specs.iter().map(|spec| renames.show_spec(spec)).collect(),
                None => specs.to_vec(),
            }
        };
        let deferred: Vec<ToolSpec> = if self.reactive() {
            let (specs, _) = self.tool_universe();
            self.deferred_of(&specs)
                .into_iter()
                .map(|index| specs[index].clone())
                .collect()
        } else {
            Vec::new()
        };
        watch.catalog(&show(offered), &show(&deferred));
    }

    /// Tells the watcher the root called a deferred tool: through `call_tool`, or by name.
    pub(super) fn report_call(&self, name: &str, proxied: bool) {
        let Some(watch) = self.watcher() else {
            return;
        };
        if self.reactive()
            && name != TOOL_SEARCH
            && !self.is_dropped(name)
            && (proxied || !self.core_here(name))
        {
            watch.called(&self.shown_name(name));
        }
    }

    /// Tells the watcher which tools a `tool_search` showed for the first time.
    pub(super) fn report_search(&self, fresh: &[String]) {
        if let Some(watch) = self.watcher()
            && !fresh.is_empty()
        {
            let shown: Vec<String> = fresh.iter().map(|name| self.shown_name(name)).collect();
            watch.searched(&shown);
        }
    }
}
