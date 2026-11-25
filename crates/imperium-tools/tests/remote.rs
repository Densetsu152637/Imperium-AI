//! Tool calls in a remote workspace go through its runtime's runner: the right program and
//! arguments for Docker, WSL, and SSH, file contents through standard input, and paths outside
//! the workspace's folders refused before anything runs.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use imperium_tools::runtime::RunFuture;
use imperium_tools::{Invocation, ProcessSpec, Remote, RunOutput, Runner, ToolError, Workspace};

/// Records every process it is asked to run and answers with `stdout`.
#[derive(Debug, Default)]
struct Fake {
    calls: Mutex<Vec<(ProcessSpec, Option<Vec<u8>>)>>,
    stdout: Mutex<Vec<u8>>,
}

impl Fake {
    fn answer(&self, stdout: &str) {
        *self.stdout.lock().unwrap() = stdout.as_bytes().to_vec();
    }

    fn calls(&self) -> Vec<(ProcessSpec, Option<Vec<u8>>)> {
        self.calls.lock().unwrap().clone()
    }

    fn last(&self) -> (ProcessSpec, Option<Vec<u8>>) {
        self.calls().pop().expect("a process ran")
    }
}

impl Runner for Fake {
    fn run<'a>(
        &'a self,
        spec: &'a ProcessSpec,
        stdin: Option<&'a [u8]>,
        _timeout: Duration,
    ) -> RunFuture<'a> {
        self.calls
            .lock()
            .unwrap()
            .push((spec.clone(), stdin.map(<[u8]>::to_vec)));
        let stdout = self.stdout.lock().unwrap().clone();
        Box::pin(async move {
            Ok(RunOutput {
                code: Some(0),
                stdout,
                stderr: Vec::new(),
            })
        })
    }
}

async fn call(workspace: &Workspace, name: &str, arguments: &str) -> Result<String, ToolError> {
    Invocation::parse(name, arguments)
        .unwrap()
        .execute(workspace)
        .await
}

#[tokio::test]
async fn wsl_tools_run_through_wsl_exe_with_translated_paths() {
    let fake = Arc::new(Fake::default());
    let workspace = Workspace::remote(
        Remote::Wsl {
            distro: "Ubuntu".into(),
        },
        &[r"C:\work\app".into(), "/home/me/notes".into()],
        fake.clone(),
    )
    .unwrap();
    assert_eq!(
        workspace.folders_display(),
        ["/mnt/c/work/app", "/home/me/notes"]
    );
    assert_eq!(workspace.isolation_label(), "runtime · WSL Ubuntu");

    let output = call(&workspace, "run_command", r#"{"command":"cargo test"}"#)
        .await
        .unwrap();
    assert!(output.starts_with("exit code 0"), "{output}");
    assert_eq!(
        fake.last().0.to_string(),
        "wsl.exe -d Ubuntu --cd /mnt/c/work/app --exec sh -c cargo test"
    );

    fake.answer("fn main() {}\n");
    let read = call(&workspace, "read_file", r#"{"path":"src/main.rs"}"#)
        .await
        .unwrap();
    assert_eq!(read, "     1\tfn main() {}\n");
    assert_eq!(
        fake.last().0.args.last().unwrap(),
        "cat -- '/mnt/c/work/app/src/main.rs'"
    );
    // A Windows path inside a folder is translated before it is checked.
    call(
        &workspace,
        "read_file",
        r#"{"path":"C:\\work\\app\\README.md"}"#,
    )
    .await
    .unwrap();
    assert_eq!(
        fake.last().0.args.last().unwrap(),
        "cat -- '/mnt/c/work/app/README.md'"
    );
}

#[tokio::test]
async fn ssh_writes_and_edits_send_the_content_on_standard_input() {
    let fake = Arc::new(Fake::default());
    let workspace = Workspace::remote(
        Remote::Ssh {
            host: "me@build:2222".into(),
        },
        &["/srv/app".into()],
        fake.clone(),
    )
    .unwrap();
    let wrote = call(
        &workspace,
        "write_file",
        r#"{"path":"docs/a.txt","content":"one two"}"#,
    )
    .await
    .unwrap();
    assert_eq!(wrote, "wrote 7 bytes to docs/a.txt");
    let (spec, stdin) = fake.last();
    assert_eq!(spec.program, "ssh");
    assert_eq!(
        &spec.args[..7],
        ["-o", "BatchMode=yes", "-T", "-p", "2222", "--", "me@build"]
    );
    assert!(
        spec.args[7].contains("mkdir -p -- ") && spec.args[7].contains("/srv/app/docs/a.txt"),
        "{spec}"
    );
    assert_eq!(stdin.as_deref(), Some(&b"one two"[..]));

    fake.answer("one two");
    call(
        &workspace,
        "edit_file",
        r#"{"path":"docs/a.txt","old_string":"two","new_string":"three"}"#,
    )
    .await
    .unwrap();
    assert_eq!(fake.last().1.as_deref(), Some(&b"one three"[..]));

    fake.answer("b.txt\nsrc/\n");
    assert_eq!(
        call(&workspace, "list_dir", r#"{"path":"."}"#)
            .await
            .unwrap(),
        "b.txt\nsrc/"
    );
}

#[tokio::test]
async fn docker_exec_runs_in_the_container_and_nothing_escapes_the_folders() {
    let fake = Arc::new(Fake::default());
    let workspace = Workspace::remote(
        Remote::Docker {
            container: "web".into(),
        },
        &["/app".into()],
        fake.clone(),
    )
    .unwrap();
    call(&workspace, "run_command", r#"{"command":"ls"}"#)
        .await
        .unwrap();
    assert_eq!(
        fake.last().0.to_string(),
        "docker exec -i -w /app -- web sh -c ls"
    );
    let before = fake.calls().len();
    for outside in [r#"{"path":"../etc/passwd"}"#, r#"{"path":"/etc/passwd"}"#] {
        assert!(matches!(
            call(&workspace, "read_file", outside).await,
            Err(ToolError::OutsideWorkspace(_))
        ));
    }
    assert!(matches!(
        call(
            &workspace,
            "write_file",
            r#"{"path":"/app2/x","content":""}"#
        )
        .await,
        Err(ToolError::OutsideWorkspace(_))
    ));
    assert_eq!(fake.calls().len(), before, "nothing ran for refused paths");
    assert!(
        Workspace::remote(
            Remote::Docker {
                container: "web".into()
            },
            &["app".into()],
            fake
        )
        .is_err()
    );
}

/// Runs a command in a real WSL distribution: `cargo test -p imperium-tools -- --ignored` on a
/// Windows machine with WSL (`IMPERIUM_TEST_WSL_DISTRO` names the distribution; Ubuntu by
/// default).
#[tokio::test]
#[ignore = "needs WSL"]
async fn a_real_wsl_distribution_runs_commands_and_file_tools() {
    let distro = std::env::var("IMPERIUM_TEST_WSL_DISTRO").unwrap_or_else(|_| "Ubuntu".into());
    let workspace = Workspace::remote(
        Remote::Wsl { distro },
        &["/tmp".into()],
        Arc::new(imperium_tools::ProcessRunner),
    )
    .unwrap();
    let output = call(
        &workspace,
        "run_command",
        r#"{"command":"uname -s && pwd"}"#,
    )
    .await
    .unwrap();
    assert!(
        output.contains("Linux") && output.contains("/tmp"),
        "{output}"
    );
    call(
        &workspace,
        "write_file",
        r#"{"path":"imperium-wsl-test/a.txt","content":"from windows"}"#,
    )
    .await
    .unwrap();
    let read = call(
        &workspace,
        "read_file",
        r#"{"path":"imperium-wsl-test/a.txt"}"#,
    )
    .await
    .unwrap();
    assert!(read.contains("from windows"), "{read}");
}
