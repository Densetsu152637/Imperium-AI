//! The macOS presence check: LocalAuthentication's `deviceOwnerAuthentication` policy (Touch ID,
//! else the login password). `objc2-local-authentication` binds the framework, but every call
//! into it is `unsafe`, so this module allows `unsafe`; each block says why it is sound.
#![allow(unsafe_code)]

use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::{NSError, NSString};
use objc2_local_authentication::{LAContext, LAError, LAPolicy};

use crate::Presence;

/// How long to wait for the framework's answer after cancelling a check that timed out.
const CANCEL_WAIT: Duration = Duration::from_secs(5);

/// Asks the user to prove they are this Mac's owner, showing `reason` in the system prompt
/// ("<process> is trying to <reason>"). Blocks until they answer, or `timeout` passes, in which
/// case the prompt is withdrawn and the answer is [`Presence::Refused`].
pub fn device_owner_check(reason: &str, timeout: Duration) -> Presence {
    if reason.trim().is_empty() {
        // The framework throws on an empty reason; never get that far.
        return Presence::Refused("no reason to show in the prompt".into());
    }
    let policy = LAPolicy::DeviceOwnerAuthentication;
    // SAFETY: `new` allocates and initialises a context and has no preconditions; the context is
    // owned here and released when this function returns.
    let context = unsafe { LAContext::new() };
    // SAFETY: the context is alive; the call reads only the policy value and returns any error as
    // an owned object.
    if let Err(error) = unsafe { context.canEvaluatePolicy_error(policy) } {
        return Presence::Unavailable(describe(&error));
    }
    let (sender, answer) = mpsc::channel::<Result<(), (isize, String)>>();
    let reply = RcBlock::new(move |success: Bool, error: *mut NSError| {
        let outcome = if success.as_bool() {
            Ok(())
        } else {
            // SAFETY: the framework passes either null or an `NSError` that stays alive for the
            // duration of this call; it is only read here, and copied out as owned values.
            match unsafe { error.as_ref() } {
                Some(error) => Err((error.code(), describe(error))),
                None => Err((0, "the check failed".to_string())),
            }
        };
        let _ = sender.send(outcome);
    });
    let reason = NSString::from_str(reason);
    // SAFETY: the context outlives the evaluation: this function holds it until the reply arrives,
    // or invalidates it first. The reply block is sendable: it captures only an `mpsc::Sender`,
    // which is `Send`, and the framework may call it on any queue.
    unsafe { context.evaluatePolicy_localizedReason_reply(policy, &reason, &reply) };
    let outcome = match answer.recv_timeout(timeout) {
        Ok(outcome) => outcome,
        Err(_) => {
            // SAFETY: the context is alive; invalidating it ends the evaluation, which then
            // replies with `LAErrorAppCancel`.
            unsafe { context.invalidate() };
            let _ = answer.recv_timeout(CANCEL_WAIT);
            return Presence::Refused(
                "nobody answered in time, so the prompt was withdrawn".into(),
            );
        }
    };
    match outcome {
        Ok(()) => Presence::Passed,
        Err((code, why)) if unavailable(code) => Presence::Unavailable(why),
        Err((_, why)) => Presence::Refused(why),
    }
}

/// Error codes meaning this Mac offers no check here, rather than that the user declined.
fn unavailable(code: isize) -> bool {
    [
        LAError::PasscodeNotSet,
        LAError::NotInteractive,
        LAError::BiometryNotAvailable,
        LAError::BiometryNotEnrolled,
    ]
    .iter()
    .any(|error| error.0 == code)
}

fn describe(error: &NSError) -> String {
    format!(
        "{} (LocalAuthentication error {})",
        error.localizedDescription(),
        error.code()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_reason_is_refused_before_the_framework_is_asked() {
        assert!(matches!(
            device_owner_check(" ", Duration::from_secs(1)),
            Presence::Refused(_)
        ));
    }

    /// What the check answers on a CI runner, which has no one to answer a prompt: the system
    /// prompt goes up and is withdrawn when nobody answers in time. Printed so the CI log records
    /// it (the stage 8c outcome). Outside CI it would prompt whoever
    /// runs the tests, so it is skipped there.
    #[test]
    fn a_headless_runner_never_passes() {
        if std::env::var_os("CI").is_none() {
            println!("skipped: outside CI this would raise a real Touch ID or password prompt");
            return;
        }
        let answer = device_owner_check("approve a test plugin", Duration::from_secs(20));
        println!("LocalAuthentication presence check on this runner: {answer:?}");
        // Not unavailable and not an early refusal: the prompt was up until it was withdrawn.
        assert!(
            matches!(&answer, Presence::Refused(why) if why.contains("nobody answered in time")),
            "{answer:?}"
        );
    }

    #[test]
    fn biometry_and_passcode_errors_mean_unavailable_and_cancels_do_not() {
        assert!(unavailable(LAError::PasscodeNotSet.0));
        assert!(unavailable(LAError::NotInteractive.0));
        assert!(!unavailable(LAError::UserCancel.0));
        assert!(!unavailable(LAError::AppCancel.0));
        assert!(!unavailable(LAError::SystemCancel.0));
    }
}
