//! `imperium phone`: pair phones with this computer, list them, and revoke them.

use anyhow::bail;
use imperium_protocol::methods::{
    PhoneDevices, PhoneDevicesParams, PhonePair, PhonePairParams, PhoneRevoke, PhoneRevokeParams,
};

use crate::{block_on, connect};

pub fn pair() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let result = client.call::<PhonePair>(PhonePairParams {}).await?;
        match result.url {
            Some(url) => println!("On the phone, open {url} and enter this code:"),
            None => println!(
                "The phone listener is off: set phone.bind in your config (for example your \
                 Tailscale address and a port) and restart the daemon. Then enter this code:"
            ),
        }
        println!(
            "\n    {}\n\nIt works once and expires in {} minutes.",
            result.pairing.code,
            result.pairing.expires_in_s / 60
        );
        Ok(())
    })
}

pub fn devices() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let devices = client
            .call::<PhoneDevices>(PhoneDevicesParams {})
            .await?
            .devices;
        if devices.is_empty() {
            println!("No phones are paired.");
        }
        for device in devices {
            println!(
                "{}  {}  ({}; paired {})",
                device.id,
                device.name,
                device.scopes.join(", "),
                device.paired_at
            );
        }
        Ok(())
    })
}

pub fn revoke(id: String) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let result = client
            .call::<PhoneRevoke>(PhoneRevokeParams { id: id.clone() })
            .await?;
        if !result.found {
            bail!("no paired phone {id}");
        }
        println!("unpaired {id}");
        Ok(())
    })
}
