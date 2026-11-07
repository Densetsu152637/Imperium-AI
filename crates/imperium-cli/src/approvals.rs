//! `imperium approvals`: answer requests from unattended work, such as auto-fix pushes.

use anyhow::bail;
use imperium_protocol::methods::{
    ApprovalList, ApprovalListParams, ApprovalRespond, ApprovalRespondParams,
};

use crate::{block_on, connect};

pub fn list() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let pending = client.call::<ApprovalList>(ApprovalListParams {}).await?;
        if pending.approvals.is_empty() {
            println!("Nothing is waiting for approval.");
        }
        for approval in pending.approvals {
            println!(
                "#{} {} ({})\n    {}",
                approval.id, approval.kind, approval.requested_at, approval.summary
            );
        }
        Ok(())
    })
}

pub fn respond(id: u64, approve: bool) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let result = client
            .call::<ApprovalRespond>(ApprovalRespondParams { id, approve })
            .await?;
        if !result.found {
            bail!("no pending approval #{id}");
        }
        println!("{} #{id}", if approve { "approved" } else { "declined" });
        Ok(())
    })
}
