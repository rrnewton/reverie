from pathlib import Path
import hashlib,json,signal,subprocess,sys
kind=sys.argv[1]
assert kind in ['zero','copyback','eventescape']
root=Path.cwd(); e=root/'ignored/epoll-pwait2-qualification-20261003'
p=root/'reverie-kvm/src/executor.rs'; original=p.read_bytes(); text=original.decode()
expected=json.loads((e/'repair-integration-001/RESULT.json').read_text())['source_after']['reverie-kvm/src/executor.rs']
assert hashlib.sha256(original).hexdigest()==expected
if kind=='zero':
 start=text.index('fn epoll_pwait2(memory:'); end=text.index('\nfn epoll_wait(',start)
 part=text[start:end]; assert part.count('ready as i64')==1
 changed=text[:start]+part.replace('ready as i64','0 /* planted false readiness */')+text[end:]
 target=['--test','static_elf','epoll_pwait2::seven_check_contract_matches_native']
elif kind=='copyback':
 old='        let result = epoll_pwait2(memory, state, args);'
 assert text.count(old)==1
 changed=text.replace(old,'        let result = epoll_wait(memory, state, args, false); // planted temporary copyback')
 target=['--test','static_elf','epoll_pwait2::scalar_faults_preserve_bytes_and_oneshot_events']
else:
 old='                        identity.escaped.store(true, Ordering::SeqCst);'
 assert text.count(old)==1
 changed=text.replace(old,'                        let _ = identity; // planted missing export invalidation')
 target=['--lib','executor::tests::epoll_pwait2_scm_failed_export_invalidates_every_alias_and_future_registration']
packet=e/('mutant-'+kind+'-source'); packet.mkdir()
(packet/'executor.original.rs').write_bytes(original)
(packet/'executor.mutant.rs').write_text(changed)
record={'kind':kind,'original_sha256':expected,'mutant_sha256':hashlib.sha256(changed.encode()).hexdigest(),'expected_test_rc':101,'cancel_signals':[]}
for sig in [signal.SIGINT,signal.SIGTERM]:
 signal.signal(sig,lambda n,_:record['cancel_signals'].append(n))
try:
 p.write_text(changed)
 command=['python3',str(e/'run_guard.py'),'--phase','mutant-'+kind,'--seconds','300','--','cargo','test','--locked','-p','reverie-kvm',*target,'--','--exact','--nocapture','--test-threads=1']
 record['command']=command
 record['guard_rc']=subprocess.run(command).returncode
finally:
 p.write_bytes(original)
 record['restored_sha256']=hashlib.sha256(p.read_bytes()).hexdigest()
 assert record['restored_sha256']==expected
 (packet/'RESULT.json').write_text(json.dumps(record,indent=2)+'\n')
print(json.dumps(record))
raise SystemExit(0 if record.get('guard_rc')==101 and not record['cancel_signals'] else 1)
