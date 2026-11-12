from pathlib import Path
import hashlib,json,difflib
P=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-proc-fd-identity-20260917/ignored/sigchld-first-guard-proposal-20260917/cli-part')
for name in ['candidate.patch','FILES.json']:
 with (P/(name+'.draft-v1')).open('xb') as f:f.write((P/name).read_bytes())
p=P/'changed/hermit-cli/src/error.rs';s=p.read_text()
needle='''fn is_policy_refusal(err: &Error) -> bool {
    if err
        .downcast_ref::<crate::KvmIndependentSignalFailure>()
        .is_some()
    {
        return false;
    }
'''
assert s.count(needle)==1
s=s.replace(needle,'''fn is_policy_refusal(err: &Error) -> bool {
    fn independent_signal_failure(err: &Error) -> bool {
        err.downcast_ref::<crate::KvmIndependentSignalFailure>()
            .is_some()
            || err.chain().any(|cause| {
                matches!(
                    cause.downcast_ref::<reverie::Error>(),
                    Some(reverie::Error::Tool(inner)) if independent_signal_failure(inner)
                )
            })
    }
    if independent_signal_failure(err) {
        return false;
    }
''')
needle='''    if err.downcast_ref::<detcore::KvmSignalRefusal>().is_some() {
        return true;
    }
'''
assert s.count(needle)==1
s=s.replace(needle,'''    if err.downcast_ref::<detcore::KvmSignalRefusal>().is_some()
        || err.downcast_ref::<detcore::UnsupportedSyscallError>().is_some()
    {
        return true;
    }
''')
needle='''            assert_eq!(serde_json::from_slice::<SerializableError>(&wire).unwrap(), error);'''
s=s.replace(needle,'''            assert_eq!(
                serde_json::from_slice::<SerializableError>(&wire).unwrap(),
                error,
            );''')
needle='''        assert_eq!(same_text.kind(), FailureKind::Error);
    }
'''
assert s.count(needle)==1
s=s.replace(needle,'''        assert_eq!(same_text.kind(), FailureKind::Error);

        // The transparent wrapper must not hide an independently meaningful
        // failure and leave only the policy cause visible to classification.
        let compound = crate::kvm_completion_error(
            Some(reverie_kvm::Error::InvalidGuestPid(-17)),
            Some(detcore::BackendFailureCleanup {
                scheduler: Ok(()),
                preemption_recording: Ok(()),
                signal_refusal: Some(refusal),
            }),
        );
        let wrapped = SerializableError::from(Error::new(reverie::Error::Tool(compound)));
        assert_eq!(wrapped.kind(), FailureKind::Error);
    }
''')
p.write_text(s)
p=P/'changed/hermit-cli/src/lib.rs';s=p.read_text();s=s.replace('''        formatter.write_str("KVM signal refusal accompanied by an independent execution or cleanup failure")''','''        formatter.write_str(
            "KVM signal refusal accompanied by an independent execution or cleanup failure",
        )''');p.write_text(s)
p=P/'changed/hermit-cli/src/kvm_failure_tests.rs';s=p.read_text();s=s.replace('''        assert_eq!(error.downcast_ref::<detcore::KvmSignalRefusal>(), Some(&refusal));''','''        assert_eq!(
            error.downcast_ref::<detcore::KvmSignalRefusal>(),
            Some(&refusal),
        );''').replace('''            2 => assert!(matches!(backend, Some(reverie_kvm::Error::InvalidGuestPid(-17)))),''','''            2 => assert!(matches!(
                backend,
                Some(reverie_kvm::Error::InvalidGuestPid(-17))
            )),''').replace('''    assert!(error.downcast_ref::<tokio::task::JoinError>().unwrap().is_panic());''','''    assert!(
        error
            .downcast_ref::<tokio::task::JoinError>()
            .unwrap()
            .is_panic()
    );''');p.write_text(s)
patch=[];records=[]
for item in json.loads((P/'FILES.json.draft-v1').read_text()):
 name=item['path'];a=(P/'base'/name).read_bytes();b=(P/'changed'/name).read_bytes()
 patch.append('diff --git a/'+name+' b/'+name+'\n'+''.join(difflib.unified_diff(a.decode().splitlines(True),b.decode().splitlines(True),fromfile='a/'+name,tofile='b/'+name)))
 records.append(dict(path=name,base_bytes=len(a),base_sha256=hashlib.sha256(a).hexdigest(),mode='100644',bytes=len(b),sha256=hashlib.sha256(b).hexdigest()))
(P/'candidate.patch').write_text(''.join(patch));(P/'FILES.json').write_text(json.dumps(records,indent=2)+'\n')
print('patch bytes',len((P/'candidate.patch').read_bytes()),'SHA',hashlib.sha256((P/'candidate.patch').read_bytes()).hexdigest())
