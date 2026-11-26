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
    DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetSecurityDescriptorSacl,
    LABEL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PROTECTED_SACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
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

/// Makes `path` a file that cannot take the folder's label, whatever privileges the test process
/// holds: first its label part of the descriptor is made protected and empty (an inherited label
/// can no longer reach it), then its access list lets this user, its owner included, do everything
/// but change its owner (which setting a label needs) or read its descriptor (which checking a
/// label needs). A process that bypasses the access list still meets the protected label part.
fn make_unlabellable(path: &std::path::Path) {
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let protect = set(
        &name,
        "S:P",
        LABEL_SECURITY_INFORMATION | PROTECTED_SACL_SECURITY_INFORMATION,
    );
    eprintln!("LOCKED protected label part: status {protect}");
    let sddl = format!(
        "D:P(D;;WORC;;;{sid})(D;;WORC;;;OW)(A;;FA;;;{sid})(A;;FA;;;SY)",
        sid = user_sid()
    );
    let status = set(
        &name,
        &sddl,
        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
    );
    assert_eq!(status, 0, "cannot set the test file's access list");
}

/// Sets the parts of `name`'s descriptor that `what` names from `sddl`; the Win32 status.
fn set(name: &[u16], sddl: &str, what: u32) -> u32 {
    let sddl: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
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
        let mut sacl: *mut ACL = std::ptr::null_mut();
        GetSecurityDescriptorDacl(
            descriptor,
            &raw mut present,
            &raw mut dacl,
            &raw mut defaulted,
        );
        GetSecurityDescriptorSacl(
            descriptor,
            &raw mut present,
            &raw mut sacl,
            &raw mut defaulted,
        );
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            what,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl,
            sacl,
        )
    }
}

#[tokio::test]
async fn a_file_that_cannot_be_labelled_makes_code_mode_unavailable_naming_it() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let stuck = data.join("not-mine.json");
    std::fs::write(&stuck, "secret").unwrap();
    make_unlabellable(&stuck);
    eprintln!(
        "LOCKED sid={} label={:?}",
        user_sid(),
        labels::label_of(&stuck)
    );

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
