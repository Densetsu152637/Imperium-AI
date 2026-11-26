//! A runner that replays scripted events, for the engine's tests (plan C3). It can send what a
//! real worker never would (a call for a name it never looked up, a call through a forged
//! handle, a silent worker), records every answer the host gives, and holds the host to the same
//! rules as the real driver: one answer per suspension, nothing after the end.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, PoisonError},
    time::Instant,
};

use monty_types::MontyObject;

use crate::{
    driver::{Call, Limits, ScriptError, ScriptEvent},
    runner::{BoxFuture, ScriptProgram, ScriptRunner},
};

/// One scripted step of a fake program.
#[derive(Debug, Clone)]
pub enum FakeStep {
    /// The program prints this.
    Print(String),
    /// The program reads an undefined name.
    NameLookup(String),
    /// The program calls a host function.
    Call(Call),
    /// The program ends with this outcome.
    Done(Result<(), ScriptError>),
    /// The worker falls silent: the next wait ends at its deadline as
    /// [`ScriptError::WallBudget`], or never without one.
    Stall,
}

/// An answer the host gave a fake program.
#[derive(Debug, Clone, PartialEq)]
pub enum FakeAnswer {
    Value(MontyObject),
    Error(String),
    NotFound,
    Name(Option<String>),
}

#[derive(Debug, Default)]
struct State {
    scripts: VecDeque<Result<Vec<FakeStep>, ScriptError>>,
    started: Vec<String>,
    answers: Vec<FakeAnswer>,
    live: usize,
}

/// Replays one scripted program per start, in order. Clones share the scripts and the record.
#[derive(Debug, Clone, Default)]
pub struct FakeRunner {
    state: Arc<Mutex<State>>,
}

impl FakeRunner {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues a program: the next start replays `steps`; after the last one it completes.
    #[must_use]
    pub fn script(self, steps: Vec<FakeStep>) -> Self {
        self.lock().scripts.push_back(Ok(steps));
        self
    }

    /// Queues a start that fails with `error` (for example [`ScriptError::Unavailable`]).
    #[must_use]
    pub fn refuse(self, error: ScriptError) -> Self {
        self.lock().scripts.push_back(Err(error));
        self
    }

    /// The code of every program started, in order.
    #[must_use]
    pub fn started(&self) -> Vec<String> {
        self.lock().started.clone()
    }

    /// Every answer the host gave, in order, across programs.
    #[must_use]
    pub fn answers(&self) -> Vec<FakeAnswer> {
        self.lock().answers.clone()
    }

    /// Programs started and not yet ended or dropped.
    #[must_use]
    pub fn live(&self) -> usize {
        self.lock().live
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl ScriptRunner for FakeRunner {
    fn start<'a>(
        &'a self,
        code: &'a str,
        _limits: &'a Limits,
    ) -> BoxFuture<'a, Result<Box<dyn ScriptProgram>, ScriptError>> {
        Box::pin(async move {
            // The same refusal as the real runner, before anything starts.
            crate::scan::dynamic_code(code).map_err(ScriptError::Refused)?;
            let mut state = self.lock();
            let steps = state.scripts.pop_front().unwrap_or_else(|| {
                Err(ScriptError::Unavailable(
                    "the fake runner has no program scripted".to_owned(),
                ))
            })?;
            state.started.push(code.to_owned());
            state.live += 1;
            drop(state);
            Ok(Box::new(FakeProgram {
                steps: steps.into(),
                pending: None,
                finished: false,
                state: Arc::clone(&self.state),
            }) as Box<dyn ScriptProgram>)
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Call,
    Name,
}

struct FakeProgram {
    steps: VecDeque<FakeStep>,
    pending: Option<Pending>,
    finished: bool,
    state: Arc<Mutex<State>>,
}

impl FakeProgram {
    fn end(&mut self, result: Result<(), ScriptError>) -> ScriptEvent {
        if !self.finished {
            self.finished = true;
            self.state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .live -= 1;
        }
        self.pending = None;
        ScriptEvent::Done(result)
    }

    fn answer(&mut self, expected: Pending, answer: FakeAnswer) -> Result<(), ScriptError> {
        if self.pending != Some(expected) {
            return Err(ScriptError::Misuse(
                "nothing of that kind is waiting for an answer".to_owned(),
            ));
        }
        self.pending = None;
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .answers
            .push(answer);
        Ok(())
    }

    async fn next(&mut self, deadline: Option<Instant>) -> ScriptEvent {
        if self.finished {
            return ScriptEvent::Done(Err(ScriptError::Misuse(
                "the program has already ended".to_owned(),
            )));
        }
        if self.pending.is_some() {
            return self.end(Err(ScriptError::Misuse(
                "the previous suspension was not answered".to_owned(),
            )));
        }
        match self.steps.pop_front() {
            None => self.end(Ok(())),
            Some(FakeStep::Print(text)) => ScriptEvent::Print(text),
            Some(FakeStep::NameLookup(name)) => {
                self.pending = Some(Pending::Name);
                ScriptEvent::NameLookup(name)
            }
            Some(FakeStep::Call(call)) => {
                self.pending = Some(Pending::Call);
                ScriptEvent::Call(call)
            }
            Some(FakeStep::Done(result)) => self.end(result),
            Some(FakeStep::Stall) => match deadline {
                Some(deadline) => {
                    tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
                    self.end(Err(ScriptError::WallBudget))
                }
                None => std::future::pending().await,
            },
        }
    }
}

impl Drop for FakeProgram {
    fn drop(&mut self) {
        if !self.finished {
            self.finished = true;
            self.state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .live -= 1;
        }
    }
}

impl ScriptProgram for FakeProgram {
    fn next_until(&mut self, deadline: Option<Instant>) -> BoxFuture<'_, ScriptEvent> {
        Box::pin(self.next(deadline))
    }

    fn resume_value(&mut self, value: MontyObject) -> BoxFuture<'_, Result<(), ScriptError>> {
        let result = self.answer(Pending::Call, FakeAnswer::Value(value));
        Box::pin(async move { result })
    }

    fn resume_error(&mut self, message: String) -> BoxFuture<'_, Result<(), ScriptError>> {
        let result = self.answer(Pending::Call, FakeAnswer::Error(message));
        Box::pin(async move { result })
    }

    fn resume_not_found(&mut self) -> BoxFuture<'_, Result<(), ScriptError>> {
        let result = self.answer(Pending::Call, FakeAnswer::NotFound);
        Box::pin(async move { result })
    }

    fn resume_name(&mut self, function: Option<String>) -> BoxFuture<'_, Result<(), ScriptError>> {
        let result = self.answer(Pending::Name, FakeAnswer::Name(function));
        Box::pin(async move { result })
    }

    fn finish(mut self: Box<Self>) -> BoxFuture<'static, ()> {
        if !self.finished {
            self.end(Ok(()));
        }
        Box::pin(async {})
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn call(function: &str) -> FakeStep {
        FakeStep::Call(Call {
            function: function.to_owned(),
            args: Vec::new(),
            kwargs: Vec::new(),
        })
    }

    #[tokio::test]
    async fn a_scripted_program_replays_and_records_the_answers() {
        let runner = FakeRunner::new().script(vec![
            FakeStep::NameLookup("grep".to_owned()),
            call("spawn_agent"),
            FakeStep::Print("3\n".to_owned()),
        ]);
        let mut program = runner.start("print(3)", &Limits::default()).await.unwrap();
        assert_eq!(runner.live(), 1);
        assert!(
            matches!(program.next_until(None).await, ScriptEvent::NameLookup(name) if name == "grep")
        );
        program.resume_name(Some("grep".to_owned())).await.unwrap();
        // A call the program never looked up, which a real worker can send too.
        assert!(
            matches!(program.next_until(None).await, ScriptEvent::Call(call) if call.function == "spawn_agent")
        );
        program.resume_error("refused".to_owned()).await.unwrap();
        assert!(
            matches!(program.next_until(None).await, ScriptEvent::Print(text) if text == "3\n")
        );
        assert!(matches!(
            program.next_until(None).await,
            ScriptEvent::Done(Ok(()))
        ));
        assert_eq!(runner.live(), 0);
        assert_eq!(
            runner.answers(),
            vec![
                FakeAnswer::Name(Some("grep".to_owned())),
                FakeAnswer::Error("refused".to_owned())
            ]
        );
        assert_eq!(runner.started(), vec!["print(3)".to_owned()]);
    }

    #[tokio::test]
    async fn the_fake_holds_the_host_to_one_answer_per_suspension() {
        let runner = FakeRunner::new().script(vec![call("f")]);
        let mut program = runner.start("f()", &Limits::default()).await.unwrap();
        assert!(program.resume_value(MontyObject::none()).await.is_err());
        assert!(matches!(
            program.next_until(None).await,
            ScriptEvent::Call(_)
        ));
        assert!(
            program.resume_name(None).await.is_err(),
            "a name answer to a call"
        );
        assert!(matches!(
            program.next_until(None).await,
            ScriptEvent::Done(Err(ScriptError::Misuse(_)))
        ));
        assert_eq!(runner.live(), 0);
    }

    #[tokio::test]
    async fn a_stalled_program_ends_at_its_deadline_and_dropping_one_ends_it() {
        let runner = FakeRunner::new()
            .script(vec![FakeStep::Stall])
            .script(vec![call("f")]);
        let mut program = runner.start("x", &Limits::default()).await.unwrap();
        let deadline = Instant::now() + Duration::from_millis(50);
        assert!(matches!(
            program.next_until(Some(deadline)).await,
            ScriptEvent::Done(Err(ScriptError::WallBudget))
        ));
        let mut program = runner.start("f()", &Limits::default()).await.unwrap();
        assert!(matches!(
            program.next_until(None).await,
            ScriptEvent::Call(_)
        ));
        drop(program);
        assert_eq!(runner.live(), 0);
    }

    #[tokio::test]
    async fn refusals_come_before_any_program() {
        let runner = FakeRunner::new()
            .refuse(ScriptError::Unavailable("no seccomp".to_owned()))
            .script(Vec::new());
        assert!(matches!(
            runner.start("eval('1')", &Limits::default()).await.err(),
            Some(ScriptError::Refused(_))
        ));
        assert!(matches!(
            runner.start("print(1)", &Limits::default()).await.err(),
            Some(ScriptError::Unavailable(reason)) if reason == "no seccomp"
        ));
        assert!(runner.start("print(1)", &Limits::default()).await.is_ok());
        assert_eq!(runner.started(), vec!["print(1)".to_owned()]);
    }
}
