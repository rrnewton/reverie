use super::handle_perf_pmu_error;
use crate::Errno;
use crate::pmu::logging_tests::RecordedEvent;
use crate::pmu::logging_tests::capture;

#[test]
fn legacy_filter_keeps_capability_warnings_and_fields() {
    let action = || {
        assert!(!handle_perf_pmu_error(Errno::EPERM));
        assert!(!handle_perf_pmu_error(Errno::EINVAL));
    };
    assert_eq!(
        capture("off,reverie_ptrace=trace", action),
        vec![
            RecordedEvent {
                target: "reverie_ptrace::perf",
                module_path: "reverie::pmu::perf",
                level: tracing::Level::WARN,
                fields: [
                    ("errno".into(), Errno::EPERM.to_string()),
                    (
                        "message".into(),
                        "PMU hardware-event capability probe failed; performance counters are unavailable".into(),
                    ),
                ].into(),
            },
            RecordedEvent {
                target: "reverie_ptrace::perf",
                module_path: "reverie::pmu::perf",
                level: tracing::Level::WARN,
                fields: [(
                    "message".into(),
                    format!("Perf feature check failed unexpectedly due to {}; assuming unsupported", Errno::EINVAL),
                )].into(),
            },
        ],
    );
    for filter in ["off", "off,reverie::pmu=trace", "off,reverie_ptrace=error"] {
        assert_eq!(capture(filter, action), Vec::new(), "{filter}");
    }
}
