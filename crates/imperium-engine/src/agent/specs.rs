use super::*;

pub(super) fn start_session_spec() -> ToolSpec {
    ToolSpec {
        name: "start_session".into(),
        description: "Start a new session in this project with a self-contained brief; it runs on its own and its report arrives later as a message. You use no tokens while it runs.".into(),
        parameters: json!({"type": "object", "properties": {
            "brief": {"type": "string"},
            "model": {"type": "string", "description": "Optional model table id or <endpoint>:<model>"},
            "reason": {"type": "string", "description": "Why you start it, in a sentence (shown to the user with the session)"},
            "source": {"type": "string", "description": "What you read that led to it: a file and section, an issue, a session"}
        }, "required": ["brief"]}),
    }
}

pub(super) fn context_load_spec() -> ToolSpec {
    ToolSpec {
        name: "context_load".into(),
        description: "Load workflow instructions or project documentation by reference, as listed in your context manifest (<pack>:<path>[#section] or project:<path>[#section]).".into(),
        parameters: json!({"type": "object", "properties": {
            "references": {"type": "array", "items": {"type": "string"}}
        }, "required": ["references"]}),
    }
}

pub(crate) fn memory_search_spec() -> ToolSpec {
    ToolSpec {
        name: "memory_search".into(),
        description: "Search saved memories (global, and this environment's Memory.md) by meaning words; returns names and descriptions."
            .into(),
        parameters: json!({"type": "object", "properties": {
            "query": {"type": "string"},
            "scope": {"type": "string", "enum": ["all", "global", "environment"], "description": "Where to look; all when left out"}
        }, "required": ["query"]}),
    }
}

pub(crate) fn memory_read_spec() -> ToolSpec {
    ToolSpec {
        name: "memory_read".into(),
        description: "Read a saved memory by name: a global entry or a section of this environment's Memory.md (the name Memory.md reads the whole file).".into(),
        parameters: json!({"type": "object", "properties": {
            "name": {"type": "string"},
            "scope": {"type": "string", "enum": ["all", "global", "environment"], "description": "Where to look; all when left out"}
        }, "required": ["name"]}),
    }
}

pub(crate) fn memory_remember_spec() -> ToolSpec {
    ToolSpec {
        name: "memory_remember".into(),
        description: "Save (or replace) a memory for future sessions.".into(),
        parameters: json!({"type": "object", "properties": {
            "name": {"type": "string", "description": "kebab-case, unique within the scope"},
            "description": {"type": "string", "description": "One line, used to decide relevance"},
            "type": {"type": "string", "enum": ["user", "feedback", "project", "reference", "lesson"]},
            "scope": {"type": "string", "enum": ["global", "environment"], "description": "environment writes this environment's Memory.md (what holds only here, including its projects); global holds for the user everywhere"},
            "body": {"type": "string", "description": "The fact; for feedback, include why and how to apply it"}
        }, "required": ["name", "description", "type", "scope", "body"]}),
    }
}

pub(crate) fn memory_edit_spec() -> ToolSpec {
    ToolSpec {
        name: "memory_edit".into(),
        description: "Replace one exact, unique string in a saved memory's body, as edit_file does; its description and type stay.".into(),
        parameters: json!({"type": "object", "properties": {
            "name": {"type": "string"},
            "old_string": {"type": "string"},
            "new_string": {"type": "string"},
            "scope": {"type": "string", "enum": ["global", "environment"], "description": "Where it is; the environment's first when left out"}
        }, "required": ["name", "old_string", "new_string"]}),
    }
}

pub(crate) fn memory_forget_spec() -> ToolSpec {
    ToolSpec {
        name: "memory_forget".into(),
        description: "Remove a memory that is wrong or obsolete. It is moved aside, not erased: the user can restore it.".into(),
        parameters: json!({"type": "object", "properties": {
            "name": {"type": "string"},
            "scope": {"type": "string", "enum": ["global", "environment"], "description": "Where it is; the environment's first when left out"}
        }, "required": ["name"]}),
    }
}

/// The tool that starts a plugin's servers.
pub(super) const ACTIVATE_PLUGIN: &str = "activate_plugin";

pub(super) fn activate_plugin_spec() -> ToolSpec {
    ToolSpec {
        name: ACTIVATE_PLUGIN.into(),
        description: "Activate a plugin from the plugin index by name: its servers start and its \
                      tools are offered from the next step."
            .into(),
        parameters: json!({"type": "object", "properties": {
            "name": {"type": "string"}
        }, "required": ["name"]}),
    }
}

pub(super) fn load_skill_spec() -> ToolSpec {
    ToolSpec {
        name: LOAD_SKILL.into(),
        description: "Load a skill's instructions by name (once per conversation), or one of its \
                      resource files with file."
            .into(),
        parameters: json!({"type": "object", "properties": {
            "name": {"type": "string"},
            "file": {"type": "string", "description": "A resource path inside the skill, as listed when it loads"}
        }, "required": ["name"]}),
    }
}

/// A protocol tool definition as the spec sent to the model.
pub(super) fn tool_spec(definition: contract::ToolDefinition) -> ToolSpec {
    ToolSpec {
        name: definition.name.into(),
        description: definition.description.into(),
        parameters: definition.parameters,
    }
}

pub(super) fn update_plan_spec() -> ToolSpec {
    tool_spec(contract::update_plan())
}

pub(super) fn spawn_agent_spec() -> ToolSpec {
    tool_spec(contract::spawn_agent())
}

pub(super) fn await_tasks_spec() -> ToolSpec {
    tool_spec(contract::await_tasks())
}

pub(super) fn steer_agent_spec() -> ToolSpec {
    tool_spec(contract::steer_agent())
}

pub(super) fn queue_agent_spec() -> ToolSpec {
    tool_spec(contract::queue_agent())
}

pub(super) fn list_agents_spec() -> ToolSpec {
    tool_spec(contract::list_agents())
}

pub(super) fn resume_agent_spec() -> ToolSpec {
    tool_spec(contract::resume_agent())
}

pub(super) fn keep_as_knowledge_spec() -> ToolSpec {
    tool_spec(contract::keep_as_knowledge())
}

pub(super) fn ask_agent_spec() -> ToolSpec {
    tool_spec(contract::ask_agent())
}

pub(super) fn list_knowledge_agents_spec() -> ToolSpec {
    tool_spec(contract::list_knowledge_agents())
}

pub(super) fn delegate_chunks_spec() -> ToolSpec {
    tool_spec(contract::delegate_chunks())
}

pub(super) fn ask_user_spec() -> ToolSpec {
    tool_spec(contract::ask_user())
}

pub(super) fn request_clarification_spec() -> ToolSpec {
    tool_spec(contract::request_clarification())
}

pub(super) fn update_state_spec() -> ToolSpec {
    tool_spec(contract::update_state())
}
