//! Front matter in Markdown files (`SKILL.md`, commands): a `---` block of `key: value` lines.
//! Only flat string values are read, which is all skills and commands use.

use std::collections::BTreeMap;

/// Splits a document into its front matter fields and its body.
pub fn front_matter(text: &str) -> (BTreeMap<String, String>, &str) {
    let mut fields = BTreeMap::new();
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return (fields, text);
    };
    let Some(end) = rest
        .match_indices("\n---")
        .map(|(index, _)| index)
        .find(|&index| {
            let after = &rest[index + 4..];
            after.is_empty() || after.starts_with('\n') || after.starts_with("\r\n")
        })
    else {
        return (fields, text);
    };
    for line in rest[..end].lines() {
        if let Some((key, value)) = line.split_once(':')
            && !key.starts_with([' ', '\t', '#'])
        {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            fields.insert(key.trim().to_string(), value.to_string());
        }
    }
    let body = rest[end + 4..].trim_start_matches(['\r', '\n']);
    (fields, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_and_body_are_separated() {
        let (fields, body) = front_matter(
            "---\nname: pdf\ndescription: \"Fill PDF forms\"\n  nested: x\n---\n\n# Body\n",
        );
        assert_eq!(fields["name"], "pdf");
        assert_eq!(fields["description"], "Fill PDF forms");
        assert!(!fields.contains_key("nested"));
        assert_eq!(body, "# Body\n");
        let (fields, body) = front_matter("no front matter");
        assert!(fields.is_empty());
        assert_eq!(body, "no front matter");
    }
}
