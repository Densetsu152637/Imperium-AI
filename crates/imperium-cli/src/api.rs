//! `imperium api`: turn the OpenAI-compatible API on and off, see how to reach it, and show or
//! replace its token.

use anyhow::bail;
use imperium_protocol::methods::{
    ApiGet, ApiGetParams, ApiSet, ApiSetParams, ApiStatus, ApiToken, ApiTokenParams,
};

use crate::{block_on, connect};

pub fn status() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        print_status(&client.call::<ApiGet>(ApiGetParams {}).await?);
        Ok(())
    })
}

pub fn set_enabled(enabled: bool) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let status = client
            .call::<ApiSet>(ApiSetParams {
                enabled: Some(enabled),
                ..ApiSetParams::default()
            })
            .await?;
        print_status(&status);
        if enabled && let Some(error) = &status.error {
            bail!("the API did not start: {error}");
        }
        if enabled {
            println!(
                "
Show the token with `imperium api token`."
            );
        }
        Ok(())
    })
}

/// Shows the token, or with `rotate` replaces it first.
pub fn token(rotate: bool) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let result = client.call::<ApiToken>(ApiTokenParams { rotate }).await?;
        if rotate {
            println!("The old token no longer works. The new one:");
        }
        println!("{}", result.token);
        Ok(())
    })
}

fn print_status(status: &ApiStatus) {
    match (&status.base_url, status.enabled) {
        (Some(url), _) => println!("The API is on: {url}"),
        (None, true) => println!("The API is enabled but not running."),
        (None, false) => println!("The API is off."),
    }
    if let Some(error) = &status.error {
        println!("  problem: {error}");
    }
    if status.allow_lan {
        println!("  allow_lan is set: other computers on the network may reach it (risky).");
    }
    let served: Vec<_> = status.models.iter().filter(|model| model.exposed).collect();
    if status.running {
        println!("Models served ({}):", served.len());
        for model in served {
            println!("  {}  {}", model.id, model.name);
        }
    }
    for model in status.models.iter().filter(|m| m.hosted && !m.exposed) {
        println!(
            "  not served (hosted; turn on in Settings -> API): {}  {}",
            model.id, model.name
        );
    }
}
