from pathlib import Path
import subprocess,json,hashlib,re
r=Path(__file__).resolve().parents[2];d=Path(__file__).resolve().parent/'prequalification-v1';d.mkdir()
paths=['reverie-kvm/src/elf.rs','reverie-kvm/src/executor.rs','reverie-kvm/src/process_signal_publication.rs']
rows=[]
for p in paths:
 b=(r/p).read_bytes();out=d/'after'/p;out.parent.mkdir(parents=True,exist_ok=True);out.write_bytes(b)
 rows.append({'path':p,'sha256':hashlib.sha256(b).hexdigest(),'bytes':len(b),'mode':'100644'})
patch=subprocess.check_output(['git','diff','--binary','HEAD','--',*paths],cwd=r)
a=subprocess.run(['git','diff','--no-index','--binary','--','/dev/null',paths[-1]],cwd=r,capture_output=True);assert a.returncode==1 and not a.stderr
patch+=a.stdout;(d/'SOURCE.patch').write_bytes(patch)
(d/'SOURCE_INPUTS.json').write_text(json.dumps({'base':subprocess.check_output(['git','rev-parse','HEAD'],cwd=r,text=True).strip(),'changed':rows,'new_product_path':paths[-1],'qualification':'none attempted'},indent=2)+'\n')
new=re.findall(r'    #\[test\]\n    fn ([a-z_0-9]+)\(', (r/paths[-1]).read_text())
old=[]
for p in ['process_alarm_signal_tests.rs','child_exit_signal_tests.rs','signal_dequeue_tests.rs']:
 old+=re.findall(r'#\[test\]\nfn ([a-z_0-9]+)\(', (r/'reverie-kvm/src'/p).read_text())
old+=['consuming_signal_refreshes_distinct_and_duplicated_signalfd_readiness','delivery_selection_refreshes_every_signalfd_alias_before_filtering','single_thread_signalfd_supports_private_pending_and_refuses_sibling_lifetimes','process_fork_with_inherited_signalfd_is_explicitly_unsupported','blocking_signalfd_forms_refuse_before_mutation_and_aliases_stay_nonblocking','exec_closes_cloexec_descriptors_and_resets_caught_signals','rt_sigreturn_ignores_invalid_altstack_restore_but_restores_the_mask','rt_sigreturn_ignores_ss_onstack_altstack_input','process_directed_signal_with_live_sibling_is_schedule_independent_refusal','process_pending_signal_prevents_a_new_host_timed_thread_consumer','sibling_signal_retirement_and_tid_reuse_preserve_exact_registration','accepted_sibling_signal_survives_exec_and_endpoint_replacement']
selectors=['executor::process_signal_publication::tests::'+n for n in new]+['executor::tests::'+n for n in old]
assert len(selectors)==len(set(selectors))
(d/'SELECTORS.json').write_text(json.dumps({'new_declarations':len(new),'unchanged_neighbors':len(old),'groups':{f'test-{i:02}':{'artifact':'lib','names':[n]}for i,n in enumerate(selectors,1)}},indent=2)+'\n')
print('new',len(new),'old',len(old),'total',len(selectors))
print('patch',hashlib.sha256(patch).hexdigest())
