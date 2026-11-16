//! `imperium mcp login <server>` and `imperium mcp logout <server>`: OAuth sign-in for remote
//! MCP servers. The daemon does the work; the command prints the address for you to open in your
//! own browser (Imperium never opens or drives one for this) and waits for the sign-in to end.

use std::time::{Duration, Instant};

use anyhow::bail;
use imperium_protocol::methods::{
    McpList, McpListParams, McpLogin, McpLoginParams, McpLogout, McpLogoutParams,
};

use crate::{block_on, connect};

/// A little longer than the daemon waits for the browser, so its own timeout is what ends it.
const WAIT: Duration = Duration::from_secs(320);

pub fn login(name: String) -> anyhow::Result<()> {
    if crate::REMOTE.get().and_then(Option::as_deref).is_some() {
        bail!(
            "the sign-in returns to 127.0.0.1 on the machine the daemon runs on, so sign in there \
             (or in that machine's app) instead of through --remote"
        );
    }
    block_on(async {
        let (mut client, _) = connect().await?;
        let started = client
            .call::<McpLogin>(McpLoginParams { name: name.clone() })
            .await?;
        println!("Open this address in your browser to sign in to {name}:\n");
        println!("  {}\n", started.authorization_url);
        println!(
            "Waiting for you to finish (Ctrl+C stops waiting; the sign-in ends on its own after a few minutes)..."
        );
        let deadline = Instant::now() + WAIT;
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let listing = client.call::<McpList>(McpListParams {}).await?;
            let Some(server) = listing.servers.iter().find(|server| server.name == name) else {
                bail!("{name} is no longer configured");
            };
            if server.status.state == "error" {
                bail!(
                    "{}",
                    server
                        .status
                        .error
                        .clone()
                        .unwrap_or_else(|| "the sign-in did not finish".into())
                );
            }
            match server.status.sign_in.as_deref() {
                Some("signing_in") => {}
                Some("signed_in") => {
                    println!("Signed in to {name}. New sessions use it.");
                    return Ok(());
                }
                _ => bail!("the sign-in did not finish"),
            }
            if Instant::now() > deadline {
                bail!("the sign-in did not finish in time");
            }
        }
    })
}

pub fn logout(name: String) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let result = client
            .call::<McpLogout>(McpLogoutParams { name: name.clone() })
            .await?;
        if result.signed_out {
            println!("Signed out of {name}; its stored tokens are gone.");
        } else {
            println!("{name} was not signed in.");
        }
        Ok(())
    })
}
