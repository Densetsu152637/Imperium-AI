//! Evaluation sessions (`session.eval_create`, `session.eval_result`, `session.prompt_preview`).

use std::time::Duration;

use imperium_protocol::methods::PromptPreview;

use super::*;
use crate::eval::{EvalRun, PREVIEW_MESSAGE, Prepared};

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

    /// Forgets what the evaluation session `id` recorded.
    pub(super) fn forget_eval(&self, id: &str) {
        self.evals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
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
            eval: Some(Arc::clone(&prepared)),
            ..SessionOptions::default()
        };
        match (environment_id, project_dir) {
            (Some(env), _) => self.create_in_environment(env, None, options()),
            (None, Some(dir)) => match self.environments().for_folder(dir) {
                Ok(env) => {
                    let created = self.create_in_environment(&env.id, None, options());
                    self.announce_environments();
                    created
                }
                Err(error) => Err(error.into()),
            },
            (None, None) => {
                self.create_in_environment(crate::environments::HOME_ID, None, options())
            }
        }
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
