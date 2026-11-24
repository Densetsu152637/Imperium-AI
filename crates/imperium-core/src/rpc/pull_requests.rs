use super::*;
use imperium_protocol::methods::{
    PullRequestsBind, PullRequestsBindParams, PullRequestsStatus, PullRequestsStatusParams,
    PullRequestsUnbind, PullRequestsUnbindParams, PullRequestsUnbindResult,
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
            _ => return None,
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

/// The session, repository and number a bind names, after the checks that need no GitHub call.
fn bind_target(
    state: &DaemonState,
    params: &PullRequestsBindParams,
) -> Result<(Arc<Session>, String, u64), ErrorObject> {
    let session = state
        .session(&params.session_id)
        .ok_or_else(|| ErrorObject::new(codes::SESSION_NOT_FOUND, "no such session"))?;
    let (repo, number) = crate::pr_monitor::parse_pr_url(&params.url).map_err(refused)?;
    crate::pr_monitor::check_repo(state, &session, &repo).map_err(refused)?;
    Ok((session, repo, number))
}
