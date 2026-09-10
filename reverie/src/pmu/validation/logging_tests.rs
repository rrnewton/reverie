use super::ScopedFd;
use crate::Errno;
use crate::pmu::logging_tests::RecordedEvent;
use crate::pmu::logging_tests::capture;

#[test]
fn legacy_filter_keeps_close_warning_and_fields() {
    let action = || drop(ScopedFd(-1));
    assert_eq!(
        capture("off,reverie_ptrace=trace", action),
        vec![RecordedEvent {
            target: "reverie_ptrace::validation",
            module_path: "reverie::pmu::validation",
            level: tracing::Level::WARN,
            fields: [(
                "message".into(),
                format!("Error while closing file descriptor - {:?}", Errno::EBADF),
            )]
            .into(),
        }],
    );
    for filter in ["off", "off,reverie::pmu=trace", "off,reverie_ptrace=error"] {
        assert_eq!(capture(filter, action), Vec::new(), "{filter}");
    }
}
