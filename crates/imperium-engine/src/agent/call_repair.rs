//! Client-side handling of malformed tool calls from small local models (docs/03-agent-engine.md,
//! "Tool-call repair"). Three pure steps, each used at one point of the turn loop:
//!
//! 1. [`from_content`]: a reply with no `tool_calls` whose text holds a call the server did
//!    not parse (Qwen's `<tool_call>{json}</tool_call>`, or a bare JSON object with `name` and
//!    `arguments`) becomes a call. Only for local models.
//! 2. [`with_close_names`]: the result of a call to a tool that does not exist names the closest
//!    valid tools.
//! 3. [`vet`]: arguments that are not valid JSON get one repair attempt (code fences and
//!    trailing commas removed); what stays unparseable is left to the tool's own reader.

use std::sync::atomic::{AtomicU64, Ordering};

use imperium_api::{FunctionCall, ToolCall};
use serde_json::Value;

const OPEN: &str = "<tool_call>";
const CLOSE: &str = "</tool_call>";
/// Most names an unknown-tool error lists.
const CLOSE_NAMES: usize = 3;
/// Most names listed when none is close to the one asked for.
const LISTED_NAMES: usize = 12;

/// The calls written into `text`, and the text without them, for a reply the server left
/// without `tool_calls`. `names` are the tools offered (`call_tool` is one when it is offered).
///
/// A call is taken only when every part of the reply agrees: each `<tool_call>` block holds
/// `{"name": ..., "arguments": ...}` naming an offered tool, no block is inside a code fence or
/// right after a quote mark, and nothing but whitespace follows the last block (text before the
/// first one is a preamble and stays as the reply). Anything else is an answer that quotes a
/// call, or tool output echoed back, and gives no call. A reply that is one bare JSON object
/// naming an offered tool is also a call.
pub(crate) fn from_content(text: &str, names: &[String]) -> Option<(Vec<ToolCall>, String)> {
    let mut calls = Vec::new();
    let mut rest = String::new();
    let mut tail = text;
    let mut at = 0;
    while let Some(start) = tail.find(OPEN) {
        if quoted(text, at + start) {
            return None;
        }
        rest.push_str(&tail[..start]);
        let after = &tail[start + OPEN.len()..];
        let (inner, next) = match after.find(CLOSE) {
            Some(end) => (&after[..end], &after[end + CLOSE.len()..]),
            None => (after, ""),
        };
        let call = parse_call(inner).filter(|call| is_offered(call, names))?;
        calls.push(call);
        at += tail.len() - next.len();
        tail = next;
    }
    if calls.is_empty() {
        let bare = text.trim();
        if !bare.starts_with('{') {
            return None;
        }
        let call = parse_call(bare).filter(|call| is_offered(call, names))?;
        return Some((vec![call], String::new()));
    }
    if !tail.trim().is_empty() {
        return None;
    }
    Some((calls, rest.trim().to_string()))
}

fn is_offered(call: &ToolCall, names: &[String]) -> bool {
    names.contains(&call.function.name)
}

/// Whether the block opening at byte `at` of `text` is inside a code fence, or follows a quote
/// mark or a blockquote marker on its line.
fn quoted(text: &str, at: usize) -> bool {
    let before = &text[..at];
    if before.matches("```").count() % 2 == 1 {
        return true;
    }
    let line = before.rsplit('\n').next().unwrap_or("");
    let line = line.trim_end();
    line.trim_start().starts_with('>') || line.ends_with(['"', '\'', '`', '“', '‘'])
}

/// Ids of calls read from text, unique across the process whatever the conversation.
fn next_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!(
        "call_repaired_{nanos:x}_{}",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// A call from `{"name": ..., "arguments": ...}` (`parameters` is accepted for `arguments`).
fn parse_call(text: &str) -> Option<ToolCall> {
    let value: Value = serde_json::from_str(strip_fences(text)).ok()?;
    let object = value.as_object()?;
    let name = object.get("name")?.as_str()?.trim();
    if name.is_empty() {
        return None;
    }
    let arguments = match object.get("arguments").or_else(|| object.get("parameters")) {
        None | Some(Value::Null) => "{}".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    };
    Some(ToolCall {
        id: next_id(),
        kind: "function".into(),
        function: FunctionCall {
            name: name.to_string(),
            arguments,
        },
    })
}

/// `text` without a surrounding Markdown code fence (``` or ```json) and whitespace.
fn strip_fences(text: &str) -> &str {
    let text = text.trim();
    let Some(body) = text.strip_prefix("```") else {
        return text;
    };
    let body = body.split_once('\n').map_or(body, |(_, rest)| rest);
    body.trim_end().strip_suffix("```").unwrap_or(body).trim()
}

/// `arguments` as valid JSON after one repair: fences stripped, trailing commas removed.
/// `None` when it still does not parse. Valid input is returned as it is.
pub(crate) fn repair_arguments(arguments: &str) -> Option<String> {
    if serde_json::from_str::<Value>(arguments).is_ok() {
        return Some(arguments.to_string());
    }
    let stripped = strip_fences(arguments);
    let fixed = remove_trailing_commas(stripped);
    serde_json::from_str::<Value>(&fixed)
        .ok()
        .map(|value| value.to_string())
}

/// `text` without a comma that is followed (past whitespace) by `}` or `]`, outside strings.
fn remove_trailing_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let (mut in_string, mut escaped) = (false, false);
    for (index, &c) in chars.iter().enumerate() {
        if in_string {
            out.push(c);
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            ',' if chars[index + 1..]
                .iter()
                .find(|next| !next.is_whitespace())
                .is_some_and(|next| matches!(next, '}' | ']')) => {}
            _ => out.push(c),
        }
    }
    out
}

/// The call with its arguments repaired once when they are not JSON (a code fence and trailing
/// commas removed). Arguments that stay unparseable are left as they are: each tool reads
/// them as leniently as it can and answers with its own error when it cannot.
pub(crate) fn vet(mut call: ToolCall) -> ToolCall {
    let arguments = call.function.arguments.as_str();
    // Empty arguments are an empty object, as the tools read them.
    if !arguments.trim().is_empty()
        && serde_json::from_str::<Value>(arguments).is_err()
        && let Some(fixed) = repair_arguments(arguments)
    {
        call.function.arguments = fixed;
    }
    call
}

/// `output`, the result of a call to `name` that failed as an unknown tool, with the closest
/// of the offered `names` added; `None` for any other result (a refusal by a profile, a hook
/// or a permission keeps its own message) or when no names were offered to point at.
pub(crate) fn with_close_names(output: &str, name: &str, names: &[String]) -> Option<String> {
    // Built-in tools say `unknown tool "name"`, MCP and plugin tools `unknown tool name`.
    if names.is_empty() || !output.starts_with("error: unknown tool") || !output.contains(name) {
        return None;
    }
    let close = closest_names(name, names);
    let hint = if close.is_empty() {
        let listed: Vec<&str> = names
            .iter()
            .take(LISTED_NAMES)
            .map(String::as_str)
            .collect();
        format!("Tools you can call include: {}.", listed.join(", "))
    } else {
        format!("Closest valid names: {}.", close.join(", "))
    };
    Some(format!("{output}\n{hint}"))
}

/// Up to three of `names` close to `name`: a small edit distance or one containing the other.
pub(crate) fn closest_names(name: &str, names: &[String]) -> Vec<String> {
    if name.is_empty() {
        return Vec::new();
    }
    let wanted = name.to_lowercase();
    let mut near: Vec<(usize, &str)> = names
        .iter()
        .filter_map(|candidate| {
            let other = candidate.to_lowercase();
            let distance = edit_distance(&wanted, &other);
            let reach = (wanted.chars().count().max(other.chars().count()) / 3).max(2);
            (distance <= reach || other.contains(&wanted) || wanted.contains(&other))
                .then_some((distance, candidate.as_str()))
        })
        .collect();
    near.sort_unstable();
    near.into_iter()
        .take(CLOSE_NAMES)
        .map(|(_, name)| name.to_string())
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        ["read_file", "write_file", "run_command", "search"]
            .map(String::from)
            .to_vec()
    }

    fn call(name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: "1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: arguments.into(),
            },
        }
    }

    #[test]
    fn a_tagged_call_in_the_text_becomes_a_call() {
        let text = "I will look.\n<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"a.rs\"}}\n</tool_call>";
        let (calls, rest) = from_content(text, &names()).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(calls[0].function.arguments, r#"{"path":"a.rs"}"#);
        assert_eq!(rest, "I will look.");
    }

    #[test]
    fn several_tagged_calls_and_a_missing_close_tag_are_read() {
        let text = "<tool_call>{\"name\":\"search\",\"arguments\":{}}</tool_call><tool_call>{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"b\\\"}\"}";
        let (calls, rest) = from_content(text, &names()).unwrap();
        assert_eq!(calls.len(), 2);
        assert_ne!(calls[0].id, calls[1].id);
        assert_eq!(calls[1].function.arguments, r#"{"path":"b"}"#);
        assert!(rest.is_empty());
    }

    #[test]
    fn a_tagged_call_to_a_tool_that_is_not_offered_is_not_a_call() {
        let text = "<tool_call>{\"name\":\"read_fle\",\"arguments\":{}}</tool_call>";
        assert!(from_content(text, &names()).is_none());
        // `call_tool` is an offered name like any other.
        let mut offered = names();
        offered.push("call_tool".into());
        let text = "<tool_call>{\"name\":\"call_tool\",\"arguments\":{\"name\":\"x\"}}</tool_call>";
        assert!(from_content(text, &offered).is_some());
    }

    #[test]
    fn quoted_echoed_and_fenced_blocks_are_not_calls() {
        let block = "<tool_call>{\"name\":\"search\",\"arguments\":{}}</tool_call>";
        // An example the model quotes in its answer.
        let quoted = format!("The format is \"{block}\" as shown.");
        assert!(from_content(&quoted, &names()).is_none());
        let inline = format!("Write `{block}` to call a tool.");
        assert!(from_content(&inline, &names()).is_none());
        let blockquote = format!("> {block}");
        assert!(from_content(&blockquote, &names()).is_none());
        // A tool output echoed back, with text after the block.
        let echoed = format!(
            "The file said:
{block}
and that is all it said."
        );
        assert!(from_content(&echoed, &names()).is_none());
        // Inside a code fence, even as the last thing in the reply.
        let fenced = format!(
            "Example:
```
{block}
```"
        );
        assert!(from_content(&fenced, &names()).is_none());
        let fenced_open = format!(
            "Example:
```xml
{block}"
        );
        assert!(from_content(&fenced_open, &names()).is_none());
        // The same block after a preamble, with only whitespace after it, is a call.
        let real = format!(
            "Let me search.
{block}

"
        );
        let (calls, rest) = from_content(&real, &names()).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(rest, "Let me search.");
    }

    #[test]
    fn a_bare_json_call_must_name_an_offered_tool() {
        let bare = r#"{"name": "search", "arguments": {"query": "x"}}"#;
        let (calls, rest) = from_content(bare, &names()).unwrap();
        assert_eq!(calls[0].function.name, "search");
        assert!(rest.is_empty());
        // Never inside a code fence.
        let fenced = format!(
            "```json
{bare}
```"
        );
        assert!(from_content(&fenced, &names()).is_none());
        // JSON that merely has a `name` field is an answer, not a call.
        assert!(from_content(r#"{"name": "Alice", "arguments": {}}"#, &names()).is_none());
        assert!(from_content("plain answer", &names()).is_none());
        assert!(from_content("<tool_call>not json</tool_call>", &names()).is_none());
    }

    #[test]
    fn ids_of_calls_read_from_text_never_repeat() {
        let text = "<tool_call>{\"name\":\"search\",\"arguments\":{}}</tool_call>";
        let first = from_content(text, &names()).unwrap().0;
        let second = from_content(text, &names()).unwrap().0;
        assert_ne!(first[0].id, second[0].id);
    }

    #[test]
    fn an_unknown_tool_result_gets_the_closest_names() {
        let output = "error: unknown tool \"read_fil\"";
        let hinted = with_close_names(output, "read_fil", &names()).unwrap();
        assert!(hinted.starts_with(output), "{hinted}");
        assert!(
            hinted.contains("Closest valid names: read_file"),
            "{hinted}"
        );
        // MCP and plugin tools say `unknown tool name`, without quotes.
        let mcp = with_close_names("error: unknown tool search_", "search_", &names()).unwrap();
        assert!(mcp.contains("Closest valid names: search"), "{mcp}");
        // Only a result that starts as an unknown-tool error is touched.
        assert!(
            with_close_names(
                "note: unknown tool \"x\" appears in the file",
                "x",
                &names()
            )
            .is_none()
        );
        assert!(
            with_close_names("the file says error: unknown tool \"x\"", "x", &names()).is_none()
        );
        let far = with_close_names("error: unknown tool \"zzzzzzzz\"", "zzzzzzzz", &names());
        assert!(far.unwrap().contains("include: read_file, write_file"));
        // Other results, and a list with nothing to point at, are left alone.
        assert!(with_close_names("error: permission denied", "read_fil", &names()).is_none());
        assert!(with_close_names(output, "read_fil", &[]).is_none());
    }

    #[test]
    fn near_names_are_found_by_edit_distance() {
        let close = closest_names("run_comand", &names());
        assert_eq!(close, vec!["run_command".to_string()]);
        assert!(closest_names("", &names()).is_empty());
    }

    #[test]
    fn arguments_with_fences_or_trailing_commas_are_repaired_once() {
        let fenced = "```json\n{\"path\": \"a.rs\"}\n```";
        let fixed = vet(call("read_file", fenced));
        assert_eq!(fixed.function.arguments, r#"{"path":"a.rs"}"#);
        let commas = r#"{"a": [1, 2,], "b": "x,}", }"#;
        let fixed = vet(call("read_file", commas));
        let value: Value = serde_json::from_str(&fixed.function.arguments).unwrap();
        assert_eq!(value["a"], serde_json::json!([1, 2]));
        assert_eq!(value["b"], "x,}");
    }

    #[test]
    fn valid_and_empty_arguments_are_left_alone() {
        let spaced = r#"{ "path" : "a" }"#;
        let kept = vet(call("read_file", spaced));
        assert_eq!(kept.function.arguments, spaced);
        let empty = vet(call("read_file", ""));
        assert_eq!(empty.function.arguments, "");
    }

    #[test]
    fn arguments_that_cannot_be_repaired_are_left_for_the_tool_to_read() {
        let raw = "{path: a.rs";
        assert_eq!(vet(call("read_file", raw)).function.arguments, raw);
    }
}
