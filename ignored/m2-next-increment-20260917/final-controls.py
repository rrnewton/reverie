from pathlib import Path
import re
D=Path(__file__).parent/'preview'
p=D/'detcore/src/logdiff.rs';s=p.read_text();a=s.index('fn is_detlog(');b=s.index('\n}',a)+2;s=s[:a]+s[b:]
a=s.index('        // Measured: with `strip_lines`');b=s.index('        // A run with no such record',a);s=s[:a]+'''        // Earlier lossy comparisons could match different committed times.
        // Keep both sides explicit even though the current exact comparison
        // rejects those differences before reaching this matching-pair output.
        //
'''+s[b:]
p.write_text(s)
p=D/'hermit-cli/src/bin/hermit/logdiff.rs';s=p.read_text();s=s.replace('''        let mut options = self.more.clone();
        if record_envelope.policy() == RecordEnvelopePolicy::CrossBackendDetcoreV1 {''','''        let mut options = self.more.clone();
        options.comparison = logdiff::LogComparisonMode::Info;
        options.canonicalize_addresses = true;
        if matches!(record_envelope.policy(), RecordEnvelopePolicy::AllRecordsV1 | RecordEnvelopePolicy::CrossBackendDetcoreV1) {''')
s=s.replace('require_structured_events: options.require_structured_events,','require_structured_events: options.require_structured_events\n                || record_envelope == RecordEnvelopePolicy::AllRecordsV1,')
pos=s.index('    #[test]\n    fn canonical_all_records_uses')
s=s[:pos]+'''    #[test]
    fn plain_logdiff_compares_all_info_without_json_or_compatibility_flags() {
        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("left.log");
        let right = directory.path().join("right.log");
        let json = directory.path().join("comparison.json");
        let stable = current_record(1, "DETLOG stable");
        let text = |value| format!("{stable}Apr 09 06:08:02.100  INFO unrelated: value={value}\\n");
        std::fs::write(&left, text(100)).unwrap();
        let global = GlobalOpts::try_parse_from(["hermit"]).unwrap();
        for compatibility in [false, true] {
            for with_json in [false, true] {
                let mut options = LogDiffCLIOpts::new(&left, &right);
                options._canonical_info = compatibility;
                options.json = with_json.then(|| json.clone());
                std::fs::write(&right, text(200)).unwrap();
                assert_eq!(options.main(&global), ExitStatus::Exited(HERMIT_VERIFICATION_DIVERGENCE_EXIT));
                if with_json {
                    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
                    assert_eq!(report["verdict"], "diverged");
                    assert_eq!(report["selected_messages"]["left"], 2);
                    assert_eq!(report["selected_messages"]["right"], 2);
                    assert_eq!(report["comparison"]["stream"], "info");
                    assert_eq!(report["comparison"]["require_structured_events"], true);
                    assert_eq!(report["comparison"]["unsafe_strip_lines"], false);
                }
                std::fs::write(&right, text(100)).unwrap();
                assert_eq!(options.main(&global), ExitStatus::Exited(0));
                std::fs::write(&right, b"INFO detcore: DETLOG invalid=\\xff").unwrap();
                assert_eq!(options.main(&global), ExitStatus::Exited(2));
            }
        }
    }

'''+s[pos:]
p.write_text(s)
# Exact old flag spellings must be the actual reason for parser refusal.
for rel in ['detcore/src/logdiff.rs','hermit-cli/src/bin/hermit/logdiff.rs','hermit-cli/src/bin/hermit/verify.rs']:
 p=D/rel;s=p.read_text();s=s.replace('assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument, "{flag}");','assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument, "{flag}");\n            assert!(error.to_string().contains(flag.split(\'=\').next().unwrap()));');p.write_text(s)
# Keep two distinct real Command construction controls without obsolete field names.
p=D/'hermit-verify/src/common/verify.rs';s=p.read_text().replace('fn test_build_command_args_ignore_lines_provided','fn log_diff_command_requires_canonical_info_without_filters').replace('fn test_build_command_args_ignore_lines_not_provided','fn log_diff_command_preserves_default_diagnostic_history');a=s.index('    fn log_diff_command_preserves_default_diagnostic_history');b=s.index('\n    }',a);q=s[a:b].replace('LogDiffOptions { syscall_history: 5 }','LogDiffOptions::default()').replace('syscall_history: 5,','syscall_history: 0,').replace('--syscall-history=5','--syscall-history=0');s=s[:a]+q+s[b:];p.write_text(s)
