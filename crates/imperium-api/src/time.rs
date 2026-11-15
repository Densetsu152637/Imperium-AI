//! The current time as a Unix timestamp, for stamps and ids.

use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch: 0 when the clock is set before it, and `u64::MAX` when
/// the count does not fit.
pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Seconds since the Unix epoch: 0 when the clock is set before it.
pub fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_units_agree() {
        let (ms, secs) = (unix_ms(), unix_secs());
        // Taken a moment apart: the same second, or the next.
        assert!(ms / 1000 == secs || ms / 1000 + 1 == secs);
        // After 2020.
        assert!(secs > 1_577_836_800);
    }
}
