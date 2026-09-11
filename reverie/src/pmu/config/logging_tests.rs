use super::has_precise_ip;
use crate::pmu::logging_tests::RecordedEvent;
use crate::pmu::logging_tests::capture;

#[test]
fn legacy_filter_keeps_precise_ip_debug_and_fields() {
    let cpu = raw_cpuid::CpuId::new();
    let features = cpu.get_feature_info();
    let expected_precise_ip = features.as_ref().is_some_and(|info| info.has_ds());
    let expected_message = format!(
        "Setting precise_ip to {} for cpu vendor {} family {:?} model {:?} stepping {:?}",
        expected_precise_ip,
        cpu.get_vendor_info().map_or_else(
            || "unknown".to_string(),
            |vendor| vendor.as_str().to_string()
        ),
        features.as_ref().map(|info| info.family_id()),
        features.as_ref().map(|info| info.model_id()),
        features.as_ref().map(|info| info.stepping_id()),
    );
    let action = || assert_eq!(has_precise_ip(), expected_precise_ip);
    assert_eq!(
        capture("off,reverie_ptrace=trace", action),
        vec![RecordedEvent {
            target: "reverie_ptrace::timer",
            module_path: "reverie::pmu::config",
            level: tracing::Level::DEBUG,
            fields: [("message".into(), expected_message)].into(),
        }],
    );
    for filter in ["off", "off,reverie::pmu=trace", "off,reverie_ptrace=warn"] {
        assert_eq!(capture(filter, action), Vec::new(), "{filter}");
    }
}
