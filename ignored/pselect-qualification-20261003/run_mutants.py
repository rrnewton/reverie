from pathlib import Path
import hashlib, json, subprocess, sys, difflib
src=Path('reverie-kvm/src/executor.rs')
e=Path('ignored/pselect-qualification-20261003')
original=src.read_bytes(); text=original.decode()
assert hashlib.sha256(original).hexdigest()=='29ed3c15526792c21c5c3af59a068cb93feb95391dd727f250b233b95f271e16'
base=subprocess.check_output(['git','show','3a196cfb9dbb775900a1acb02890b8ca10aedd0c:reverie-kvm/src/executor.rs'],text=True)
end='\n// AUTONOMOUS-BOT-IMPLEMENTED\n// TODO-HUMAN-REVIEW(PR-205): Review deterministic select'
old=base[base.index('fn pselect6_validation_preflight('):base.index(end,base.index('fn pselect6_validation_preflight('))]
old=old.replace('fn pselect6_validation_preflight(memory: &GuestMemory, args: &[u64; 6])', 'fn pselect6(memory: &GuestMemory, _state: &LoadedStaticElf, args: &[u64; 6])',1)
a=text.index('fn pselect6(');b=text.index(end,a)
mutations=[('mutant-old-enosys',text[:a]+old+text[b:],['--test','static_elf','pselect_zero::readiness_matches_native_pipe_socket_and_path_states']),('mutant-signal-guard',text.replace('if ready_count == 0 && has_eligible_pending_signal(state) {','if false && ready_count == 0 && has_eligible_pending_signal(state) {',1),['--lib','executor::tests::pselect6_zero_pending_signal_guard_is_read_only_and_ignores_delivery_permits'])]
results=[]
try:
 for name, changed, selector in mutations:
  assert changed != text
  src.write_text(changed)
  (e/(name+'.patch')).write_text(''.join(difflib.unified_diff(text.splitlines(True),changed.splitlines(True),fromfile='candidate/executor.rs',tofile='mutant/executor.rs')))
  cmd=[sys.executable,str(e/'run_guard.py'),'--phase',name,'--seconds','300','--','cargo','test','--locked','-p','reverie-kvm',*selector,'-j','8','--','--exact','--test-threads=1','--nocapture']
  rc=subprocess.run(cmd).returncode
  log=(e/name/'output.log').read_text()
  record={'phase':name,'rc':rc,'source_sha256':hashlib.sha256(changed.encode()).hexdigest(),'compiled_and_ran': 'Running ' in log and 'test result: FAILED.' in log}
  results.append(record)
  if rc!=101 or not record['compiled_and_ran']:
   raise RuntimeError('unexpected mutant outcome '+json.dumps(record))
finally:
 src.write_bytes(original)
 assert src.read_bytes()==original
 (e/'MUTANTS.json').write_text(json.dumps({'results':results,'restored_sha256':hashlib.sha256(src.read_bytes()).hexdigest()},indent=2)+'\n')
print(json.dumps(results),flush=True)
