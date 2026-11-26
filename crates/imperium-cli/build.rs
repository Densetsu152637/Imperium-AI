//! Stamps the release tag and commit into `--version` (see `build-support/build_version.rs`).

include!("../../build-support/build_version.rs");

fn main() {
    emit_build_version();
}
