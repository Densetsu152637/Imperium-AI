//! Test tools and models, as Imperium's providers and clients.

#![allow(dead_code, reason = "each test file uses some of the helpers")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use imperium_api::{ToolCheck, ToolContext, ToolFuture, ToolGroup, ToolProvider, ToolSpec};
use imperium_models::{ChatClient, ChatFuture, ChatRequest, Completion, TokenFuture, Tokenizer};

/// The size of the big error answer, over Ratio's 1 KiB blob threshold.
pub const BIG_ERROR: usize = 2048;

const TOOLS: &[&str] = &[
    "read_file",
    "write_file",
    "web_fetch",
    "run_command",
    "slow_read",
    "slow_remote",
    "never_drain",
    "needs_approval",
];

fn spec(name: &str) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: String::new(),
        parameters: serde_json::json!({"type": "object"}),
    }
}

/// Built-in style tools, counting the calls that reach them.
#[derive(Debug, Default)]
pub struct TestTools {
    pub calls: Arc<AtomicUsize>,
}

impl ToolProvider for TestTools {
    fn specs(&self) -> Vec<ToolSpec> {
        TOOLS.iter().map(|n| spec(n)).collect()
    }

    fn handles(&self, name: &str) -> bool {
        TOOLS.contains(&name)
    }

    fn check(&self, name: &str, _arguments: &str) -> Result<ToolCheck, String> {
        Ok(ToolCheck {
            summary: format!("run `{name}`"),
            ask: name == "needs_approval",
            needs_context: false,
            command: None,
        })
    }

    fn call<'a>(
        &'a self,
        _context: &'a ToolContext<'a>,
        name: &'a str,
        arguments: &'a str,
    ) -> ToolFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            match name {
                // An error answer over 1 KiB: Ratio's tape stores it as an `xb` blob.
                "read_file" if arguments.contains("big-error") => Err("e".repeat(BIG_ERROR)),
                "slow_read" | "slow_remote" => {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    Ok("done".to_owned())
                }
                "never_drain" => std::future::pending::<Result<String, String>>().await,
                "read_file" => Ok("the notes".to_owned()),
                _ => Ok("done".to_owned()),
            }
        })
    }
}

/// An outside server's tools, every one claiming to be read-only.
#[derive(Debug, Default)]
pub struct TestMcp;

impl ToolProvider for TestMcp {
    fn group(&self) -> ToolGroup {
        ToolGroup::External
    }

    fn read_only(&self, _name: &str) -> bool {
        true
    }

    fn specs(&self) -> Vec<ToolSpec> {
        vec![spec("mcp__srv__t"), spec("mcp__server__claims_read_only")]
    }

    fn handles(&self, name: &str) -> bool {
        name.starts_with("mcp__")
    }

    fn server_of(&self, name: &str) -> Option<String> {
        name.split("__").nth(1).map(str::to_owned)
    }

    fn check(&self, name: &str, _arguments: &str) -> Result<ToolCheck, String> {
        Ok(ToolCheck {
            summary: format!("call `{name}`"),
            ask: false,
            needs_context: false,
            command: None,
        })
    }

    fn call<'a>(
        &'a self,
        _context: &'a ToolContext<'a>,
        _name: &'a str,
        _arguments: &'a str,
    ) -> ToolFuture<'a> {
        Box::pin(async { Ok("{}".to_owned()) })
    }
}

/// A model endpoint that answers at once.
#[derive(Debug, Default)]
pub struct Echo;

impl ChatClient for Echo {
    fn stream<'a>(
        &'a self,
        request: ChatRequest<'a>,
        _on_text: &'a mut (dyn FnMut(&str) + Send),
    ) -> ChatFuture<'a> {
        let n = request.messages.len();
        Box::pin(async move {
            Ok(Completion {
                text: format!("echo of {n} messages"),
                ..Completion::default()
            })
        })
    }
}

/// What [`Counted`]'s tokenizer answers for any text.
pub const COUNTED: u64 = 42;

/// A local model server with its own tokenizer (as llama.cpp's `/tokenize`).
#[derive(Debug, Default)]
pub struct Counted;

impl ChatClient for Counted {
    fn stream<'a>(
        &'a self,
        _request: ChatRequest<'a>,
        _on_text: &'a mut (dyn FnMut(&str) + Send),
    ) -> ChatFuture<'a> {
        Box::pin(async { Ok(Completion::default()) })
    }

    fn tokenizer(&self) -> Option<&dyn Tokenizer> {
        Some(self)
    }
}

impl Tokenizer for Counted {
    fn server(&self) -> &str {
        "counted"
    }

    fn count_tokens<'a>(&'a self, _text: &'a str) -> TokenFuture<'a, u64> {
        Box::pin(async {
            // Asynchronous, as a real server's answer is.
            tokio::time::sleep(Duration::from_millis(5)).await;
            Ok(COUNTED)
        })
    }

    fn template_tokens<'a>(
        &'a self,
        _messages: &'a serde_json::Value,
        _tools: &'a [serde_json::Value],
    ) -> TokenFuture<'a, u64> {
        Box::pin(async { Ok(0) })
    }

    fn context_window(&self) -> TokenFuture<'_, Option<u64>> {
        Box::pin(async { Ok(None) })
    }
}

/// Imperium's runtime for a test.
pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime")
}
