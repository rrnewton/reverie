from pathlib import Path
r=Path.cwd()
p=r/'reverie-kvm/src/error.rs'
s=p.read_text()
old='''    /// Cleanup failed in addition to the original execution failure.
'''
new='''    /// A typed worker failure with its original guest thread identity.
    #[error("KVM worker cleanup failed: thread {tid}: {error}")]
    WorkerFailure {
        /// Guest thread whose physical join returned this failure.
        tid: i32,
        /// Original typed cause retained by the worker-error cache.
        #[source]
        error: std::sync::Arc<Error>,
    },

'''+old
assert s.count(old)==1;s=s.replace(old,new)
s=s.replace('Self::SharedFailure(error) => error.primary(),','Self::SharedFailure(error) | Self::WorkerFailure { error, .. } => error.primary(),')
s=s.replace('Self::SharedFailure(error) => {\n                std::sync::Arc::ptr_eq(error, primary) || error.retains_primary(primary)\n            }', 'Self::SharedFailure(error) | Self::WorkerFailure { error, .. } => {\n                std::sync::Arc::ptr_eq(error, primary) || error.retains_primary(primary)\n            }')
p.write_text(s)
p=r/'reverie-kvm/src/vm.rs';s=p.read_text()
old='''            errors
                .values()
                .flatten()
                .cloned()
                .map(Error::SharedFailure)
                .collect(),'''
new='''            errors
                .iter()
                .flat_map(|(&tid, errors)| {
                    errors.iter().cloned().map(move |error| Error::WorkerFailure {
                        tid,
                        error,
                    })
                })
                .collect(),'''
assert s.count(old)==1;s=s.replace(old,new);p.write_text(s)
p=r/'reverie-kvm/src/runtime/failure_tests.rs';s=p.read_text()
marker='''fn joined_rpc_control(fail: bool, cleanup_fails: bool) {'''
insert='''// Install the response rescue and join ownership before the first ordering
// assertion. A precondition panic must reap the real worker as well as fail the
// test; it must not detach a live RPC waiter into the remaining test process.
struct RpcControlCleanup {
    response: Option<oneshot::Sender<i64>>,
    group: Arc<GuestThreadGroup>,
    joiner: Option<std::thread::JoinHandle<()>>,
}

impl RpcControlCleanup {
    fn rescue(&mut self) {
        if let Some(response) = self.response.take() {
            let _ = response.send(99);
        }
    }
}

impl Drop for RpcControlCleanup {
    fn drop(&mut self) {
        self.rescue();
        if let Some(joiner) = self.joiner.take() {
            // The original ordering assertion remains the test failure during
            // unwind. The production group also retains any worker panic.
            let _ = joiner.join();
        } else {
            self.group.join_workers();
        }
    }
}

'''+marker
assert s.count(marker)==1;s=s.replace(marker,insert)
old='''    let group = Arc::new(GuestThreadGroup::default());
    let worker_global = global.clone();'''
new='''    let group = Arc::new(GuestThreadGroup::default());
    let mut cleanup = RpcControlCleanup {
        response: Some(response),
        group: group.clone(),
        joiner: None,
    };
    let worker_global = global.clone();'''
assert s.count(old)==1;s=s.replace(old,new)
s=s.replace('''    let joiner = std::thread::spawn(move || {
        join_group.join_workers();
        joined.send(()).unwrap();
    });''','''    cleanup.joiner = Some(std::thread::spawn(move || {
        join_group.join_workers();
        joined.send(()).unwrap();
    }));''')
s=s.replace('''    let mut response = Some(response);
    if fail {''','''    if fail {''')
s=s.replace('''        response.take().unwrap().send(37).unwrap();''','''        cleanup.response.take().unwrap().send(37).unwrap();''')
s=s.replace('''        if let Some(response) = response.take() {
            let _ = response.send(99);
        }
    }
    joiner.join().unwrap();''','''        cleanup.rescue();
    }
    cleanup.joiner.take().unwrap().join().unwrap();''')
s=s.replace('''        if cleanup_fails {
            assert!(has_cleanup_eio(&error), "cleanup cause was lost: {error:?}");
        }''','''        assert!(
            error.to_string().contains("thread 2:"),
            "worker TID diagnostic was lost: {error:?}"
        );
        assert!(matches!(error.primary(), Error::GuestClock(_)));
        if cleanup_fails {
            assert!(has_cleanup_eio(&error), "cleanup cause was lost: {error:?}");
        }''')
s=s.replace('''        Error::SharedFailure(error) => has_guest_clock_primary(error),''','''        Error::SharedFailure(error) | Error::WorkerFailure { error, .. } => {
            has_guest_clock_primary(error)
        }''')
s=s.replace('''        Error::SharedFailure(error) => has_cleanup_eio(error),''','''        Error::SharedFailure(error) | Error::WorkerFailure { error, .. } => has_cleanup_eio(error),''')
p.write_text(s)
