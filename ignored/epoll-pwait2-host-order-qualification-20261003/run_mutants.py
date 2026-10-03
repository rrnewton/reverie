#!/usr/bin/env python3
"""Run the four declared causal controls serially; always restore exact source."""
import hashlib,json,os,pathlib,re,signal,subprocess,sys,time
N=pathlib.Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-epoll-pwait2-20261003')
E=N/'ignored/epoll-pwait2-host-order-qualification-20261003'
source=N/'reverie-kvm/src/executor.rs'
head='f78354c066fc77e160216e959d1fb979c0348696'
assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=N,text=True).strip()==head
plan=json.loads((E/'MUTANT-PLAN.json').read_text())
base=source.read_bytes(); digest=hashlib.sha256(base).hexdigest()
assert digest==json.loads((E/'DECLARED-INVENTORY.json').read_text())['source']['reverie-kvm/src/executor.rs']
(E/'RESTORE-executor.rs').write_bytes(base)
cancel=[]
for sig in [signal.SIGINT,signal.SIGTERM]:signal.signal(sig,lambda n,_:cancel.append(n))
results=[]
try:
 for row in plan['cases']:
  if cancel:raise RuntimeError('cancelled before mutation')
  assert source.read_bytes()==base
  text=base.decode();start=text.index('fn epoll_pwait2_missing_fd_with_host(');end=text.index('\nfn epoll_pwait2_missing_fd(',start)
  block=text[start:end];assert block.count(row['old'])==1,row
  mutant=(text[:start]+block.replace(row['old'],row['new'],1)+text[end:]).encode()
  source.write_bytes(mutant)
  record={'phase':row['phase'],'base_sha256':digest,'mutant_sha256':hashlib.sha256(mutant).hexdigest(),'head':head}
  try:
   command=[sys.executable,str(E/'run_guard.py'),'--phase',row['phase'],'--seconds','300','--','cargo','test','--locked','-p','reverie-kvm','--lib',row['exact_test'],'-j8','--','--exact','--nocapture','--test-threads=1']
   record['command']=command
   with (E/(row['phase']+'-launch.log')).open('wb') as log:
    child=subprocess.Popen(command,cwd=N,stdout=log,stderr=subprocess.STDOUT)
    sent=False
    while child.poll() is None:
     if cancel and not sent:
      child.send_signal(signal.SIGTERM);sent=True
     time.sleep(0.2)
   record['launcher_rc']=child.returncode
  finally:
   # Signal handlers record cancellation without raising; no cancellation
   # checkpoint may interrupt this mandatory byte restoration.
   source.write_bytes(base)
   record['restored_sha256']=hashlib.sha256(source.read_bytes()).hexdigest()
   assert source.read_bytes()==base
   (E/(row['phase']+'-RESTORATION.json')).write_text(json.dumps(record,indent=2)+'\n')
  if cancel:raise RuntimeError('cancelled; bytes restored')
  receipt=json.loads((E/row['phase']/'RESULT.json').read_text())
  log=(E/row['phase']/'output.log').read_text()
  assert record['launcher_rc']==receipt['rc']==receipt['actual_command_rc']==101,receipt
  assert receipt['stop_reason'] is None and receipt['remaining_descendants']==[],receipt
  assert 'running 1 test' in log
  assert row['exact_test'] in log
  assert 'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 1113 filtered out;' in log
  record.update({'expected_failure_confirmed':True,'wall_seconds':receipt['wall_seconds'],'output_sha256':receipt['output_sha256']})
  results.append(record)
  print(json.dumps({'phase':row['phase'],'rc':receipt['rc'],'wall_seconds':receipt['wall_seconds'],'restored':record['restored_sha256']==digest}),flush=True)
finally:
 source.write_bytes(base)
 assert source.read_bytes()==base
 (E/'MUTANT-RESULTS.json').write_text(json.dumps({'head':head,'base_sha256':digest,'cancel_signals':cancel,'results':results,'restored_sha256':hashlib.sha256(source.read_bytes()).hexdigest(),'complete':len(results)==4 and not cancel},indent=2)+'\n')
