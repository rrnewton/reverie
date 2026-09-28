import hashlib,json,os,re,subprocess,sys
from pathlib import Path
r=Path(__file__).resolve().parents[2];d=Path(__file__).resolve().parent;old=d/'negative-before-v1';q=d/sys.argv[1];q.mkdir()
for name in ['observer','controls','loader','tmp','cache']:(q/name).mkdir()
origins=[]
paths=subprocess.check_output(['git','-C',str(r),'diff','HEAD','--name-only'],timeout=30).decode().splitlines()
paths+=subprocess.check_output(['git','-C',str(r),'ls-files','--others','--exclude-standard','--','reverie/src','reverie-kvm/src','reverie-kvm/tests'],timeout=30).decode().splitlines()
paths=sorted(p for p in paths if p.endswith('.rs'))
for name in ['common.py','cache_lease.py','prepare.py','phase.py','dependency-inputs.json','dependency-symlinks.json','dependency-closure.json','observer/observer.py','observer/before_exec.py','observer/unit_reference.py','observer/source-inputs.json']:
 data=(old/name).read_bytes();before=hashlib.sha256(data).hexdigest()
 if name=='prepare.py':
  s=data.decode();s=re.sub(r'        paths=\[.*?\]\n', '        paths='+repr(paths)+'\n',s,count=1);s=s.replace("--name-only'],timeout=5","--name-only'],timeout=30").replace("'reverie-kvm/tests'],timeout=5","'reverie-kvm/tests'],timeout=30");data=s.encode()
 (q/name).write_bytes(data);origins.append(dict(source=str(old/name),source_sha256=before,destination=str(q/name),sha256=hashlib.sha256(data).hexdigest()))
selection=json.loads((r/'ignored/timer-integration-review-response-20260918/qualification-v6/SELECTORS.json').read_text())
selection['groups']['test-correction-lib']=dict(artifact='lib',names=[
 'runtime::signal_cleanup_tests::signal_cleanup_pending_callback_observes_published_peer_failure',
 'runtime::signal_cleanup_tests::signal_cleanup_ready_result_precedes_terminal_cancellation',
 'runtime::signal_cleanup_tests::signal_cleanup_real_fifo_wait_retains_only_waiting_owner_after_failure',
 'runtime::signal_cleanup_tests::signal_bookkeeping_failure_cannot_resume_injected_or_unsubscribed_syscall',
 'executor::tests::signal_dequeue_raw_consumers_keep_bookkeeping_failure_terminal',
 'executor::tests::signal_dequeue_no_effect_error_preserves_live_sibling_stream',
 'executor::tests::signal_dequeue_full_journal_failure_retains_partial_read_and_every_removal',
 'runtime::signal_cleanup_tests::signal_cleanup_terminal_race_keeps_error_already_transferred_by_callback'])
selection['groups']['test-correction-vm']=dict(artifact='static',names=[
 'parked_signals::parked_signal_worker_panic_retains_both_dequeue_owners',
 'parked_signals::parked_signal_independent_delayed_worker_ack_wakes_sibling',
 'parked_signals::parked_signal_caught_posthook_mask_and_altstack_reach_real_frame',
 'parked_signals::parked_signal_admission_resolves_actual_thread_owner_before_global_or_guest'])
selection['groups']['test-terminal-exec-lib']=dict(artifact='lib',names=[
 'runtime::failure_tests::caught_worker_panic_publishes_before_retirement_and_pending_rpc_join',
 'runtime::failure_tests::failure_precedes_ready_callback_and_keeps_child_start_gate_closed',
 'vm::tests::worker_exec_action_preserves_cancelled_group_without_parking',
 'vm::tests::worker_exec_action_preserves_cancelled_group_before_parking',
 'vm::tests::exec_teardown_disposition_survives_unstarted_child_cleanup'])
selection['groups']['test-exec-vm']=dict(artifact='static',names=[
 'exec_worker_error_diagnostic::exec_worker_error_still_consumes_root_and_process_hooks',
 'worker_exec_is_refused_without_replacing_shared_memory'])
for group in selection['groups'].values():group['names'].sort()
(q/'SELECTORS.json').write_text(json.dumps(selection,indent=2)+'\n')
rows=[]
for line in subprocess.check_output(['git','-C',str(r),'ls-files','--stage','-z'],timeout=30).split(b'\0'):
 if not line:continue
 header,name=line.split(b'\t',1);mode,blob,stage=header.decode().split();name=name.decode();p=r/name
 if mode=='160000':rows.append(dict(path=name,mode=mode,git_blob=blob));continue
 raw=os.fsencode(os.readlink(p)) if mode=='120000' else p.read_bytes();rows.append(dict(path=name,mode=mode,git_blob=hashlib.sha1(b'blob '+str(len(raw)).encode()+b'\0'+raw).hexdigest(),bytes=len(raw),sha256=hashlib.sha256(raw).hexdigest()))
for name in subprocess.check_output(['git','-C',str(r),'ls-files','--others','--exclude-standard','--','reverie/src','reverie-kvm/src','reverie-kvm/tests'],timeout=30).decode().splitlines():
 raw=(r/name).read_bytes();rows.append(dict(path=name,mode='100644',git_blob=hashlib.sha1(b'blob '+str(len(raw)).encode()+b'\0'+raw).hexdigest(),bytes=len(raw),sha256=hashlib.sha256(raw).hexdigest()))
(q/'source-manifest.json').write_text(json.dumps(rows,indent=2)+'\n')
(q/'RUNNER_ORIGINS.json').write_text(json.dumps(dict(origins=origins,adjustments='Exact corrected-source selectors and changed Rust paths. Same approved observer, limits, owned lease/cache, toolchain and source/dependency admission. Existing metadata/dependency closure bound and rechecked.'),indent=2)+'\n')
print(q,hashlib.sha256((q/'source-manifest.json').read_bytes()).hexdigest(),len(rows))
