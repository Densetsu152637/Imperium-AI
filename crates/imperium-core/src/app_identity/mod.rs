//! Whether a local connection comes from the installed desktop app, and the challenge an approval
//! answers ([plan](../../../../docs/plans/plugin-marketplace.md#who-may-approve),
//! docs/06-extensibility.md#who-may-approve-a-plugin).
//!
//! A plugin approval counts only from the native dialog of the desktop app installed beside this
//! `imperium-core`. The daemon checks that itself, from what the operating system says about the
//! process on the other end of the connection, never from anything the caller sends:
//!
//! 1. the connection came in on the local socket (never loopback TCP, a paired phone or a mod
//!    frame); this is decided before any process is looked at;
//! 2. the peer was identified at accept without races ([`Peer`], per platform);
//! 3. its executable is the app installed beside `imperium-core`, outside any sandbox;
//! 4. its environment holds none of the loader or WebView2 overrides [`refused_variable`] names;
//! 5. it answers a single-use challenge ([`Challenges`]) issued to the same connection for the
//!    same folder, hash and run summary, at most two minutes old, and the folder still has that
//!    hash and run summary when it answers;
//! 6. the OS presence check, which the daemon raises itself after the challenge ([`presence`]):
//!    passed records `counted`, unavailable records `dialog-only`, refused records nothing (a
//!    debug build records `dev` without asking);
//! 7. on Windows, a release build compiled by the signing job, running from under the
//!    per-machine Program Files folder (`windows::may_count`); a debug build's `dev` approval is
//!    exempt.
//!
//! Linux, Windows and macOS are implemented (Windows reads no peer environment: the app checks
//! its own, stage 8b); another platform refuses every approval.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use imperium_api::encoding::hex;
use imperium_config::Paths;
use imperium_protocol::methods::{PluginsApprovalChallengeResult, PluginsApproveResult};

use crate::plugin_records::{self, ApprovalKind, ApprovalVerifier, Subject};

pub(crate) mod presence;

use presence::PresenceProvider;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod unsupported;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
use unsupported as platform;

pub(crate) use platform::Peer;

/// How long a challenge may be answered, in milliseconds.
const CHALLENGE_MS: u64 = 120_000;
/// Most unanswered challenges one connection holds; asking for more drops the oldest.
const MAX_OPEN: usize = 8;
/// The desktop app's executable name, installed beside `imperium-core`.
const APP_NAME: &str = "imperium-desktop";
/// Why a connection that did not come in on the local socket is refused.
const LOCAL_ONLY: &str =
    "approvals are taken only from the desktop app on the local socket: nothing was recorded";

/// The identity of the process on the other end of an accepted local-socket connection, taken at
/// accept; `None` when the operating system could not say.
pub(crate) fn at_accept(stream: &imperium_ipc::Stream) -> Option<Arc<Peer>> {
    platform::at_accept(stream).map(Arc::new)
}

/// The desktop app installed beside this `imperium-core`, as a canonical path.
pub(crate) fn installed_app() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let app = exe.with_file_name(format!("{APP_NAME}{}", std::env::consts::EXE_SUFFIX));
    app.canonicalize().map_err(|_| {
        format!(
            "no desktop app is installed beside imperium-core ({})",
            app.display()
        )
    })
}

/// The first variable in `names` whose presence refuses a peer (check 4). The list lives in
/// `imperium-ipc`, so the desktop app applies the same one to its own environment (stage 8b);
/// Windows reads no peer environment.
#[cfg_attr(windows, allow(unused_imports))]
pub(crate) use imperium_ipc::refused_variable;

/// Check 1: the connection came in on the local socket. Callers run it before anything else
/// about an approval, so a TCP caller learns nothing more.
pub(crate) fn local_only(peer: Option<&Peer>) -> Result<&Peer, String> {
    peer.ok_or_else(|| LOCAL_ONLY.to_string())
}

/// Checks 1 to 4 (and 7, on Windows) for a connection's peer; `None` is a connection that did
/// not come in on the local socket.
pub(crate) trait Identify {
    fn identify(&self, peer: Option<&Peer>) -> Result<(), String>;
}

/// The production check: the desktop app installed beside `imperium-core`.
pub(crate) struct InstalledApp;

impl Identify for InstalledApp {
    fn identify(&self, peer: Option<&Peer>) -> Result<(), String> {
        // Check 1 comes first: a TCP caller is refused before any process is looked at.
        let peer = local_only(peer)?;
        platform::identify(peer, &installed_app()?)
    }
}

/// One challenge waiting for its answer.
#[derive(Debug)]
struct Open {
    token: String,
    folder: PathBuf,
    hash: String,
    runs: String,
    expires_ms: u64,
}

/// The challenges issued to one connection (check 5). Each [`crate::rpc::Connection`] holds its
/// own, so a challenge cannot be answered on another connection.
#[derive(Debug, Default)]
pub(crate) struct Challenges {
    open: Vec<Open>,
}

impl Challenges {
    /// A new challenge for `folder` (canonical) with `hash` and `runs`, and when it expires.
    fn issue(&mut self, folder: PathBuf, hash: &str, runs: &str, now_ms: u64) -> (String, u64) {
        self.open.retain(|open| open.expires_ms > now_ms);
        if self.open.len() >= MAX_OPEN {
            self.open.remove(0);
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("the OS random source is available");
        let token = hex(&bytes);
        let expires_ms = now_ms.saturating_add(CHALLENGE_MS);
        self.open.push(Open {
            token: token.clone(),
            folder,
            hash: hash.to_string(),
            runs: runs.to_string(),
            expires_ms,
        });
        (token, expires_ms)
    }

    /// Takes the challenge `token`, which is gone whatever the answer, and checks that it was
    /// issued for `folder`, `hash` and `runs` and has not expired.
    fn redeem(
        &mut self,
        token: &str,
        folder: &Path,
        hash: &str,
        runs: &str,
        now_ms: u64,
    ) -> Result<(), String> {
        let index = self
            .open
            .iter()
            .position(|open| constant_time_eq::constant_time_eq(open.token.as_bytes(), token.as_bytes()))
            .ok_or("unknown challenge: a challenge is answered once, on the connection that asked for it")?;
        let open = self.open.remove(index);
        if open.expires_ms <= now_ms {
            return Err("the challenge expired: ask for a new one".into());
        }
        if open.folder != folder || open.hash != hash || open.runs != runs {
            return Err("the challenge was issued for other content".into());
        }
        Ok(())
    }
}

/// What one approval call is about: the plugin folder and the hash and run-summary digest the
/// dialog showed, asked at `now_ms` (Unix milliseconds).
pub(crate) struct Asked<'a> {
    pub(crate) folder: &'a Path,
    pub(crate) hash: &'a str,
    pub(crate) runs: &'a str,
    pub(crate) now_ms: u64,
    /// A plugin folder or a skill folder.
    pub(crate) subject: Subject,
}

/// The folder (canonical), its content hash, the digest an approval is bound to and the name the
/// presence prompt shows, as they are now.
fn content_for(
    subject: Subject,
    folder: &Path,
) -> Result<(PathBuf, String, String, String), String> {
    match subject {
        Subject::Plugin => {
            let (canon, hash, runs, plugin) = plugin_records::content(folder)?;
            Ok((canon, hash, runs, plugin.name))
        }
        Subject::Skill => crate::skill_records::content(folder),
    }
}

/// `plugins.approval_challenge`: answered only to a peer passing checks 1 to 4, and only while
/// the folder still has the hash and run summary the caller names.
pub(crate) fn challenge(
    identify: &dyn Identify,
    peer: Option<&Peer>,
    challenges: &mut Challenges,
    asked: &Asked<'_>,
) -> Result<PluginsApprovalChallengeResult, String> {
    identify.identify(peer)?;
    let (canon, hash, runs, _) = content_for(asked.subject, asked.folder)?;
    if hash != asked.hash || runs != asked.runs {
        return Err("the plugin is not what was shown: inspect it again".into());
    }
    let (challenge, expires_unix_ms) = challenges.issue(canon, &hash, &runs, asked.now_ms);
    Ok(PluginsApprovalChallengeResult {
        challenge,
        expires_unix_ms,
    })
}

/// What checks 1 to 5 established about an approval: the folder and the content the challenge
/// was issued for, and the plugin id the presence prompt names.
pub(crate) struct Redeemed {
    subject: Subject,
    folder: PathBuf,
    hash: String,
    runs: String,
    plugin: String,
}

/// `plugins.approve`, first part: checks 1 to 5 again, using the challenge up. Quick, so it runs
/// on the connection that holds the challenge; [`conclude`] then asks the user.
pub(crate) fn redeem(
    paths: &Paths,
    identify: &dyn Identify,
    peer: Option<&Peer>,
    challenges: &mut Challenges,
    asked: &Asked<'_>,
    token: &str,
) -> Result<Redeemed, String> {
    // Checks 1 to 4 again: the peer must still be the app that asked for the challenge.
    identify.identify(peer)?;
    let canon = asked
        .folder
        .canonicalize()
        .map_err(|error| format!("{}: {error}", asked.folder.display()))?;
    challenges.redeem(token, &canon, asked.hash, asked.runs, asked.now_ms)?;
    // The folder must still hold what the challenge was issued for.
    let (_, hash, runs, name) = content_for(asked.subject, &canon)?;
    if hash != asked.hash || runs != asked.runs {
        return Err("the plugin changed since the challenge: nothing was recorded".into());
    }
    let plugin = match asked.subject {
        Subject::Plugin => plugin_records::record_for(paths, &canon)
            .and_then(|record| record.id)
            .unwrap_or(name),
        Subject::Skill => name,
    };
    Ok(Redeemed {
        subject: asked.subject,
        folder: canon,
        hash,
        runs,
        plugin,
    })
}

/// `plugins.approve`, second part: check 7 (Windows) before anyone is asked, check 6 (the
/// presence check, which may wait minutes for the user), the identity once more, and the record.
/// The kind recorded, or why nothing was.
pub(crate) fn conclude(
    paths: &Paths,
    identify: &dyn Identify,
    verifier: &dyn ApprovalVerifier,
    presence: &dyn PresenceProvider,
    peer: Option<&Peer>,
    redeemed: &Redeemed,
) -> Result<ApprovalKind, String> {
    // A verifier that may write a counted approval raises the presence check: where check 7
    // refuses such an approval anyway (Windows before U7), the user is not asked for nothing.
    if verifier.may_write(ApprovalKind::Counted) {
        platform::may_count(ApprovalKind::Counted)?;
    }
    let kind = verifier.approval_kind(presence, &redeemed.plugin)?;
    platform::may_count(kind)?;
    // The check can take minutes: the peer must still be the app that asked.
    identify.identify(peer)?;
    // `record_approval` reads the folder again and refuses content other than the challenged.
    let recorded = match redeemed.subject {
        Subject::Plugin => plugin_records::record_approval(
            paths,
            verifier,
            &redeemed.folder,
            kind,
            &redeemed.hash,
            &redeemed.runs,
        ),
        Subject::Skill => crate::skill_records::record_approval(
            paths,
            verifier,
            &redeemed.folder,
            kind,
            &redeemed.hash,
            &redeemed.runs,
        ),
    };
    recorded?;
    Ok(kind)
}

/// The protocol answer for an approval's outcome: a refusal records nothing and says why.
pub(crate) fn answer(outcome: Result<ApprovalKind, String>) -> PluginsApproveResult {
    match outcome {
        Ok(kind) => PluginsApproveResult {
            approved: true,
            kind: Some(kind.as_str().into()),
            reason: None,
        },
        Err(reason) => PluginsApproveResult {
            approved: false,
            kind: None,
            reason: Some(reason),
        },
    }
}

/// `plugins.approve` in one call: every check of the module documentation; records the approval
/// only when all pass and `verifier` gives a kind.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn approve(
    paths: &Paths,
    identify: &dyn Identify,
    verifier: &dyn ApprovalVerifier,
    presence: &dyn PresenceProvider,
    peer: Option<&Peer>,
    challenges: &mut Challenges,
    asked: &Asked<'_>,
    token: &str,
) -> PluginsApproveResult {
    answer(
        redeem(paths, identify, peer, challenges, asked, token)
            .and_then(|redeemed| conclude(paths, identify, verifier, presence, peer, &redeemed)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_identity::presence::Presence;
    use crate::plugin_records::{ReleaseVerifier, TestVerifier};

    /// Passes checks 1 to 4 for any local-socket connection, so the challenge can be tested on
    /// every platform.
    struct LocalPeer;

    impl Identify for LocalPeer {
        fn identify(&self, peer: Option<&Peer>) -> Result<(), String> {
            local_only(peer).map(|_| ())
        }
    }

    struct Home {
        _dir: tempfile::TempDir,
        paths: Paths,
        folder: PathBuf,
    }

    /// A plugin with a hook under `<config>/plugins`.
    fn home() -> Home {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        // Migrate first, so the plugin below is not carried over.
        plugin_records::migrate(&paths, &[]).unwrap();
        let folder = paths.config_dir().join("plugins/p");
        std::fs::create_dir_all(folder.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(folder.join("hooks")).unwrap();
        std::fs::write(folder.join(".claude-plugin/plugin.json"), r#"{"name":"p"}"#).unwrap();
        std::fs::write(
            folder.join("hooks/hooks.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"true"}]}]}}"#,
        )
        .unwrap();
        Home {
            _dir: dir,
            paths,
            folder,
        }
    }

    fn content(home: &Home) -> (String, String) {
        let (_, hash, runs, _) = plugin_records::content(&home.folder).unwrap();
        (hash, runs)
    }

    #[test]
    fn a_tcp_connection_is_refused_before_any_process_check() {
        // `None` is what a loopback TCP connection, a phone or a mod frame carries.
        let error = InstalledApp.identify(None).unwrap_err();
        assert!(error.contains("local socket"), "{error}");
        let home = home();
        let (hash, runs) = content(&home);
        let asked = Asked {
            folder: &home.folder,
            hash: &hash,
            runs: &runs,
            now_ms: 0,
            subject: Subject::Plugin,
        };
        let mut challenges = Challenges::default();
        let error = challenge(&InstalledApp, None, &mut challenges, &asked).unwrap_err();
        assert!(error.contains("local socket"), "{error}");
    }

    #[test]
    fn a_challenge_is_single_use_short_lived_and_bound_to_its_content() {
        let folder = PathBuf::from("/plugins/p");
        let mut mine = Challenges::default();
        let (token, expires) = mine.issue(folder.clone(), "h", "r", 1_000);
        assert_eq!(expires, 1_000 + CHALLENGE_MS);
        // Another connection holds its own challenges and does not know this one.
        let mut other = Challenges::default();
        assert!(
            other
                .redeem(&token, &folder, "h", "r", 1_000)
                .unwrap_err()
                .contains("unknown")
        );
        // Used once, it is gone.
        mine.redeem(&token, &folder, "h", "r", 2_000).unwrap();
        assert!(
            mine.redeem(&token, &folder, "h", "r", 2_000)
                .unwrap_err()
                .contains("unknown")
        );
        // Expired.
        let (token, expires) = mine.issue(folder.clone(), "h", "r", 1_000);
        assert!(
            mine.redeem(&token, &folder, "h", "r", expires)
                .unwrap_err()
                .contains("expired")
        );
        // For another hash, run summary or folder; a refused answer uses the challenge up too.
        for (folder, hash, runs) in [
            ("/plugins/p", "x", "r"),
            ("/plugins/p", "h", "x"),
            ("/plugins/q", "h", "r"),
        ] {
            let (token, _) = mine.issue(PathBuf::from("/plugins/p"), "h", "r", 1_000);
            let error = mine
                .redeem(&token, Path::new(folder), hash, runs, 1_000)
                .unwrap_err();
            assert!(error.contains("other content"), "{error}");
            assert!(
                mine.redeem(&token, Path::new("/plugins/p"), "h", "r", 1_000)
                    .is_err()
            );
        }
    }

    #[test]
    fn a_challenge_is_issued_only_for_the_content_shown() {
        let home = home();
        let (hash, runs) = content(&home);
        let mut challenges = Challenges::default();
        let peer = Peer::test_peer();
        let stale = Asked {
            folder: &home.folder,
            hash: "0000",
            runs: &runs,
            now_ms: 0,
            subject: Subject::Plugin,
        };
        assert!(challenge(&LocalPeer, Some(&peer), &mut challenges, &stale).is_err());
        let stale = Asked {
            folder: &home.folder,
            hash: &hash,
            runs: "0000",
            now_ms: 0,
            subject: Subject::Plugin,
        };
        assert!(challenge(&LocalPeer, Some(&peer), &mut challenges, &stale).is_err());
        assert!(challenges.open.is_empty());
    }

    #[test]
    fn a_folder_changed_between_challenge_and_approve_is_refused() {
        let home = home();
        let (hash, runs) = content(&home);
        let asked = Asked {
            folder: &home.folder,
            hash: &hash,
            runs: &runs,
            now_ms: 0,
            subject: Subject::Plugin,
        };
        let peer = Peer::test_peer();
        let mut challenges = Challenges::default();
        let issued = challenge(&LocalPeer, Some(&peer), &mut challenges, &asked).unwrap();
        std::fs::write(home.folder.join("README.md"), "changed").unwrap();
        let presence = Answers::new(Presence::Passed);
        let result = approve(
            &home.paths,
            &LocalPeer,
            &TestVerifier,
            &presence,
            Some(&peer),
            &mut challenges,
            &asked,
            &issued.challenge,
        );
        assert!(!result.approved);
        assert!(result.reason.unwrap().contains("changed since"));
        assert!(presence.asked().is_empty());
        assert!(plugin_records::record_for(&home.paths, &home.folder).is_none());
    }

    /// A presence provider that answers `answer` and records which plugins it was asked about.
    struct Answers {
        answer: Presence,
        asked: std::sync::Mutex<Vec<String>>,
    }

    impl Answers {
        fn new(answer: Presence) -> Self {
            Answers {
                answer,
                asked: std::sync::Mutex::default(),
            }
        }

        fn asked(&self) -> Vec<String> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl PresenceProvider for Answers {
        fn check(&self, plugin: &str) -> Presence {
            self.asked.lock().unwrap().push(plugin.to_string());
            self.answer.clone()
        }
    }

    /// One whole approval of `home`'s plugin with `verifier` and `presence`.
    fn approve_with(
        home: &Home,
        verifier: &dyn ApprovalVerifier,
        presence: &dyn PresenceProvider,
    ) -> PluginsApproveResult {
        let (hash, runs) = content(home);
        let asked = Asked {
            folder: &home.folder,
            hash: &hash,
            runs: &runs,
            now_ms: 0,
            subject: Subject::Plugin,
        };
        let peer = Peer::test_peer();
        let mut challenges = Challenges::default();
        let issued = challenge(&LocalPeer, Some(&peer), &mut challenges, &asked).unwrap();
        approve(
            &home.paths,
            &LocalPeer,
            verifier,
            presence,
            Some(&peer),
            &mut challenges,
            &asked,
            &issued.challenge,
        )
    }

    fn recorded_kind(home: &Home) -> Option<ApprovalKind> {
        plugin_records::record_for(&home.paths, &home.folder)
            .and_then(|record| record.approval)
            .map(|approval| approval.kind)
    }

    /// The release verifier asks the injected presence provider after a valid challenge, and its
    /// answer decides the record: passed is `counted`, unavailable is `dialog-only`, a refusal or
    /// cancel records nothing.
    #[test]
    fn the_presence_check_decides_what_is_recorded() {
        for (answer, kind) in [
            (Presence::Passed, Some(ApprovalKind::Counted)),
            (
                Presence::Unavailable("no agent".into()),
                Some(ApprovalKind::DialogOnly),
            ),
            (Presence::Refused("cancelled".into()), None),
        ] {
            let home = home();
            let presence = Answers::new(answer.clone());
            let result = approve_with(&home, &ReleaseVerifier, &presence);
            if cfg!(windows) {
                // Check 7: this unsigned test build counts nothing on Windows, and the user is not
                // asked about an approval that could not count.
                assert!(!result.approved);
                let reason = result.reason.unwrap();
                assert!(reason.contains("not a signed release build"), "{reason}");
                assert!(presence.asked().is_empty());
                assert_eq!(recorded_kind(&home), None);
                continue;
            }
            // The prompt names the plugin.
            assert_eq!(presence.asked(), vec!["p".to_string()], "{answer:?}");
            assert_eq!(result.approved, kind.is_some(), "{answer:?}: {result:?}");
            assert_eq!(result.kind, kind.map(|kind| kind.as_str().to_string()));
            assert_eq!(recorded_kind(&home), kind, "{answer:?}");
            if kind.is_none() {
                let reason = result.reason.unwrap();
                assert!(reason.contains("cancelled"), "{reason}");
                assert!(reason.contains("nothing was recorded"), "{reason}");
            }
        }
    }

    /// A refused identity or challenge never reaches the presence check, so the user is never
    /// asked about an approval that could not be recorded.
    #[test]
    fn nobody_is_asked_before_the_challenge_holds() {
        let home = home();
        let (hash, runs) = content(&home);
        let asked = Asked {
            folder: &home.folder,
            hash: &hash,
            runs: &runs,
            now_ms: 0,
            subject: Subject::Plugin,
        };
        let peer = Peer::test_peer();
        let presence = Answers::new(Presence::Passed);
        let mut challenges = Challenges::default();
        let result = approve(
            &home.paths,
            &LocalPeer,
            &ReleaseVerifier,
            &presence,
            Some(&peer),
            &mut challenges,
            &asked,
            "not-a-challenge",
        );
        assert!(!result.approved);
        let result = approve(
            &home.paths,
            &InstalledApp,
            &ReleaseVerifier,
            &presence,
            None,
            &mut challenges,
            &asked,
            "not-a-challenge",
        );
        assert!(result.reason.unwrap().contains("local socket"));
        assert!(presence.asked().is_empty());
        assert_eq!(recorded_kind(&home), None);
    }

    /// `plugins.approve` has no field for a presence result: one a client adds is dropped when the
    /// request is read, and the kind still comes from the daemon's own check.
    #[test]
    fn a_presence_result_sent_by_a_client_is_not_accepted() {
        let params: imperium_protocol::methods::PluginsApproveParams =
            serde_json::from_value(serde_json::json!({
                "name": "p",
                "sha256": "h",
                "runs_digest": "r",
                "challenge": "c",
                "presence": "passed",
                "approved_kind": "counted",
                "approval_kind": "counted",
            }))
            .unwrap();
        let read = serde_json::to_value(&params).unwrap();
        let mut keys: Vec<_> = read.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["challenge", "name", "runs_digest", "sha256"]);
        // `kind` names what is approved (a plugin or a skill), never an approval kind: one that
        // claims `counted` is refused.
        assert!(crate::plugin_records::Subject::parse(Some("counted")).is_err());

        // And with the check refused, nothing is recorded whatever the client claimed.
        let home = home();
        let presence = Answers::new(Presence::Refused("cancelled".into()));
        let result = approve_with(&home, &ReleaseVerifier, &presence);
        assert!(!result.approved);
        assert_eq!(recorded_kind(&home), None);
    }

    /// The test verifier counts without a presence check (the flow's other checks are tested with
    /// it): recorded once, and the challenge cannot be answered again.
    #[test]
    fn a_counted_approval_is_recorded_once() {
        let home = home();
        let (hash, runs) = content(&home);
        let asked = Asked {
            folder: &home.folder,
            hash: &hash,
            runs: &runs,
            now_ms: 0,
            subject: Subject::Plugin,
        };
        let peer = Peer::test_peer();
        let mut challenges = Challenges::default();
        let presence = Answers::new(Presence::Passed);
        let issued = challenge(&LocalPeer, Some(&peer), &mut challenges, &asked).unwrap();
        let first = approve(
            &home.paths,
            &LocalPeer,
            &TestVerifier,
            &presence,
            Some(&peer),
            &mut challenges,
            &asked,
            &issued.challenge,
        );
        if cfg!(windows) {
            // Check 7: this unsigned test build counts nothing on Windows, whatever the verifier.
            assert!(!first.approved);
            let reason = first.reason.unwrap();
            assert!(reason.contains("not a signed release build"), "{reason}");
            return;
        }
        assert!(first.approved, "{:?}", first.reason);
        assert_eq!(first.kind.as_deref(), Some("counted"));
        let again = approve(
            &home.paths,
            &LocalPeer,
            &TestVerifier,
            &presence,
            Some(&peer),
            &mut challenges,
            &asked,
            &issued.challenge,
        );
        assert!(!again.approved);
    }

    #[test]
    fn the_dev_verifier_exists_only_in_debug_builds() {
        // `DebugVerifier` is compiled only under `debug_assertions` and records `dev` without
        // asking anyone; the production verifier of a release build asks the presence check.
        let refused = Answers::new(Presence::Refused("cancelled".into()));
        #[cfg(debug_assertions)]
        {
            assert_eq!(
                plugin_records::production_verifier().approval_kind(&refused, "p"),
                Ok(ApprovalKind::Dev)
            );
            assert!(refused.asked().is_empty());
        }
        #[cfg(not(debug_assertions))]
        assert!(
            plugin_records::production_verifier()
                .approval_kind(&refused, "p")
                .is_err()
        );
        assert!(ReleaseVerifier.approval_kind(&refused, "p").is_err());
        assert!(!ReleaseVerifier.may_write(ApprovalKind::Dev));
        assert!(!ReleaseVerifier.may_write(ApprovalKind::CarriedOver));
        assert!(ReleaseVerifier.may_write(ApprovalKind::Counted));
        assert!(ReleaseVerifier.may_write(ApprovalKind::DialogOnly));
    }
    // ---- A folder that did not come from a marketplace (`plugins.inspect_folder`) ----

    fn state_of(home: &Home) -> Arc<crate::DaemonState> {
        let settings = imperium_config::Config::from_layers([imperium_config::Layer::defaults()])
            .settings()
            .unwrap();
        crate::DaemonState::new(home.paths.clone(), settings, None)
    }

    fn folder_params(name: &str) -> imperium_protocol::methods::PluginsInspectFolderParams {
        imperium_protocol::methods::PluginsInspectFolderParams {
            name: name.into(),
            kind: None,
        }
    }

    /// A symbolic link or junction `link` to the folder `target`, or false where the OS or the
    /// account does not allow one.
    fn link_dir(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(target, link).is_ok()
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, link);
            false
        }
    }

    /// The challenge and the approval for what `plugins.inspect_folder` shows of `name`, with the
    /// test verifier and a presence check that passes.
    fn approve_inspected(
        home: &Home,
        state: &crate::DaemonState,
        name: &str,
    ) -> PluginsApproveResult {
        let preview = crate::plugin_install::inspect_folder(state, &folder_params(name)).unwrap();
        let asked = Asked {
            folder: &home.folder,
            hash: &preview.sha256,
            runs: &preview.runs_digest,
            now_ms: 0,
            subject: Subject::Plugin,
        };
        let peer = Peer::test_peer();
        let mut challenges = Challenges::default();
        let issued = challenge(&LocalPeer, Some(&peer), &mut challenges, &asked).unwrap();
        approve(
            &home.paths,
            &LocalPeer,
            &TestVerifier,
            &Answers::new(Presence::Passed),
            Some(&peer),
            &mut challenges,
            &asked,
            &issued.challenge,
        )
    }

    #[test]
    fn a_folder_is_inspected_with_the_hash_and_digest_the_gate_computes() {
        let home = home();
        let state = state_of(&home);
        let preview = crate::plugin_install::inspect_folder(&state, &folder_params("p")).unwrap();
        let (hash, runs) = content(&home);
        assert_eq!(preview.sha256, hash);
        assert_eq!(preview.runs_digest, runs);
        assert_eq!(preview.id, "p");
        assert_eq!(preview.name, "p");
        assert_eq!(preview.source_kind, "folder");
        assert!(preview.marketplace.is_empty());
        assert_eq!(preview.hooks.len(), 1);
        assert!(preview.hooks[0].command.contains("true"));
        assert!(preview.installed.as_ref().is_some_and(|i| i.same_content));
        assert!(preview.notes.iter().any(|note| note.contains("no record")));
        // The gate withholds this very folder, for want of a record.
        let gate = plugin_records::gate(&home.paths, &TestVerifier);
        assert_eq!(gate.withheld.len(), 1);
        assert!(
            gate.withheld[0]
                .reason
                .contains("not installed by Imperium")
        );
        // Inspecting changed nothing.
        assert!(plugin_records::record_for(&home.paths, &home.folder).is_none());
    }

    #[test]
    fn only_the_exact_name_of_an_installed_folder_is_inspected() {
        let home = home();
        let state = state_of(&home);
        let absolute = home.folder.display().to_string();
        let names = [
            "",
            ".",
            "..",
            "../p",
            "p/..",
            "p/",
            "./p",
            "a/b",
            "a\\b",
            "..\\p",
            "/p",
            "\\p",
            "C:\\p",
            "C:p",
            "p:stream",
            "p.",
            "p ",
            "con",
            "NUL",
            "aux.txt",
            "Com1",
            "lpt9.log",
            "CONIN$",
            "p\0",
            "p\n",
            "p*",
            // Not an installed folder, or not this folder's exact name.
            "missing",
            "P",
            "p@marketplace",
            "p2",
            absolute.as_str(),
        ];
        for name in names {
            let error = crate::plugin_install::inspect_folder(&state, &folder_params(name))
                .expect_err(&format!("{name:?} was accepted"));
            assert!(!error.is_empty(), "{name:?}");
        }
        crate::plugin_install::inspect_folder(&state, &folder_params("p")).unwrap();
    }

    #[test]
    fn a_link_that_leaves_the_plugins_folder_is_not_inspected() {
        let home = home();
        let state = state_of(&home);
        // A real plugin outside `<config>/plugins`, and a link to it inside.
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("elsewhere");
        std::fs::create_dir_all(target.join(".claude-plugin")).unwrap();
        std::fs::write(
            target.join(".claude-plugin/plugin.json"),
            r#"{"name":"elsewhere"}"#,
        )
        .unwrap();
        let link = home.paths.config_dir().join("plugins/escape");
        if !link_dir(&target, &link) {
            eprintln!("skipped: this account cannot make a symbolic link");
            return;
        }
        let error =
            crate::plugin_install::inspect_folder(&state, &folder_params("escape")).unwrap_err();
        assert!(error.contains("link"), "{error}");
    }

    #[test]
    fn an_unrecorded_folder_is_approved_through_the_challenge_and_then_loads() {
        let home = home();
        let state = state_of(&home);
        let result = approve_inspected(&home, &state, "p");
        if cfg!(windows) {
            // Check 7: this unsigned test build counts nothing on Windows, whatever the verifier.
            assert!(!result.approved);
            let gate = plugin_records::gate(&home.paths, &TestVerifier);
            assert_eq!(gate.withheld.len(), 1);
            return;
        }
        assert!(result.approved, "{:?}", result.reason);
        let gate = plugin_records::gate(&home.paths, &TestVerifier);
        assert!(gate.withheld.is_empty(), "{:?}", gate.withheld);
        assert_eq!(gate.allowed.len(), 1);
        // The record binds exactly what the inspection showed.
        let preview = crate::plugin_install::inspect_folder(&state, &folder_params("p")).unwrap();
        let record = plugin_records::record_for(&home.paths, &home.folder).unwrap();
        let approval = record.approval.unwrap();
        assert_eq!(approval.hash, preview.sha256);
        assert_eq!(approval.runs, preview.runs_digest);
        assert!(preview.notes.iter().all(|note| !note.contains("no record")));
    }

    /// Asks for and answers the challenge for the previewed folder `folder` of `subject`.
    fn approve_folder(
        home: &Home,
        folder: &Path,
        subject: Subject,
        preview: &imperium_protocol::methods::PluginPreview,
    ) -> PluginsApproveResult {
        let asked = Asked {
            folder,
            hash: &preview.sha256,
            runs: &preview.runs_digest,
            now_ms: 0,
            subject,
        };
        let peer = Peer::test_peer();
        let mut challenges = Challenges::default();
        let issued = challenge(&LocalPeer, Some(&peer), &mut challenges, &asked).unwrap();
        approve(
            &home.paths,
            &LocalPeer,
            &TestVerifier,
            &Answers::new(Presence::Passed),
            Some(&peer),
            &mut challenges,
            &asked,
            &issued.challenge,
        )
    }

    #[test]
    fn an_agent_installed_url_plugin_is_approved_through_the_challenge_and_then_loads() {
        let home = home();
        let state = state_of(&home);
        // A plugin with no hooks or servers that an agent installed from a git URL: recorded
        // with `origin = agent` and no approval.
        let folder = home.paths.config_dir().join("plugins/from-url");
        std::fs::create_dir_all(folder.join(".claude-plugin")).unwrap();
        std::fs::write(
            folder.join(".claude-plugin/plugin.json"),
            r#"{"name":"from-url"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(folder.join("agents")).unwrap();
        std::fs::write(
            folder.join("agents/helper.md"),
            "---
name: helper
description: Helps.
---
Help.
",
        )
        .unwrap();
        let (canon, hash, runs, plugin) = plugin_records::content(&folder).unwrap();
        plugin_records::add_record(
            &home.paths,
            &canon,
            plugin_records::Record {
                name: plugin.name,
                origin: plugin_records::Origin::Agent,
                id: None,
                source: Some("https://example.test/from-url.git".into()),
                commit: None,
                hash,
                runs,
                approval: None,
            },
        )
        .unwrap();
        let gate = plugin_records::gate(&home.paths, &TestVerifier);
        assert!(
            gate.withheld.iter().any(
                |withheld| withheld.folder == folder && withheld.reason.contains("desktop app")
            ),
            "{:?}",
            gate.withheld
        );
        // The folder preview (item 60's path) names where an agent got it.
        let preview =
            crate::plugin_install::inspect_folder(&state, &folder_params("from-url")).unwrap();
        assert!(
            preview
                .notes
                .iter()
                .any(|note| note.contains("https://example.test/from-url.git"))
        );
        assert_eq!(preview.agents.len(), 1);
        let result = approve_folder(&home, &canon, Subject::Plugin, &preview);
        if cfg!(windows) {
            // Check 7: this unsigned test build counts nothing on Windows, whatever the verifier.
            assert!(!result.approved);
            let gate = plugin_records::gate(&home.paths, &TestVerifier);
            assert!(
                gate.withheld
                    .iter()
                    .any(|withheld| withheld.folder == folder)
            );
            return;
        }
        assert!(result.approved, "{:?}", result.reason);
        let gate = plugin_records::gate(&home.paths, &TestVerifier);
        assert!(
            gate.withheld
                .iter()
                .all(|withheld| withheld.folder != folder),
            "{:?}",
            gate.withheld
        );
        assert!(gate.allowed.contains(&folder));
        // The record is still the agent's, now with the approval.
        let record = plugin_records::record_for(&home.paths, &folder).unwrap();
        assert_eq!(record.origin, plugin_records::Origin::Agent);
        assert!(record.approval.is_some());
    }

    #[test]
    fn an_agent_installed_skill_is_approved_through_the_challenge_and_then_loads() {
        let home = home();
        let state = state_of(&home);
        let folder = home.paths.config_dir().join("skills/notes");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("SKILL.md"),
            "---
name: notes
description: Takes notes.
---
Write things down.
",
        )
        .unwrap();
        crate::skill_records::record_install(
            &home.paths,
            &folder,
            "notes",
            Some("https://example.test/notes.git".into()),
            None,
        )
        .unwrap();
        let gate = crate::skill_records::gate(&home.paths, &TestVerifier);
        assert_eq!(gate.withheld.len(), 1);
        let params = imperium_protocol::methods::PluginsInspectFolderParams {
            name: "notes".into(),
            kind: Some("skill".into()),
        };
        let preview = crate::plugin_install::inspect_folder(&state, &params).unwrap();
        assert_eq!(preview.source_kind, "skill");
        assert_eq!(preview.skills, ["notes"]);
        assert!(preview.hooks.is_empty() && preview.servers.is_empty());
        // A skill's name does not resolve as a plugin folder.
        assert!(crate::plugin_install::resolve_subject(&state, Subject::Plugin, "notes").is_err());
        let canon = folder.canonicalize().unwrap();
        let result = approve_folder(&home, &canon, Subject::Skill, &preview);
        if cfg!(windows) {
            assert!(!result.approved);
            assert_eq!(
                crate::skill_records::gate(&home.paths, &TestVerifier)
                    .withheld
                    .len(),
                1
            );
            return;
        }
        assert!(result.approved, "{:?}", result.reason);
        let gate = crate::skill_records::gate(&home.paths, &TestVerifier);
        assert!(gate.withheld.is_empty(), "{:?}", gate.withheld);
        assert_eq!(gate.agent.len(), 1);
    }

    #[test]
    fn a_change_after_the_challenge_or_the_approval_voids_it() {
        let home = home();
        let state = state_of(&home);
        let preview = crate::plugin_install::inspect_folder(&state, &folder_params("p")).unwrap();
        let asked = Asked {
            folder: &home.folder,
            hash: &preview.sha256,
            runs: &preview.runs_digest,
            now_ms: 0,
            subject: Subject::Plugin,
        };
        let peer = Peer::test_peer();
        let mut challenges = Challenges::default();
        let issued = challenge(&LocalPeer, Some(&peer), &mut challenges, &asked).unwrap();
        // Any file at all, not only the ones a hook names.
        std::fs::write(home.folder.join("notes.txt"), "added after the challenge").unwrap();
        let result = approve(
            &home.paths,
            &LocalPeer,
            &TestVerifier,
            &Answers::new(Presence::Passed),
            Some(&peer),
            &mut challenges,
            &asked,
            &issued.challenge,
        );
        assert!(!result.approved);
        assert!(plugin_records::record_for(&home.paths, &home.folder).is_none());
        std::fs::remove_file(home.folder.join("notes.txt")).unwrap();
        if cfg!(windows) {
            return;
        }
        // An approval that was recorded stops counting once any file changes.
        assert!(approve_inspected(&home, &state, "p").approved);
        assert!(
            plugin_records::gate(&home.paths, &TestVerifier)
                .withheld
                .is_empty()
        );
        std::fs::write(home.folder.join("notes.txt"), "added after the approval").unwrap();
        let gate = plugin_records::gate(&home.paths, &TestVerifier);
        assert_eq!(gate.withheld.len(), 1);
        assert!(gate.withheld[0].reason.contains("changed"));
    }
    /// A copy of `home`'s plugin folder under another name.
    fn copy_plugin(home: &Home, name: &str) -> PathBuf {
        let copy = home.paths.config_dir().join("plugins").join(name);
        for relative in [".claude-plugin/plugin.json", "hooks/hooks.json"] {
            let target = copy.join(relative);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(home.folder.join(relative), target).unwrap();
        }
        copy
    }

    #[test]
    fn an_approval_names_the_exact_folder_not_one_that_looks_like_it() {
        let home = home();
        let state = state_of(&home);
        let spaced = copy_plugin(&home, " p");
        // The preview refuses a name with white space around it.
        assert!(crate::plugin_install::inspect_folder(&state, &folder_params(" p")).is_err());
        assert!(crate::plugin_install::inspect_folder(&state, &folder_params("p ")).is_err());
        // The approval of the folder " p" (identical content to "p") lands on " p", never on "p".
        let found = crate::plugin_install::resolve_for_approval(&state, " p").unwrap();
        assert_eq!(found, spaced.canonicalize().unwrap());
        let plain = crate::plugin_install::resolve_for_approval(&state, "p").unwrap();
        assert_eq!(plain, home.folder.canonicalize().unwrap());
        let (hash, runs) = content(&home);
        let asked = Asked {
            folder: &found,
            hash: &hash,
            runs: &runs,
            now_ms: 0,
            subject: Subject::Plugin,
        };
        let peer = Peer::test_peer();
        let mut challenges = Challenges::default();
        let issued = challenge(&LocalPeer, Some(&peer), &mut challenges, &asked).unwrap();
        let result = approve(
            &home.paths,
            &LocalPeer,
            &TestVerifier,
            &Answers::new(Presence::Passed),
            Some(&peer),
            &mut challenges,
            &asked,
            &issued.challenge,
        );
        if cfg!(windows) {
            assert!(!result.approved);
            return;
        }
        assert!(result.approved, "{:?}", result.reason);
        assert!(plugin_records::record_for(&home.paths, &spaced).is_some());
        assert!(plugin_records::record_for(&home.paths, &home.folder).is_none());
    }

    #[test]
    fn a_hook_that_runs_from_dot_git_is_flagged_because_the_hash_leaves_it_out() {
        for text in [
            "sh /x/p/.git/hooks/h.sh",
            "C:\\x\\.git\\h.bat",
            "cat /x/.git",
            "\"/x/.git\"",
        ] {
            assert!(crate::plugin_install::mentions_dot_git(text), "{text}");
        }
        for text in [
            "true",
            "/x/.github/h.sh",
            "/x/a.git/h",
            "/x/.gitignore",
            "git status",
        ] {
            assert!(!crate::plugin_install::mentions_dot_git(text), "{text}");
        }
        let home = home();
        let state = state_of(&home);
        let clean = crate::plugin_install::inspect_folder(&state, &folder_params("p")).unwrap();
        assert!(clean.notes.iter().all(|note| !note.contains(".git")));
        std::fs::write(
            home.folder.join("hooks/hooks.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"sh ${CLAUDE_PLUGIN_ROOT}/.git/h.sh"}]}]}}"#,
        )
        .unwrap();
        let flagged = crate::plugin_install::inspect_folder(&state, &folder_params("p")).unwrap();
        assert!(
            flagged
                .notes
                .iter()
                .any(|note| note.contains("WARNING") && note.contains(".git"))
        );
        // The exclusion the warning is about: a file under the top-level `.git` is not hashed.
        std::fs::create_dir_all(home.folder.join(".git")).unwrap();
        std::fs::write(home.folder.join(".git/h.sh"), "one").unwrap();
        let first = crate::plugin_install::inspect_folder(&state, &folder_params("p")).unwrap();
        std::fs::write(home.folder.join(".git/h.sh"), "two").unwrap();
        let second = crate::plugin_install::inspect_folder(&state, &folder_params("p")).unwrap();
        assert_eq!(first.sha256, second.sha256);
    }

    #[test]
    fn a_folder_swapped_for_a_link_is_neither_hashed_nor_approved() {
        let home = home();
        let state = state_of(&home);
        let moved = home.paths.config_dir().join("moved-away");
        std::fs::rename(&home.folder, &moved).unwrap();
        if !link_dir(&moved, &home.folder) {
            eprintln!("skipped: this account cannot make a symbolic link");
            return;
        }
        let error = plugin_records::content(&home.folder).unwrap_err();
        assert!(error.contains("link"), "{error}");
        assert!(crate::plugin_install::inspect_folder(&state, &folder_params("p")).is_err());
        let error = plugin_records::record_approval(
            &home.paths,
            &TestVerifier,
            &home.folder,
            ApprovalKind::Counted,
            "x",
            "y",
        )
        .unwrap_err();
        assert!(error.contains("link"), "{error}");
        assert!(crate::plugin_install::resolve_for_approval(&state, "p").is_err());
    }
}
