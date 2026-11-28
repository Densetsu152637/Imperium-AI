//! The browser's tab bookkeeping: which pages the browser lists, which of them are tabs the
//! session knows, and how page-controlled text (titles, URLs) is flattened before an agent
//! reads it. Pure functions, so the rules are tested without a browser.

use serde_json::Value;

/// A tab the session knows: a short label for the agent and the browser's id for the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TabRec {
    pub label: u32,
    /// The browser's target id.
    pub target: String,
}

/// A page the browser lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Listed {
    pub id: String,
    pub url: String,
    pub title: String,
}

/// What [`reconcile`] changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Reconciled {
    /// Tabs added because a page opened them: the label and the target id.
    pub opened: Vec<(u32, String)>,
    /// Targets that did not fit within the limit and are to be closed.
    pub excess: Vec<String>,
}

/// The pages in the `targetInfos` of a `Target.getTargets` reply: real pages only (not workers,
/// extensions or the debugger's own front end), in the browser's order.
pub(crate) fn listed(reply: &Value) -> Vec<Listed> {
    reply
        .as_array()
        .into_iter()
        .flatten()
        .filter(|target| target["type"] == "page")
        .filter_map(|target| {
            let id = target["targetId"].as_str().filter(|id| is_target_id(id))?;
            let url = target["url"].as_str().unwrap_or_default();
            if url.starts_with("devtools://") {
                return None;
            }
            Some(Listed {
                id: id.to_string(),
                url: url.to_string(),
                title: target["title"].as_str().unwrap_or_default().to_string(),
            })
        })
        .collect()
}

/// Whether `id` looks like a target id (letters, digits, `-` and `_`): anything else the browser
/// lists is not a page of ours.
pub(crate) fn is_target_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Brings `tabs` in line with `listed`: tabs the browser no longer lists are dropped, pages it
/// lists that are not tabs yet are added with fresh labels while there is room (`max` tabs in
/// all), and the ones that do not fit are reported for closing. Tabs keep their labels and
/// order.
pub(crate) fn reconcile(
    tabs: &mut Vec<TabRec>,
    next_label: &mut u32,
    listed: &[Listed],
    max: usize,
) -> Reconciled {
    tabs.retain(|tab| listed.iter().any(|page| page.id == tab.target));
    let mut changes = Reconciled::default();
    for page in listed {
        if tabs.iter().any(|tab| tab.target == page.id) {
            continue;
        }
        if tabs.len() >= max {
            changes.excess.push(page.id.clone());
            continue;
        }
        let label = *next_label;
        *next_label += 1;
        tabs.push(TabRec {
            label,
            target: page.id.clone(),
        });
        changes.opened.push((label, page.id.clone()));
    }
    changes
}

/// `text` on one line, at most `max` characters, with control characters replaced: a page
/// chooses its titles, URLs and suggested file names, and none of them may add lines to what
/// an agent reads.
pub(crate) fn one_line(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    imperium_api::truncate_chars(&flat, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn page(id: &str) -> Listed {
        Listed {
            id: id.into(),
            url: format!("http://x/{id}"),
            title: String::new(),
        }
    }

    fn rec(label: u32, target: &str) -> TabRec {
        TabRec {
            label,
            target: target.into(),
        }
    }

    #[test]
    fn only_real_pages_are_listed() {
        let reply = json!([
            {"targetId": "A", "type": "page", "url": "http://a/", "title": "A"},
            {"targetId": "W", "type": "service_worker", "url": "http://a/sw.js"},
            {"targetId": "D", "type": "page", "url": "devtools://devtools/bundled/inspector.html"},
            {"targetId": "bad id/..", "type": "page", "url": "http://b/"},
            {"targetId": "B", "type": "page", "url": "http://b/"}
        ]);
        let ids: Vec<String> = listed(&reply).into_iter().map(|page| page.id).collect();
        assert_eq!(ids, ["A", "B"]);
        assert!(listed(&json!({})).is_empty());
    }

    #[test]
    fn target_ids_cannot_change_a_debugger_url() {
        assert!(is_target_id("5F3A-9c_1"));
        for bad in ["", "a/b", "a?b", "a b", "..", "a#b", "a\\b", "é"] {
            assert!(!is_target_id(bad), "{bad}");
        }
        assert!(!is_target_id(&"a".repeat(129)));
    }

    #[test]
    fn tabs_a_page_closed_go_and_tabs_a_page_opened_arrive_with_fresh_labels() {
        let mut tabs = vec![rec(1, "A"), rec(2, "B")];
        let mut next = 3;
        let changes = reconcile(&mut tabs, &mut next, &[page("B"), page("C")], 8);
        assert_eq!(tabs, [rec(2, "B"), rec(3, "C")]);
        assert_eq!(changes.opened, [(3, "C".to_string())]);
        assert!(changes.excess.is_empty());
        assert_eq!(next, 4);
        // Nothing changed: nothing reported.
        let again = reconcile(&mut tabs, &mut next, &[page("B"), page("C")], 8);
        assert_eq!(again, Reconciled::default());
        // A label is never reused for another page.
        reconcile(&mut tabs, &mut next, &[page("B"), page("D")], 8);
        assert_eq!(tabs, [rec(2, "B"), rec(4, "D")]);
    }

    #[test]
    fn pages_beyond_the_limit_are_closed_not_adopted() {
        let mut tabs = vec![rec(1, "A"), rec(2, "B")];
        let mut next = 3;
        let listed = [page("A"), page("B"), page("C"), page("D"), page("E")];
        let changes = reconcile(&mut tabs, &mut next, &listed, 3);
        assert_eq!(tabs.len(), 3);
        assert_eq!(changes.opened, [(3, "C".to_string())]);
        assert_eq!(changes.excess, ["D", "E"]);
    }

    #[test]
    fn page_text_is_flattened_and_capped() {
        assert_eq!(one_line("a\nb\r\n\tc  d", 50), "a b c d");
        assert_eq!(one_line("x\u{1b}[31my", 50), "x [31my");
        assert_eq!(one_line("abcdefghij", 4), "abcd…");
        assert_eq!(one_line("", 4), "");
        // Not cut inside a multi-byte character.
        assert_eq!(one_line("ééééé", 2), "éé…");
    }
}
