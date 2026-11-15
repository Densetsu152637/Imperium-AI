//! Text encodings of bytes.

/// `bytes` as lowercase hexadecimal, two digits each.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::hex;

    #[test]
    fn bytes_become_two_lowercase_digits_each() {
        assert_eq!(hex(&[]), "");
        assert_eq!(hex(&[0x00, 0x0a, 0xff, 0x7f]), "000aff7f");
    }
}
