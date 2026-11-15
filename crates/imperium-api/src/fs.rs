//! File helpers.

use std::io;
use std::path::Path;

/// Replaces the file at `path` with `bytes`: writes them to `temp` first, then renames `temp`
/// over `path`, so a reader sees the old file or the whole new one. `temp` is the caller's
/// choice and should lie beside `path` (a rename across volumes can fail); it is left behind
/// when the write or the rename fails. The parent folder must exist.
pub fn replace_file(
    path: impl AsRef<Path>,
    temp: impl AsRef<Path>,
    bytes: impl AsRef<[u8]>,
) -> io::Result<()> {
    std::fs::write(temp.as_ref(), bytes)?;
    std::fs::rename(temp.as_ref(), path.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_is_replaced_whole_and_the_temp_name_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let (path, temp) = (dir.path().join("a.json"), dir.path().join("a.json.tmp"));
        replace_file(&path, &temp, "one").unwrap();
        replace_file(&path, &temp, b"two".as_slice()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        assert!(!temp.exists());
    }

    #[test]
    fn a_missing_folder_is_an_error_and_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let (path, temp) = (dir.path().join("no/a"), dir.path().join("no/a.tmp"));
        assert!(replace_file(&path, &temp, "x").is_err());
    }
}
