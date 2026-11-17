//! Subagent recordings, client side: reading the file. The daemon answers subagents from them
//! (see `imperium_protocol::methods::EvalReply`).

use std::path::Path;

use anyhow::{Context, bail};
use imperium_protocol::methods::EvalReply;
use serde_json::Value;

/// Reads recordings from a file: a `{"replies": [...]}` object, or any object that holds one
/// under `subagent_replay` (so a task file can carry its own recordings).
pub fn load(path: &Path) -> anyhow::Result<Vec<EvalReply>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("could not read the recordings {}", path.display()))?;
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("{} is not valid JSON", path.display()))?;
    from_value(value).with_context(|| format!("recordings {}", path.display()))
}

/// The recordings in `value`, which is a recordings object or holds one under
/// `subagent_replay`.
pub fn from_value(value: Value) -> anyhow::Result<Vec<EvalReply>> {
    let value = match value {
        Value::Object(mut map) if !map.contains_key("replies") => map
            .remove("subagent_replay")
            .context("expected {\"replies\": [...]} or a \"subagent_replay\" object")?,
        other => other,
    };
    let Value::Object(mut map) = value else {
        bail!("the recordings must be an object with a \"replies\" list");
    };
    let replies = map
        .remove("replies")
        .context("the recordings have no \"replies\" list")?;
    serde_json::from_value(replies).context("the recordings are malformed")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_task_file_can_carry_its_recordings() {
        let value = json!({"prompt": "x", "subagent_replay": {"replies": [
            {"task_id": "t1", "report": "r"}
        ]}});
        assert_eq!(from_value(value).unwrap().len(), 1);
        assert!(from_value(json!({"prompt": "x"})).is_err());
        let once =
            from_value(json!({"replies": [{"role": "reduce", "report": "r", "once": true}]}));
        assert!(once.unwrap()[0].once);
    }
}
