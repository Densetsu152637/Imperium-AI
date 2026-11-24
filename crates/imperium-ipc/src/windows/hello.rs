//! The Windows presence check: Windows Hello through `UserConsentVerifier`, raised for a small
//! top-level window this process owns (`IUserConsentVerifierInterop`), since a background daemon
//! has no window of its own (the parent module allows `unsafe`; each block says why it is sound).
//!
//! This is the stage 8c prototype. Whether the prompt reaches the foreground from the daemon is
//! not known: no CI runner has Hello set up, so CI exercises only the "not available" answer.
//! Moot until U7 holds: no approval counts on Windows before release builds are signed.

use std::io;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows_api::Security::Credentials::UI::{
    UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
};
use windows_api::Win32::System::WinRT::IUserConsentVerifierInterop;
use windows_api::core::{HSTRING, factory};
use windows_future::{AsyncStatus, IAsyncOperation};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, MSG, PM_REMOVE, PeekMessageW,
    RegisterClassW, SetForegroundWindow, TranslateMessage, WNDCLASSW, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};

use crate::Presence;

/// How often the answer is looked for while the window's messages are pumped.
const POLL: Duration = Duration::from_millis(50);

/// Asks the user to verify with Windows Hello, showing `message` in the prompt. Blocks until they
/// answer, or `timeout` passes, in which case the prompt is cancelled and the answer is
/// [`Presence::Refused`]. Hello not set up (or no device) is [`Presence::Unavailable`], and so is
/// a prompt the system would not raise for this process's window.
pub fn hello_check(message: &str, timeout: Duration) -> Presence {
    if let Err(why) = available() {
        return Presence::Unavailable(why);
    }
    let window = match OwnerWindow::create() {
        Ok(window) => window,
        Err(error) => {
            return Presence::Unavailable(format!("no window to raise Windows Hello for: {error}"));
        }
    };
    let interop = match factory::<UserConsentVerifier, IUserConsentVerifierInterop>() {
        Ok(interop) => interop,
        Err(error) => {
            return Presence::Unavailable(format!(
                "Windows Hello cannot be raised for a window: {error}"
            ));
        }
    };
    let message = HSTRING::from(message);
    // SAFETY: the window handle is live (owned by `window`, destroyed only when it drops, after
    // the operation ends) and the message is an owned HSTRING that outlives the call.
    let requested: windows_api::core::Result<IAsyncOperation<UserConsentVerificationResult>> = unsafe {
        interop.RequestVerificationForWindowAsync(
            windows_api::Win32::Foundation::HWND(window.0),
            &message,
        )
    };
    let operation = match requested {
        Ok(operation) => operation,
        Err(error) => {
            return Presence::Unavailable(format!(
                "Windows Hello would not prompt for this process: {error}"
            ));
        }
    };
    let deadline = Instant::now() + timeout;
    loop {
        window.pump();
        match operation.Status() {
            Ok(AsyncStatus::Started) => {}
            Ok(_) => break,
            Err(error) => return Presence::Refused(error.to_string()),
        }
        if Instant::now() >= deadline {
            let _ = operation.Cancel();
            return Presence::Refused(
                "nobody answered in time, so the prompt was cancelled".into(),
            );
        }
        std::thread::sleep(POLL);
    }
    match operation.GetResults() {
        Ok(result) => verdict(result),
        Err(error) => Presence::Refused(error.to_string()),
    }
}

/// Whether Windows Hello can verify the user here at all.
fn available() -> Result<(), String> {
    let answer = UserConsentVerifier::CheckAvailabilityAsync()
        .and_then(|operation| operation.join())
        .map_err(|error| format!("Windows Hello did not say whether it is available: {error}"))?;
    if answer == UserConsentVerifierAvailability::Available {
        Ok(())
    } else {
        Err(format!("Windows Hello is not available here ({answer:?})"))
    }
}

/// What a finished verification means for an approval.
fn verdict(result: UserConsentVerificationResult) -> Presence {
    if result == UserConsentVerificationResult::Verified {
        Presence::Passed
    } else if [
        UserConsentVerificationResult::DeviceNotPresent,
        UserConsentVerificationResult::NotConfiguredForUser,
        UserConsentVerificationResult::DisabledByPolicy,
    ]
    .contains(&result)
    {
        Presence::Unavailable(format!("Windows Hello is not available here ({result:?})"))
    } else {
        Presence::Refused(format!("Windows Hello answered {result:?}"))
    }
}

/// A one-pixel top-level window that owns the Hello prompt, destroyed on drop.
struct OwnerWindow(HWND);

impl OwnerWindow {
    fn create() -> io::Result<Self> {
        let class: Vec<u16> = "ImperiumPresenceCheck\0".encode_utf16().collect();
        let title: Vec<u16> = "Imperium\0".encode_utf16().collect();
        // SAFETY: a null name asks for this executable's own module handle; nothing is written.
        let instance = unsafe { GetModuleHandleW(null()) };
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(DefWindowProcW),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..WNDCLASSW::default()
        };
        // SAFETY: the structure and the class name it points at are alive for the call, which
        // copies them. A second registration fails harmlessly (the class exists); a real failure
        // shows up as `CreateWindowExW` failing below.
        unsafe { RegisterClassW(&window_class) };
        // SAFETY: the class and title are NUL-terminated UTF-16 alive for the call; no parent,
        // menu or creation data is passed.
        let window = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                class.as_ptr(),
                title.as_ptr(),
                WS_POPUP | WS_VISIBLE,
                0,
                0,
                1,
                1,
                null_mut(),
                null_mut(),
                instance,
                null(),
            )
        };
        if window.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the window is live; the call only asks for it to come to the front (Windows may
        // decline for a background process, which is part of what the prototype finds out).
        unsafe { SetForegroundWindow(window) };
        Ok(OwnerWindow(window))
    }

    /// Handles the window's pending messages, so it stays responsive while the prompt is up.
    fn pump(&self) {
        let mut message = MSG::default();
        // SAFETY: `message` is a valid, writable MSG; a null window takes this thread's messages.
        while unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } != 0 {
            // SAFETY: `message` was just filled in by `PeekMessageW`.
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}

impl Drop for OwnerWindow {
    fn drop(&mut self) {
        // SAFETY: the handle was created by this thread and is destroyed once, here.
        unsafe { DestroyWindow(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_verified_answer_passes() {
        assert_eq!(
            verdict(UserConsentVerificationResult::Verified),
            Presence::Passed
        );
        for unavailable in [
            UserConsentVerificationResult::DeviceNotPresent,
            UserConsentVerificationResult::NotConfiguredForUser,
            UserConsentVerificationResult::DisabledByPolicy,
        ] {
            assert!(matches!(verdict(unavailable), Presence::Unavailable(_)));
        }
        for refused in [
            UserConsentVerificationResult::Canceled,
            UserConsentVerificationResult::RetriesExhausted,
            UserConsentVerificationResult::DeviceBusy,
        ] {
            assert!(matches!(verdict(refused), Presence::Refused(_)));
        }
    }

    /// What the prototype answers on a CI runner, where Hello is never set up: unavailable, so an
    /// approval there would be `dialog-only`. Printed so the CI log records it. Outside CI it
    /// would prompt whoever runs the tests, so it is skipped there.
    #[test]
    fn a_runner_without_windows_hello_is_unavailable() {
        if std::env::var_os("CI").is_none() {
            println!("skipped: outside CI this would raise a real Windows Hello prompt");
            return;
        }
        let answer = hello_check("approve a test plugin", Duration::from_secs(20));
        println!("Windows Hello presence check on this runner: {answer:?}");
        assert!(matches!(answer, Presence::Unavailable(_)), "{answer:?}");
    }
}
