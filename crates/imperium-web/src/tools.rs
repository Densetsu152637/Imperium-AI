//! The session's web and browser tools as a [`ToolProvider`]: `web_fetch`, `web_search`, and,
//! when a browser is set up, `browser_open`, `browser_read`, `browser_click`, and
//! `browser_type`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use imperium_api::{
    ToolCheck, ToolContext, ToolEvent, ToolFuture, ToolProvider, ToolSpec, parse_arguments,
};
use imperium_protocol::methods::BrowserAction;
use serde::Deserialize;
use serde_json::json;
use url::Url;

use crate::browser::{Browser, View};
use crate::{CITATION_INSTRUCTIONS, Web, render_hits};

const BROWSER_TOOLS: [&str; 4] = [
    "browser_open",
    "browser_read",
    "browser_click",
    "browser_type",
];

/// A host as compared with the allowed domains: lower case, no trailing dot.
fn normal_host(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

/// Web search and fetch for a session, with the domains `web_fetch` may read without asking.
#[derive(Debug, Clone)]
pub struct WebTools {
    pub web: Web,
    pub allow_domains: Vec<String>,
    /// The browser to drive and its profile folder, when browser tools are on.
    pub browser: Option<BrowserSetup>,
}

#[derive(Debug, Clone)]
pub struct BrowserSetup {
    pub program: PathBuf,
    pub profile_dir: PathBuf,
    pub headless: bool,
    /// Started on first use and shared by the session's agents.
    pub instance: Arc<tokio::sync::Mutex<Option<Browser>>>,
    /// Whether the user has taken the browser over from the agent.
    pub takeover: Arc<Takeover>,
}

/// What the agent's browser tools say while the user drives.
pub const USER_DRIVING: &str = "the user is driving the browser right now (they took it over \
    in the Browser panel), so browser tools are unavailable until they give it back; carry on \
    with other work or ask them to give the browser back";

/// Who drives a session's browser. The user's takeover is a flag the agent's tools check at
/// the start of each call and again once they hold the browser, so a call that was waiting
/// while the user took over does not act.
#[derive(Debug, Default)]
pub struct Takeover {
    user: AtomicBool,
}

impl Takeover {
    pub fn user_driving(&self) -> bool {
        self.user.load(Ordering::SeqCst)
    }

    /// Sets who drives; true when that changed it.
    pub fn set_user(&self, user: bool) -> bool {
        self.user.swap(user, Ordering::SeqCst) != user
    }
}

impl BrowserSetup {
    /// The running browser, started first if it is not yet.
    async fn running<'a>(&self, guard: &'a mut Option<Browser>) -> Result<&'a mut Browser, String> {
        if guard.is_none() {
            *guard = Some(Browser::launch(&self.program, &self.profile_dir, self.headless).await?);
        }
        guard.as_mut().ok_or("the browser is not running".into())
    }

    /// The user takes the browser over (`session.browser_control`); returns its page. Taking
    /// over what the user already drives changes nothing. Fails, leaving the agent in
    /// control, if the browser cannot start.
    pub async fn take_over(&self) -> Result<View, String> {
        let changed = self.takeover.set_user(true);
        let result = async {
            let mut guard = self.instance.lock().await;
            self.running(&mut guard).await?.view().await
        }
        .await;
        if result.is_err() && changed {
            self.takeover.set_user(false);
        }
        result
    }

    /// The user gives the browser back to the agent; true when they had it.
    pub fn give_back(&self) -> bool {
        self.takeover.set_user(false)
    }

    /// Does an action of the user (`session.browser_act`) and returns the page after it.
    /// Refused unless the user has taken the browser over.
    pub async fn user_act(&self, action: &BrowserAction) -> Result<View, String> {
        if !self.takeover.user_driving() {
            return Err("take the browser over before driving it".into());
        }
        let mut guard = self.instance.lock().await;
        let browser = self.running(&mut guard).await?;
        browser.act(action).await?;
        browser.view().await
    }
}

#[derive(Deserialize)]
struct WebArgs {
    #[serde(default)]
    url: String,
    #[serde(default)]
    query: String,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: usize,
    #[serde(default)]
    count: Option<usize>,
}

#[derive(Deserialize)]
struct BrowserArgs {
    #[serde(default)]
    url: String,
    #[serde(default)]
    selector: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    submit: bool,
}

impl WebTools {
    /// Whether `url`'s host is an allowed domain or a subdomain of one. The host is the one
    /// `web_fetch` will connect to: it comes from the same URL parser (so a backslash, userinfo,
    /// case, IDN and a trailing dot cannot disguise another host), and only http(s) URLs qualify.
    pub fn allows(&self, url: &str) -> bool {
        let Ok(parsed) = Url::parse(url.trim()) else {
            return false;
        };
        if !matches!(parsed.scheme(), "http" | "https") {
            return false;
        }
        let Some(host) = parsed.host_str().map(normal_host) else {
            return false;
        };
        self.allow_domains.iter().any(|domain| {
            let domain = Url::parse(&format!("http://{}/", domain.trim()))
                .ok()
                .and_then(|parsed| parsed.host_str().map(normal_host))
                .unwrap_or_else(|| normal_host(domain.trim()));
            !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
        })
    }

    /// Whether a redirect to `target` is followed from a page at `origin`: within its scheme,
    /// host and port, or to an allowed domain.
    fn may_follow(&self, origin: Option<&url::Origin>, target: &Url) -> bool {
        Some(&target.origin()) == origin || self.allows(target.as_str())
    }

    /// Runs `web_search` (through the configured provider) or `web_fetch`.
    async fn run_web(&self, name: &str, arguments: &str) -> Result<String, String> {
        let args: WebArgs = parse_arguments(arguments)?;
        let result = if name == "web_fetch" {
            // A redirect is followed only within the page's own scheme, host and port, or to an
            // allowed domain (never from https to http): the user
            // approved (or the allow list covers) the URL asked for, not wherever it leads.
            let origin = Url::parse(&args.url).ok().map(|url| url.origin());
            let follow = |target: &Url| self.may_follow(origin.as_ref(), target);
            self.web
                .fetch_with(&args.url, args.offset, args.limit, &follow)
                .await
        } else {
            self.web
                .search(&args.query, args.count.unwrap_or(8))
                .await
                .map(|hits| render_hits(&hits))
        };
        result.map_err(|error| error.to_string())
    }

    /// Drives the session's browser, starting it on first use. Each action is mirrored to
    /// clients as a browser view.
    async fn run_browser(
        &self,
        context: &ToolContext<'_>,
        name: &str,
        arguments: &str,
    ) -> Result<String, String> {
        let setup = self
            .browser
            .as_ref()
            .ok_or("browser tools are off".to_string())?;
        let args: BrowserArgs = parse_arguments(arguments)?;
        if setup.takeover.user_driving() {
            return Err(USER_DRIVING.into());
        }
        let mut guard = setup.instance.lock().await;
        // The user may have taken over while this call waited for the browser.
        if setup.takeover.user_driving() {
            return Err(USER_DRIVING.into());
        }
        let browser = setup.running(&mut guard).await?;
        let result = match name {
            "browser_open" => browser.open(&args.url).await,
            "browser_click" => browser.click(&args.selector).await,
            "browser_type" => {
                browser
                    .type_text(&args.selector, &args.text, args.submit)
                    .await
            }
            _ => browser.read().await,
        };
        if let Ok(view) = browser.view().await {
            (context.emit)(ToolEvent::BrowserView {
                url: view.url,
                title: view.title,
                screenshot: view.screenshot,
            });
        }
        result
    }
}

impl ToolProvider for WebTools {
    fn instructions(&self) -> Option<&str> {
        Some(CITATION_INSTRUCTIONS)
    }

    fn specs(&self) -> Vec<ToolSpec> {
        let mut specs = self.web.tool_specs();
        if self.browser.is_some() {
            specs.extend(browser_specs());
        }
        specs
    }

    fn handles(&self, name: &str) -> bool {
        matches!(name, "web_fetch" | "web_search")
            || (self.browser.is_some() && BROWSER_TOOLS.contains(&name))
    }

    /// `web_fetch` and `browser_open` ask unless the domain is allowed; clicking and typing
    /// always ask; searching and reading never do.
    fn check(&self, name: &str, arguments: &str) -> Result<ToolCheck, String> {
        let (summary, ask) = if name.starts_with("browser_") {
            // Refused before asking, so the user is not prompted for a call that cannot run.
            if self
                .browser
                .as_ref()
                .is_some_and(|setup| setup.takeover.user_driving())
            {
                return Err(USER_DRIVING.into());
            }
            let args: BrowserArgs = parse_arguments(arguments)?;
            match name {
                "browser_open" => (
                    format!("open {} in the browser", args.url),
                    !self.allows(&args.url),
                ),
                "browser_click" => (format!("click {} in the browser", args.selector), true),
                "browser_type" => (format!("type into {} in the browser", args.selector), true),
                _ => ("read the browser page".into(), false),
            }
        } else {
            let args: WebArgs = parse_arguments(arguments)?;
            if name == "web_fetch" {
                (format!("fetch {}", args.url), !self.allows(&args.url))
            } else {
                (format!("search the web for {:?}", args.query), false)
            }
        };
        Ok(ToolCheck {
            summary,
            ask,
            needs_context: false,
            command: None,
        })
    }

    fn call<'a>(
        &'a self,
        context: &'a ToolContext<'a>,
        name: &'a str,
        arguments: &'a str,
    ) -> ToolFuture<'a> {
        Box::pin(async move {
            if name.starts_with("browser_") {
                self.run_browser(context, name, arguments).await
            } else {
                self.run_web(name, arguments).await
            }
        })
    }
}

fn browser_specs() -> Vec<ToolSpec> {
    let spec = |name: &str, description: &str, parameters: serde_json::Value| ToolSpec {
        name: name.into(),
        description: description.into(),
        parameters,
    };
    vec![
        spec(
            "browser_open",
            "Open a page in the browser and return its text. Use for pages that need JavaScript or interaction; web_fetch is cheaper for plain pages.",
            json!({"type": "object", "properties": {"url": {"type": "string"}}, "required": ["url"]}),
        ),
        spec(
            "browser_read",
            "Return the current browser page's title, URL, and text.",
            json!({"type": "object", "properties": {}}),
        ),
        spec(
            "browser_click",
            "Click the first element matching a CSS selector.",
            json!({"type": "object", "properties": {"selector": {"type": "string"}}, "required": ["selector"]}),
        ),
        spec(
            "browser_type",
            "Type text into the first element matching a CSS selector; submit its form if asked.",
            json!({"type": "object", "properties": {
                "selector": {"type": "string"}, "text": {"type": "string"}, "submit": {"type": "boolean"}
            }, "required": ["selector", "text"]}),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools(browser: bool) -> WebTools {
        WebTools {
            web: Web::new(None),
            allow_domains: vec!["docs.rs".into(), "Example.COM".into()],
            browser: browser.then(|| BrowserSetup {
                program: PathBuf::from("chrome"),
                profile_dir: PathBuf::from("profile"),
                headless: true,
                instance: Arc::default(),
                takeover: Arc::default(),
            }),
        }
    }

    #[test]
    fn allowed_domains_cover_their_subdomains_only() {
        let tools = tools(false);
        assert!(tools.allows("https://docs.rs/tokio"));
        assert!(tools.allows("https://www.example.com:8443/x"));
        assert!(tools.allows("https://user@docs.rs/"));
        assert!(!tools.allows("https://notdocs.rs/"));
        assert!(!tools.allows("https://docs.rs.evil.com/"));
    }

    #[test]
    fn redirects_stay_within_scheme_host_and_port_or_an_allowed_domain() {
        let tools = tools(false);
        let url = |text: &str| Url::parse(text).unwrap();
        let origin = url("https://other.org:8443/a").origin();
        let follow = |target: &str| tools.may_follow(Some(&origin), &url(target));
        assert!(follow("https://other.org:8443/b"));
        assert!(!follow("https://other.org/b"), "another port");
        assert!(!follow("http://other.org:8443/b"), "another scheme");
        assert!(!follow("https://www.other.org:8443/b"), "another host");
        assert!(follow("https://docs.rs/x"), "an allowed domain");
        assert!(!tools.may_follow(None, &url("https://other.org/")));
    }

    #[test]
    fn allowed_domains_use_the_hosts_the_fetch_parser_sees() {
        let tools = tools(false);
        // The URL parser reads a backslash as a path separator for http(s), so the host is
        // evil.com, not docs.rs.
        assert!(!tools.allows("https://evil.com\\@docs.rs/"));
        assert!(!tools.allows("https://evil.com:80@evil.com/@docs.rs"));
        assert!(!tools.allows("https://evil.com/?@docs.rs"));
        assert!(tools.allows("https://user:pw@docs.rs/"));
        assert!(tools.allows("HTTPS://DOCS.RS/"));
        assert!(tools.allows("https://docs.rs./tokio"));
        assert!(tools.allows("https://Sub.Example.com/"));
        assert!(!tools.allows("ftp://docs.rs/"));
        assert!(!tools.allows("docs.rs/tokio"));
        assert!(!tools.allows("not a url"));
        // An internationalised host is compared in its ASCII form.
        let mut idn = tools;
        idn.allow_domains = vec!["b\u{fc}cher.example".into()];
        assert!(idn.allows("https://B\u{dc}CHER.example/x"));
        assert!(idn.allows("https://xn--bcher-kva.example/x"));
        assert!(!idn.allows("https://bucher.example/x"));
    }

    #[test]
    fn browser_tools_are_offered_only_with_a_browser() {
        let names = |tools: &WebTools| -> Vec<String> {
            tools.specs().into_iter().map(|spec| spec.name).collect()
        };
        assert_eq!(names(&tools(false)), ["web_fetch"]);
        assert_eq!(
            names(&tools(true)),
            [
                "web_fetch",
                "browser_open",
                "browser_read",
                "browser_click",
                "browser_type"
            ]
        );
        assert!(!tools(false).handles("browser_open"));
        assert!(tools(true).handles("browser_open"));
    }

    #[test]
    fn fetching_asks_unless_the_domain_is_allowed() {
        let tools = tools(true);
        let check = |name: &str, arguments: &str| tools.check(name, arguments).unwrap();
        assert!(!check("web_fetch", r#"{"url":"https://docs.rs/x"}"#).ask);
        assert!(check("web_fetch", r#"{"url":"https://other.org/"}"#).ask);
        assert!(!check("web_search", r#"{"query":"q"}"#).ask);
        assert!(check("browser_click", r#"{"selector":"a"}"#).ask);
        assert!(!check("browser_read", "").ask);
        assert_eq!(check("browser_read", "").summary, "read the browser page");
        assert!(tools.check("web_fetch", "{").is_err());
    }

    async fn agent_call(tools: &WebTools, name: &str) -> Result<String, String> {
        let emit = |_: ToolEvent| {};
        let context = ToolContext {
            root: std::path::Path::new("."),
            emit: &emit,
        };
        tools
            .call(&context, name, r#"{"url":"http://localhost/"}"#)
            .await
    }

    #[tokio::test]
    async fn the_agents_browser_is_refused_while_the_user_drives_and_returns_when_given_back() {
        let mut tools = tools(true);
        // No browser can start here, so a call that is let through fails differently.
        let profile = tempfile::tempdir().unwrap();
        let setup = tools.browser.as_mut().unwrap();
        setup.program = PathBuf::from("imperium-no-such-browser");
        setup.profile_dir = profile.path().join("profile");
        let setup = tools.browser.clone().unwrap();
        assert!(!setup.takeover.user_driving());

        // The user takes over (the browser cannot start, so the takeover is withdrawn).
        assert!(setup.take_over().await.is_err());
        assert!(!setup.takeover.user_driving());

        // With the user driving, every browser tool is refused, before any approval or launch.
        setup.takeover.set_user(true);
        for name in BROWSER_TOOLS {
            let error = agent_call(&tools, name).await.unwrap_err();
            assert_eq!(error, USER_DRIVING, "{name}");
            assert_eq!(
                tools.check(name, r#"{"url":"http://x/"}"#).unwrap_err(),
                USER_DRIVING
            );
        }
        // Reading the web is not the browser: web_fetch is unaffected by the takeover.
        assert!(
            tools
                .check("web_fetch", r#"{"url":"https://docs.rs/x"}"#)
                .is_ok()
        );

        // Giving back restores the agent's tools (they get as far as starting the browser).
        assert!(setup.give_back());
        assert!(!setup.give_back());
        let error = agent_call(&tools, "browser_open").await.unwrap_err();
        assert_ne!(error, USER_DRIVING);
        assert!(error.contains("could not start"), "{error}");
        assert!(tools.check("browser_read", "").is_ok());
    }

    #[tokio::test]
    async fn a_call_waiting_for_the_browser_does_not_act_after_a_takeover() {
        let tools = tools(true);
        let setup = tools.browser.clone().unwrap();
        // The agent's call holds the browser slot while the user takes over.
        let slot = setup.instance.lock().await;
        let waiting = {
            let tools = tools.clone();
            tokio::spawn(async move { agent_call(&tools, "browser_read").await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        setup.takeover.set_user(true);
        drop(slot);
        assert_eq!(waiting.await.unwrap().unwrap_err(), USER_DRIVING);
    }

    #[tokio::test]
    async fn the_user_can_only_act_after_taking_over() {
        let setup = tools(true).browser.unwrap();
        let error = setup.user_act(&BrowserAction::Reload).await.unwrap_err();
        assert!(error.contains("take the browser over"), "{error}");
    }
}
