//! Model downloads from Hugging Face: listing what a repository offers, download jobs running
//! in the background with `models.download_progress` notifications, and a rescan of the model
//! folders when one finishes so the model appears in `models.local`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use imperium_local::hf::{Choice, HfError, Hub, Weights, choose, default_variant, projectors};
use imperium_protocol::jsonrpc::{ErrorObject, codes};
use imperium_protocol::methods::{
    DownloadJob, HfFile, HfFilesParams, HfFilesResult, HfVariant, MODEL_DOWNLOAD_PROGRESS,
    ModelDownloadParams,
};
use tokio::task::AbortHandle;

use crate::DaemonState;

/// Progress notifications are sent at most this often per job, plus one as each file ends.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
const DEFAULT_REVISION: &str = "main";

/// Download jobs since the daemon started.
#[derive(Debug, Default)]
pub struct Downloads {
    jobs: Mutex<Vec<Entry>>,
    next_id: AtomicU64,
}

#[derive(Debug)]
struct Entry {
    job: DownloadJob,
    /// Stops the job's task; `None` once it has ended.
    abort: Option<AbortHandle>,
}

impl Downloads {
    fn lock(&self) -> MutexGuard<'_, Vec<Entry>> {
        // Every update is a single assignment, so the list stays consistent across a panic.
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Every job, oldest first.
    pub fn list(&self) -> Vec<DownloadJob> {
        self.lock().iter().map(|entry| entry.job.clone()).collect()
    }

    /// Stops a running job, leaving its `.part` files to resume from. False when no running
    /// job has that id.
    pub fn cancel(&self, state: &DaemonState, job_id: &str) -> bool {
        let job = {
            let mut jobs = self.lock();
            let Some(entry) = jobs
                .iter_mut()
                .find(|entry| entry.job.job_id == job_id && entry.job.state == "running")
            else {
                return false;
            };
            if let Some(abort) = entry.abort.take() {
                abort.abort();
            }
            entry.job.state = "cancelled".into();
            entry.job.clone()
        };
        state.announce(MODEL_DOWNLOAD_PROGRESS, job);
        true
    }

    /// Runs `change` on a job; `None` when there is no such job.
    fn update<R>(&self, job_id: &str, change: impl FnOnce(&mut Entry) -> R) -> Option<R> {
        let mut jobs = self.lock();
        let entry = jobs.iter_mut().find(|entry| entry.job.job_id == job_id)?;
        Some(change(entry))
    }
}

/// An RPC error for a Hub failure: the request's fault (a missing token, an unknown repository
/// or quantisation) or the network's.
fn rpc_error(error: HfError) -> ErrorObject {
    let code = match error {
        HfError::Http { .. } | HfError::Listing { .. } | HfError::Io(..) => codes::INTERNAL_ERROR,
        _ => codes::INVALID_PARAMS,
    };
    ErrorObject::new(code, error.to_string())
}

/// `<download_dir>/<owner>/<name>` for a repository id already checked by the Hub.
fn destination(state: &DaemonState, repo: &str) -> std::path::PathBuf {
    let mut dir = state.settings().models.download_dir(state.data_dir());
    dir.extend(repo.split('/'));
    dir
}

/// `models.hf_files`.
pub async fn files(
    state: Arc<DaemonState>,
    params: HfFilesParams,
) -> Result<HfFilesResult, ErrorObject> {
    let revision = params
        .revision
        .unwrap_or_else(|| DEFAULT_REVISION.to_string());
    let files = state
        .hub()
        .files(&params.repo, &revision)
        .await
        .map_err(rpc_error)?;
    let budget = match state.local_budget() {
        Some(budget) => budget,
        None => {
            let probing = Arc::clone(&state);
            tokio::task::spawn_blocking(move || crate::local::probed_budget(&probing.settings()))
                .await
                .map_err(|error| ErrorObject::new(codes::INTERNAL_ERROR, error.to_string()))?
        }
    };
    let variants = imperium_local::hf::variants(&files);
    let file_row = |file: &imperium_local::hf::RepoFile| HfFile {
        path: file.path.clone(),
        size_bytes: file.size,
        sha256: file.sha256.clone(),
    };
    Ok(HfFilesResult {
        default: default_variant(&variants).map(|variant| variant.name.clone()),
        variants: variants
            .iter()
            .map(|variant| HfVariant {
                name: variant.name.clone(),
                runtime: variant.weights.runtime().into(),
                model_id: variant.model_id(&params.repo),
                files: variant.files.iter().map(|file| file.path.clone()).collect(),
                size_bytes: variant.size_bytes(),
                fit: crate::local::fit(variant.size_bytes(), variant.weights, budget).into(),
            })
            .collect(),
        projectors: projectors(&files).into_iter().map(file_row).collect(),
        files: files.iter().map(file_row).collect(),
        download_dir: destination(&state, &params.repo).display().to_string(),
        repo: params.repo,
        revision,
    })
}

/// `models.download`: lists the repository, chooses the files, and starts a job downloading
/// them. Problems with the request (a token needed, no such quantisation) are reported here
/// rather than by the job.
pub async fn start(
    state: Arc<DaemonState>,
    params: ModelDownloadParams,
) -> Result<DownloadJob, ErrorObject> {
    let revision = params
        .revision
        .unwrap_or_else(|| DEFAULT_REVISION.to_string());
    let hub = state.hub();
    let files = hub
        .files(&params.repo, &revision)
        .await
        .map_err(rpc_error)?;
    let choice = Choice {
        quant: params.quant,
        file: params.file,
        mmproj: params.mmproj,
    };
    let variant = choose(&params.repo, &files, &choice).map_err(rpc_error)?;
    let dir = destination(&state, &params.repo);
    let downloads = state.downloads();
    let job = {
        let mut jobs = downloads.lock();
        // Two jobs writing the same `.part` files would corrupt each other.
        if jobs
            .iter()
            .any(|entry| entry.job.repo == params.repo && entry.job.state == "running")
        {
            return Err(ErrorObject::new(
                codes::INVALID_PARAMS,
                format!("{} is already downloading", params.repo),
            ));
        }
        let number = downloads.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let job = DownloadJob {
            job_id: format!("dl-{number}"),
            repo: params.repo.clone(),
            revision: revision.clone(),
            variant: variant.name.clone(),
            model: format!(
                "{}:{}",
                variant.weights.runtime(),
                variant.model_id(&params.repo)
            ),
            files: variant.files.iter().map(|file| file.path.clone()).collect(),
            dir: dir.display().to_string(),
            file: None,
            file_done_bytes: 0,
            file_total_bytes: 0,
            done_bytes: 0,
            total_bytes: variant.size_bytes(),
            state: "running".into(),
            message: None,
        };
        jobs.push(Entry {
            job: job.clone(),
            abort: None,
        });
        job
    };
    state.announce(MODEL_DOWNLOAD_PROGRESS, &job);
    let task = tokio::spawn(run(
        Arc::clone(&state),
        hub,
        job.clone(),
        variant.files,
        variant.weights,
    ));
    downloads.update(&job.job_id, |entry| match entry.job.state.as_str() {
        "running" => entry.abort = Some(task.abort_handle()),
        // Cancelled before the handle was stored.
        "cancelled" => task.abort(),
        _ => {}
    });
    Ok(job)
}

/// Downloads a job's files, reporting progress, then rescans the model folders.
async fn run(
    state: Arc<DaemonState>,
    hub: Hub,
    job: DownloadJob,
    files: Vec<imperium_local::hf::RepoFile>,
    weights: Weights,
) {
    let downloads = state.downloads();
    let dir = std::path::PathBuf::from(&job.dir);
    let mut last_sent: Option<Instant> = None;
    let outcome = hub
        .download(&job.repo, &job.revision, &files, &dir, |progress| {
            let send = progress.file_done == progress.file_total
                || last_sent.is_none_or(|sent| sent.elapsed() >= PROGRESS_INTERVAL);
            // Called for every chunk, so the job is only copied when it is sent.
            let updated = downloads.update(&job.job_id, |entry| {
                let job = &mut entry.job;
                if job.file.as_deref() != Some(progress.file) {
                    job.file = Some(progress.file.to_string());
                }
                job.file_done_bytes = progress.file_done;
                job.file_total_bytes = progress.file_total;
                job.done_bytes = progress.done;
                job.total_bytes = progress.total;
                send.then(|| job.clone())
            });
            if let Some(updated) = updated.flatten() {
                last_sent = Some(Instant::now());
                state.announce(MODEL_DOWNLOAD_PROGRESS, updated);
            }
        })
        .await;
    let (state_name, message) = match outcome {
        Ok(_) => {
            tracing::info!(repo = %job.repo, variant = %job.variant, ?weights, "model downloaded");
            let rescanning = Arc::clone(&state);
            let rescanned =
                tokio::task::spawn_blocking(move || rescanning.rescan_local_models()).await;
            match rescanned {
                Ok(()) => ("done", None),
                Err(error) => ("failed", Some(format!("rescan failed: {error}"))),
            }
        }
        Err(error) => {
            tracing::warn!(repo = %job.repo, %error, "model download failed");
            ("failed", Some(error.to_string()))
        }
    };
    let ended = downloads.update(&job.job_id, |entry| {
        // A cancelled job has already said so.
        (entry.job.state == "running").then(|| {
            entry.abort = None;
            entry.job.state = state_name.into();
            entry.job.message = message;
            entry.job.file = None;
            entry.job.clone()
        })
    });
    if let Some(job) = ended.flatten() {
        state.announce(MODEL_DOWNLOAD_PROGRESS, job);
    }
}
