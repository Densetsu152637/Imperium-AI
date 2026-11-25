//! Test binary: a worker that speaks the protocol wrongly, to prove the daemon's event allowlist
//! kills it. The first argument names the misbehaviour. It answers `Configure` with `Ok`, then
//! misbehaves on the first `Feed`. Built only with `test-support`.

use std::{
    io::{self, Write as _},
    process::ExitCode,
    thread,
    time::Duration,
};

use monty_proto::{FrameReader, pb, write_frame};

fn event(kind: pb::worker_event::Kind) -> pb::WorkerEvent {
    pb::WorkerEvent {
        kind: Some(kind),
        ..Default::default()
    }
}

fn send(event: &pb::WorkerEvent) {
    write_frame(&mut io::stdout(), event).expect("stdout is open");
}

fn main() -> ExitCode {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let mut reader = FrameReader::new(io::stdin().lock());

    // Configure -> Ok.
    match reader.read::<pb::ClientRequest>() {
        Ok(Some(_)) => send(&event(pb::worker_event::Kind::Ok(pb::Ok {}))),
        _ => return ExitCode::FAILURE,
    }
    // The first Feed.
    match reader.read::<pb::ClientRequest>() {
        Ok(Some(_)) => {}
        _ => return ExitCode::FAILURE,
    }

    use pb::worker_event::Kind;
    match mode.as_str() {
        "resolve_futures" => send(&event(Kind::ResolveFutures(pb::ResolveFutures::default()))),
        "streamed_calls" => send(&event(Kind::StreamedCalls(pb::StreamedCalls::default()))),
        "dump_result" => send(&event(Kind::DumpResult(pb::DumpResult::default()))),
        "typing_error" => send(&event(Kind::TypingError(pb::TypingError::default()))),
        "shutdown_dump" => send(&event(Kind::Shutdown(pb::ShutdownDump::default()))),
        "ok_while_running" => send(&event(Kind::Ok(pb::Ok {}))),
        "unknown_tag" => {
            // A frame whose only field has tag 99: decodes, but carries no known event kind.
            let body = [0x9A_u8, 0x06, 0x01, 0x00];
            let mut stdout = io::stdout();
            stdout
                .write_all(&u32::try_from(body.len()).expect("small").to_le_bytes())
                .ok();
            stdout.write_all(&body).ok();
            stdout.flush().ok();
        }
        "garbage" => {
            let body = [0xFF_u8; 16];
            let mut stdout = io::stdout();
            stdout
                .write_all(&u32::try_from(body.len()).expect("small").to_le_bytes())
                .ok();
            stdout.write_all(&body).ok();
            stdout.flush().ok();
        }
        "ping_then_complete" => {
            send(&event(Kind::Ping(pb::Ping {})));
            // The driver must answer with a Pong before the program can finish.
            match reader.read::<pb::ClientRequest>() {
                Ok(Some(request))
                    if matches!(request.kind, Some(pb::client_request::Kind::Pong(_))) =>
                {
                    send(&event(Kind::Complete(pb::Complete::default())));
                }
                _ => return ExitCode::FAILURE,
            }
        }
        "hang" => loop {
            thread::sleep(Duration::from_secs(3600));
        },
        "crash" => return ExitCode::from(9),
        other => {
            eprintln!("unknown misbehaviour {other:?}");
            return ExitCode::FAILURE;
        }
    }
    // Stay alive until the daemon closes stdin or kills this process.
    let _ = reader.read::<pb::ClientRequest>();
    ExitCode::SUCCESS
}
