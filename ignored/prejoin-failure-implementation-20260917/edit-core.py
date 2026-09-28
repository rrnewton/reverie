from pathlib import Path
p=Path('reverie/src/tool.rs');s=p.read_text();anchor='    /// Reports that a backend observed a child transition and committed its\n';s=s.replace(anchor,'''    /// Reports a fatal backend failure before cleanup can wait on another Tool
    /// callback. This is a failed run, not a guest exit, signal, or RPC reply.
    /// Implementations must finish their terminal transition synchronously,
    /// including making concurrent consuming cleanup safe, before returning.
    fn report_backend_failure(&self, _event: BackendFailure) {}

    /// Waits until this run cannot continue faithfully. Each call must subscribe
    /// independently: multiple Tool callbacks and the scheduler may be waiting.
    /// The default preserves Tools that do not own a scheduler.
    async fn wait_for_backend_failure(&self) {
        std::future::pending::<()>().await
    }

'''+anchor,1);anchor='/// A child state and waitability decision observed by an execution backend.';s=s.replace(anchor,'''/// The location of a fatal backend failure. The backend retains its typed cause;
/// this notification only ends dependent waits and must not invent guest status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackendFailure {
    /// Guest process owning the failed operation.
    pub pid: Pid,
    /// Guest thread owning the failed operation.
    pub tid: Tid,
    /// Backend operation that failed.
    pub phase: &'static str,
}

'''+anchor,1);p.write_text(s)
p=Path('reverie-kvm/src/error.rs');s=p.read_text().replace('pub enum Error {','''pub enum Error {
    /// A peer or the Tool scheduler has made this run terminal. This internal
    /// outcome is never a successful guest status or a syscall errno.
    #[error("KVM execution stopped after a fatal run failure")]
    RunAborted,

    /// A shared typed failure retained until the root has joined all children.
    #[error(transparent)]
    SharedFailure(std::sync::Arc<Error>),

    /// Cleanup failed in addition to the original execution failure.
    #[error("{primary}; additional cleanup failures: {cleanup:?}")]
    WithCleanup {
        /// Original typed cause, also exposed through the source chain.
        #[source]
        primary: Box<Error>,
        /// Additional failures, without replacing the original cause.
        cleanup: Vec<Error>,
    },
''',1);s+='''
impl Error {
    pub(crate) fn with_cleanup(self, cleanup: Vec<Error>) -> Self {
        if cleanup.is_empty() {
            self
        } else {
            Self::WithCleanup { primary: Box::new(self), cleanup }
        }
    }

    pub(crate) fn combine(mut errors: Vec<Error>) -> crate::Result<()> {
        if errors.is_empty() {
            Ok(())
        } else {
            let primary = errors.remove(0);
            Err(primary.with_cleanup(errors))
        }
    }
}
''';p.write_text(s)
p=Path('reverie-kvm/src/lib.rs');s=p.read_text().replace('mod fdinfo;','mod fdinfo;\nmod failure;').replace('pub use runtime::KvmStack;','pub use runtime::KvmStack;\npub use runtime::ToolRunCompletion;');p.write_text(s)
