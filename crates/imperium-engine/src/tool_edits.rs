//! A harness variant's tool edits at the model boundary (docs/plans/reactive-tools.md, R7).
//!
//! An evaluation applies the variant's drops, renames and descriptions to the **root** agent
//! where the engine builds a request, and reverses the names where a completion arrives, so
//! the engine's messages, journal and events keep the engine's names and the model sees one
//! consistent set of names. The functions here are pure; the agent calls them.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use imperium_api::{ChatMessage, ToolCall, ToolSpec};
use imperium_protocol::methods::ToolEdits;
use serde_json::Value;

use crate::agent::{CALL_TOOL, TOOL_SEARCH, first_sentence};

/// What an evaluation harness is told about the root agent's tools. Every name it receives is
/// the one the model sees (after the variant's renames).
pub trait ToolWatch: Send + Sync + std::fmt::Debug {
    /// The root's tool list (`offered`) and, with loading on, the tools it reaches through
    /// `tool_search` and `call_tool` (`deferred`), as the model sees them.
    fn catalog(&self, offered: &[ToolSpec], deferred: &[ToolSpec]);
    /// A `tool_search` showed the parameters of these tools for the first time.
    fn searched(&self, names: &[String]);
    /// The root called a deferred tool, through `call_tool` or by name.
    fn called(&self, name: &str);
    /// An edit found nothing to change, or a dropped tool was called and refused.
    fn warn(&self, message: &str);
}

/// The variant's edits of the root agent's tools, with what the engine resolves from them.
#[derive(Debug)]
pub(crate) struct ToolEditing {
    raw: ToolEdits,
    pub watch: Option<Arc<dyn ToolWatch>>,
    resolved: OnceLock<Resolved>,
}

/// The renames and descriptions in force, once the root's tools are known.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Resolved {
    /// Engine name to the name the model sees.
    forward: HashMap<String, String>,
    /// The name the model sees to the engine's.
    back: HashMap<String, String>,
    /// Engine name to the description the model sees.
    descriptions: HashMap<String, String>,
}

impl ToolEditing {
    pub(crate) fn new(raw: ToolEdits, watch: Option<Arc<dyn ToolWatch>>) -> Self {
        ToolEditing {
            raw,
            watch,
            resolved: OnceLock::new(),
        }
    }

    pub(crate) fn dropped(&self, name: &str) -> bool {
        self.raw.drop.iter().any(|dropped| dropped == name)
    }

    pub(crate) fn drops(&self) -> &[String] {
        &self.raw.drop
    }

    /// The names forced into the core; empty keeps the default core.
    pub(crate) fn core(&self) -> &[String] {
        &self.raw.core
    }

    /// The edits resolved against the root's tools: `known` are the names of its universe
    /// before the drops (resolved once, at the first list; the warnings go to the watcher).
    pub(crate) fn resolved(&self, known: &[String]) -> &Resolved {
        self.resolved.get_or_init(|| {
            let (resolved, warnings) = resolve(&self.raw, known);
            if let Some(watch) = &self.watch {
                for warning in &warnings {
                    watch.warn(warning);
                }
            }
            resolved
        })
    }

    /// The resolved edits if the root's tools were known already.
    pub(crate) fn resolved_now(&self) -> Option<&Resolved> {
        self.resolved.get()
    }
}

/// Resolves `edits` against the universe's names. A name that matches no tool adds a line to
/// the warnings; a rename onto the name of a tool that stays as it is, or two renames to one
/// name, is skipped with a warning.
pub(crate) fn resolve(edits: &ToolEdits, known: &[String]) -> (Resolved, Vec<String>) {
    let is_known = |name: &str| known.iter().any(|tool| tool == name);
    let mut warnings = Vec::new();
    for name in &edits.drop {
        if !is_known(name) {
            warnings.push(format!("tools.drop: no tool named {name:?} is offered"));
        }
    }
    for name in edits.rename.keys() {
        if !is_known(name) {
            warnings.push(format!("tools.rename: no tool named {name:?} is offered"));
        }
    }
    for name in edits.description.keys() {
        if !is_known(name) {
            warnings.push(format!(
                "tools.description: no tool named {name:?} is offered"
            ));
        }
    }
    for name in &edits.core {
        if !is_known(name) {
            warnings.push(format!("tools.core: no tool named {name:?} is offered"));
        }
    }
    let mut resolved = Resolved::default();
    // Names a rename may not take: those of the tools that stay as they are.
    let unchanged: Vec<&str> = known
        .iter()
        .filter(|name| !edits.drop.contains(name) && !edits.rename.contains_key(*name))
        .map(String::as_str)
        .collect();
    for (old, new) in &edits.rename {
        if edits.drop.contains(old) {
            continue;
        }
        if unchanged.contains(&new.as_str()) || resolved.back.contains_key(new) {
            warnings.push(format!(
                "tools.rename: {old:?} cannot become {new:?}, which another tool already has; \
                 the rename is skipped"
            ));
            continue;
        }
        resolved.back.insert(new.clone(), old.clone());
        resolved.forward.insert(old.clone(), new.clone());
    }
    resolved.descriptions = edits
        .description
        .iter()
        .map(|(name, text)| (name.clone(), text.clone()))
        .collect();
    (resolved, warnings)
}

impl Resolved {
    /// No rename and no description: the model sees the engine's tools as they are.
    pub(crate) fn is_empty(&self) -> bool {
        self.forward.is_empty() && self.descriptions.is_empty()
    }

    /// The name the model sees for the engine's `name`.
    pub(crate) fn show<'a>(&'a self, name: &'a str) -> &'a str {
        self.forward.get(name).map_or(name, String::as_str)
    }

    /// The engine's name for the name the model used.
    pub(crate) fn engine<'a>(&'a self, name: &'a str) -> &'a str {
        self.back.get(name).map_or(name, String::as_str)
    }

    /// `spec` as the model sees it.
    pub(crate) fn show_spec(&self, spec: &ToolSpec) -> ToolSpec {
        let mut shown = spec.clone();
        if let Some(text) = self.descriptions.get(&spec.name) {
            shown.description.clone_from(text);
        }
        shown.name = self.show(&spec.name).to_string();
        shown
    }

    /// The conversation as the model sees it, or `None` when nothing in it changes: earlier
    /// calls carry the renamed tools' new names (including the inner names of `call_tool` and
    /// the `names` of `tool_search`), and the results of `tool_search` show the new names and
    /// descriptions.
    pub(crate) fn show_messages(&self, messages: &[ChatMessage]) -> Option<Vec<ChatMessage>> {
        if self.is_empty() {
            return None;
        }
        let mut searches: HashSet<String> = HashSet::new();
        let mut changed = false;
        let shown: Vec<ChatMessage> = messages
            .iter()
            .map(|message| match message {
                ChatMessage::Assistant {
                    content,
                    tool_calls,
                } => {
                    let calls = tool_calls
                        .iter()
                        .map(|call| {
                            if call.function.name == TOOL_SEARCH {
                                searches.insert(call.id.clone());
                            }
                            let seen = self.map_call(call, Resolved::show);
                            changed |= seen != *call;
                            seen
                        })
                        .collect();
                    ChatMessage::Assistant {
                        content: content.clone(),
                        tool_calls: calls,
                    }
                }
                ChatMessage::Tool {
                    tool_call_id,
                    content,
                } if searches.contains(tool_call_id) => {
                    let text = self.show_search_result(content);
                    changed |= text != *content;
                    ChatMessage::Tool {
                        tool_call_id: tool_call_id.clone(),
                        content: text,
                    }
                }
                other => other.clone(),
            })
            .collect();
        changed.then_some(shown)
    }

    /// Turns the names in a reply's calls back into the engine's (including the inner name of
    /// `call_tool` and the `names` of `tool_search`), before anything is stored.
    pub(crate) fn engine_calls(&self, calls: &mut [ToolCall]) {
        for call in calls {
            *call = self.map_call(call, Resolved::engine);
        }
    }

    /// `call` with its tool names mapped by `map` (`show` or `engine`).
    fn map_call(
        &self,
        call: &ToolCall,
        map: for<'a> fn(&'a Resolved, &'a str) -> &'a str,
    ) -> ToolCall {
        let mut out = call.clone();
        let name = call.function.name.as_str();
        if name == CALL_TOOL {
            if let Some(arguments) = map_argument(&call.function.arguments, "name", |value| {
                value.as_str().map(|inner| Value::from(map(self, inner)))
            }) {
                out.function.arguments = arguments;
            }
        } else if name == TOOL_SEARCH {
            if let Some(arguments) =
                map_argument(&call.function.arguments, "names", |value| match value {
                    Value::String(one) => Some(Value::from(map(self, one))),
                    Value::Array(many) => Some(Value::Array(
                        many.iter()
                            .map(|item| match item.as_str() {
                                Some(text) => Value::from(map(self, text)),
                                None => item.clone(),
                            })
                            .collect(),
                    )),
                    _ => None,
                })
            {
                out.function.arguments = arguments;
            }
        } else {
            out.function.name = map(self, name).to_string();
        }
        out
    }

    /// A `tool_search` result with the new names and descriptions; text that is not a result
    /// is left as it is.
    fn show_search_result(&self, content: &str) -> String {
        let Ok(mut value) = serde_json::from_str::<Value>(content) else {
            return content.to_string();
        };
        let Some(object) = value.as_object_mut() else {
            return content.to_string();
        };
        if let Some(Value::Array(tools)) = object.get_mut("tools") {
            for tool in tools {
                let Some(entry) = tool.as_object_mut() else {
                    continue;
                };
                let Some(name) = entry
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                else {
                    continue;
                };
                if let Some(text) = self.descriptions.get(&name) {
                    entry.insert("description".into(), Value::from(text.as_str()));
                }
                entry.insert("name".into(), Value::from(self.show(&name)));
            }
        }
        if let Some(Value::Array(lines)) = object.get_mut("more") {
            for line in lines {
                let Some(text) = line.as_str() else { continue };
                let Some((name, rest)) = text.split_once(": ") else {
                    continue;
                };
                let rest = match self.descriptions.get(name) {
                    Some(own) => first_sentence(own).to_string(),
                    None => rest.to_string(),
                };
                *line = Value::from(format!("{}: {rest}", self.show(name)));
            }
        }
        if let Some(Value::Array(unknown)) = object.get_mut("unknown") {
            for item in unknown {
                if let Some(Value::Array(close)) = item.get_mut("close") {
                    for name in close {
                        if let Some(text) = name.as_str() {
                            *name = Value::from(self.show(text));
                        }
                    }
                }
            }
        }
        value.to_string()
    }
}

/// `arguments` with the member `key` replaced by `map(member)`; `None` when the arguments are
/// not an object with that member, or the mapping changes nothing (so an untouched call keeps
/// its text byte for byte).
fn map_argument(
    arguments: &str,
    key: &str,
    map: impl Fn(&Value) -> Option<Value>,
) -> Option<String> {
    let mut value: Value = serde_json::from_str(arguments).ok()?;
    let member = value.as_object_mut()?.get_mut(key)?;
    let new = map(member)?;
    if new == *member {
        return None;
    }
    *member = new;
    Some(value.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_string()).collect()
    }

    fn edits() -> ToolEdits {
        ToolEdits {
            drop: names(&["web_fetch"]),
            rename: BTreeMap::from([("run_command".to_string(), "sh".to_string())]),
            description: BTreeMap::from([(
                "update_plan".to_string(),
                "Plan it. More.".to_string(),
            )]),
            core: Vec::new(),
        }
    }

    fn known() -> Vec<String> {
        names(&["read_file", "run_command", "update_plan", "web_fetch"])
    }

    fn call(id: &str, name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: imperium_api::FunctionCall {
                name: name.into(),
                arguments: arguments.to_string(),
            },
        }
    }

    #[test]
    fn names_that_match_nothing_and_clashing_renames_warn() {
        let mut bad = edits();
        bad.drop.push("nope".into());
        bad.core.push("ghost".into());
        bad.rename.insert("update_plan".into(), "read_file".into());
        let (resolved, warnings) = resolve(&bad, &known());
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        // `read_file` stays, so `update_plan` cannot take its name; the other rename holds.
        assert_eq!(resolved.show("read_file"), "read_file");
        assert_eq!(resolved.show("update_plan"), "update_plan");
        assert_eq!(resolved.engine("sh"), "run_command");
        // The pilot's case: dropping a tool that exists warns of nothing.
        let (_, none) = resolve(&edits(), &known());
        assert!(none.is_empty(), "{none:?}");
    }

    #[test]
    fn specs_and_calls_are_translated_both_ways() {
        let (resolved, _) = resolve(&edits(), &known());
        let spec = ToolSpec {
            name: "run_command".into(),
            description: "Runs.".into(),
            parameters: json!({"type": "object"}),
        };
        assert_eq!(resolved.show_spec(&spec).name, "sh");
        let plan = ToolSpec {
            name: "update_plan".into(),
            description: "Engine text.".into(),
            parameters: json!({}),
        };
        assert_eq!(resolved.show_spec(&plan).description, "Plan it. More.");
        let mut calls = vec![
            call("1", "sh", json!({"command": "ls"})),
            call("2", "call_tool", json!({"name": "sh", "arguments": {}})),
            call("3", "tool_search", json!({"names": ["sh", "update_plan"]})),
            call("4", "read_file", json!({"path": "a"})),
        ];
        let before = calls[3].clone();
        resolved.engine_calls(&mut calls);
        assert_eq!(calls[0].function.name, "run_command");
        assert!(calls[1].function.arguments.contains("\"run_command\""));
        assert!(calls[2].function.arguments.contains("\"run_command\""));
        assert_eq!(calls[3], before);
    }

    #[test]
    fn the_conversation_shows_new_names_and_descriptions_and_changes_nothing_else() {
        let (resolved, _) = resolve(&edits(), &known());
        let result = json!({
            "tools": [{"name": "run_command", "description": "Runs.", "parameters": {}},
                      {"name": "update_plan", "description": "Engine.", "parameters": {}}],
            "more": ["run_command: Runs a thing.", "update_plan: Engine."],
            "unknown": [{"name": "shell", "close": ["run_command"]}],
        })
        .to_string();
        let messages = vec![
            ChatMessage::user("hi"),
            ChatMessage::Assistant {
                content: None,
                tool_calls: vec![
                    call("a", "tool_search", json!({"names": ["run_command"]})),
                    call(
                        "b",
                        "call_tool",
                        json!({"name": "run_command", "arguments": {"command": "ls"}}),
                    ),
                ],
            },
            ChatMessage::Tool {
                tool_call_id: "a".into(),
                content: result,
            },
            ChatMessage::Tool {
                tool_call_id: "b".into(),
                content: "run_command ok".into(),
            },
        ];
        let shown = resolved.show_messages(&messages).expect("a change");
        let ChatMessage::Assistant { tool_calls, .. } = &shown[1] else {
            panic!("{shown:?}");
        };
        assert!(tool_calls[0].function.arguments.contains("\"sh\""));
        assert!(tool_calls[1].function.arguments.contains("\"sh\""));
        let ChatMessage::Tool { content, .. } = &shown[2] else {
            panic!("{shown:?}");
        };
        let parsed: Value = serde_json::from_str(content).unwrap();
        assert_eq!(parsed["tools"][0]["name"], "sh");
        assert_eq!(parsed["tools"][1]["description"], "Plan it. More.");
        assert_eq!(parsed["more"][0], "sh: Runs a thing.");
        assert_eq!(parsed["more"][1], "update_plan: Plan it.");
        assert_eq!(parsed["unknown"][0]["close"][0], "sh");
        // A result that is not a search result's, and a call to another tool, are untouched.
        assert_eq!(shown[3], messages[3]);
        let plain = vec![ChatMessage::user("hi")];
        assert!(resolved.show_messages(&plain).is_none());
        let (none, _) = resolve(&ToolEdits::default(), &known());
        assert!(none.show_messages(&messages).is_none());
    }
}
