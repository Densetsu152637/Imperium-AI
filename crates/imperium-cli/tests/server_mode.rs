//! The command line on a server (`daemon.server`): `imperium runtime vllm setup --docker` is
//! refused before anything is checked or downloaded.

use std::process::{Command, Stdio};

#[test]
fn a_server_refuses_to_set_up_vllm_in_docker() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_imperium"))
        .env("IMPERIUM_HOME", home.path())
        .env("IMPERIUM_DAEMON__SERVER", "true")
        .env("IMPERIUM_NO_DAEMON_SPAWN", "1")
        .args(["runtime", "vllm", "setup", "--docker", "--yes"])
        .stdin(Stdio::null())
        .output()
        .expect("the imperium binary runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("vLLM in Docker is unavailable on a server"),
        "{stderr}"
    );
    // Nothing was set up.
    assert!(!home.path().join("data").join("vllm").exists());
}
