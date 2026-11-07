//! Turns an HTML page into readable text: scripts, styles, and markup removed, block elements
//! on their own lines, entities decoded. Enough for an agent to read documentation and articles.

/// Elements whose content is never text.
const SKIPPED: [&str; 6] = ["script", "style", "noscript", "svg", "template", "head"];
/// Elements that start a new line.
const BLOCKS: [&str; 22] = [
    "p",
    "div",
    "br",
    "li",
    "ul",
    "ol",
    "tr",
    "table",
    "section",
    "article",
    "header",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "pre",
    "blockquote",
    "hr",
    "nav",
];

/// The page's `<title>`, if any.
pub fn title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let open_end = start + lower[start..].find('>')? + 1;
    let close = open_end + lower[open_end..].find("</title")?;
    let text = decode(html[open_end..close].trim());
    (!text.is_empty()).then_some(text)
}

/// The visible text of an HTML document.
pub fn to_text(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        out.push_str(&decode(&rest[..open]));
        let after = &rest[open + 1..];
        if let Some(comment) = after.strip_prefix("!--") {
            rest = comment.find("-->").map_or("", |end| &comment[end + 3..]);
            continue;
        }
        let Some(close) = after.find('>') else {
            rest = "";
            break;
        };
        let tag = &after[..close];
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        rest = &after[close + 1..];
        if SKIPPED.contains(&name.as_str()) && !tag.starts_with('/') && !tag.ends_with('/') {
            let lower = rest.to_ascii_lowercase();
            let end_tag = format!("</{name}");
            rest = match lower.find(&end_tag) {
                Some(end) => {
                    let after_end = &rest[end..];
                    after_end.find('>').map_or("", |gt| &after_end[gt + 1..])
                }
                None => "",
            };
            continue;
        }
        if BLOCKS.contains(&name.as_str()) {
            out.push('\n');
        } else if name == "td" || name == "th" {
            out.push('\t');
        }
    }
    out.push_str(&decode(rest));
    tidy(&out)
}

/// Collapses runs of spaces within lines and blank lines between them.
fn tidy(text: &str) -> String {
    let mut lines = Vec::new();
    let mut blank = false;
    for line in text.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            if !blank && !lines.is_empty() {
                lines.push(String::new());
            }
            blank = true;
        } else {
            lines.push(line);
            blank = false;
        }
    }
    lines.join("\n").trim().to_string()
}

/// Decodes the common named entities and numeric references.
fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let Some(semi) = after.find(';').filter(|semi| *semi <= 10) else {
            out.push('&');
            rest = after;
            continue;
        };
        let entity = &after[..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            "hellip" => Some('…'),
            "copy" => Some('©'),
            numeric => numeric
                .strip_prefix("#x")
                .or_else(|| numeric.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| numeric.strip_prefix('#').and_then(|n| n.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_become_readable_text() {
        let html = r#"<!doctype html><html><head><title>Rust &amp; Tokio</title>
            <style>p { color: red }</style><script>var x = "<p>no</p>";</script></head>
            <body><nav>Home</nav><h1>Async   Rust</h1><!-- hidden --><p>Tasks are <b>cheap</b>.</p>
            <ul><li>one</li><li>two&nbsp;&#x2713;</li></ul><table><tr><td>a</td><td>b</td></tr></table></body></html>"#;
        assert_eq!(title(html).as_deref(), Some("Rust & Tokio"));
        let text = to_text(html);
        // Blocks are separated by blank lines.
        assert_eq!(
            text,
            "Home\n\nAsync Rust\n\nTasks are cheap.\n\none\n\ntwo ✓\n\na b"
        );
        assert!(!text.contains("color") && !text.contains("var x"));
    }

    #[test]
    fn stray_ampersands_survive() {
        assert_eq!(
            decode("Q&A & more &unknown; &#65;"),
            "Q&A & more &unknown; A"
        );
    }
}
