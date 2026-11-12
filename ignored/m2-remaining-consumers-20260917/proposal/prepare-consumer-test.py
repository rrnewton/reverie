from pathlib import Path
import hashlib,json,re,stat
proposal=Path(__file__).resolve().parent
series=proposal.parent.parent/'m2-next-increment-20260917'
path='hermit-cli/tests/verification_report_consumers.rs'
source=series/'preview-v10'/path
before=source.read_text()
paths=[item['path'] for item in json.loads((proposal.parent/'shell-helper/CONSUMER-MAP.json').read_text())]
after=before
changed=[]
for consumer_path in paths:
 pattern=r'    Consumer \{\n        path: "'+re.escape(consumer_path)+r'",\n.*?\n    \},'
 found=list(re.finditer(pattern,after,re.S)); assert len(found)==1, consumer_path
 old=found[0].group(0)
 assert old.count('requirement: "matched"')==1
 assert old.count('\\" matched \\"')==0  # The actual Rust string uses one backslash per quote.
 new=old.replace('requirement: "matched"','requirement: "canonical-match"')
 assert new.count(' matched ')==1, consumer_path
 new=new.replace(' matched ',' canonical-match ')
 after=after[:found[0].start()]+new+after[found[0].end():]
 changed.append({'path':consumer_path,'old':old,'new':new})
control=r'''#[test]
fn remaining_shell_consumers_require_current_canonical_match_evidence() {
    let selected_paths = [
        "tests/e2e/lib/data-handling/common.bash",
        "tests/e2e/lib/determinism-stress/common.sh",
        "tests/e2e/lib/language-runtimes/run.sh",
        "tests/e2e/lib/system-utils/_common.sh",
        "tests/qemu-boot/strict_l2_userspace_test.sh",
        "tests/standalone/strict_setitimer.sh",
        "tests/standalone/strict_timer_create.sh",
    ];
    let temporary = temporary_directory();
    let path = temporary.join("verify.json");
    for selected_path in selected_paths {
        let consumer = CONSUMERS
            .iter()
            .find(|consumer| consumer.path == selected_path)
            .expect("retain every named shell consumer");
        assert_eq!(consumer.requirement, "canonical-match", "{selected_path}");
        let script = fs::read_to_string(root().join(selected_path)).unwrap();
        assert_eq!(script.matches(consumer.invocation).count(), 1, "{selected_path}");
        assert!(
            !script.contains("\"$VERIFICATION_REPORT_BIN\" matched "),
            "{selected_path} still admits the weaker requirement"
        );
        for case in [
            "matched",
            "stripped",
            "no-log-comparison",
            "zero-counts",
            "unequal-counts",
            "missing-report",
            "invalid-json",
            "missing-current-outputs",
        ] {
            let mut report = current_synthetic_match();
            report["compared_log_messages"] =
                serde_json::json!({"left": 123, "right": 123});
            match case {
                "stripped" => {
                    report["comparison"]["strictness"] = serde_json::json!("stripped");
                }
                "no-log-comparison" => {
                    report["comparison"]["compare_logs"] = serde_json::json!(false);
                }
                "zero-counts" => {
                    report["compared_log_messages"] =
                        serde_json::json!({"left": 0, "right": 0});
                }
                "unequal-counts" => {
                    report["compared_log_messages"]["right"] = serde_json::json!(124);
                }
                "missing-current-outputs" => {
                    report.as_object_mut().unwrap().remove("compared_outputs");
                }
                "matched" | "missing-report" | "invalid-json" => {}
                _ => unreachable!(),
            }
            write_report(&path, &report);
            let expected_status = match case {
                "matched" => 0,
                "missing-report" => {
                    fs::remove_file(&path).unwrap();
                    2
                }
                "invalid-json" => {
                    fs::write(&path, b"{").unwrap();
                    2
                }
                "missing-current-outputs" => 2,
                _ => 1,
            };
            let output = verdict(consumer.requirement, &path);
            assert_eq!(
                output.status.code(),
                Some(expected_status),
                "{selected_path} {case}: {output:?}"
            );
            if case == "unequal-counts" {
                assert!(
                    String::from_utf8_lossy(&output.stderr)
                        .contains("canonical match has unequal compared log-message counts: 123/124"),
                    "{selected_path}: {output:?}"
                );
            }
        }
    }
    fs::remove_dir_all(temporary).unwrap();
}

'''
anchor='#[test]\nfn json_output_retains_failure_evidence_without_satisfying_a_match_requirement() {'
assert after.count(anchor)==1
assert 'fn remaining_shell_consumers_require_current_canonical_match_evidence()' not in before
after=after.replace(anchor,control+anchor)
assert before.count('    Consumer {')==after.count('    Consumer {')==13
# Exact unchanged population and functions, with the one declared addition only.
old_paths=re.findall(r'        path: "([^"]+)",',before.split('fn root()')[0])
new_paths=re.findall(r'        path: "([^"]+)",',after.split('fn root()')[0])
assert old_paths==new_paths
old_tests=re.findall(r'#\[test\]\nfn ([a-z0-9_]+)\(',before)
new_tests=re.findall(r'#\[test\]\nfn ([a-z0-9_]+)\(',after)
assert set(new_tests)-set(old_tests)=={'remaining_shell_consumers_require_current_canonical_match_evidence'}
assert all(name in new_tests for name in old_tests)
# Undo the exact additions and substitutions; every other byte must survive.
restored=after.replace(control,'')
for item in changed:
 assert restored.count(item['new'])==1
 restored=restored.replace(item['new'],item['old'])
assert restored==before
out=proposal/'preview'/path
out.parent.mkdir(parents=True,exist_ok=True)
with out.open('x') as f:f.write(after)
out.chmod(stat.S_IMODE(source.stat().st_mode))
def rec(p):
 b=p.read_bytes();return {'path':str(p),'bytes':len(b),'mode':oct(stat.S_IMODE(p.stat().st_mode)),'sha256':hashlib.sha256(b).hexdigest()}
manifest={'base_kind':'exact immutable M2 preview-v10 file','before':rec(source),'after':rec(out),'changed_entries':changed,'retained_thirteen_paths':old_paths,'retained_declared_test_names':old_tests,'added_declared_test_name':'remaining_shell_consumers_require_current_canonical_match_evidence','actual_compiled_inventory':None,'all_other_bytes_retained':True,'formatter_or_tests_run':False}
with (proposal/'consumer-test-changes.json').open('x') as f:json.dump(manifest,f,indent=2);f.write('\n')
print(json.dumps(rec(out)))
