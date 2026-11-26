//! Windows confinement of the worker (plan C1, "Windows"; stage 1b tests). A probe is launched
//! through the daemon's confined launch and reports what it can do.
#![cfg(windows)]
#![allow(unsafe_code)]

mod common;

use std::{
    collections::HashMap, ffi::OsString, net::TcpListener, os::windows::io::AsRawHandle as _,
    path::PathBuf, time::Duration,
};

use imperium_script::windows::{
    Confined,
    labels::{self, Labelled},
    spawn,
};
use tokio::io::AsyncReadExt as _;

/// What the probe reported, and its working folder with that folder's label (read while the
/// probe's launch still holds the folder).
struct Probed {
    seen: HashMap<String, String>,
    folder: PathBuf,
    folder_label: String,
}

async fn probe(args: Vec<OsString>) -> Probed {
    common::labelled_stand_in();
    let Confined {
        process,
        stdin,
        mut stdout,
        mut stderr,
        folder,
    } = spawn(&common::windows_probe_path(), &args, 64, None).expect("the probe launches");
    for pipe in [&stdin, &stdout, &stderr] {
        pipe.connect().await.unwrap();
    }
    drop(stdin);
    let folder_label = labels::label_of(folder.path()).unwrap();
    let mut out = Vec::new();
    let mut err = Vec::new();
    tokio::time::timeout(Duration::from_secs(60), async {
        let _ = stdout.read_to_end(&mut out).await;
        let _ = stderr.read_to_end(&mut err).await;
    })
    .await
    .expect("the probe finishes");
    let code = process.wait().await;
    let text = String::from_utf8_lossy(&out).into_owned();
    eprintln!(
        "PROBE exit={code:?}\n{text}\nSTDERR {}",
        String::from_utf8_lossy(&err)
    );
    assert_eq!(code, Some(0), "the probe failed");
    let seen = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.trim().to_owned()))
        .collect();
    Probed {
        seen,
        folder: folder.path().to_path_buf(),
        folder_label,
    }
}

fn arg(text: impl Into<OsString>) -> OsString {
    text.into()
}

/// A handle to `file` that a child created with plain inheritance would get.
fn make_inheritable(file: &std::fs::File) {
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    // SAFETY: a handle owned by `file`, which outlives the call.
    let set = unsafe {
        SetHandleInformation(
            file.as_raw_handle(),
            HANDLE_FLAG_INHERIT,
            HANDLE_FLAG_INHERIT,
        )
    };
    assert_ne!(set, 0, "{}", std::io::Error::last_os_error());
}

/// A Credential Manager entry for the test, deleted when dropped.
struct Credential(Vec<u16>);

impl Credential {
    fn write(target: &str) -> Self {
        use windows_sys::Win32::Security::Credentials::{
            CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredWriteW,
        };
        let mut name: Vec<u16> = target.encode_utf16().chain(Some(0)).collect();
        let mut secret = *b"not for a Low process";
        // SAFETY: an all-zero CREDENTIALW is valid; the fields set point to buffers that outlive
        // the call.
        let written = unsafe {
            let mut credential: CREDENTIALW = std::mem::zeroed();
            credential.Type = CRED_TYPE_GENERIC;
            credential.TargetName = name.as_mut_ptr();
            credential.CredentialBlobSize = secret.len() as u32;
            credential.CredentialBlob = secret.as_mut_ptr();
            credential.Persist = CRED_PERSIST_LOCAL_MACHINE;
            CredWriteW(&raw const credential, 0)
        };
        assert_ne!(written, 0, "{}", std::io::Error::last_os_error());
        Self(name)
    }
}

impl Drop for Credential {
    fn drop(&mut self) {
        use windows_sys::Win32::Security::Credentials::{CRED_TYPE_GENERIC, CredDeleteW};
        // SAFETY: a NUL-terminated name.
        unsafe { CredDeleteW(self.0.as_ptr(), CRED_TYPE_GENERIC, 0) };
    }
}

#[tokio::test]
async fn a_low_worker_cannot_write_read_the_daemons_files_reach_the_daemon_or_spawn() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::create_dir(&data).unwrap();
    // A file from before the label, and one created after it.
    std::fs::write(data.join("daemon.json"), "{\"token\":\"secret\"}").unwrap();
    labels::apply(std::slice::from_ref(&data)).unwrap();
    std::fs::write(data.join("secrets.enc"), "secret").unwrap();
    let outside = tempfile::tempdir().unwrap();
    let control = outside.path().join("control.txt");
    std::fs::write(&control, "readable").unwrap();
    let inherited_path = outside.path().join("inherited.txt");
    std::fs::write(&inherited_path, "inherit").unwrap();
    let inherited = std::fs::File::open(&inherited_path).unwrap();
    make_inheritable(&inherited);
    let profile = PathBuf::from(std::env::var_os("USERPROFILE").expect("USERPROFILE is set"))
        .join(format!("imperium-probe-{}.txt", std::process::id()));
    let pipe = imperium_ipc::Listener::bind(root.path()).unwrap();
    let target = format!("imperium-code-mode-probe-{}", std::process::id());
    let _credential = Credential::write(&target);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();

    let probed = probe(vec![
        arg("--write"),
        profile.clone().into(),
        arg("--read"),
        data.join("daemon.json").into(),
        arg("--read"),
        data.join("secrets.enc").into(),
        arg("--read"),
        control.clone().into(),
        arg("--pipe"),
        arg(pipe.address()),
        arg("--handle"),
        arg((inherited.as_raw_handle() as usize).to_string()),
        arg("--handle-len"),
        arg("7"),
        arg("--cred"),
        arg(&target),
        arg("--port"),
        arg(listener.local_addr().unwrap().port().to_string()),
    ])
    .await;
    let seen = &probed.seen;

    let confined = &seen["confined"];
    assert!(confined.contains("integrity=0x1000"), "{seen:?}");
    assert!(confined.contains("privileges=0"), "{seen:?}");
    assert!(confined.contains("job=yes"), "{seen:?}");
    assert_eq!(seen["env_count"], "0", "{seen:?}");
    assert_eq!(seen["cwd_empty"], "true", "{seen:?}");
    assert!(seen["write_profile"].starts_with("denied"), "{seen:?}");
    assert!(
        !profile.exists(),
        "a Low process wrote to the user's profile"
    );
    assert!(
        seen["read_0"].starts_with("denied"),
        "daemon.json: {seen:?}"
    );
    assert!(
        seen["read_1"].starts_with("denied"),
        "secrets.enc: {seen:?}"
    );
    assert_eq!(seen["read_2"], "allowed", "an unlabelled file: {seen:?}");
    assert!(seen["pipe"].starts_with("denied"), "{seen:?}");
    assert!(seen["spawn"].starts_with("denied"), "{seen:?}");
    assert_eq!(seen["handle"], "closed", "{seen:?}");
    // The working folder is outside the labelled folders, and labelled Low.
    assert!(
        !probed.folder.starts_with(root.path()),
        "{:?}",
        probed.folder
    );
    assert!(
        probed.folder_label.contains("LW"),
        "{}",
        probed.folder_label
    );
    assert!(
        !probed.folder.exists(),
        "the working folder outlived the probe"
    );
    // Recorded, not asserted (plan C9): the keyring and the network.
    eprintln!("KEYRING low_read={}", seen["cred_read"]);
    eprintln!("NETWORK low_connect={}", seen["connect"]);
}

#[tokio::test]
async fn labels_are_set_once_and_cover_files_created_and_renamed_later() {
    let root = tempfile::tempdir().unwrap();
    let folders: Vec<PathBuf> = ["data", "config", "sessions", "scratch"]
        .iter()
        .map(|name| root.path().join(name))
        .collect();
    std::fs::create_dir(&folders[0]).unwrap();
    std::fs::write(folders[0].join("before.txt"), "secret").unwrap();

    let first = labels::apply(&folders).unwrap();
    assert!(
        first.iter().all(|(_, done)| *done == Labelled::Set),
        "{first:?}"
    );
    assert!(labels::carries(
        &labels::label_of(&folders[0].join("before.txt")).unwrap(),
        false
    ));
    // A second start reads the labels and writes nothing.
    let second = labels::apply(&folders).unwrap();
    assert!(
        second.iter().all(|(_, done)| *done == Labelled::Already),
        "{second:?}"
    );
    assert!(labels::status().is_ok());

    let mut reads: Vec<PathBuf> = Vec::new();
    for folder in &folders {
        let file = folder.join("later.txt");
        std::fs::write(&file, "secret").unwrap();
        reads.push(file);
    }
    // The atomic-write pattern: written beside, then renamed over.
    let staged = folders[0].join("history.json.tmp");
    std::fs::write(&staged, "secret").unwrap();
    std::fs::rename(&staged, folders[0].join("history.json")).unwrap();
    reads.push(folders[0].join("history.json"));
    // A file moved in from an unlabelled folder.
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("moved.txt"), "secret").unwrap();
    std::fs::rename(
        outside.path().join("moved.txt"),
        folders[0].join("moved.txt"),
    )
    .unwrap();
    let control = outside.path().join("control.txt");
    std::fs::write(&control, "readable").unwrap();

    let moved_in = folders[0].join("moved.txt");
    let mut args = Vec::new();
    for path in reads.iter().chain([&moved_in, &control]) {
        args.push(arg("--read"));
        args.push(path.clone().into());
    }
    let probed = probe(args).await;
    let seen = &probed.seen;
    for (index, path) in reads.iter().enumerate() {
        assert!(
            seen[&format!("read_{index}")].starts_with("denied"),
            "{} was readable at Low: {seen:?}",
            path.display()
        );
    }
    let moved = reads.len();
    assert_eq!(seen[&format!("read_{}", moved + 1)], "allowed", "{seen:?}");
    // Recorded (plan C1, N2): a file moved in keeps the descriptor it had.
    eprintln!(
        "MOVED_IN low_read={} label={:?}",
        seen[&format!("read_{moved}")],
        labels::label_of(&moved_in).ok()
    );
}
