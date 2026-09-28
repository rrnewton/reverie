from pathlib import Path
import re
D=Path(__file__).parent/'preview'
def f(s,name,repl):
 m=re.search(r'^    fn '+re.escape(name)+r'\b',s,re.M);assert m,name;e=s.index('\n    }',m.start())+6;return s[:m.start()]+repl+s[e:]
def body(s,name):
 m=re.search(r'^    fn '+re.escape(name)+r'\b',s,re.M);e=s.index('\n    }',m.start())+6;return s[m.start():e]
p=D/'detcore/src/logdiff.rs';s=p.read_text()
s=f(s,'unsafe_strip_lines_cli_name_and_warning_are_explicit','''    fn legacy_comparison_options_are_deleted_and_default_is_canonical_info() {
        let defaults = super::LogDiffOpts::try_parse_from(["log-diff"]).unwrap();
        assert_eq!(defaults.comparison, super::LogComparisonMode::Info);
        assert!(defaults.canonicalize_addresses);
        for flag in ["--strip-lines", "--unsafe-strip-lines", "--ignore-lines=payload", "--skip-commit", "--skip-detlog", "--include-detlogs=syscall", "--git-diff"] {
            let error = super::LogDiffOpts::try_parse_from(["log-diff", flag]).unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument, "{flag}");
        }
    }''')
s=s.replace('''        assert!(
            super::LogDiffOpts::default()
                .filter_deterministic(&[internal])
                .is_empty()
        );''','''        assert_eq!(super::filter_infos(&[internal]).len(), 1);''')
s=s.replace('''            super::LogDiffOpts::default()
                .filter_deterministic(&[maps_read])
                .len(),''','''            super::filter_infos(&[maps_read]).len(),''')
s=f(s,'printed_logs_are_the_exact_selected_comparator_inputs','''    fn printed_logs_are_the_exact_selected_comparator_inputs() -> std::io::Result<()> {
        let left = "2026-08-15T01:02:03.000000Z INFO detcore: DETLOG value=100\\n2026-08-15T01:02:03.000001Z INFO unrelated: value=303";
        let right = "2026-08-15T04:05:06.000000Z INFO detcore: DETLOG value=200\\n2026-08-15T04:05:06.000001Z INFO unrelated: value=404";
        let options = super::LogDiffOpts { print_logs: true, no_color: true, ..Default::default() };
        let mut output = Vec::new();
        let summary = super::log_diff_summary_from_strs(left, right, &options, &mut output)?;
        let output = String::from_utf8(output).unwrap();
        assert!(summary.diff_found);
        assert!(!summary.matched_with_evidence());
        assert_eq!((summary.compared_left, summary.compared_right), (2, 2));
        assert!(output.contains("Comparison policy: Canonical\\n"));
        assert_eq!(printed_log(&output, 1), "INFO detcore: DETLOG value=100\\nINFO unrelated: value=303\\n");
        assert_eq!(printed_log(&output, 2), "INFO detcore: DETLOG value=200\\nINFO unrelated: value=404\\n");
        Ok(())
    }''')
# All four supported scope/normalization combinations remain checked.
s=f(s,'printed_policy_name_tracks_the_selected_scope_and_normalization','''    fn printed_policy_name_tracks_the_selected_scope_and_normalization() -> std::io::Result<()> {
        let log = "2026-08-15T01:02:03.000000Z INFO detcore: DETLOG stable=1 address=<hostaddr 0xaaaa>\\n2026-08-15T01:02:03.000001Z DEBUG unrelated: diagnostic=2";
        for (comparison, canonicalize_addresses, expected_name, expected_log) in [
            (super::LogComparisonMode::Info, false, "Info", "INFO detcore: DETLOG stable=1 address=<hostaddr 0xaaaa>\\n"),
            (super::LogComparisonMode::Info, true, "Canonical", "INFO detcore: DETLOG stable=1 address=<addr1>\\n"),
            (super::LogComparisonMode::FullTrace, false, "FullTrace", "INFO detcore: DETLOG stable=1 address=<hostaddr 0xaaaa>\\nDEBUG unrelated: diagnostic=2\\n"),
            (super::LogComparisonMode::FullTrace, true, "FullTrace with Canonical host-address normalization", "INFO detcore: DETLOG stable=1 address=<addr1>\\nDEBUG unrelated: diagnostic=2\\n"),
        ] {
            let options = super::LogDiffOpts { comparison, canonicalize_addresses, print_logs: true, no_color: true, ..Default::default() };
            let mut output = Vec::new();
            let summary = super::log_diff_summary_from_strs(log, log, &options, &mut output)?;
            let output = String::from_utf8(output).unwrap();
            assert!(summary.matched_with_evidence());
            assert!(output.contains(&format!("Comparison policy: {expected_name}\\n")));
            assert_eq!(printed_log(&output, 1), expected_log);
            assert_eq!(printed_log(&output, 2), expected_log);
        }
        Ok(())
    }''')
b=body(s,'printed_canonical_policy_uses_the_info_scope_and_address_ordinals');a=b.index('        let deterministic =');e=b.index('        let options =',a);b=b[:a]+b[e:];s=f(s,'printed_canonical_policy_uses_the_info_scope_and_address_ordinals',b)
b=body(s,'test_full_trace_detects_unnormalized_timing_difference');b=b.replace('assert!(!super::log_diff_from_strs','assert!(super::log_diff_from_strs');s=f(s,'test_full_trace_detects_unnormalized_timing_difference',b)
# Selector tests now require every INFO record, including transport bookkeeping.
for name in ['test_filter_deterministic','test_filter_deterministic_with_filter','test_filter_deterministic_drops_io_polling_bookkeeping','test_filter_deterministic_drops_sabre_internal_pipe_resource_turn','test_filter_deterministic_drops_sabre_loopback_poll_yield']:
 b=body(s,name)
 a=b.index('        let opts =');e=b.index('        let v =',a);b=b[:a]+b[e:]
 b=b.replace('let v = opts.filter_deterministic(','let input = ').replace('let input = \n            &[','let input = [').replace('let input = &[','let input = [')
 # Replace selector call ending with a normal array declaration, preserving exact fixtures.
 a=b.index('        let input =');e=b.index('        assert_eq!(',a)
 arr=b[a:e];arr=arr[:arr.rfind(']);')]+'];\n' if ']);' in arr else arr[:arr.rfind(');')]+ ';\n'
 # Normalize multiline call tail `],\n        );`.
 arr=re.sub(r'\],\s*;\s*$','];\n',arr)
 expected='input.iter().filter(|message| message.text.starts_with("INFO ")).copied().collect::<Vec<_>>()'
 b=b[:a]+arr+'''        let v = super::filter_infos(&input);
        assert_eq!(indexed_text(&v), indexed_text(&'''+expected+'''));
    }'''
 b=b.replace('fn '+name,'fn '+name.replace('test_filter_deterministic','test_info_selection').replace('_drops_','_preserves_').replace('_with_filter','_preserves_all_classes'))
 s=f(s,name,b)
b=body(s,'test_log_diff_ignores_extra_io_poll_retries');b=b.replace('fn test_log_diff_ignores_extra_io_poll_retries','fn test_log_diff_detects_extra_io_poll_retries').replace('assert!(!super::log_diff_from_strs','assert!(super::log_diff_from_strs');b=b.replace('// Differ only in retry count -> deterministic (no diff reported):','// An extra INFO scheduler turn must be compared even if syscalls match:');b=b.replace('// But a real divergence in the guest-observable syscall result is still caught. (Use a\n        // non-numeric change: numeric-only differences are erased by `strip_lines` normalization.)','// Real syscall return values remain exact too.');s=f(s,'test_log_diff_ignores_extra_io_poll_retries',b)
b=body(s,'canonical_allocation_order_difference_compares_unequal');b=b.replace('!super::log_diff_from_strs(run_a, run_b, &stripped','super::log_diff_from_strs(run_a, run_b, &stripped').replace('// And wholesale stripping DOES hide it: both addresses collapse to a\n        // single <ADDR> token.','// The default canonical policy must retain the same distinction.').replace('let stripped =','let default =').replace('&stripped','&default').replace('"wholesale stripping erases the allocation-order difference (the defect)"','"the default comparison must preserve allocation order"');s=f(s,'canonical_allocation_order_difference_compares_unequal',b)
# Replace removed erasure function tests by direct exact-payload controls.
for old,new,pairs in [
 ('test_strip_log','default_comparison_preserves_numeric_values', [('value=100','value=200'),('time=800.709_180s','time=800.709_181s'),('value=98.91618ms','value=98.91619ms')]),
 ('strip_tmp_path_does_not_swallow_rest_of_line','default_comparison_preserves_fields_after_tmp_paths',[('open path="/tmp/scratch" flags="O_RDONLY"','open path="/tmp/scratch" flags="O_WRONLY"')]),
 ('strip_tmp_path_still_erases_a_differing_tmp_path','default_comparison_preserves_differing_tmp_paths',[('open path="/tmp/hermit-aaaa/f" flags="O_RDONLY"','open path="/tmp/hermit-bbbb/f" flags="O_RDONLY"')]),
 ('strip_tmp_path_erases_each_path_separately','default_comparison_preserves_each_tmp_path',[('rename from="/tmp/a" to="/tmp/b" ok="1"','rename from="/tmp/a" to="/tmp/c" ok="1"'),('rename from="/tmp/a" to="/tmp/b" ok="1"','rename from="/tmp/c" to="/tmp/b" ok="1"')])]:
 cases=',\n'.join('            (r#"'+a+'"#, r#"'+b+'"#)' for a,b in pairs)
 s=f(s,old,'''    fn '''+new+'''() -> std::io::Result<()> {
        for (left, right) in [
'''+cases+'''
        ] {
            let left = format!("INFO detcore: DETLOG {left}");
            let right = format!("INFO detcore: DETLOG {right}");
            let options = super::LogDiffOpts::default();
            let summary = super::log_diff_summary_from_strs(&left, &right, &options, &mut Vec::new())?;
            assert!(summary.diff_found, "{left} versus {right}");
            assert_eq!((summary.compared_left, summary.compared_right), (1, 1));
            assert!(!summary.matched_with_evidence());
            assert!(super::log_diff_summary_from_strs(&left, &left, &options, &mut Vec::new())?.matched_with_evidence());
        }
        Ok(())
    }''')
b=body(s,'both_records_are_retained_under_the_default_comparison_mode');a=b.index('        let mut out =');e=b.index('        // The same path',a);b=b[:a]+'''        let mut out = Vec::new();
        let summary = super::log_diff_summary_from_strs(log_with_kick(), log_without_kick(), &default_opts, &mut out)?;
        assert!(summary.diff_found, "the default INFO comparison must include the kick asymmetry");
        assert!(!summary.matched_with_evidence());

'''+b[e:];s=f(s,'both_records_are_retained_under_the_default_comparison_mode',b)
s=f(s,'a_stripped_pass_shows_both_runs_diverging_map_read_times','''    fn default_comparison_rejects_diverging_map_read_times() -> std::io::Result<()> {
        let options = super::LogDiffOpts { no_color: true, ..Default::default() };
        let mut output = Vec::new();
        let summary = super::log_diff_summary_from_strs(log_with_maps_read("12.345_678_901s"), log_with_maps_read("12.345_678_902s"), &options, &mut output)?;
        assert!(summary.diff_found);
        assert!(!summary.matched_with_evidence());
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("12.345_678_901s"));
        assert!(output.contains("12.345_678_902s"));
        Ok(())
    }''')
# Both public file wrappers must reject malformed UTF-8 instead of sharing replacement characters.
pos=s.index('    #[test]\n    fn bitwise_info_v1_binds')
s=s[:pos]+'''    #[test]
    fn public_file_comparisons_reject_invalid_utf8_on_either_side() -> std::io::Result<()> {
        for invalid_left in [true, false] {
            let left = temp_log("INFO detcore: DETLOG value=100");
            let right = temp_log("INFO detcore: DETLOG value=100");
            std::fs::write(if invalid_left { left.path() } else { right.path() }, b"INFO detcore: DETLOG value=\\xff")?;
            let options = super::LogDiffOpts::default();
            assert_eq!(super::try_log_diff_detailed_with_filter(left.path(), right.path(), &options, |_| true).unwrap_err().kind(), std::io::ErrorKind::InvalidData);
            assert_eq!(super::try_log_diff_with_records_and_filter(left.path(), right.path(), &options, |_| true).unwrap_err().kind(), std::io::ErrorKind::InvalidData);
        }
        Ok(())
    }

'''+s[pos:]
p.write_text(s)
# CLI refusal test uses actual parser, not an impossible removed option literal.
p=D/'hermit-cli/src/bin/hermit/logdiff.rs';s=p.read_text();b=body(s,'canonical_json_refuses_relaxed_comparisons_and_starts_as_no_result');a=b.index('        assert!(canonical_comparison');e=b.index('        let report =',a);b=b[:a]+'''        for flag in ["--strip-lines", "--unsafe-strip-lines", "--ignore-lines=payload", "--skip-commit", "--skip-detlog", "--include-detlogs=syscall", "--git-diff"] {
            let error = LogDiffCLIOpts::try_parse_from(["log-diff", "left", "right", flag]).unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument, "{flag}");
        }
        for argv in [vec!["log-diff", "left", "right"], vec!["log-diff", "--canonical-info", "left", "right"]] {
            let parsed = LogDiffCLIOpts::try_parse_from(argv).unwrap();
            assert_eq!(parsed.more.comparison, logdiff::LogComparisonMode::Info);
            assert!(parsed.more.canonicalize_addresses);
        }
'''+b[e:];s=f(s,'canonical_json_refuses_relaxed_comparisons_and_starts_as_no_result',b);p.write_text(s)
p=D/'hermit-verify/src/trace_replay.rs';s=p.read_text();a=s.index('            // A chaos recording');b=s.index('            verify_exit_statuses:',a);s=s[:a]+'''            // Comparison includes all INFO records, including instrumentation
            // differences. A mismatch must be diagnosed rather than filtered.
'''+s[b:]
for name in ['non_chaos_replay_compares_all_detlogs','chaos_replay_excludes_instrumentation_only_detlogs']:
 b=body(s,name);a=b.index('        assert!');b=b[:a]+'''        assert!(options.verify_stdout);
        assert!(options.verify_stderr);
        assert!(options.verify_exit_statuses);
        assert!(options.verify_desync);
    }''';b=b.replace('chaos_replay_excludes_instrumentation_only_detlogs','chaos_replay_preserves_output_and_status_checks');s=f(s,name,b)
p.write_text(s)
