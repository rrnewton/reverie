from pathlib import Path
import hashlib,json,os,subprocess,shutil
root=Path.cwd(); out=root/'ignored/prejoin-failure-implementation-20260917/source-v2';out.mkdir()
def h(b):return hashlib.sha256(b).hexdigest()
base=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip();assert base=='114b309413612fafc2657c74e83811c71aac7b19'
paths=subprocess.check_output(['git','diff','--name-only'],text=True).splitlines()+['reverie-kvm/src/failure.rs','reverie-kvm/src/runtime/failure_tests.rs']
patch=subprocess.check_output(['git','diff','--binary'])
for f in paths[-2:]:
 r=subprocess.run(['git','diff','--no-index','--binary','/dev/null',f],capture_output=True);assert r.returncode==1;patch+=r.stdout
(out/'candidate.patch').write_bytes(patch)
entries=[]
for path in sorted(paths):
 b=(root/path).read_bytes();dest=out/'source'/path;dest.parent.mkdir(parents=True,exist_ok=True);dest.write_bytes(b);entries.append({'path':path,'bytes':len(b),'sha256':h(b),'mode':oct((root/path).stat().st_mode & 0o7777)[2:]})
manifest=[]
for entry in subprocess.check_output(['git','ls-files','--stage','-z']).split(b'\0'):
 if not entry:continue
 meta,path=entry.split(b'\t');mode,blob,stage=meta.decode().split();path=path.decode();row={'path':path,'mode':mode,'git_blob':blob}
 if mode=='160000':row['gitlink']=blob
 else:row['sha256']=h(os.readlink(root/path).encode() if mode=='120000' else (root/path).read_bytes())
 manifest.append(row)
for path in paths[-2:]:manifest.append({'path':path,'mode':'100644','git_blob':None,'sha256':h((root/path).read_bytes())})
(out/'tracked-source-manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
new=[
'failure::tests::independent_failure_subscribers_wake_before_and_after_publication',
'failure::tests::local_failure_wake_waits_for_synchronous_tool_terminal_transition',
'failure::tests::refused_host_spawn_returns_exact_initialized_owner',
'runtime::failure_tests::fatal_notification_releases_pending_tool_rpc_before_owned_join',
'runtime::failure_tests::fatal_worker_primary_survives_separate_consuming_hook_error',
'runtime::failure_tests::normal_rpc_keeps_status_and_worker_before_leader_hooks',
'runtime::failure_tests::failure_precedes_ready_callback_and_keeps_child_start_gate_closed',
'executor::tests::failed_child_cleanup_cancels_all_gates_before_joining']
old=[
'runtime::tests::handler_suspension_releases_registered_child_start',
'runtime::tests::handler_runtime_error_precedes_and_preserves_unstarted_child_gate',
'runtime::tests::erestartsys_requests_a_restart_and_every_other_result_is_returned',
'runtime::tests::converts_linux_error_results',
'runtime::tests::unresolved_children_refuse_every_nonreturning_injection_before_mutation',
'runtime::tests::signal_hook_tail_injection_is_rejected_before_every_executor_side_effect',
'runtime::tests::signal_hook_ordinary_nonreturning_injection_is_rejected_before_side_effects',
'runtime::terminal_tests::terminal_cancellation_and_into_guest_forwarding_do_not_inject_or_start_children',
'runtime::terminal_tests::terminal_exit_retires_only_current_identity_and_preserves_existing_status',
'vm::tests::completed_failure_interrupts_a_later_natural_join',
'vm::tests::later_failure_interrupts_an_existing_natural_join',
'vm::tests::failure_cancels_pending_start_in_batch_being_joined',
'vm::tests::cancelling_named_tool_thread_joins_only_that_unstarted_worker',
'executor::tests::forked_process_waits_for_parent_registration_gate',
'executor::tests::exiting_workers_transfer_every_child_handle_in_virtual_order',
'executor::tests::child_process_cleanup_joins_every_handle_and_preserves_each_error',
'executor::tests::child_start_gate_resolves_once_and_preserves_failed_delivery',
'executor::tests::discarding_named_fork_joins_only_that_unstarted_child',
'executor::tests::discarding_unstarted_fork_propagates_the_child_result']
(out/'selected-tests.json').write_text(json.dumps({'new':new,'existing':old,'selected':sorted(new+old)},indent=2)+'\n')
binding={'base':base,'base_tree':subprocess.check_output(['git','rev-parse','HEAD^{tree}'],text=True).strip(),'files':entries,'source_file_count':len(manifest),'patch_sha256':h(patch),'patch_bytes':len(patch),'new_tests':len(new),'existing_selected':len(old),'selected_count':len(new+old),'validation':'Source preparation only; rustfmt and git diff --check. No build, test, guest, commit, or public operation.'}
(out/'binding.json').write_text(json.dumps(binding,indent=2)+'\n');print(json.dumps({'binding':str(out/'binding.json'),'binding_sha256':h((out/'binding.json').read_bytes()),**binding},indent=2))
