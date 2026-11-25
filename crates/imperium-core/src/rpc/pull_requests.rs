use super::*;
use imperium_protocol::methods::{
    PullRequestsAutoMergeChallenge, PullRequestsAutoMergeChallengeParams,
    PullRequestsAutoMergeChallengeResult, PullRequestsBind, PullRequestsBindParams,
    PullRequestsSetAutoMerge, PullRequestsSetAutoMergeParams, PullRequestsStatus,
    PullRequestsStatusParams, PullRequestsUnbind, PullRequestsUnbindParams,
    PullRequestsUnbindResult,
};

impl Connection {
    /// The `pull_requests` methods: `None` for a method of another group.
    pub(super) fn dispatch_pull_requests(
        &mut self,
        c: &mut Dispatch<'_>,
    ) -> Option<Result<Value, ErrorObject>> {
        Some(match c.method {
            PullRequestsBind::NAME => {
                self.show_pull_requests();
                call_later::<PullRequestsBind, _>(c.params.take(), c.later, |params| {
                    let state = Arc::clone(&self.state);
                    let prepared = bind_target(&state, &params);
                    async move {
                        let (session, repo, number) = prepared?;
                        let monitor = state.pr_monitor();
                        monitor.bind(&state, &session, &repo, number);
                        if let Some(status) = monitor.fresh_status(session.id()) {
                            return Ok(status);
                        }
                        match monitor.refresh(&state, session.id()).await {
                            Ok(status) => Ok(status),
                            Err(error) => {
                                // Nothing read: the card would have nothing to show.
                                monitor.unbind(&state, &session);
                                Err(refused(error))
                            }
                        }
                    }
                })
            }
            PullRequestsUnbind::NAME => {
                call::<PullRequestsUnbind>(c.params.take(), |params: PullRequestsUnbindParams| {
                    let session = self.state.session(&params.session_id).ok_or_else(|| {
                        ErrorObject::new(codes::SESSION_NOT_FOUND, "no such session")
                    })?;
                    Ok(PullRequestsUnbindResult {
                        was_bound: self.state.pr_monitor().unbind(&self.state, &session),
                    })
                })
            }
            PullRequestsStatus::NAME => {
                self.show_pull_requests();
                call::<PullRequestsStatus>(c.params.take(), |params: PullRequestsStatusParams| {
                    self.state
                        .pr_monitor()
                        .status(&params.session_id)
                        .map_err(refused)
                })
            }
            // Synchronous: a challenge belongs to this connection.
            PullRequestsAutoMergeChallenge::NAME => call::<PullRequestsAutoMergeChallenge>(
                c.params.take(),
                |params: PullRequestsAutoMergeChallengeParams| {
                    self.require_desktop_app()?;
                    let asked = crate::automerge::Asked {
                        session_id: params.session_id,
                        url: params.url,
                        head_sha: params.head_sha,
                        method: params.merge_method,
                    };
                    let (challenge, expires_unix_ms) = self
                        .auto_merge_challenges
                        .issue(asked, imperium_api::time::unix_ms());
                    Ok(PullRequestsAutoMergeChallengeResult {
                        challenge,
                        expires_unix_ms,
                    })
                },
            ),
            PullRequestsSetAutoMerge::NAME => self.set_auto_merge(c),
            _ => return None,
        })
    }

    /// `pull_requests.set_auto_merge`. Turning it on is checked here, on the connection that holds
    /// the challenge: the caller is the desktop app (never a phone, a mod's page, an agent, a hook
    /// or a plugin), the challenge is this connection's, unused, young and for exactly this
    /// session, pull request, head commit and method. Only then does the call go to GitHub.
    /// Turning it off is allowed for any caller that may call the method at all.
    fn set_auto_merge(&mut self, c: &mut Dispatch<'_>) -> Result<Value, ErrorObject> {
        let params: PullRequestsSetAutoMergeParams =
            decode::<PullRequestsSetAutoMerge>(c.params.take())?;
        self.show_pull_requests();
        let state = Arc::clone(&self.state);
        if !params.enabled {
            *c.later = Some(Box::pin(async move {
                state
                    .pr_monitor()
                    .disable_auto_merge(&state, &params.session_id, &params.url)
                    .await
                    .map(|status| to_value(&status))
                    .map_err(refused)
            }));
            return Ok(Value::Null);
        }
        self.require_desktop_app()?;
        let (Some(method), Some(token)) = (params.merge_method, params.challenge.as_deref()) else {
            return Err(forbidden(
                "turning auto-merge on needs a merge method and a challenge from the desktop app",
            ));
        };
        let asked = crate::automerge::Asked {
            session_id: params.session_id.clone(),
            url: params.url.clone(),
            head_sha: params.head_sha.clone(),
            method,
        };
        self.auto_merge_challenges
            .redeem(token, &asked, imperium_api::time::unix_ms())
            .map_err(forbidden)?;
        *c.later = Some(Box::pin(async move {
            state
                .pr_monitor()
                .enable_auto_merge(
                    &state,
                    &params.session_id,
                    &params.url,
                    &params.head_sha,
                    method,
                )
                .await
                .map(|status| to_value(&status))
                .map_err(refused)
        }));
        Ok(Value::Null)
    }

    /// Whether this connection is the desktop app installed beside `imperium-core`, on the local
    /// socket (checks 1 to 4 of [`crate::app_identity`]). A phone, a page reaching the daemon over
    /// loopback TCP (a mod's), and any other local process (an agent's command, a hook, a plugin's
    /// server) fail it.
    fn require_desktop_app(&self) -> Result<(), ErrorObject> {
        if self.allowed.is_some() {
            return Err(forbidden("a paired device cannot change auto-merge"));
        }
        #[cfg(feature = "testing")]
        if self.desktop_app_for_tests {
            return Ok(());
        }
        crate::app_identity::Identify::identify(
            &crate::app_identity::InstalledApp,
            self.identity.as_deref(),
        )
        .map_err(|reason| {
            forbidden(format!(
                "only the desktop app can turn auto-merge on: {reason}"
            ))
        })
    }

    /// Marks this connection as showing pull requests, so the poller runs at the faster
    /// interval while it is connected.
    fn show_pull_requests(&mut self) {
        if self.pr_watch.is_none() {
            self.pr_watch = Some(self.state.pr_monitor().watch());
        }
    }
}

/// A refusal for a caller that may not do this: `permission denied`.
fn forbidden(reason: impl std::fmt::Display) -> ErrorObject {
    ErrorObject::new(codes::FORBIDDEN, format!("permission denied: {reason}"))
}

/// The session, repository and number a bind names, after the checks that need no GitHub call.
fn bind_target(
    state: &DaemonState,
    params: &PullRequestsBindParams,
) -> Result<(Arc<Session>, String, u64), ErrorObject> {
    let session = state
        .session(&params.session_id)
        .ok_or_else(|| ErrorObject::new(codes::SESSION_NOT_FOUND, "no such session"))?;
    let (repo, number) = crate::pr_monitor::parse_pr_url(&params.url).map_err(refused)?;
    crate::pr_monitor::check_repo(&session, &repo).map_err(refused)?;
    Ok((session, repo, number))
}
