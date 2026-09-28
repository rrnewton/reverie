from pathlib import Path
import json,hashlib,subprocess,shutil
D=Path(__file__).resolve().parent;R='/home/newton/work/dev-hermit/worktrees/slots/kvm-prejoin-main-20260917';B='8051335e87104f7cf832204f74920d38416393b2'
P=D/'preview-v9';P.mkdir(exist_ok=False)
for row in json.loads((D/'PREVIEW-MANIFEST-v8.json').read_text())['files']:
 src=Path(row['preview_path']);assert hashlib.sha256(src.read_bytes()).hexdigest()==row['sha256']
 p=P/row['path'];p.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(src,p)
for path in ['tests/qemu-boot/strict_l2_test.sh','scripts/lib/validate_cell_results.rs']:
 data=subprocess.check_output(['git','show',B+':'+path],cwd=R,timeout=20)
 p=P/path;p.parent.mkdir(parents=True,exist_ok=True);p.write_bytes(data)
 mode=subprocess.check_output(['git','ls-tree',B,'--',path],cwd=R,text=True,timeout=20).split()[0];p.chmod(0o755 if mode=='100755' else 0o644)
p=P/'tests/qemu-boot/strict_l2_test.sh';s=p.read_text();old='"$VERIFICATION_REPORT_BIN" matched "$verify_report"';assert s.count(old)==1;s=s.replace(old,'"$VERIFICATION_REPORT_BIN" canonical-match "$verify_report"');p.write_text(s)
p=P/'hermit-cli/tests/verification_report_consumers.rs';s=p.read_text();old='''        path: "tests/qemu-boot/strict_l2_test.sh",
        requirement: "matched",
        invocation: "\\\"$VERIFICATION_REPORT_BIN\\\" matched \\\"$verify_report\\\"",''';new=old.replace('requirement: "matched"','requirement: "canonical-match"').replace(' matched ', ' canonical-match ');assert s.count(old)==1;s=s.replace(old,new)
anchor='#[test]\nfn json_output_retains_failure_evidence_without_satisfying_a_match_requirement()'
assert s.count(anchor)==1
control='''#[test]
fn qemu_boot_consumer_rejects_noncanonical_and_unequal_match_claims() {
    let consumer = CONSUMERS
        .iter()
        .find(|consumer| consumer.path == "tests/qemu-boot/strict_l2_test.sh")
        .unwrap();
    assert_eq!(consumer.requirement, "canonical-match");
    let script = fs::read_to_string(root().join(consumer.path)).unwrap();
    assert_eq!(script.matches(consumer.invocation).count(), 1);
    let temporary = temporary_directory();
    let path = temporary.join("verify.json");
    for case in ["matched", "stripped", "no-log-comparison", "unequal-counts"] {
        let mut report = current_synthetic_match();
        report["compared_log_messages"] = serde_json::json!({"left": 123, "right": 123});
        match case {
            "matched" => {},
            "stripped" => report["comparison"]["strictness"] = serde_json::json!("stripped"),
            "no-log-comparison" => report["comparison"]["compare_logs"] = serde_json::json!(false),
            "unequal-counts" => report["compared_log_messages"]["right"] = serde_json::json!(124),
            _ => unreachable!(),
        }
        write_report(&path, &report);
        let output = verdict(consumer.requirement, &path);
        assert_eq!(output.status.code(), Some(if case == "matched" { 0 } else { 1 }), "{case}: {output:?}");
        if case == "unequal-counts" {
            assert!(String::from_utf8_lossy(&output.stderr).contains("canonical match has unequal compared log-message counts: 123/124"));
        }
    }
    fs::remove_dir_all(temporary).unwrap();
}

'''
s=s.replace(anchor,control+anchor);p.write_text(s)
p=P/'scripts/lib/validate_cell_results.rs';s=p.read_text();old='''        let matched = report.verified
            && report.verdict == VerificationVerdict::Matched
            && report.bitwise_parity;''';new='''        let matched = report.require_canonical_match().is_ok();''';assert s.count(old)==1;s=s.replace(old,new)
anchor='    #[test]\n    fn schema7_binds_virtual_time_to_the_comparison_mode()';assert s.count(anchor)==1
control='''    #[test]
    fn matched_projection_rejects_unequal_counts_and_retains_sticky_divergence() {
        let mut row = result_row("validate-counts", "1818181818181818181818181818181818181818");
        assert!(matches!(cell_verdict(&row).unwrap(), CellVerdict::ComparedAndMatched { .. }));
        let mut contradictory: Value = serde_json::from_str(&report("matched", "info")).unwrap();
        contradictory["compared_log_messages"]["right"] = Value::from(124);
        replace_report(&mut row, &contradictory);
        let CellVerdict::UnavailableWithReason { reason, .. } = cell_verdict(&row).unwrap() else {
            panic!("a claimed 123/124 match became compared evidence");
        };
        assert_eq!(reason, "typed canonical report was neither a match nor a divergence");
        let unequal_match = serde_json::to_string(&contradictory).unwrap();
        let divergent = report("diverged", "info");
        let matched = report("matched", "info");
        for attempts in [
            vec![attempt(&divergent)],
            vec![attempt(&unequal_match), attempt(&divergent)],
            vec![attempt(&divergent), attempt(&unequal_match)],
            vec![attempt(&matched), attempt(&divergent)],
            vec![attempt(&divergent), attempt(&matched)],
        ] {
            row["attempts"] = Value::Array(attempts);
            let CellVerdict::ComparedAndDiverged {
                comparison_tier,
                bitwise_parity,
                compared_log_messages: RequiredNullable::Value(counts),
                ..
            } = cell_verdict(&row).unwrap() else {
                panic!("the real unequal-count divergence was lost: {row}");
            };
            assert_eq!(comparison_tier, ComparisonTier::CanonicalBitwise);
            assert!(!bitwise_parity);
            assert_eq!(counts, ComparedLogCounts { left: 123, right: 124 });
        }
    }

'''
s=s.replace(anchor,control+anchor);p.write_text(s)
print('Prepared three-path v9 increment in isolated preview-v9; no product execution.')
