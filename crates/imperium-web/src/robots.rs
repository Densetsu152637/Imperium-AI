//! `robots.txt` handling for `web_fetch`: parsed rules per host, cached in memory.
//!
//! Rules follow RFC 9309: the group naming the `Imperium` product token wins, `*` is the
//! fallback, the longest matching `Allow` or `Disallow` pattern decides (`Allow` on a tie), `*`
//! matches any run of characters and a trailing `$` anchors the end. `Crawl-delay` is not part of
//! the RFC and is ignored: `web_fetch` reads one page a call on the agent's request, never
//! crawls. `Sitemap` lines are ignored too.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use url::Url;

/// The product token groups are matched against.
pub const PRODUCT_TOKEN: &str = "imperium";
/// Longest `robots.txt` read; the rest is ignored (RFC 9309 asks parsers for at least 500 KiB).
const MAX_BYTES: usize = 512 * 1024;
/// How long a host's rules are reused.
const RULES_TTL: Duration = Duration::from_secs(3600);
/// How long an unreachable `robots.txt` is remembered, so a down host is not asked every call.
const UNREACHABLE_TTL: Duration = Duration::from_secs(60);
/// Hosts remembered at once; the table is cleared when it fills.
const MAX_HOSTS: usize = 256;

/// What a host's `robots.txt` says about this agent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Rules {
    /// `(allow, pattern)` of the groups that apply; empty means everything is allowed.
    rules: Vec<(bool, String)>,
}

struct Group {
    agents: Vec<String>,
    rules: Vec<(bool, String)>,
}

impl Rules {
    /// Rules that allow everything (a missing `robots.txt`).
    pub fn allow_all() -> Self {
        Rules::default()
    }

    /// Rules that allow nothing (an unreachable `robots.txt` when failing closed).
    pub fn deny_all() -> Self {
        Rules {
            rules: vec![(false, "/".into())],
        }
    }

    /// The rules in `text` that apply to the `agent` product token.
    pub fn parse(text: &str, agent: &str) -> Self {
        let agent = agent.to_ascii_lowercase();
        let mut groups: Vec<Group> = Vec::new();
        let mut collecting_agents = false;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match key.trim().to_ascii_lowercase().as_str() {
                "user-agent" => {
                    if !collecting_agents {
                        groups.push(Group {
                            agents: Vec::new(),
                            rules: Vec::new(),
                        });
                    }
                    collecting_agents = true;
                    if let Some(group) = groups.last_mut() {
                        group.agents.push(value.to_ascii_lowercase());
                    }
                }
                name @ ("allow" | "disallow") => {
                    collecting_agents = false;
                    // A rule before any user-agent line belongs to no group; an empty value is
                    // no rule (`Disallow:` allows everything).
                    if let Some(group) = groups.last_mut()
                        && !value.is_empty()
                    {
                        group.rules.push((name == "allow", value.to_string()));
                    }
                }
                _ => {}
            }
        }
        let named = |group: &&Group| {
            group
                .agents
                .iter()
                .any(|name| name != "*" && !name.is_empty() && agent.contains(name.as_str()))
        };
        let star = |group: &&Group| group.agents.iter().any(|name| name == "*");
        let mut chosen: Vec<&Group> = groups.iter().filter(named).collect();
        if chosen.is_empty() {
            chosen = groups.iter().filter(star).collect();
        }
        Rules {
            rules: chosen
                .into_iter()
                .flat_map(|group| group.rules.iter().cloned())
                .collect(),
        }
    }

    /// Whether `path` (with its query, as in a request line) may be fetched.
    pub fn allows(&self, path: &str) -> bool {
        let mut best: Option<(usize, bool)> = None;
        for (allow, pattern) in &self.rules {
            if !pattern_matches(pattern, path) {
                continue;
            }
            let length = pattern.len();
            best = match best {
                Some((len, _)) if len > length => best,
                Some((len, true)) if len == length => best,
                _ => Some((length, *allow)),
            };
        }
        best.is_none_or(|(_, allow)| allow)
    }
}

/// `*` matches any run of characters; a trailing `$` anchors the end; otherwise a prefix match.
fn pattern_matches(pattern: &str, path: &str) -> bool {
    let (pattern, anchored) = match pattern.strip_suffix('$') {
        Some(rest) => (rest, true),
        None => (pattern, false),
    };
    let parts: Vec<&str> = pattern.split('*').collect();
    let first = parts[0];
    if !path.starts_with(first) {
        return false;
    }
    let mut position = first.len();
    if parts.len() == 1 {
        return !anchored || path.len() == position;
    }
    let last = parts[parts.len() - 1];
    for part in &parts[1..parts.len() - 1] {
        match path[position..].find(part) {
            Some(found) => position += found + part.len(),
            None => return false,
        }
    }
    if anchored {
        path.len() >= position + last.len() && path.ends_with(last)
    } else {
        path[position..].contains(last)
    }
}

/// Per-host `robots.txt` rules, fetched on first use and kept in memory.
#[derive(Debug)]
pub struct Robots {
    /// Treat a `robots.txt` that cannot be read (a server error or no connection) as
    /// disallowing everything; otherwise as allowing everything.
    fail_closed: bool,
    hosts: Mutex<HashMap<String, (Instant, Arc<Rules>)>>,
}

impl Robots {
    pub fn new(fail_closed: bool) -> Self {
        Robots {
            fail_closed,
            hosts: Mutex::new(HashMap::new()),
        }
    }

    /// Whether `url` may be fetched. Loads the host's `robots.txt` when it is not held yet: a
    /// 2xx answer is parsed, a 4xx answer (no file, or not for us to read) allows everything, and
    /// a 5xx answer or a failed connection follows the fail-open or fail-closed choice.
    pub async fn allowed(&self, http: &reqwest::Client, url: &Url) -> bool {
        let Some(host) = url.host_str() else {
            return true;
        };
        let origin = match url.port() {
            Some(port) => format!("{}://{host}:{port}", url.scheme()),
            None => format!("{}://{host}", url.scheme()),
        };
        let held = self
            .hosts
            .lock()
            .ok()
            .and_then(|hosts| hosts.get(&origin).cloned())
            .filter(|(until, _)| *until > Instant::now())
            .map(|(_, rules)| rules);
        let rules = match held {
            Some(rules) => rules,
            None => {
                let (rules, ttl) = self.load(http, &origin).await;
                let rules = Arc::new(rules);
                if let Ok(mut hosts) = self.hosts.lock() {
                    if hosts.len() >= MAX_HOSTS {
                        hosts.clear();
                    }
                    hosts.insert(origin, (Instant::now() + ttl, rules.clone()));
                }
                rules
            }
        };
        let mut target = url.path().to_string();
        if let Some(query) = url.query() {
            target.push('?');
            target.push_str(query);
        }
        rules.allows(&target)
    }

    async fn load(&self, http: &reqwest::Client, origin: &str) -> (Rules, Duration) {
        let unreachable = || {
            let rules = if self.fail_closed {
                Rules::deny_all()
            } else {
                Rules::allow_all()
            };
            (rules, UNREACHABLE_TTL)
        };
        let Ok(response) = http.get(format!("{origin}/robots.txt")).send().await else {
            return unreachable();
        };
        let status = response.status();
        if status.is_success() {
            return match response.bytes().await {
                Ok(bytes) => {
                    let bytes = &bytes[..bytes.len().min(MAX_BYTES)];
                    let text = String::from_utf8_lossy(bytes);
                    (Rules::parse(&text, PRODUCT_TOKEN), RULES_TTL)
                }
                Err(_) => unreachable(),
            };
        }
        if status.is_client_error() {
            (Rules::allow_all(), RULES_TTL)
        } else {
            unreachable()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(text: &str) -> Rules {
        Rules::parse(text, "Imperium/0.34.0")
    }

    #[test]
    fn disallow_blocks_a_prefix_and_allow_carves_out() {
        let rules = rules(
            "User-agent: *\nDisallow: /private\nAllow: /private/open\nDisallow: /tmp/*.zip$\n",
        );
        assert!(rules.allows("/"));
        assert!(rules.allows("/public/page"));
        assert!(!rules.allows("/private"));
        assert!(!rules.allows("/private/secret?x=1"));
        assert!(rules.allows("/private/open/page"));
        assert!(!rules.allows("/tmp/a/b.zip"));
        assert!(rules.allows("/tmp/a/b.zip.txt"));
    }

    #[test]
    fn the_named_group_beats_the_star_group_and_ties_allow() {
        let text = "User-agent: *\nDisallow: /\n\nUser-agent: OtherBot\nDisallow: /other\n\n\
                    User-agent: Imperium\nUser-agent: Another\nDisallow: /named\nAllow: /named\n";
        let ours = rules(text);
        assert!(ours.allows("/anything"), "the star group does not apply");
        assert!(ours.allows("/named"), "an equal-length Allow wins");
        let other = Rules::parse(text, "OtherBot");
        assert!(!other.allows("/other"));
        assert!(other.allows("/elsewhere"));
        let unknown = Rules::parse(text, "Somebody");
        assert!(!unknown.allows("/anything"), "the star group applies");
    }

    #[test]
    fn crawl_delay_sitemap_comments_and_empty_rules_are_ignored() {
        let rules = rules(
            "# c\nSitemap: https://x/s.xml\nUser-agent: * # all\nCrawl-delay: 30\nDisallow:\n\
             Disallow: /a # note\n",
        );
        assert!(rules.allows("/"));
        assert!(!rules.allows("/a/b"));
    }

    #[test]
    fn deny_all_allow_all_and_missing_groups() {
        assert!(!Rules::deny_all().allows("/"));
        assert!(Rules::allow_all().allows("/x"));
        assert!(
            rules("Disallow: /x\n").allows("/x"),
            "a rule outside a group"
        );
        assert!(rules("").allows("/x"));
    }

    #[test]
    fn wildcards_and_anchors() {
        assert!(pattern_matches("/a*c", "/abxc"));
        assert!(pattern_matches("/a*c", "/ac/d"));
        assert!(!pattern_matches("/a*c$", "/ac/d"));
        assert!(pattern_matches("/a*b*c$", "/a1b2c"));
        assert!(!pattern_matches("/a*b*c$", "/a1c2b"));
        assert!(pattern_matches("/exact$", "/exact"));
        assert!(!pattern_matches("/exact$", "/exact2"));
    }
}
