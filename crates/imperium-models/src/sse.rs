//! Incremental parsing of `text/event-stream` bodies into `data:` payloads.

/// Splits a byte stream into the payloads of `data:` lines. Feed chunks as they arrive;
/// partial lines are kept until their newline arrives.
#[derive(Debug, Default)]
pub struct DataLines {
    pending: Vec<u8>,
}

impl DataLines {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(chunk);
        let mut payloads = Vec::new();
        while let Some(end) = self.pending.iter().position(|&byte| byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if let Some(data) = line.strip_prefix("data:") {
                payloads.push(data.trim_start().to_string());
            }
        }
        payloads
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_split_across_chunks_are_reassembled() {
        let mut lines = DataLines::default();
        assert!(lines.push(b"data: {\"a\"").is_empty());
        assert_eq!(
            lines.push(b":1}\r\n\r\n: comment\ndata: [DONE]\n"),
            ["{\"a\":1}", "[DONE]"]
        );
    }
}
