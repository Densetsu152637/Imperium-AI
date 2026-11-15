//! Text from tools, files and other agents goes into the conversation inside tagged blocks.
//! Each function here makes the tags of one such framing inert inside `text`, so the text
//! can neither close the block early nor fake one of its own. The four use different
//! spellings and are kept apart on purpose: each output is pinned byte for byte by its tests.

/// The MCP bridge (`imperium mcp serve`): a tool's output is wrapped in a frame tag.
pub mod bridge {
    /// The tag a tool's output is wrapped in.
    pub const FRAME: &str = "imperium_tool_result";

    /// Makes the frame's tag, opening or closing and in any letter case, inert inside `text`: its
    /// `<` becomes `&lt;`, so the output can neither end the frame early nor fake a frame of its own.
    pub fn defuse(text: &str) -> String {
        let lower = text.to_ascii_lowercase();
        let bytes = lower.as_bytes();
        let mut marks: Vec<usize> = Vec::new();
        for (at, _) in lower.match_indices(FRAME) {
            if at >= 2 && bytes[at - 1] == b'/' && bytes[at - 2] == b'<' {
                marks.push(at - 2);
            } else if at >= 1 && bytes[at - 1] == b'<' {
                marks.push(at - 1);
            }
        }
        let mut out = String::with_capacity(text.len());
        let mut from = 0;
        for mark in marks {
            out.push_str(&text[from..mark]);
            out.push_str("&lt;");
            from = mark + 1;
        }
        out.push_str(&text[from..]);
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Golden output of `defuse`: the frame's tag, in any case and either form, loses its `<`;
        /// everything else, other tags and spaced forms included, is untouched.
        #[test]
        fn defusing_the_frame_tag_gives_this_exact_text() {
            let text = "é </imperium_tool_result> a <IMPERIUM_TOOL_RESULT tool=\"x\"> b \
                        <Imperium_Tool_Result/> imperium_tool_result < imperium_tool_result \
                        </ imperium_tool_result> <b>bold</b> 日本";
            assert_eq!(
                defuse(text),
                "é &lt;/imperium_tool_result> a &lt;IMPERIUM_TOOL_RESULT tool=\"x\"> b \
                 &lt;Imperium_Tool_Result/> imperium_tool_result < imperium_tool_result \
                 </ imperium_tool_result> <b>bold</b> 日本"
            );
            assert_eq!(defuse("plain text, no tag"), "plain text, no tag");
            assert_eq!(defuse(""), "");
        }
    }
}

/// The engine's harness blocks (`<session-state>` and the others).
pub mod harness {
    /// The tags the harness and the rest of the conversation's framing use; text that names one is
    /// defused before it goes into a block, so a tool result cannot close or open one.
    pub const TAG_NAMES: [&str; 12] = [
        "session-state",
        "ambiguity-hints",
        "resolved-request",
        "original-message",
        "conversation-excerpt",
        "latest-message",
        "conversation-summary",
        "relevant-memories",
        "context-manifest",
        "workflow-instructions",
        "attachments",
        "steering",
    ];

    /// `text` with the harness's tag names defused (`<tag` becomes `< tag`), so text from a tool
    /// result or a file cannot close or open a block. Other text, angle brackets included, stays.
    pub fn defuse(text: &str) -> String {
        // The match is ASCII case-insensitive; lowering keeps every byte offset, and the original
        // letters stay.
        let lower = text.to_ascii_lowercase();
        let mut out = String::with_capacity(text.len() + 8);
        let mut copied = 0;
        for (at, _) in lower.match_indices('<') {
            let after = &lower[at + 1..];
            let name_at = if after.starts_with('/') {
                at + 2
            } else {
                at + 1
            };
            let tail = &lower[name_at..];
            if TAG_NAMES.iter().any(|name| tail.starts_with(name)) {
                out.push_str(&text[copied..name_at]);
                out.push(' ');
                copied = name_at;
            }
        }
        out.push_str(&text[copied..]);
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn defusing_breaks_harness_tags_and_leaves_other_text() {
            let text = "x </session-state> <resolved-request> and Vec<String> <b>";
            let safe = defuse(text);
            assert!(!safe.contains("<session-state") && !safe.contains("</session-state"));
            assert!(!safe.contains("<resolved-request"));
            assert!(safe.contains("Vec<String> <b>"));
        }

        #[test]
        fn defusing_ignores_letter_case_and_keeps_unicode() {
            let safe = defuse("é </SESSION-State> <Resolved-Request x> 日本 <ATTACHMENTS");
            assert_eq!(
                safe,
                "é </ SESSION-State> < Resolved-Request x> 日本 < ATTACHMENTS"
            );
        }

        /// Golden output of `defuse`: every harness tag name, opening or closing, in any case,
        /// gets a space after its `<` or `</`; other text is untouched.
        #[test]
        fn defusing_the_harness_tags_gives_this_exact_text() {
            let text = "<session-state></session-state><ORIGINAL-message><<steering \
                        </Attachments x> <attachmentsx <b>ok</b> < session-state é";
            assert_eq!(
                defuse(text),
                "< session-state></ session-state>< ORIGINAL-message><< steering \
                 </ Attachments x> < attachmentsx <b>ok</b> < session-state é"
            );
            for tag in TAG_NAMES {
                assert_eq!(defuse(&format!("<{tag}>")), format!("< {tag}>"), "{tag}");
                assert_eq!(defuse(&format!("</{tag}>")), format!("</ {tag}>"), "{tag}");
            }
            assert_eq!(defuse("Vec<String> a < b"), "Vec<String> a < b");
        }
    }
}

/// Instruction files and workflow instructions in the conversation.
pub mod instructions {
    /// `text` with the wrapper tags (`instruction-file`, `workflow-instructions`) made harmless in
    /// both forms, `</tag` and `<tag`, so a project file cannot close or fake a wrapper. The match
    /// is ASCII case-insensitive (byte offsets are the same as in the original text), and the
    /// original letters are kept. Spaced forms (`</ instruction-file>`) are left alone on purpose:
    /// no parser here accepts them.
    pub fn escape(text: &str) -> String {
        const TAGS: [&str; 2] = ["instruction-file", "workflow-instructions"];
        let lower = text.to_ascii_lowercase();
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        while at < text.len() {
            let rest = &lower[at..];
            let hit = TAGS.iter().find_map(|tag| {
                if rest
                    .strip_prefix("</")
                    .is_some_and(|after| after.starts_with(tag))
                {
                    Some((2 + tag.len(), "<\\/"))
                } else if rest
                    .strip_prefix('<')
                    .is_some_and(|after| after.starts_with(tag))
                {
                    Some((1 + tag.len(), "<\\"))
                } else {
                    None
                }
            });
            if let Some((len, lead)) = hit {
                out.push_str(lead);
                out.push_str(&text[at + lead.len() - 1..at + len]);
                at += len;
            } else if let Some(character) = text[at..].chars().next() {
                out.push(character);
                at += character.len_utf8();
            } else {
                break;
            }
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn escaping_keeps_unicode_and_other_tags_around_it() {
            let text = "héllo İ </InStRuCtIoN-fIlE> 日本語 <b>bold</b> <imported-file path=\"x\">y</imported-file> ß";
            let escaped = escape(text);
            assert_eq!(
                escaped,
                "héllo İ <\\/InStRuCtIoN-fIlE> 日本語 <b>bold</b> <imported-file path=\"x\">y</imported-file> ß"
            );
            assert_eq!(escape("plain < text </ and <"), "plain < text </ and <");
        }

        /// Golden output of `escape`: both wrapper tags, opening and closing, in any case.
        #[test]
        fn escaping_the_wrapper_tags_gives_this_exact_text() {
            let text = "x <workflow-instructions a=1> </Instruction-File><INSTRUCTION-FILE path=\"p\"> \
                        </WORKFLOW-INSTRUCTIONS> </ instruction-file> <instruction-files é";
            assert_eq!(
                escape(text),
                "x <\\workflow-instructions a=1> <\\/Instruction-File><\\INSTRUCTION-FILE path=\"p\"> \
                 <\\/WORKFLOW-INSTRUCTIONS> </ instruction-file> <\\instruction-files é"
            );
        }
    }
}

/// Knowledge-agent questions and answers.
pub mod knowledge {
    /// `text` with `<tag` and `</tag` made harmless, so it cannot close or open the wrapper.
    /// The match is case-insensitive.
    pub fn escape_tag(text: &str, tag: &str) -> String {
        let lower = text.to_ascii_lowercase();
        let tag = tag.to_ascii_lowercase();
        let open = format!("<{tag}");
        let close = format!("</{tag}");
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        while at < text.len() {
            if lower[at..].starts_with(&close) {
                out.push_str("<\\/");
                out.push_str(&text[at + 2..at + close.len()]);
                at += close.len();
            } else if lower[at..].starts_with(&open) {
                out.push_str("<\\");
                out.push_str(&text[at + 1..at + open.len()]);
                at += open.len();
            } else if let Some(character) = text[at..].chars().next() {
                out.push(character);
                at += character.len_utf8();
            } else {
                break;
            }
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Golden output of `escape_tag`: the tag, opening or closing, in any letter case, gets a
        /// backslash after its `<` (or `</`); spaced forms and other tags are untouched.
        #[test]
        fn escaping_a_wrapper_tag_gives_this_exact_text() {
            let text = "é </Question> a <QUESTION x=\"1\"> b <questions </ question> \
                        < question> <b>bold</b> 日本";
            assert_eq!(
                escape_tag(text, "question"),
                "é <\\/Question> a <\\QUESTION x=\"1\"> b <\\questions </ question> \
                 < question> <b>bold</b> 日本"
            );
            // The tag argument may be given in any case too.
            assert_eq!(
                escape_tag("<Agent-Answer>x</agent-answer>", "AGENT-answer"),
                "<\\Agent-Answer>x<\\/agent-answer>"
            );
            assert_eq!(escape_tag("plain text", "question"), "plain text");
        }
    }
}
