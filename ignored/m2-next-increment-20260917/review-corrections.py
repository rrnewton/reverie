from pathlib import Path
D=Path(__file__).resolve().parent
P=D/'preview'
def update(path, old, new, count=1):
 p=P/path;s=p.read_text();assert s.count(old)==count,(path,old,s.count(old));p.write_text(s.replace(old,new))
update('detcore/src/logdiff.rs','    /// (see `canonicalize_addresses_in_line`). This\n    /// this discards ONLY','    /// (see `canonicalize_addresses_in_line`). This\n    /// discards ONLY')
update('detcore/src/logdiff.rs','/// This preserves syscall arguments,\n/// resource names, thread identities, payload bytes, and all other numbers stay\n/// exact.','/// Syscall arguments, resource names, thread identities, payload bytes, and all\n/// other numbers stay exact.')
update('detcore/src/logdiff.rs','            canonicalize_addresses: false,\n            canonicalize_addresses: false,','            canonicalize_addresses: false,')
update('detcore/src/logdiff.rs','fn structured_scheduler_flags_control_filtering_and_retained_counts()', 'fn structured_scheduler_flags_preserve_info_and_retained_counts()')
update('detcore/src/logdiff.rs','let normalized = super::LogDiffOpts {','let ordinary = super::LogDiffOpts {')
update('detcore/src/logdiff.rs','            &normalized,','            &ordinary,')
update('hermit-cli/src/bin/hermit/backends.rs','#[cfg(feature = "dbt")]\n#[cfg(feature = "dbt")]','#[cfg(feature = "dbt")]')
update('hermit-cli/src/bin/hermit/analyze/phases.rs','let mut ldopts = LogDiffCLIOpts::new(run1_log_path, run2_log_path);','let ldopts = LogDiffCLIOpts::new(run1_log_path, run2_log_path);')
update('hermit-cli/src/bin/hermit/verify.rs','    /// window), NOT the comparison semantics. The canonical comparison is always enabled so a quiet run is still\n    /// bitwise-strict', '    /// window), NOT the comparison semantics. The canonical comparison is always\n    /// enabled, so a quiet run is still bitwise-strict')
update('hermit-cli/src/bin/hermit/verify.rs','fn comparison_spec_maps_strictness_to_concrete_flags()', 'fn comparison_spec_binds_canonical_policy_and_preserves_historical_reports()')
update('hermit-verify/src/trace_replay.rs','fn non_chaos_replay_compares_all_detlogs()', 'fn non_chaos_replay_preserves_output_and_status_checks()')
update('hermit-verify/src/common/verify.rs','        vec![\n            "--canonical-info".to_owned(),', '''        // An older binary may still have a lossy default. Require its explicit
        // canonical capability; an unsupported flag must fail without a fallback.
        vec![
            "--canonical-info".to_owned(),''')
# Keep exact rejected option diagnostics in addition to the structured Clap class.
for path in ['detcore/src/logdiff.rs','hermit-cli/src/bin/hermit/logdiff.rs']:
 update(path,'''                "{flag}"
            );
        }
''','''                "{flag}"
            );
            assert!(error.to_string().contains(flag.split('=').next().unwrap()));
        }
''')
update('hermit-cli/src/bin/hermit/verify.rs','''            assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
''','''            assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
            assert!(error.to_string().contains(flag.split('=').next().unwrap()));
''')
# Preserve the earlier historical report shape and printed disclosure without an
# active lossy constructor or invoking the comparator with historical scope.
update('hermit-cli/src/bin/hermit/verify.rs','''        let no_io_buffers = ComparisonSpec {
''','''        let historical_stripped = ComparisonSpec {
            strictness: LogCompareStrictness::Stripped,
            strip_lines: true,
            canonicalize_addresses: false,
            full_trace: false,
            exact_remainder: false,
            log_scope: ComparedLogScope::Deterministic,
            stripped_prefixes: &[STRIP_WALL_CLOCK_PREFIX_V1, STRIP_UNSAFE_NORMALIZATION_V1],
            canonicalizations: &[],
            display_name: "Stripped",
            ..canonical
        };
        assert!(!historical_stripped.is_bitwise_parity());
        assert_eq!(
            comparison_evidence_line(&historical_stripped).as_deref(),
            Some(":: comparison=Stripped relaxations=unsafe-numeric-address-and-path-normalization/v1")
        );

        let no_io_buffers = ComparisonSpec {
''')
# Exercise the actual COMMIT helper without running a tracee.
update('detcore/tests/testutils/src/lib.rs','''    #[test]
    fn isolated_workdir_request_is_exact_and_fail_closed()''','''    #[test]
    fn commit_comparison_preserves_numeric_values_and_record_count() {
        let output = reverie::process::Output {
            status: ExitStatus::Exited(0),
            stdout: b"same output\\n".to_vec(),
            stderr: Vec::new(),
        };
        let line = |time| {
            format!("INFO detcore::scheduler: COMMIT turn 3, dettid 2 at time {time}")
        };
        let mut same = super::DetTestState::default();
        super::check_output(&output, vec![line(100)], &mut same);
        super::check_output(&output, vec![line(100)], &mut same);
        std::assert_eq!(same.test_run_num, 2);
        std::assert_eq!(same.last_log, Some(vec![" turn 3, dettid 2 at time 100".to_owned()]));

        for differing in [vec![line(200)], vec![line(100), line(100)]] {
            let mut state = super::DetTestState::default();
            super::check_output(&output, vec![line(100)], &mut state);
            let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                super::check_output(&output, differing, &mut state);
            }));
            assert!(failure.is_err(), "a COMMIT value or count difference must fail");
        }
    }

    #[test]
    fn isolated_workdir_request_is_exact_and_fail_closed()''')
print('Updated 8 preview files; no product source or tests executed.')
