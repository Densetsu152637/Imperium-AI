//! The scrollback ring: the last [`RING_CAPACITY`] bytes of a terminal's output, addressed by a
//! running byte offset (`seq`) so a reader can say where it stopped and learn what it missed.

use std::collections::VecDeque;

/// Output kept per terminal, in bytes; older output is dropped.
pub const RING_CAPACITY: usize = 1024 * 1024;

/// A byte ring that remembers how many bytes came before its first one.
#[derive(Debug)]
pub struct Ring {
    bytes: VecDeque<u8>,
    /// The `seq` of the first byte kept (the number of bytes dropped so far).
    start: u64,
    capacity: usize,
}

/// What a reader gets back: the bytes from `from` on, and how many it asked for that are gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replay {
    /// The `seq` of the first byte of `data` (the requested one, or the oldest still kept).
    pub from: u64,
    /// Bytes between the requested `seq` and `from` that were dropped from the ring.
    pub dropped: u64,
    pub data: Vec<u8>,
}

impl Ring {
    pub fn new(capacity: usize) -> Self {
        Self {
            bytes: VecDeque::new(),
            start: 0,
            capacity,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend(chunk);
        let excess = self.bytes.len().saturating_sub(self.capacity);
        if excess > 0 {
            self.bytes.drain(..excess);
            self.start += excess as u64;
        }
    }

    /// The `seq` of the oldest byte kept.
    pub fn start(&self) -> u64 {
        self.start
    }

    /// The `seq` the next byte will get (the total written so far).
    pub fn end(&self) -> u64 {
        self.start + self.bytes.len() as u64
    }

    /// The bytes from `from` on; a `from` that was dropped starts at the oldest byte kept, and one
    /// past the end gives nothing.
    pub fn replay(&self, from: u64) -> Replay {
        let first = from.clamp(self.start, self.end());
        // At most `bytes.len()`, so it fits.
        let skip = usize::try_from(first - self.start).unwrap_or(self.bytes.len());
        Replay {
            from: first,
            dropped: self.start.saturating_sub(from),
            data: self.bytes.range(skip..).copied().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_everything_below_the_capacity() {
        let mut ring = Ring::new(8);
        ring.push(b"abc");
        ring.push(b"de");
        assert_eq!((ring.start(), ring.end()), (0, 5));
        let replay = ring.replay(0);
        assert_eq!(replay.data, b"abcde");
        assert_eq!((replay.from, replay.dropped), (0, 0));
        assert_eq!(ring.replay(3).data, b"de");
        assert!(ring.replay(99).data.is_empty());
    }

    #[test]
    fn a_two_mebibyte_burst_keeps_the_last_mebibyte_and_reports_the_drop() {
        let mut ring = Ring::new(RING_CAPACITY);
        let chunk = vec![b'x'; 64 * 1024];
        for index in 0..32 {
            // The last byte of the burst is different, to show the end is what is kept.
            let mut chunk = chunk.clone();
            if index == 31 {
                *chunk.last_mut().unwrap() = b'!';
            }
            ring.push(&chunk);
        }
        assert_eq!(ring.end(), 2 * 1024 * 1024);
        assert_eq!(ring.start(), 1024 * 1024);
        let replay = ring.replay(0);
        assert_eq!(replay.data.len(), RING_CAPACITY);
        assert_eq!(replay.dropped, 1024 * 1024);
        assert_eq!(replay.from, 1024 * 1024);
        assert_eq!(replay.data.last(), Some(&b'!'));
        // A reader that was current has lost nothing.
        assert_eq!(ring.replay(ring.end() - 10).dropped, 0);
    }

    #[test]
    fn one_chunk_larger_than_the_ring_keeps_its_end() {
        let mut ring = Ring::new(4);
        ring.push(b"abcdefgh");
        assert_eq!(ring.replay(0).data, b"efgh");
        assert_eq!(ring.start(), 4);
    }
}
