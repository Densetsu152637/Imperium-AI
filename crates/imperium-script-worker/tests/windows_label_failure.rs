//! Windows: a file in the daemon's folders that cannot take the `NO_READ_UP` label makes code mode
//! unavailable, naming that file (plan C1, "Windows"; N2). Its own test binary, because the
//! failure is recorded for the rest of the process, as it is for the daemon.
#![cfg(windows)]
#![allow(unsafe_code)]

mod common;

use std::os::windows::ffi::OsStrExt as _;

use imperium_script::{Launcher, ScriptError, windows::labels};
use windows_sys::Win32::Security::{
    ACL,
    Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
        SetNamedSecurityInfoW,
    },
    DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR,
};

/// The current user's SID as text, from `whoami /user /fo csv /nh`.
fn user_sid() -> String {
    let output = std::process::Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    text.trim()
        .rsplit(',')
        .next()
        .unwrap()
        .trim_matches('"')
        .to_owned()
}

/// Gives `path` a protected access list that lets this user do everything but change its owner,
/// which setting a mandatory label needs.
fn deny_relabelling(path: &std::path::Path) {
    let sddl = format!(
        "D:P(D;;WO;;;{sid})(A;;FA;;;{sid})(A;;FA;;;SY)",
        sid = user_sid()
    );
    let sddl: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: NUL-terminated strings and valid out-pointers; the descriptor is used while alive
    // and then leaked (a test).
    unsafe {
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        assert_ne!(
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                std::ptr::null_mut(),
            ),
            0
        );
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl: *mut ACL = std::ptr::null_mut();
        assert_ne!(
            GetSecurityDescriptorDacl(
                descriptor,
                &raw mut present,
                &raw mut dacl,
                &raw mut defaulted
            ),
            0
        );
        let status = SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl,
            std::ptr::null(),
        );
        assert_eq!(status, 0, "cannot set the test file's access list");
    }
}

#[tokio::test]
async fn a_file_that_cannot_be_labelled_makes_code_mode_unavailable_naming_it() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let stuck = data.join("not-mine.json");
    std::fs::write(&stuck, "secret").unwrap();
    deny_relabelling(&stuck);

    let reason = labels::apply(std::slice::from_ref(&data)).unwrap_err();
    eprintln!("LABEL FAILURE {reason}");
    assert!(reason.contains("not-mine.json"), "{reason}");
    assert_eq!(labels::status(), Err(reason.clone()));

    let launcher = Launcher::new(common::worker_path(), 8);
    match launcher.start("print(1)", &common::quick_limits()).await {
        Err(ScriptError::Unavailable(why)) => {
            assert!(
                why.contains("labels") && why.contains("not-mine.json"),
                "{why}"
            );
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
    assert!(
        launcher
            .check()
            .await
            .unwrap_err()
            .contains("not-mine.json")
    );
}
