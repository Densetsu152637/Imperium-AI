//! The Linux presence check: polkit's `CheckAuthorization` over the system bus, for the action
//! `org.imperium.plugins.approve` (`auth_self`, with no `_keep`, so every approval asks), with
//! `AllowUserInteraction`, and this daemon's own process as the subject. The `.deb` and `.rpm`
//! install the action file under `/usr/share/polkit-1/actions`; the session's polkit
//! authentication agent draws the prompt.
//!
//! No details go with the request: polkit (0.121 and later) refuses `CheckAuthorization` details
//! from a caller that is not root or the action's owner, so the prompt cannot name the plugin; the
//! app's dialog, shown just before, does.
//!
//! Unavailable (so the approval is `dialog-only`): no system bus; no polkit on it (its name has
//! no owner and is not activatable); the action not registered (an AppImage, a source build); or
//! no authentication agent for this process (polkit answers "challenge", as when headless).
//! Every other "no" is a refusal, and so is every error once polkit is known to be there: the bus
//! failing to say whether polkit runs, `EnumerateActions` failing (`ServiceUnknown`,
//! `NameHasNoOwner` from a polkit that went away, anything else), or `CheckAuthorization`
//! failing.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use imperium_api::encoding::hex;
use zbus::blocking::Connection;
use zbus::zvariant::Value;

use super::Presence;

/// The polkit action the package installs.
pub(crate) const ACTION: &str = "org.imperium.plugins.approve";
const POLKIT: &str = "org.freedesktop.PolicyKit1";
const AUTHORITY_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
const AUTHORITY: &str = "org.freedesktop.PolicyKit1.Authority";
/// `CheckAuthorizationFlags.AllowUserInteraction`.
const ALLOW_USER_INTERACTION: u32 = 1;

/// One entry of `EnumerateActions`: `(ssssssuuua{ss})`, the action id first.
type ActionDescription = (
    String,
    String,
    String,
    String,
    String,
    String,
    u32,
    u32,
    u32,
    HashMap<String, String>,
);

/// What `CheckAuthorization` answers: authorized, challenge, details.
type AuthorizationResult = (bool, bool, HashMap<String, String>);

/// Asks polkit to authenticate the user for [`ACTION`] before `plugin` is approved.
pub(super) fn check(plugin: &str, timeout: Duration) -> Presence {
    tracing::info!("asking polkit to authenticate the user before approving {plugin}");
    check_action(ACTION, timeout)
}

fn check_action(action: &str, timeout: Duration) -> Presence {
    let bus = match Connection::system() {
        Ok(bus) => bus,
        Err(error) => return Presence::Unavailable(format!("no system bus: {error}")),
    };
    let present = polkit_present(&bus).map_err(|error| error.to_string());
    let listed = || registered(&bus, action).map_err(|error| error.to_string());
    if let Some(answer) = before_asking(present, listed, action) {
        return answer;
    }
    let subject = match own_subject() {
        Ok(subject) => subject,
        Err(error) => return Presence::Refused(error),
    };
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the OS random source is available");
    let cancellation = format!("imperium-{}", hex(&bytes));
    let details: HashMap<String, String> = HashMap::new();
    let (sender, answer) = mpsc::channel();
    let asking = bus.clone();
    let body = (
        subject,
        action.to_string(),
        details,
        ALLOW_USER_INTERACTION,
        cancellation.clone(),
    );
    std::thread::spawn(move || {
        let reply = asking
            .call_method(
                Some(POLKIT),
                AUTHORITY_PATH,
                Some(AUTHORITY),
                "CheckAuthorization",
                &body,
            )
            .and_then(|message| message.body().deserialize::<AuthorizationResult>());
        let _ = sender.send(reply);
    });
    match answer.recv_timeout(timeout) {
        Ok(Ok(result)) => verdict(result),
        Ok(Err(error)) => Presence::Refused(format!("polkit refused: {error}")),
        Err(_) => {
            let _ = bus.call_method(
                Some(POLKIT),
                AUTHORITY_PATH,
                Some(AUTHORITY),
                "CancelCheckAuthorization",
                &(cancellation,),
            );
            Presence::Refused("nobody answered in time, so the prompt was cancelled".into())
        }
    }
}

/// What the bus said before polkit is asked: `None` to go on and ask. `present` is whether
/// polkit is on the bus at all; `registered` lists its actions, asked only when it is.
fn before_asking(
    present: Result<bool, String>,
    registered: impl FnOnce() -> Result<bool, String>,
    action: &str,
) -> Option<Presence> {
    match present {
        Err(error) => Some(Presence::Refused(format!(
            "the system bus did not say whether polkit runs: {error}"
        ))),
        Ok(false) => Some(Presence::Unavailable(
            "no polkit on the system bus (not installed)".into(),
        )),
        Ok(true) => match registered() {
            Ok(true) => None,
            Ok(false) => Some(Presence::Unavailable(format!(
                "polkit has no action {action}: the package's action file is not installed"
            ))),
            Err(error) => Some(Presence::Refused(format!(
                "polkit did not list its actions: {error}"
            ))),
        },
    }
}

/// Whether polkit is on the system bus: its name has an owner now, or the bus can start it.
fn polkit_present(bus: &Connection) -> zbus::Result<bool> {
    let call = |method: &str| {
        bus.call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            method,
            &(),
        )
    };
    let owned: bool = bus
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "NameHasOwner",
            &(POLKIT,),
        )?
        .body()
        .deserialize()?;
    if owned {
        return Ok(true);
    }
    let activatable: Vec<String> = call("ListActivatableNames")?.body().deserialize()?;
    Ok(activatable.iter().any(|name| name == POLKIT))
}

/// Whether polkit knows `action` (its action file is installed).
fn registered(bus: &Connection, action: &str) -> zbus::Result<bool> {
    let actions: Vec<ActionDescription> = bus
        .call_method(
            Some(POLKIT),
            AUTHORITY_PATH,
            Some(AUTHORITY),
            "EnumerateActions",
            &("",),
        )?
        .body()
        .deserialize()?;
    Ok(actions.iter().any(|description| description.0 == action))
}

/// What polkit's answer means. With user interaction allowed, a "challenge" answer means polkit
/// found no authentication agent to ask through.
fn verdict((authorized, challenge, details): AuthorizationResult) -> Presence {
    if authorized {
        Presence::Passed
    } else if challenge {
        Presence::Unavailable("no polkit authentication agent runs for this process".into())
    } else if details.get("polkit.dismissed").map(String::as_str) == Some("true") {
        Presence::Refused("the polkit prompt was dismissed".into())
    } else {
        Presence::Refused("polkit did not authorize it".into())
    }
}

/// This process as a polkit `unix-process` subject: its pid, its start time (`/proc/self/stat`
/// field 22, which with the pid names one process) and its user.
fn own_subject() -> Result<(&'static str, HashMap<&'static str, Value<'static>>), String> {
    let stat = std::fs::read("/proc/self/stat").map_err(|error| error.to_string())?;
    let start = super::super::linux::start_ticks(&stat)
        .ok_or("this process's start time could not be read")?;
    let uid = i32::try_from(rustix::process::getuid().as_raw())
        .map_err(|_| "this process's user id does not fit polkit's subject")?;
    Ok((
        "unix-process",
        HashMap::from([
            ("pid", Value::from(std::process::id())),
            ("start-time", Value::from(start)),
            ("uid", Value::from(uid)),
        ]),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polkits_answers_map_to_presence() {
        assert_eq!(verdict((true, false, HashMap::new())), Presence::Passed);
        assert!(matches!(
            verdict((false, true, HashMap::new())),
            Presence::Unavailable(_)
        ));
        let dismissed = HashMap::from([("polkit.dismissed".to_string(), "true".to_string())]);
        assert!(matches!(
            verdict((false, false, dismissed)),
            Presence::Refused(why) if why.contains("dismissed")
        ));
        assert!(matches!(
            verdict((false, false, HashMap::new())),
            Presence::Refused(_)
        ));
    }

    /// Only a missing polkit or an unregistered action is unavailable before polkit is asked; an
    /// error once polkit is known to be there is a refusal, and the actions are listed only when
    /// polkit is there.
    #[test]
    fn errors_before_the_check_map_to_unavailable_or_refused() {
        let action = "org.imperium.plugins.approve";
        let never = || -> Result<bool, String> { panic!("polkit's actions must not be listed") };
        assert!(matches!(
            before_asking(Ok(false), never, action),
            Some(Presence::Unavailable(why)) if why.contains("no polkit")
        ));
        assert!(matches!(
            before_asking(Err("Disconnected".into()), never, action),
            Some(Presence::Refused(_))
        ));
        assert!(matches!(
            before_asking(Ok(true), || Ok(false), action),
            Some(Presence::Unavailable(why)) if why.contains("action file")
        ));
        for error in [
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "org.freedesktop.DBus.Error.NameHasNoOwner",
            "org.freedesktop.DBus.Error.NoReply",
        ] {
            assert!(matches!(
                before_asking(Ok(true), || Err(error.to_string()), action),
                Some(Presence::Refused(why)) if why.contains(error)
            ));
        }
        assert_eq!(before_asking(Ok(true), || Ok(true), action), None);
    }

    /// An action polkit does not know (as when the action file is not installed) is unavailable,
    /// never passed; so is a machine without polkit or a system bus. Nothing prompts.
    #[test]
    fn an_action_polkit_does_not_know_is_unavailable() {
        let answer = check_action("org.imperium.test.never-installed", Duration::from_secs(10));
        println!("polkit check for an unknown action: {answer:?}");
        assert!(matches!(answer, Presence::Unavailable(_)), "{answer:?}");
    }

    #[test]
    fn the_subject_is_this_process() {
        let (kind, subject) = own_subject().unwrap();
        assert_eq!(kind, "unix-process");
        assert_eq!(subject["pid"], Value::from(std::process::id()));
        assert!(matches!(subject["start-time"], Value::U64(ticks) if ticks > 0));
    }
}
