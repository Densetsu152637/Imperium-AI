//! Reactive tool loading (docs/plans/reactive-tools.md, stage 2a): with `harness.tool_loading`
//! on, an agent is offered a small fixed list (the core tools, `tool_search` and `call_tool`)
//! and reaches every other tool of its universe by searching for it and calling it by name.
//!
//! The universe is the list the agent is offered with loading off (`Agent::tool_universe`);
//! the offered list is derived from it (`Agent::tool_offered`) and does not change within a
//! session. Every call is dispatched against the universe as it is when the call runs.

use serde_json::Value;

use super::*;

/// The proxy tool that finds tools outside the offered list.
pub(super) const TOOL_SEARCH: &str = "tool_search";
/// The proxy tool that runs a tool outside the offered list.
pub(super) const CALL_TOOL: &str = "call_tool";

/// The tools offered directly while loading is on, in the order of the full list: the file and
/// command tools, and the loop's own protocol tools when the agent has them.
const CORE_TOOLS: [&str; 12] = [
    "read_file",
    "write_file",
    "edit_file",
    "list_dir",
    "glob",
    "grep",
    "run_command",
    "context_load",
    LOAD_SKILL,
    ACTIVATE_PLUGIN,
    "request_clarification",
    "update_state",
];

/// A loader costs more than it saves when fewer tools than this are deferred.
const MIN_DEFERRED: usize = 3;
/// Most bytes of full schemas one search result holds (the first schema, and every schema
/// asked for by exact name, are returned in full whatever their size).
pub(super) const SEARCH_CAP: usize = 8_000;
/// Most schemas a query returns in full, and most further tools it lists by name.
const FULL_RESULTS: usize = 5;
const LISTED_RESULTS: usize = 10;
/// Most close names offered for a name that is not a tool.
const CLOSE_NAMES: usize = 3;

/// Whether the tool is offered directly while loading is on.
pub(super) fn is_core(name: &str) -> bool {
    CORE_TOOLS.contains(&name)
}

/// The names kept in the offered list while loading is on.
fn core_names() -> Vec<String> {
    CORE_TOOLS.iter().map(|name| (*name).to_string()).collect()
}

pub(super) fn call_tool_spec() -> ToolSpec {
    tool_spec(contract::call_tool())
}

/// `tool_search` with its description fixed from the names of the deferred built-in tools.
fn tool_search_spec(deferred: &[String]) -> ToolSpec {
    let definition = contract::tool_search();
    let description = if deferred.is_empty() {
        format!(
            "{} It finds the tools of connected MCP servers and activated plugins.",
            definition.description
        )
    } else {
        format!(
            "{} Tools you can reach this way: {}, and the tools of connected MCP servers and \
             activated plugins: search for them.",
            definition.description,
            deferred.join(", ")
        )
    };
    ToolSpec {
        name: definition.name.into(),
        description,
        parameters: definition.parameters,
    }
}

/// The positions in `specs` of the tools that are deferred while loading is on.
pub(super) fn deferred_indices(specs: &[ToolSpec]) -> Vec<usize> {
    specs
        .iter()
        .enumerate()
        .filter(|(_, spec)| !is_core(&spec.name))
        .map(|(index, _)| index)
        .collect()
}

/// What a `tool_search` found.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Found {
    /// Tools in full: name, description and parameters.
    pub tools: Vec<Value>,
    /// Further tools as `name: first sentence`.
    pub more: Vec<String>,
    /// Names that are no tool, with the close names.
    pub unknown: Vec<Value>,
    /// The names in `tools`.
    pub shown: Vec<String>,
}

impl Found {
    /// The result text the model reads.
    pub(super) fn render(&self) -> String {
        serde_json::json!({
            "tools": self.tools,
            "more": self.more,
            "unknown": self.unknown,
        })
        .to_string()
    }
}

fn entry(spec: &ToolSpec) -> Value {
    serde_json::json!({
        "name": spec.name,
        "description": spec.description,
        "parameters": spec.parameters,
    })
}

/// The first sentence of a description (at most its first line).
fn first_sentence(description: &str) -> &str {
    let end = [
        description.find(". ").map(|at| at + 1),
        description.find('\n'),
    ]
    .into_iter()
    .flatten()
    .min()
    .unwrap_or(description.len());
    description[..end].trim()
}

fn listed(spec: &ToolSpec) -> String {
    format!("{}: {}", spec.name, first_sentence(&spec.description))
}

/// `name: first sentence` lines for `specs`, as many as fit in the result cap.
fn catalog_lines(specs: &[&ToolSpec]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut bytes = 0;
    for (index, spec) in specs.iter().enumerate() {
        let line = listed(spec);
        if bytes + line.len() > SEARCH_CAP {
            lines.push(format!(
                "... and {} more: search with words from their names",
                specs.len() - index
            ));
            break;
        }
        bytes += line.len();
        lines.push(line);
    }
    lines
}

/// The words of a query that can match a name or description.
fn query_words(query: &str) -> Vec<String> {
    const STOP: [&str; 8] = ["the", "and", "for", "with", "from", "that", "this", "tool"];
    query
        .split(|character: char| !character.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|word| word.chars().count() >= 2 && !STOP.contains(&word.as_str()))
        .collect()
}

/// How well a tool matches the words: a hit in the name counts three, one in the description one.
fn score(spec: &ToolSpec, words: &[String]) -> usize {
    let name = spec.name.to_lowercase();
    let description = spec.description.to_lowercase();
    words
        .iter()
        .map(|word| {
            (if name.contains(word.as_str()) { 3 } else { 0 })
                + usize::from(description.contains(word.as_str()))
        })
        .sum()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, left) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, right) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(left != *right))
                .min(row[j] + 1)
                .min(above + 1);
            diagonal = above;
        }
    }
    row[b.len()]
}

/// Up to three tool names close to `name`.
fn close_names(name: &str, specs: &[ToolSpec]) -> Vec<String> {
    if name.is_empty() {
        return Vec::new();
    }
    let wanted = name.to_lowercase();
    let mut near: Vec<(usize, &str)> = specs
        .iter()
        .filter_map(|spec| {
            let other = spec.name.to_lowercase();
            let distance = edit_distance(&wanted, &other);
            let reach = (wanted.chars().count().max(other.chars().count()) / 3).max(2);
            (distance <= reach || other.contains(&wanted) || wanted.contains(&other))
                .then_some((distance, spec.name.as_str()))
        })
        .collect();
    near.sort_unstable();
    near.into_iter()
        .take(CLOSE_NAMES)
        .map(|(_, name)| name.to_string())
        .collect()
}

/// Finds tools in the universe `specs`, whose deferred tools are at `catalog`.
///
/// `names` (exact names, or a prefix ending in `*`) return those tools in full, all of them
/// whatever their size. Otherwise `query` ranks the deferred tools by its words: the best
/// five come back in full while their schemas fit in [`SEARCH_CAP`] (the first always does),
/// the rest and up to ten further matches as one-line entries. Neither lists the catalog.
pub(super) fn search(
    specs: &[ToolSpec],
    catalog: &[usize],
    query: &str,
    names: &[String],
) -> Found {
    let mut found = Found::default();
    if !names.is_empty() {
        for requested in names {
            let requested = requested.trim();
            let matches: Vec<&ToolSpec> = match requested.strip_suffix('*') {
                Some(prefix) if !prefix.is_empty() => specs
                    .iter()
                    .filter(|spec| spec.name.starts_with(prefix))
                    .collect(),
                _ => specs.iter().filter(|spec| spec.name == requested).collect(),
            };
            if matches.is_empty() {
                found.unknown.push(serde_json::json!({
                    "name": requested,
                    "close": close_names(requested, specs),
                }));
            }
            for spec in matches {
                if !found.shown.contains(&spec.name) {
                    found.shown.push(spec.name.clone());
                    found.tools.push(entry(spec));
                }
            }
        }
        return found;
    }
    let deferred: Vec<&ToolSpec> = catalog.iter().map(|index| &specs[*index]).collect();
    let words = query_words(query);
    let mut ranked: Vec<(usize, usize)> = deferred
        .iter()
        .enumerate()
        .map(|(position, spec)| (score(spec, &words), position))
        .filter(|(score, _)| *score > 0)
        .collect();
    if words.is_empty() || ranked.is_empty() {
        found.more = catalog_lines(&deferred);
        return found;
    }
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut bytes = 0;
    let mut spilled = Vec::new();
    let mut rest = Vec::new();
    for (rank, (_, position)) in ranked.into_iter().enumerate() {
        let spec = deferred[position];
        if rank < FULL_RESULTS {
            let value = entry(spec);
            let size = value.to_string().len();
            if found.tools.is_empty() || bytes + size <= SEARCH_CAP {
                bytes += size;
                found.shown.push(spec.name.clone());
                found.tools.push(value);
            } else {
                spilled.push(listed(spec));
            }
        } else if rest.len() < LISTED_RESULTS {
            rest.push(listed(spec));
        }
    }
    found.more = spilled;
    found.more.extend(rest);
    found
}

/// What `tool_search` takes: `names` may be one name or a list.
#[derive(Deserialize)]
struct SearchArgs {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    names: Option<OneOrMany>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl Agent {
    /// Whether this agent's tool list is the file and command tools with the two loader
    /// tools, which the setting and the model decide at each turn (`auto` is on for a model
    /// that runs on this machine). A named agent whose file lists its tools is offered exactly
    /// those, and a single-call agent has no tools.
    fn loading_mode(&self) -> bool {
        !self.single_call
            && !self
                .named
                .as_ref()
                .is_some_and(|named| named.tools.is_some())
            && mode_on(self.shared.harness.tool_loading, self.model.local)
    }

    /// Whether the agent is offered the loader list now: the mode is on, and its catalog of
    /// deferred tools is big enough that a loader saves something (decided once, when the
    /// agent first needs to know). A question's fork follows its source and skips that check.
    pub(super) fn reactive(&self) -> bool {
        if !self.loading_mode() {
            return false;
        }
        if self.fork.is_some() {
            return true;
        }
        *self.loader_worth.get_or_init(|| {
            let (specs, _) = self.build_universe(false);
            deferred_indices(&specs).len() >= MIN_DEFERRED
        })
    }

    /// Whether `activate_plugin` stays offered for the whole session: it is when the agent
    /// could activate a plugin at the start. Without this it would leave the list after the
    /// last activation and change it.
    pub(super) fn plugin_loader(&self) -> bool {
        *self.plugin_loader.get_or_init(|| {
            let profile = self.shared.workspace.profile();
            let lazy = &self.shared.extensions.plugins;
            lazy.plugins()
                .iter()
                .any(|plugin| !lazy.is_active(&plugin.name) && profile.allows_plugin(&plugin.name))
        })
    }

    /// The tools offered to the model: today's full list with loading off; with it on, the
    /// core tools in their usual order, then `tool_search` and `call_tool`.
    pub(super) fn tool_offered(&self) -> (Vec<ToolSpec>, Vec<(Category, Range<usize>)>) {
        if !self.reactive() {
            return self.tool_universe();
        }
        // Decided now, at the first list, before any plugin is active.
        self.plugin_loader();
        let (specs, groups) = self.tool_universe();
        let (mut specs, groups) = narrow_tools(specs, groups, &core_names());
        specs.push(self.tool_search_spec());
        specs.push(call_tool_spec());
        (specs, groups)
    }

    /// `tool_search`, its description fixed when the session starts: the names of the deferred
    /// built-in tools then. Tools that appear later are found by searching, not listed here.
    fn tool_search_spec(&self) -> ToolSpec {
        let description = self.search_text.get_or_init(|| {
            let (specs, groups) = self.build_universe(false);
            let names: Vec<String> = specs
                .iter()
                .enumerate()
                .filter(|(index, spec)| {
                    !is_core(&spec.name) && !groups.iter().any(|(_, range)| range.contains(index))
                })
                .map(|(_, spec)| spec.name.clone())
                .collect();
            tool_search_spec(&names).description
        });
        ToolSpec {
            name: TOOL_SEARCH.into(),
            description: description.clone(),
            parameters: contract::tool_search().parameters,
        }
    }

    /// The tool of the agent's universe called `name`, as it is now.
    fn universe_spec(&self, name: &str) -> Option<ToolSpec> {
        self.tool_universe()
            .0
            .into_iter()
            .find(|spec| spec.name == name)
    }

    /// Whether `name` is one of the agent's tools as it is now (the universe is worked out
    /// again for each call, so a plugin another agent activated is reachable at once).
    pub(super) fn in_universe(&self, name: &str) -> bool {
        self.universe_spec(name).is_some()
    }

    /// Runs `tool_search`: tools of the universe, found by exact name or by words.
    pub(super) fn tool_search(&self, arguments: &str) -> Result<String, String> {
        let args: SearchArgs = parse(arguments)?;
        let names = match args.names {
            Some(OneOrMany::One(name)) => vec![name],
            Some(OneOrMany::Many(names)) => names,
            None => Vec::new(),
        };
        let (specs, _) = self.tool_universe();
        let catalog = deferred_indices(&specs);
        let found = search(
            &specs,
            &catalog,
            args.query.as_deref().unwrap_or(""),
            &names,
        );
        self.note_loaded(&found.shown, ToolLoadVia::Search);
        Ok(found.render())
    }

    /// Turns a `call_tool` call into the call it stands for: the inner tool's name and
    /// arguments with the outer call's id, so the checks, hooks, approvals and events that
    /// follow see the inner tool. Any other call is returned as it is. The flag says whether
    /// the call came through `call_tool`.
    pub(super) fn unwrap_call(&self, call: &ToolCall) -> Result<(ToolCall, bool), String> {
        if call.function.name != CALL_TOOL || !self.reactive() {
            return Ok((call.clone(), false));
        }
        #[derive(Deserialize)]
        struct Args {
            name: String,
            #[serde(default)]
            arguments: Value,
        }
        let args: Args = parse(&call.function.arguments)?;
        let name = args.name.trim().to_string();
        if name.is_empty() {
            return Err("name must be the name of a tool".into());
        }
        if name == CALL_TOOL || name == TOOL_SEARCH {
            return Err(format!(
                "call_tool does not run {name}; call {name} directly"
            ));
        }
        let arguments = match args.arguments {
            Value::Null => "{}".to_string(),
            Value::String(text) if text.trim().is_empty() => "{}".to_string(),
            Value::String(text) => text,
            other => other.to_string(),
        };
        let mut inner = call.clone();
        inner.function.name = name;
        inner.function.arguments = arguments;
        Ok((inner, true))
    }

    /// Records the tools whose parameters the model has seen or that it ran through
    /// `call_tool`, once each, and reports them.
    pub(super) fn note_loaded(&self, names: &[String], via: ToolLoadVia) {
        let mut loaded = lock(&self.loaded_tools);
        let mut fresh: Vec<String> = Vec::new();
        for name in names {
            if !loaded.contains(name) && !fresh.contains(name) {
                fresh.push(name.clone());
            }
        }
        loaded.extend(fresh.iter().cloned());
        drop(loaded);
        if !fresh.is_empty() {
            self.emit(SessionEvent::ToolsLoaded { names: fresh, via });
        }
    }

    /// What follows a call's result: the tool is noted as loaded when it ran through
    /// `call_tool`, and a call to a deferred tool (through `call_tool` or by name) that failed
    /// gets the tool's parameters appended, since the model may only have guessed them. The
    /// tool's own parser judged the arguments, as for a direct call.
    pub(super) fn with_schema_advice(
        &self,
        call: &ToolCall,
        proxied: bool,
        output: String,
    ) -> String {
        let name = call.function.name.as_str();
        if !self.reactive() || name == TOOL_SEARCH || (!proxied && is_core(name)) {
            return output;
        }
        let Some(spec) = self.universe_spec(name) else {
            return output;
        };
        if proxied {
            self.note_loaded(std::slice::from_ref(&spec.name), ToolLoadVia::Call);
        }
        if self.last_tool_ok.load(Ordering::SeqCst) {
            return output;
        }
        format!(
            "{output}\n\nThe parameters of {name}, to check the arguments against: {}",
            spec.parameters
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, description: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: description.into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    fn universe() -> Vec<ToolSpec> {
        vec![
            spec("read_file", "Read a text file."),
            spec(
                "update_plan",
                "Replace your plan. Keep one step in progress.",
            ),
            spec("web_search", "Search the web; returns titles and snippets."),
            spec("web_fetch", "Fetch a web page."),
            spec("spawn_agent", "Delegate a task to a subagent."),
            spec("mcp__dict__lookup", "Look up a word in the dictionary."),
        ]
    }

    #[test]
    fn the_core_is_the_file_and_command_tools_and_the_protocol_tools() {
        for name in ["read_file", "run_command", "load_skill", "activate_plugin"] {
            assert!(is_core(name), "{name}");
        }
        for name in ["update_plan", "spawn_agent", "tool_search", "call_tool"] {
            assert!(!is_core(name), "{name}");
        }
        let specs = universe();
        assert_eq!(deferred_indices(&specs), [1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_query_ranks_names_over_descriptions_and_lists_the_rest() {
        let specs = universe();
        let catalog = deferred_indices(&specs);
        let found = search(&specs, &catalog, "web", &[]);
        assert_eq!(found.shown, ["web_search", "web_fetch"]);
        assert!(found.more.is_empty() && found.unknown.is_empty());
        let found = search(&specs, &catalog, "dictionary word", &[]);
        assert_eq!(found.shown, ["mcp__dict__lookup"]);
        // Nothing matches: the catalog is listed by name instead.
        let found = search(&specs, &catalog, "zzzz", &[]);
        assert!(found.tools.is_empty());
        assert_eq!(found.more.len(), 5);
        assert_eq!(found.more[0], "update_plan: Replace your plan.");
    }

    #[test]
    fn exact_names_return_every_schema_and_unknown_names_get_close_ones() {
        let specs = universe();
        let catalog = deferred_indices(&specs);
        let names = ["update_plan".to_string(), "spawn_agnt".to_string()];
        let found = search(&specs, &catalog, "", &names);
        assert_eq!(found.shown, ["update_plan"]);
        assert_eq!(found.unknown.len(), 1);
        assert_eq!(found.unknown[0]["name"], "spawn_agnt");
        assert_eq!(found.unknown[0]["close"][0], "spawn_agent");
        let found = search(&specs, &catalog, "", &["mcp__dict__*".to_string()]);
        assert_eq!(found.shown, ["mcp__dict__lookup"]);
    }

    #[test]
    fn a_result_stops_at_the_cap_but_the_first_and_named_schemas_stay_whole() {
        let big = |name: &str| ToolSpec {
            name: name.into(),
            description: "Search tool.".into(),
            parameters: serde_json::json!({"type": "object", "description": "x".repeat(5_000)}),
        };
        let specs = vec![big("search_a"), big("search_b"), big("search_c")];
        let catalog = deferred_indices(&specs);
        let found = search(&specs, &catalog, "search", &[]);
        // Each schema is about 5,100 bytes: the first fits, the second would pass the cap.
        assert_eq!(found.shown, ["search_a"]);
        assert_eq!(
            found.more,
            ["search_b: Search tool.", "search_c: Search tool."]
        );
        assert!(found.tools[0].to_string().len() < SEARCH_CAP);
        // A schema bigger than the cap is still returned whole, as the first match and by name.
        let huge = vec![ToolSpec {
            name: "mcp__big__tool".into(),
            description: "Huge.".into(),
            parameters: serde_json::json!({"description": "y".repeat(20_000)}),
        }];
        let catalog = deferred_indices(&huge);
        let found = search(&huge, &catalog, "huge", &[]);
        assert_eq!(found.shown, ["mcp__big__tool"]);
        assert!(found.tools[0].to_string().len() > SEARCH_CAP);
        let found = search(&huge, &catalog, "", &["mcp__big__tool".to_string()]);
        assert_eq!(found.shown, ["mcp__big__tool"]);
        // Asking for several by name returns all of them whatever their size.
        let found = search(
            &specs,
            &deferred_indices(&specs),
            "",
            &["search_a".into(), "search_b".into(), "search_c".into()],
        );
        assert_eq!(found.shown.len(), 3);
    }

    #[test]
    fn the_search_description_names_the_deferred_tools_once() {
        let spec = tool_search_spec(&["update_plan".into(), "spawn_agent".into()]);
        assert!(spec.description.contains("update_plan, spawn_agent"));
        assert!(
            spec.description
                .contains("MCP servers and activated plugins")
        );
        let bare = tool_search_spec(&[]);
        assert!(
            bare.description
                .contains("MCP servers and activated plugins")
        );
    }

    #[test]
    fn the_result_is_json_with_the_three_lists() {
        let specs = universe();
        let found = search(
            &specs,
            &deferred_indices(&specs),
            "",
            &["update_plan".into()],
        );
        let value: Value = serde_json::from_str(&found.render()).unwrap();
        assert_eq!(value["tools"][0]["name"], "update_plan");
        assert!(value["more"].as_array().unwrap().is_empty());
        assert!(value["unknown"].as_array().unwrap().is_empty());
    }
}
