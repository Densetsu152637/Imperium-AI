//! Read-only calls of one model step that run at the same time (roadmap R2.9,
//! `harness.parallel_reads`).
//!
//! Only the reading itself overlaps. When the loop reaches the first of a run of file reads, the
//! run is executed at once (at most `parallel_reads` at a time) and the outputs are kept by call
//! id; then every call of the run goes through `run_tool` one after another, in call order, as
//! before: the permission judge, `tool.before` and `tool.after` hooks, approvals, events and the
//! result messages are all unchanged, and the call takes its output from the kept ones where it
//! would have read the file. Nothing is shown to the model, or even reported as started, ahead of
//! its place in the order.
//!
//! The results the model sees are those of a sequential run. A read whose output the run did not
//! keep (its task panicked, or the session was stopped while the run was reading) is simply read
//! again when its call's turn comes, exactly as without the setting. A stop therefore changes
//! nothing but time: reads still running are dropped, but a `glob` or `grep` already walking the
//! tree on the blocking pool cannot be aborted and finishes in the background, its result
//! discarded.
//!
//! A call is run ahead only when nothing in `run_tool` could stop or change it: it is a built-in
//! file read ([`Invocation::is_file_read`]) or a provider tool that opted in with
//! [`ToolProvider::read_only`] (never by default), the permission judge lets it run without asking, no
//! `tool.before` or `tool.after` hook applies to it, and none of the refusals `run_tool_once`
//! checks first (a fork's, a launcher's, the agent's tool list and permission profile, a variant's
//! dropped tools) applies. Any call that may write, run a command, spawn, or ask ends the run, so
//! every read still sees the effects of the calls before it.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use super::*;

/// The most reads that run at once, whatever `harness.parallel_reads` says.
const MOST_PARALLEL_READS: usize = 8;

/// A call's output as it came back (the error as the model is told it, after `error: `) and
/// how long the read took.
pub(super) type ReadOutput = (Result<String, String>, Duration);

/// What a run ahead executes: a built-in file read, or a read-only provider tool's call.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Ran {
    File(Invocation),
    Provider { name: String, arguments: String },
}

/// A call chosen to run ahead: what it runs, and the provider when it is a provider's tool.
pub(super) struct Ahead {
    ran: Ran,
    provider: Option<Arc<dyn ToolProvider>>,
}

/// What the run kept for a call: the call it ran and its output.
pub(super) type Kept = (Ran, ReadOutput);

type ReadJob = Pin<Box<dyn Future<Output = ReadOutput> + Send>>;

/// Runs `jobs` with at most `limit` in flight and stores each output in `slots` at the job's
/// position as it finishes. A job that panics leaves its slot empty. Dropping the future (the
/// session is stopped) cancels every job still running; the outputs stored so far stay.
pub(super) async fn run_bounded<T: Send + 'static>(
    jobs: Vec<Pin<Box<dyn Future<Output = T> + Send>>>,
    limit: usize,
    slots: &mut Vec<Option<T>>,
) {
    slots.clear();
    slots.resize_with(jobs.len(), || None);
    let mut waiting = jobs.into_iter().enumerate();
    let mut running = tokio::task::JoinSet::new();
    loop {
        while running.len() < limit.max(1)
            && let Some((index, job)) = waiting.next()
        {
            running.spawn(async move { (index, job.await) });
        }
        match running.join_next().await {
            Some(Ok((index, output))) => slots[index] = Some(output),
            Some(Err(_)) => {}
            None => break,
        }
    }
}

impl Agent {
    /// How many reads may run at once for this agent (below 2 means none run ahead).
    fn read_limit(&self) -> usize {
        (self.shared.harness.parallel_reads as usize).min(MOST_PARALLEL_READS)
    }

    /// The parsed call when it is a read that may run ahead of its turn in the order (see the
    /// module docs); `None` for every other call.
    fn concurrent_read(&self, call: &ToolCall) -> Option<Ahead> {
        // Without tool loading `call_tool` is no tool: `run_tool` does not unwrap it, so the
        // inner tool is not what would run, and nothing is run ahead.
        if call.function.name == CALL_TOOL && !self.reactive() {
            return None;
        }
        // The call `run_tool` runs: a `call_tool` call is the tool it stands for, and a
        // variant's rename is undone, as `run_tool_once` does before any check.
        let effective = effective_call(call);
        let mapped = self.engine_name(&effective.function.name);
        let name = mapped.as_deref().unwrap_or(&effective.function.name);
        let provider = self.provider_for(name);
        let invocation = match &provider {
            Some(provider) => {
                if !provider.read_only(name) || provider.always_asks(name) {
                    return None;
                }
                None
            }
            None => {
                let invocation = Invocation::parse(name, &effective.function.arguments).ok()?;
                if !invocation.is_file_read() {
                    return None;
                }
                Some(invocation)
            }
        };
        if self.early_refusal(name, false).is_some() {
            return None;
        }
        let tool = invocation
            .as_ref()
            .map_or(name, |invocation| invocation.tool_name());
        let hooks = &self.shared.extensions.hooks;
        if hooks.applies(HookEvent::ToolBefore, Some(tool))
            || hooks.applies(HookEvent::ToolAfter, Some(tool))
        {
            return None;
        }
        if let Some(provider) = provider {
            // What `run_provider` checks before it asks or runs the call: it must be valid, not
            // a command, not asking, and not waiting for workflow context.
            let arguments = &effective.function.arguments;
            let check = provider.check(name, arguments).ok()?;
            if check.ask || check.needs_context || check.command.is_some() {
                return None;
            }
            let judged = self.workspace.permissions().judge(
                name,
                &Target::Other,
                CallKind::Other { asks: false },
                Some(false),
            );
            return matches!(judged.decision, Decision::Run).then(|| Ahead {
                ran: Ran::Provider {
                    name: name.to_string(),
                    arguments: arguments.clone(),
                },
                provider: Some(provider),
            });
        }
        let invocation = invocation?;
        let tool = invocation.tool_name();
        let target = invocation
            .path()
            .map_or(Target::Other, |path| self.workspace.rule_target(path));
        let judged = self
            .workspace
            .permissions()
            .judge(tool, &target, CallKind::Read, None);
        matches!(judged.decision, Decision::Run).then_some(Ahead {
            ran: Ran::File(invocation),
            provider: None,
        })
    }

    /// Called when the loop reaches `calls[0]`: when it starts a run of two or more reads that
    /// may run ahead, runs them at once and keeps their outputs for `run_tool`. Returns how many
    /// calls it covered (at least 1), so the loop asks again after them.
    pub(super) async fn prefetch_reads(&self, calls: &[ToolCall]) -> usize {
        let limit = self.read_limit();
        if limit < 2 {
            return 1;
        }
        let mut run: Vec<(String, Ahead)> = Vec::new();
        for call in calls {
            // Outputs are kept by call id, so a repeated id ends the run.
            if run.iter().any(|(id, _)| *id == call.id) {
                break;
            }
            let Some(ahead) = self.concurrent_read(call) else {
                break;
            };
            run.push((call.id.clone(), ahead));
        }
        let covered = run.len().max(1);
        if run.len() < 2 {
            return covered;
        }
        let mut stop = self.shared.controls.stop_signal();
        // A stopped session runs its calls as it always did.
        let stopped = *stop.borrow_and_update();
        if stopped || !self.shared.snapshot_ready().await {
            return covered;
        }
        let window = self.expected_window();
        let jobs: Vec<ReadJob> = run
            .iter()
            .map(|(_, ahead)| {
                let (ahead, workspace) = (ahead.clone_parts(), self.workspace.clone());
                #[cfg(feature = "testing")]
                let gate = self.shared.read_gate.clone();
                Box::pin(async move {
                    let began = Instant::now();
                    #[cfg(feature = "testing")]
                    if let (Some(gate), Ran::File(invocation)) = (gate, &ahead.0) {
                        gate.before(invocation).await;
                    }
                    let result = match ahead {
                        (Ran::File(invocation), _) => invocation
                            .execute_in(&workspace, window)
                            .await
                            .map_err(|error| error.to_string()),
                        (Ran::Provider { name, arguments }, Some(provider)) => {
                            // No events and no sampling while it runs ahead: the call's own
                            // turn in the order is where the session reports it.
                            let emit = |_: ToolEvent| {};
                            let context = ToolContext {
                                root: workspace.root(),
                                emit: &emit,
                                sampler: None,
                            };
                            provider.call(&context, &name, &arguments).await
                        }
                        (Ran::Provider { .. }, None) => Err("no provider".into()),
                    };
                    (result, began.elapsed())
                }) as ReadJob
            })
            .collect();
        let mut slots = Vec::new();
        {
            let running = run_bounded(jobs, limit, &mut slots);
            // A stop drops the reads still running and keeps the ones that finished; the rest
            // are read again in order when their calls come, as in a sequential run.
            tokio::select! {
                () = running => {}
                Ok(_) = stop.wait_for(|stopped| *stopped) => {}
            }
        }
        let mut kept = lock(&self.prefetched);
        for ((id, ahead), slot) in run.into_iter().zip(slots) {
            if let Some(output) = slot {
                kept.insert(id, (ahead.ran, output));
            }
        }
        covered
    }

    /// The outputs kept for calls that did not take them (one a check refused after the run).
    pub(super) fn clear_prefetched(&self) {
        lock(&self.prefetched).clear();
    }

    /// Drops the output kept for the call `id`: its turn came and it is done with it, or it asks.
    pub(super) fn forget_prefetched(&self, id: &str) {
        lock(&self.prefetched).remove(id);
    }

    /// What `invocation` returns for the call `call`: the output the run kept for it, or its
    /// own execution. `started` is when the call was announced; the kept read's duration is
    /// counted back from it so events report how long the read really took.
    pub(super) async fn read_result(
        &self,
        call: &ToolCall,
        invocation: &Invocation,
        started: Instant,
    ) -> (Result<String, String>, Instant) {
        let kept = lock(&self.prefetched).remove(&call.id);
        match kept {
            Some((Ran::File(ran), (result, took))) if ran == *invocation => {
                (result, started.checked_sub(took).unwrap_or(started))
            }
            _ => (
                invocation
                    .execute_in(&self.workspace, self.expected_window())
                    .await
                    .map_err(|error| error.to_string()),
                started,
            ),
        }
    }

    /// The output the run kept for a provider's call `call` (`name` with `arguments`), if it ran
    /// ahead with exactly these; the provider is called otherwise.
    pub(super) fn kept_provider_result(
        &self,
        call: &ToolCall,
        name: &str,
        arguments: &str,
    ) -> Option<Result<String, String>> {
        let kept = lock(&self.prefetched).remove(&call.id);
        match kept {
            Some((
                Ran::Provider {
                    name: ran,
                    arguments: with,
                },
                (result, _),
            )) if ran == name && with == arguments => Some(result),
            _ => None,
        }
    }
}

impl Ahead {
    /// What a job needs, moved out of the run (the run keeps `ran` for the kept output).
    fn clone_parts(&self) -> (Ran, Option<Arc<dyn ToolProvider>>) {
        (self.ran.clone(), self.provider.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::Barrier;

    use super::*;

    type Job = Pin<Box<dyn Future<Output = usize> + Send>>;

    fn job(future: impl Future<Output = usize> + Send + 'static) -> Job {
        Box::pin(future)
    }

    #[tokio::test]
    async fn jobs_run_at_the_same_time_and_outputs_keep_their_positions() {
        // Each job waits for the other two, so they only finish if all three overlap.
        let barrier = Arc::new(Barrier::new(3));
        let jobs = (0..3)
            .map(|index| {
                let barrier = Arc::clone(&barrier);
                job(async move {
                    // The first job to start is the last to finish.
                    tokio::time::sleep(Duration::from_millis(10 * (3 - index as u64))).await;
                    barrier.wait().await;
                    index
                })
            })
            .collect();
        let mut slots = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), run_bounded(jobs, 3, &mut slots))
            .await
            .expect("three jobs overlap");
        assert_eq!(slots, vec![Some(0), Some(1), Some(2)]);
    }

    #[tokio::test]
    async fn no_more_than_the_limit_run_at_once() {
        let (now, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let jobs = (0..6)
            .map(|index| {
                let (now, peak) = (Arc::clone(&now), Arc::clone(&peak));
                job(async move {
                    let running = now.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(running, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    now.fetch_sub(1, Ordering::SeqCst);
                    index
                })
            })
            .collect();
        let mut slots = Vec::new();
        run_bounded(jobs, 2, &mut slots).await;
        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert_eq!(slots, (0..6).map(Some).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn a_limit_of_one_runs_the_jobs_in_order() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let jobs = (0..4)
            .map(|index| {
                let order = Arc::clone(&order);
                job(async move {
                    order.lock().unwrap().push(index);
                    index
                })
            })
            .collect();
        let mut slots = Vec::new();
        run_bounded(jobs, 1, &mut slots).await;
        assert_eq!(*order.lock().unwrap(), vec![0, 1, 2, 3]);
    }

    #[tokio::test]
    async fn dropping_the_run_cancels_what_is_running_and_keeps_what_finished() {
        let started = Arc::new(AtomicUsize::new(0));
        let still_running = Arc::new(AtomicUsize::new(0));
        let jobs = (0..3)
            .map(|index| {
                let (started, still_running) = (Arc::clone(&started), Arc::clone(&still_running));
                job(async move {
                    started.fetch_add(1, Ordering::SeqCst);
                    if index == 0 {
                        return index;
                    }
                    // A guard that records the job is dropped, as a cancelled task's is.
                    struct Counted(Arc<AtomicUsize>);
                    impl Drop for Counted {
                        fn drop(&mut self) {
                            self.0.fetch_sub(1, Ordering::SeqCst);
                        }
                    }
                    still_running.fetch_add(1, Ordering::SeqCst);
                    let _guard = Counted(Arc::clone(&still_running));
                    std::future::pending::<()>().await;
                    index
                })
            })
            .collect();
        let mut slots = Vec::new();
        let outcome =
            tokio::time::timeout(Duration::from_millis(200), run_bounded(jobs, 3, &mut slots))
                .await;
        assert!(outcome.is_err(), "two jobs never finish");
        assert_eq!(started.load(Ordering::SeqCst), 3);
        assert_eq!(slots, vec![Some(0), None, None]);
        // The hung jobs were cancelled with the run.
        tokio::time::timeout(Duration::from_secs(5), async {
            while still_running.load(Ordering::SeqCst) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the running jobs were dropped");
    }

    #[tokio::test]
    async fn a_panicking_job_leaves_its_slot_empty() {
        let jobs = vec![
            job(async { 1 }),
            job(async { panic!("a read panicked") }),
            job(async { 3 }),
        ];
        let mut slots = Vec::new();
        run_bounded(jobs, 2, &mut slots).await;
        assert_eq!(slots, vec![Some(1), None, Some(3)]);
    }
}
