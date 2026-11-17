//! Evaluation sessions (`session.eval_create`, `session.eval_result`, `session.prompt_preview`).

use std::time::Duration;

use imperium_protocol::methods::PromptPreview;

use super::*;
use crate::eval::{EvalRun, PREVIEW_MESSAGE, Prepared};

/// How long a session that hit its deadline stays, so its client can still read the record.
const EVAL_GRACE: Duration = Duration::from_secs(30);

/// The longest a prompt preview waits for the stand-in model's turn.
const PREVIEW_WAIT: Duration = Duration::from_secs(60);

impl DaemonState {
    /// What the evaluation session `id` recorded, if it is one.
    pub(crate) fn eval_run(&self, id: &str) -> Option<Arc<EvalRun>> {
        self.evals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    /// Forgets what the evaluation session `id` recorded, and deletes the environment the run
    /// made for its folder.
    pub(super) fn forget_eval(&self, id: &str) {
        let run = self
            .evals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        if let Some(env) = run.and_then(|run| run.take_ephemeral_env()) {
            let _ = self.environments().delete(&env);
        }
    }

    /// Whether evaluation sessions may be made over loopback TCP (only a local socket is let in
    /// otherwise, since a page in the app's window reaches the daemon over TCP).
    pub(crate) fn eval_over_tcp(&self) -> bool {
        self.eval_tcp.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Lets evaluation sessions be made over loopback TCP, for tests.
    #[cfg(feature = "testing")]
    pub fn allow_eval_over_tcp(&self) {
        self.eval_tcp
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Starts an evaluation session in an environment, or in the environment of a folder, or in
    /// Home: the same choice `session.create` makes.
    pub(crate) fn create_eval(
        &self,
        environment_id: Option<&str>,
        project_dir: Option<&str>,
        prepared: Arc<Prepared>,
    ) -> Result<(Arc<Session>, broadcast::Receiver<AgentEvent>), CreateError> {
        let options = || SessionOptions {
            permission_mode: Some(prepared.permission_mode),
            effort: prepared.effort,
            eval: Some(EvalSetup(Arc::clone(&prepared))),
            ..SessionOptions::default()
        };
        // A folder that is already an environment's uses it. Any other gets an environment for
        // the run only: it is deleted with the session, and nobody is told of it.
        let mut made = None;
        let environment = match (environment_id, project_dir) {
            (Some(env), _) => env.to_string(),
            (None, Some(dir)) => match self.environments().existing_for_folder(dir)? {
                Some(env) => env.id,
                None => {
                    let env = self.environments().for_folder(dir)?;
                    made = Some(env.id.clone());
                    env.id
                }
            },
            (None, None) => crate::environments::HOME_ID.to_string(),
        };
        let created = self.create_in_environment(&environment, None, options());
        match (&created, made) {
            (Ok(_), made) => prepared.run.set_ephemeral_env(made),
            (Err(_), Some(made)) => {
                let _ = self.environments().delete(&made);
            }
            (Err(_), None) => {}
        }
        if let (Ok((session, _)), Some(seconds), Some(me)) =
            (&created, prepared.timeout_s, self.me.upgrade())
        {
            // The deadline is the daemon's: the session stops, and goes shortly after, even if
            // the client that made it has gone.
            let session = Arc::downgrade(session);
            let run = Arc::clone(&prepared.run);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(seconds)).await;
                let Some(session) = session.upgrade() else {
                    return;
                };
                run.mark_timeout();
                session.stop();
                let id = session.id().to_string();
                drop(session);
                tokio::time::sleep(EVAL_GRACE).await;
                let _ = me.delete_session(&id, false);
            });
        }
        created
    }

    /// The system prompt and tools a session for `prepared` sends first, from a session started
    /// for the purpose, answered by a stand-in model, and deleted again.
    pub(crate) async fn prompt_preview(
        &self,
        environment_id: Option<&str>,
        project_dir: Option<&str>,
        prepared: Arc<Prepared>,
    ) -> Result<PromptPreview, String> {
        let run = Arc::clone(&prepared.run);
        let (session, mut events) = self
            .create_eval(environment_id, project_dir, prepared)
            .map_err(|error| error.to_string())?;
        let id = session.id().to_string();
        let root = session.project_dir().to_string();
        let waited = async {
            session
                .send(PREVIEW_MESSAGE.to_string())
                .map_err(|error| error.to_string())?;
            loop {
                match events.recv().await {
                    Ok(event) if event.agent_id == imperium_protocol::session::ROOT_AGENT => {
                        match event.event {
                            imperium_protocol::session::SessionEvent::TurnCompleted { .. } => {
                                return Ok(());
                            }
                            imperium_protocol::session::SessionEvent::TurnFailed { message } => {
                                return Err(message);
                            }
                            _ => {}
                        }
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err("the session closed".to_string());
                    }
                }
            }
        };
        let outcome = tokio::time::timeout(PREVIEW_WAIT, waited).await;
        drop(session);
        let _ = self.delete_session(&id, false);
        match outcome {
            Ok(Ok(())) => run
                .preview(&root)
                .ok_or_else(|| "the root agent made no request".to_string()),
            Ok(Err(message)) => Err(message),
            Err(_) => Err("the preview took too long".to_string()),
        }
    }
}
