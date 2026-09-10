use reverie_liteinst::run_evidence::RunCompletion;
use reverie_liteinst::run_evidence::RunEvidence;
use reverie_liteinst::run_evidence::StdioMode;
use reverie_liteinst::run_evidence::StreamState;

fn mode(snapshot: &RunEvidence, inherited: bool) -> Result<(), &'static str> {
    if snapshot.mode
        != if inherited {
            StdioMode::Inherited
        } else {
            StdioMode::Captured
        }
    {
        return Err("wrong actual stdio mode");
    }
    Ok(())
}

pub fn prepared(snapshot: &RunEvidence, inherited: bool) -> Result<(), &'static str> {
    mode(snapshot, inherited)?;
    if snapshot.polled
        || snapshot.worker_submitted
        || snapshot.spawned
        || snapshot.pid.is_some()
        || snapshot.reaped
        || snapshot.wait_status.is_some()
        || snapshot.caller_cancelled
        || snapshot.completion != RunCompletion::Pending
        || snapshot.first_error.is_some()
    {
        return Err("preparation claimed execution before polling");
    }
    let expected = if inherited {
        StreamState::Inherited
    } else {
        StreamState::NotStarted
    };
    for stream in [&snapshot.stdout, &snapshot.stderr] {
        if stream.state != expected || !stream.bytes().is_empty() {
            return Err("unpolled stream is not unstarted/inherited");
        }
    }
    Ok(())
}

pub fn before_cancel(snapshot: &RunEvidence, inherited: bool) -> Result<(), &'static str> {
    mode(snapshot, inherited)?;
    if !snapshot.polled
        || !snapshot.worker_submitted
        || !snapshot.spawned
        || snapshot.pid.is_none()
        || snapshot.reaped
        || snapshot.wait_status.is_some()
        || snapshot.caller_cancelled
        || snapshot.completion != RunCompletion::Pending
        || snapshot.first_error.is_some()
    {
        return Err("cancellation must interrupt the observed live worker");
    }
    let expected = if inherited {
        StreamState::Inherited
    } else {
        StreamState::Reading
    };
    for stream in [&snapshot.stdout, &snapshot.stderr] {
        if stream.state != expected || !stream.bytes().is_empty() {
            return Err("sample-one cancellation precedes the guest's stdio writes");
        }
    }
    Ok(())
}

pub fn terminal(snapshot: &RunEvidence, inherited: bool, case: u8) -> Result<(), &'static str> {
    mode(snapshot, inherited)?;
    if !snapshot.polled
        || !snapshot.worker_submitted
        || !snapshot.spawned
        || snapshot.pid.is_none()
        || !snapshot.reaped
        || snapshot.wait_status.is_none()
        || snapshot.wait_error.is_some()
        || snapshot.caller_cancelled != (case == 1)
    {
        return Err("missing actual worker/wait facts or wrong cancellation state");
    }
    match (&snapshot.completion, case) {
        (RunCompletion::Succeeded, 0) if snapshot.first_error.is_none() => {}
        (RunCompletion::Interrupted, 1) => {}
        (RunCompletion::Failed(_), 1..=5) if snapshot.first_error.is_some() => {}
        _ => return Err("worker outcome is not the expected retained outcome"),
    }
    for (stream, expected) in [
        (&snapshot.stdout, b"OUT\0\xff".as_slice()),
        (&snapshot.stderr, b"ERR\0\xfe".as_slice()),
    ] {
        if inherited {
            if stream.state != StreamState::Inherited || !stream.bytes().is_empty() {
                return Err("inherited stdio was substituted by capture");
            }
        } else {
            if !matches!(stream.state, StreamState::Eof)
                && !(case == 1 && stream.state == StreamState::Interrupted)
            {
                return Err("captured stream lacks actual EOF or cancellation cutoff");
            }
            if stream.bytes() != if case == 0 { expected } else { b"" } {
                return Err("actual guest stdio prefix differs from the original oracle");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use reverie_rpc_transport::guest_log::Options;
    use reverie_rpc_transport::guest_log::retained_log;

    use super::*;

    #[test]
    fn actual_prepared_observers_exist_outside_any_runtime_and_survive_unpolled_drop() {
        for inherited in [false, true] {
            let (sink, _handle) = retained_log(Options::bounded(1024));
            let command = reverie::process::Command::new("/nonexistent-v4-evidence-test");
            let (observer, future) = super::super::super::prepare::<()>(
                command,
                (),
                std::ffi::OsStr::new("/bin/true"),
                Vec::new(),
                sink,
                if inherited {
                    StdioMode::Inherited
                } else {
                    StdioMode::Captured
                },
            );
            let future: std::pin::Pin<Box<dyn std::future::Future<Output = _>>> = Box::pin(future);
            let before = observer.try_snapshot().unwrap();
            prepared(&before, inherited).unwrap();
            assert!(before_cancel(&before, inherited).is_err());
            assert!(terminal(&before, inherited, 1).is_err());
            drop(future);
            let after = observer.try_snapshot().unwrap();
            assert!(after.caller_cancelled);
            assert_eq!(after.completion, RunCompletion::Interrupted);
            assert!(!after.polled && !after.worker_submitted && !after.spawned && !after.reaped);
            assert!(after.pid.is_none() && after.wait_status.is_none());
            assert_eq!(after.stdout.state, before.stdout.state);
            assert_eq!(after.stderr.state, before.stderr.state);
            assert_eq!(after.stdout.bytes(), before.stdout.bytes());
            assert_eq!(after.stderr.bytes(), before.stderr.bytes());
            assert!(terminal(&after, inherited, 1).is_err());
        }
    }
}
