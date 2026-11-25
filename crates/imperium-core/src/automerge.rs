//! The challenge that turning auto-merge on needs
//! ([plan](../../../docs/plans/pr-card.md#threat-model), docs/14-projects-and-automation.md).
//!
//! Auto-merge lands code without another look, so asking GitHub for it counts only from the
//! desktop app installed beside this `imperium-core`, which the daemon identifies itself
//! ([`crate::app_identity`], checks 1 to 4) and which answers a single-use challenge issued to the
//! same connection for the same session, pull request, head commit and merge method, at most two
//! minutes old. The same pattern as `plugins.approval_challenge` and `plugins.approve`. Turning
//! auto-merge off needs none of this: anyone local may do that.

use imperium_api::encoding::hex;
use imperium_protocol::methods::PrMergeMethod;

/// How long a challenge may be answered, in milliseconds.
const CHALLENGE_MS: u64 = 120_000;
/// Most unanswered challenges one connection holds; asking for more drops the oldest.
const MAX_OPEN: usize = 8;

/// What a challenge was issued for, and what an enabling call must name again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Asked {
    pub(crate) session_id: String,
    pub(crate) url: String,
    pub(crate) head_sha: String,
    pub(crate) method: PrMergeMethod,
}

#[derive(Debug)]
struct Open {
    token: String,
    asked: Asked,
    expires_ms: u64,
}

/// The challenges issued to one connection. Each [`crate::rpc::Connection`] holds its own, so a
/// challenge cannot be answered on another connection.
#[derive(Debug, Default)]
pub(crate) struct Challenges {
    open: Vec<Open>,
}

impl Challenges {
    /// A new challenge for `asked`, and when it expires.
    pub(crate) fn issue(&mut self, asked: Asked, now_ms: u64) -> (String, u64) {
        self.open.retain(|open| open.expires_ms > now_ms);
        if self.open.len() >= MAX_OPEN {
            self.open.remove(0);
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("the OS random source is available");
        let token = hex(&bytes);
        let expires_ms = now_ms.saturating_add(CHALLENGE_MS);
        self.open.push(Open {
            token: token.clone(),
            asked,
            expires_ms,
        });
        (token, expires_ms)
    }

    /// Takes the challenge `token`, which is gone whatever the answer, and checks that it was
    /// issued for `asked` and has not expired.
    pub(crate) fn redeem(&mut self, token: &str, asked: &Asked, now_ms: u64) -> Result<(), String> {
        let index = self
            .open
            .iter()
            .position(|open| {
                constant_time_eq::constant_time_eq(open.token.as_bytes(), token.as_bytes())
            })
            .ok_or("unknown challenge: a challenge is answered once, on the connection that asked for it")?;
        let open = self.open.remove(index);
        if open.expires_ms <= now_ms {
            return Err("the challenge expired: ask for a new one".into());
        }
        if open.asked != *asked {
            return Err(
                "the challenge was issued for another session, pull request, commit or merge method"
                    .into(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked(head: &str, method: PrMergeMethod) -> Asked {
        Asked {
            session_id: "s".into(),
            url: "https://github.com/o/r/pull/1".into(),
            head_sha: head.into(),
            method,
        }
    }

    #[test]
    fn a_challenge_is_single_use_short_lived_and_bound_to_what_was_asked() {
        let mut mine = Challenges::default();
        let want = asked("abc", PrMergeMethod::Squash);
        let (token, expires) = mine.issue(want.clone(), 1_000);
        assert_eq!(expires, 1_000 + CHALLENGE_MS);

        // Another connection holds its own challenges and does not know this one.
        let mut other = Challenges::default();
        assert!(
            other
                .redeem(&token, &want, 1_000)
                .unwrap_err()
                .contains("unknown")
        );

        // Used once, it is gone.
        mine.redeem(&token, &want, 1_001).unwrap();
        assert!(
            mine.redeem(&token, &want, 1_002)
                .unwrap_err()
                .contains("unknown")
        );

        // Another head, method, session or address is refused, and uses the challenge up.
        for other_ask in [
            asked("def", PrMergeMethod::Squash),
            asked("abc", PrMergeMethod::Rebase),
            Asked {
                session_id: "t".into(),
                ..want.clone()
            },
            Asked {
                url: "https://github.com/o/r/pull/2".into(),
                ..want.clone()
            },
        ] {
            let (token, _) = mine.issue(want.clone(), 1_000);
            assert!(
                mine.redeem(&token, &other_ask, 1_001)
                    .unwrap_err()
                    .contains("another")
            );
            assert!(
                mine.redeem(&token, &want, 1_001)
                    .unwrap_err()
                    .contains("unknown")
            );
        }

        // Expired.
        let (token, expires) = mine.issue(want.clone(), 1_000);
        assert!(
            mine.redeem(&token, &want, expires)
                .unwrap_err()
                .contains("expired")
        );
    }

    #[test]
    fn a_connection_holds_a_few_open_challenges() {
        let mut mine = Challenges::default();
        let want = asked("abc", PrMergeMethod::Merge);
        let first = mine.issue(want.clone(), 0).0;
        for _ in 0..MAX_OPEN {
            mine.issue(want.clone(), 0);
        }
        assert!(
            mine.redeem(&first, &want, 1)
                .unwrap_err()
                .contains("unknown")
        );
    }
}
