from pathlib import Path
P=Path(__file__).resolve().parent/'preview'
p=P/'detcore/src/logdiff.rs';s=p.read_text()
s=s.replace('/// host addresses to first-appearance ordinals. It does not run the lossy\n/// `--unsafe-strip-lines` transformation: scheduler turns, virtual time, syscall', '/// host addresses to first-appearance ordinals. Scheduler turns, virtual time, syscall')
s=s.replace('/// control over how to present the (stripped/unstripped) differences, and to focus on the','/// control over how to present original and canonical differences, and to focus on the')
s=s.replace('''///  * Some stripping of nondeterministic information is needed for direct comparability.
///  * Certain lines are intended to be deterministic/comparable, in their contents,
///    and others in their *presence* but not their details.''','''///  * Real wall-clock prefixes are removed and marked host addresses are canonicalized.
///  * Every selected message is compared exactly, including its numeric values.''')
s=s.replace('// alignment. There\'s also no reason we can\'t output the stripped relevant lines and use a separate\n// diff tool.','// alignment.')
s=s.replace('''            (r#"value=100"#, r#"value=200"#),''','''            (r#"value=100"#, r#"value=200"#),
            (r#"CHAOSRAND value=100"#, r#"CHAOSRAND value=200"#),
            (r#"SCHEDRAND value=100"#, r#"SCHEDRAND value=200"#),''')
p.write_text(s)
p=P/'hermit-cli/src/bin/hermit/logdiff.rs';s=p.read_text();needle='''    #[test]
    fn canonical_all_records_uses_the_fixed_shared_comparator()''';assert s.count(needle)==1
s=s.replace(needle,'''    #[test]
    fn plain_follow_compares_all_info_without_json_or_compatibility_flags() {
        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("left.log");
        let right = directory.path().join("right.log");
        let text = |value| {
            format!(
                "{}Apr 09 06:08:02.100  INFO unrelated: value={value}\\n{}",
                current_record(1, "DETLOG stable"),
                current_record(3, "DETLOG unfinished tail"),
            )
        };
        std::fs::write(&left, text(100)).unwrap();
        std::fs::write(&right, text(200)).unwrap();
        let mut options = LogDiffCLIOpts::new(&left, &right);
        options.follow = true;
        options.follow_interval_ms = 1;
        options.follow_timeout_secs = 1;
        let global = GlobalOpts::try_parse_from(["hermit"]).unwrap();
        assert_eq!(
            options.main(&global),
            ExitStatus::Exited(HERMIT_VERIFICATION_DIVERGENCE_EXIT)
        );
    }

    #[test]
    fn canonical_all_records_uses_the_fixed_shared_comparator()''')
p.write_text(s)
print('Final preview controls prepared without execution.')
