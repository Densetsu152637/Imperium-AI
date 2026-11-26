//! The worker's own protocol loop over `monty::MontyRun` (src/serve.rs; plan F12), spoken to
//! directly: what it refuses, what it serves, and that it serves exactly one program. Unix,
//! where the worker confines itself and can be started by hand.
#![cfg(unix)]

mod common;

use std::{
    io::Write as _,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use monty_proto::{FrameReader, PROTOCOL_VERSION, pb, write_frame};

struct Worker {
    child: Child,
    stdin: ChildStdin,
    events: FrameReader<ChildStdout>,
}

impl Worker {
    fn start() -> Self {
        let mut child = Command::new(common::worker_path())
            .args(["--memory-mb", "64"])
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let events = FrameReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            events,
        }
    }

    fn send(&mut self, kind: pb::client_request::Kind) {
        write_frame(
            &mut self.stdin,
            &pb::ClientRequest {
                kind: Some(kind),
                trace_parent: None,
            },
        )
        .unwrap();
        self.stdin.flush().unwrap();
    }

    fn event(&mut self) -> pb::worker_event::Kind {
        self.events
            .read::<pb::WorkerEvent>()
            .unwrap()
            .expect("an event")
            .kind
            .expect("a known kind")
    }

    fn configure(&mut self, configure: pb::Configure) -> pb::worker_event::Kind {
        self.send(pb::client_request::Kind::Configure(pb::Configure {
            protocol_version: PROTOCOL_VERSION,
            ..configure
        }));
        self.event()
    }

    fn feed(&mut self, code: &str) {
        self.send(pb::client_request::Kind::Feed(pb::Feed {
            code: code.to_owned(),
            ..Default::default()
        }));
    }
}

fn is_violation(kind: &pb::worker_event::Kind) -> bool {
    matches!(kind, pb::worker_event::Kind::Error(error)
        if error.exception.as_ref().and_then(|exc| exc.message.as_deref()).is_some_and(|message| message.starts_with("protocol violation")))
}

#[test]
fn configurations_the_worker_does_not_serve_are_refused() {
    for configure in [
        pb::Configure {
            type_check: true,
            ..Default::default()
        },
        pb::Configure {
            accepts_streamed_calls: true,
            ..Default::default()
        },
        pb::Configure {
            execution_clock: Some(pb::ExecutionClock::Cpu as i32),
            ..Default::default()
        },
        pb::Configure {
            first: Some(pb::configure::First::Feed(pb::Feed::default())),
            ..Default::default()
        },
    ] {
        let mut worker = Worker::start();
        let answer = worker.configure(configure.clone());
        assert!(
            matches!(answer, pb::worker_event::Kind::SessionRefused(_)),
            "{configure:?} -> {answer:?}"
        );
        // Nothing was configured: a program is refused too.
        worker.feed("print(1)");
        assert!(is_violation(&worker.event()));
        worker.child.kill().unwrap();
        worker.child.wait().unwrap();
    }
}

#[test]
fn requests_outside_the_one_program_are_violations_that_change_nothing() {
    use pb::client_request::Kind;
    let mut worker = Worker::start();
    assert!(matches!(
        worker.configure(pb::Configure::default()),
        pb::worker_event::Kind::Ok(_)
    ));
    for request in [
        Kind::Dump(pb::Dump::default()),
        Kind::Load(pb::Load::default()),
        Kind::Reset(pb::Reset::default()),
        Kind::ResumeFutures(pb::ResumeFutures::default()),
        Kind::InstallDependencies(pb::InstallDependencies::default()),
        Kind::ResumeCall(pb::ResumeCall::default()),
        Kind::Configure(pb::Configure {
            protocol_version: PROTOCOL_VERSION,
            ..Default::default()
        }),
    ] {
        worker.send(request.clone());
        let answer = worker.event();
        assert!(is_violation(&answer), "{request:?} -> {answer:?}");
    }
    // The session is still ready for its program, which runs.
    worker.feed("print(1 + 1)");
    match worker.event() {
        pb::worker_event::Kind::Print(print) => assert_eq!(print.segments[0].text, "2\n"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        worker.event(),
        pb::worker_event::Kind::Complete(_)
    ));
    // One program per worker.
    worker.feed("print(2)");
    assert!(is_violation(&worker.event()));
    worker.send(Kind::Shutdown(pb::Shutdown::default()));
    assert!(matches!(worker.event(), pb::worker_event::Kind::Ok(_)));
    assert!(worker.child.wait().unwrap().success());
}
