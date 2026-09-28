import hashlib,json,os,subprocess,sys
from pathlib import Path
repo=Path(__file__).resolve().parents[2];own=Path(__file__).resolve().parent;old=repo/'ignored/process-alarm-qualification-v5';q=own/('qualification-v'+sys.argv[1]);q.mkdir();origins=[]
for d in ['observer','controls','loader','tmp','cache']:(q/d).mkdir()
for name in ['common.py','cache_lease.py','prepare.py','phase.py','SELECTORS.json','dependency-inputs.json','dependency-symlinks.json','dependency-closure.json','observer/observer.py','observer/before_exec.py','observer/unit_reference.py','observer/source-inputs.json']:
 data=(old/name).read_bytes();dest=q/name
 if name in ['common.py','prepare.py','phase.py']:
  data=data.replace(b'REPO = HERE.parents[1]',b'REPO = HERE.parents[2]').replace(b'REPO=HERE.parents[1]',b'REPO=HERE.parents[2]')
 if name=='prepare.py':
  data=data.replace(b"REPO/'ignored/process-alarm-qualification-v1/lane.lease'",b"REPO/'ignored/timer-integration-20260918/lane.lease'")
  data=data.replace(b"REPO/'ignored/process-alarm-qualification-v1/lease-state'",b"REPO/'ignored/timer-integration-20260918/lease-state'")
  data=data.replace(b"Unwired Reverie component only; no Hermit scheduler delivery, setitimer, periodic rearm or parity claim",b"Reverie parked signal component only; no Hermit timer integration or parity claim")
 dest.write_bytes(data);origins.append({'source':str(old/name),'source_sha256':hashlib.sha256((old/name).read_bytes()).hexdigest(),'destination':str(dest),'sha256':hashlib.sha256(data).hexdigest()})
rows=[]
for line in subprocess.check_output(['git','-C',str(repo),'ls-files','--stage','-z']).split(b'\0'):
 if not line:continue
 header,name=line.split(b'\t',1);mode,blob,stage=header.decode().split();name=name.decode();path=repo/name
 if mode=='160000':rows.append({'path':name,'mode':mode,'git_blob':blob});continue
 raw=os.fsencode(os.readlink(path)) if mode=='120000' else path.read_bytes()
 actual_blob=hashlib.sha1(b'blob '+str(len(raw)).encode()+b'\0'+raw).hexdigest()
 rows.append({'path':name,'mode':mode,'git_blob':actual_blob,'bytes':len(raw),'sha256':hashlib.sha256(raw).hexdigest()})
for name in subprocess.check_output(['git','-C',str(repo),'ls-files','--others','--exclude-standard','--','reverie/src','reverie-kvm/src','reverie-kvm/tests']).decode().splitlines():
 raw=(repo/name).read_bytes();rows.append({'path':name,'mode':'100644','git_blob':hashlib.sha1(b'blob '+str(len(raw)).encode()+b'\0'+raw).hexdigest(),'bytes':len(raw),'sha256':hashlib.sha256(raw).hexdigest()})
(q/'source-manifest.json').write_text(json.dumps(rows,indent=2)+'\n')
(q/'RUNNER_ORIGINS.json').write_text(json.dumps({'copied_inputs':origins,'adjustments':'Owned nested evidence root and lease paths, component scope, and concrete selector additions only. Original bounds and observer unchanged.'},indent=2)+'\n')
selection=json.loads((q/'SELECTORS.json').read_text())
selection['groups']['test-domain-lib']['names'] += ['executor::tests::'+n for n in ['signal_dequeue_all_consumers_preserve_domain_and_post_copyout_effects','signal_dequeue_readiness_error_retains_complete_removal','signal_dequeue_sequence_exhaustion_refuses_before_removal','signal_dequeue_fork_resets_and_exec_retains_process_sequence','signal_dequeue_reused_task_refuses_before_all_three_removals','signal_dequeue_first_private_nonalarm_is_bound_before_any_parked_call']]
selection['groups']['test-domain-lib']['names'].append('error::tests::signal_effects_preserve_first_failure_and_cancellation_evidence')
selection['groups']['test-alarm-vm']['names'].append('parked_signals::parked_signal_actual_callback_frame_and_dequeue_contract')
for group in selection['groups'].values():group['names'].sort()
(q/'SELECTORS.json').write_text(json.dumps(selection,indent=2)+'\n')
print(str(q),len(rows))

retired=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-setitimer-20260918/ignored/reporting-cgroup-retirement-v1/observer.py')
data=retired.read_bytes();assert hashlib.sha256(data).hexdigest()=='137c9b42c3f9e081db59954e0ef692ed861f488be7c2bb8a15ef595c271b2179'
(q/'observer/observer.py').write_bytes(data)
r=json.loads((q/'RUNNER_ORIGINS.json').read_text());r['retirement_successor']={'source':str(retired),'sha256':hashlib.sha256(data).hexdigest(),'scope':'Root-approved exact terminal cgroup retirement delta; original bounds unchanged.'}
(q/'RUNNER_ORIGINS.json').write_text(json.dumps(r,indent=2)+'\n')
