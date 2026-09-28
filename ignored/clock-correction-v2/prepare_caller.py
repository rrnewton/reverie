#!/usr/bin/python3
"""Prepare a fresh exact finite caller, without metadata/build/test/lease admission."""
import difflib,hashlib,json,os,stat,sys
from pathlib import Path
W=Path(__file__).resolve().parent;Q=W/'qualification-v1';L=W.parent.parent;R=W/'source'
C=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-replay-prerequisites-20260918/ignored/kvm-clock-origin-correction-source-v1')
sys.path.insert(0,str(Q))
from common import check_file,file_record,json_read,require,write_new
manifest=json_read(W/'SOURCE-MANIFEST.json')
for row in manifest:
 p=R/row['path']
 if row['mode']=='120000':require(p.is_symlink() and hashlib.sha256(os.fsencode(os.readlink(p))).hexdigest()==row['sha256'],'source alias drift')
 else:require(file_record(p)['sha256']==row['sha256'] and bool(p.stat().st_mode&0o111)==(row['mode']=='100755'),'source drift')
base_context=json_read(C/'q-corrected/CONTEXT.json')
external=[row for row in base_context['inputs'] if not Path(row['path']).is_relative_to(C)]
for row in external:check_file(row)
for name in ['/usr/bin/ldd','/usr/bin/readelf']:require(sum(x['path']==name for x in external)==1,'missing explicit loader query tool')
aliases=base_context['input_symlinks']
for row in aliases:require(Path(row['path']).is_symlink() and os.readlink(row['path'])==row['target'],'external alias drift')
target=W/'private-target';target.mkdir(mode=0o700);s=target.stat()
lease=Q/'lane.lease';fd=os.open(lease,os.O_CREAT|os.O_EXCL|os.O_WRONLY,0o600);os.close(fd);ls=lease.stat()
lease_binding=dict(path=str(lease),owner_slot=str(L),identity=[ls.st_dev,ls.st_ino,ls.st_uid],state_directory=str(Q/'lease-state'))
setup=json_read(C/'q-corrected/SETUP.json');setup.update(owner_slot=str(L),source_root=str(R),composed_sources=[str(R)],reverie_root=str(R),source_bases={'reverie':'3d4a401ed8959befad0b2f10db59093725f27432'},status='Fresh isolated clock/injection successor; no execution')
write_new(Q/'SETUP.json',setup)
history=dict(previous_failed_elf=file_record(C/'corrected-component-continuation-v1/retained-binaries/ptrace.elf'),prior_clock_patch=file_record(C/'SOURCE.patch'),prior_clock_source=file_record(C/'source/reverie-ptrace/src/clock_origin_tests.rs'),observation_target=file_record(C/'clock-boundary-observation-v2/final-v1/TARGET.json'),observation_report=file_record(C/'clock-boundary-observation-v2/final-v1/REPORT.md'),observation_proposal=file_record(C/'clock-boundary-observation-v2/final-v1/PROPOSAL.md'),observation_result=file_record(C/'clock-boundary-observation-v2/final-v1/RESULTS.json'),observation_readback=file_record(C/'clock-boundary-observation-v2/final-v1/READBACK.json'),original_failed_result=file_record(C/'corrected-component-continuation-v1/final-v1/RESULTS.json'),baseline_failed_result=file_record(C/'baseline-component-continuation-v1/final-v1/RESULTS.json'),old_metadata_terminal_proof=file_record(C/'metadata-terminal-diagnosis-v1/final-v1/READBACK.json'),first_debugger_refusal=file_record(C/'clock-boundary-observation-v1/final-v1/RESULTS.json'),old_unresolved_lease_not_accessed=True,new_source_and_target_independent=True)
write_new(Q/'HISTORY.json',history)
oldphases=json_read(C/'corrected-component-continuation-v1/PHASES.json')['phases']
limits_compile=next(p['limits'] for p in oldphases if p['name']=='compile');limits_short=next(p['limits'] for p in oldphases if p['name']=='format')
oldtests=[p['selected'][0] for p in oldphases if p['kind']=='test']
newtests=[('post_exec_injections_leave_entry_memory_untouched',['PostExec']),('seccomp_replacement_suppresses_original_write_once',['Replace']),('same_reinjection_then_private_injection_does_not_step_guest',['SameThenPrivate']),('same_tail_injection_preserves_original_kernel_call',['TailSame']),('different_tail_injection_replaces_original_once',['TailOther']),('post_exec_tail_exit_leaves_entry_unexecuted',['PostExecTail']),('emulated_return_consumes_stop_before_instruction_callback',['Emulate','EmulateErrno'])]
neighbors=['tracer::tests::'+n for n in ['pre_ready_exec_skip_accepts_exact_kernel_breakpoint_transition','pre_ready_nested_signal_is_rejected_during_context_none_reinjection','pre_ready_external_sigtrap_is_rejected_during_injected_syscall_step','pre_ready_mutated_private_stub_cannot_impersonate_injected_syscall_completion','pre_ready_nested_signal_is_rejected_while_skipping_seccomp_syscall','pre_ready_nested_signal_is_rejected_during_tracee_preinit','pre_ready_external_sigtrap_is_rejected_after_exec_event','normal_preinit_resume_clears_held_root_stop_lease','normal_liteinst_completion_leaves_no_stale_root_stop_lease','cancellation_at_generic_syscall_handler_reaps_root','cancellation_at_signal_handler_reaps_root','child_death_before_initialization_error_does_not_hang','cancelling_at_new_child_event_reaps_root_and_child']]
neighbors+=['task::tests::'+n for n in ['syscall_skip_breakpoint_requires_exact_captured_provenance','exec_generation_replaces_image_state_without_changing_old_holders','liteinst_rejects_stack_pointer_updates_without_weakening_shared_frame','liteinst_helper_clears_only_abi_sensitive_transient_flags','active_hook_footprint_rejects_destructive_mapping_overlap','short_successful_mapping_invalidates_the_whole_attempted_page','cancellable_returns_a_completed_result','cancellable_observes_cancellation_in_the_same_poll']]
all_lib=oldtests+['tracer::injection_stop_tests::'+n for n,_ in newtests]+neighbors
require(len(all_lib)==len(set(all_lib)),'duplicate selection')
# Confirm source declarations exist now; the actual fresh inventory must still authenticate exact full names before tests.
for name in all_lib:
 module=name.split('::')[0];file='clock_origin_tests.rs' if '::clock_origin_tests::' in name else 'injection_stop_tests.rs' if '::injection_stop_tests::' in name else module+'.rs'
 require('fn '+name.split('::')[-1]+'(' in (R/'reverie-ptrace/src'/file).read_text(),'selected declaration absent: '+name)
tool=Path(base_context['environment']['RUSTC']).parent;cargo=str(tool/'cargo')
selectors=[dict(id='ptrace',target='reverie_ptrace',kind=['lib'],manifest_path=str(R/'reverie-ptrace/Cargo.toml'),manifest_relative='reverie-ptrace/Cargo.toml',test_profile=True),dict(id='vsyscall',target='vsyscall',kind=['test'],manifest_path=str(R/'reverie-ptrace/Cargo.toml'),manifest_relative='reverie-ptrace/Cargo.toml',test_profile=True)]
phases=[]
def phase(name,kind,argv,selected=None,artifact_selectors=None,limits=None,**extra):phases.append(dict(name=name,kind=kind,argv=argv,selected=selected or [],artifact_selectors=artifact_selectors or [],limits=limits or limits_short,**extra))
phase('metadata','metadata',[cargo,'metadata','--offline','--locked','--format-version','1'],limits=base_context['limits'])
phase('compile','compile',[cargo,'test','--offline','--locked','-p','reverie-ptrace','--lib','--test','vsyscall','--no-run','--message-format=json'],artifact_selectors=selectors,limits=limits_compile)
phase('format','format',[str(tool/'rustfmt'),'--check','--edition','2024','--config','skip_children=true',*['reverie-ptrace/src/'+n for n in ['perf.rs','timer.rs','task.rs','tracer.rs','clock_origin_tests.rs','injection_stop_tests.rs']]])
for key,selected in [('ptrace',all_lib),('vsyscall',['suppressed_legacy_vsyscall_returns_to_caller'])]:phase('list-'+key,'list',[str(Q/'retained-binaries'/(key+'.elf')),'--list','-Z','unstable-options','--format=json'],selected=selected)
phase('core-check','check',[cargo,'check','--offline','--locked','-p','reverie-core','-p','reverie-ptrace','--message-format=json'],limits=limits_compile)
phase('clippy','check',[cargo,'clippy','--offline','--locked','-p','reverie-ptrace','--lib','--tests','--message-format=json','--','-D','warnings'],limits=limits_compile)
markers={'tracer::injection_stop_tests::'+name:['INJECTION_STOP_COMPLETE '+m for m in modes] for name,modes in newtests}
names=all_lib+['suppressed_legacy_vsyscall_returns_to_caller']
for i,name in enumerate(names,1):
 key='vsyscall' if name=='suppressed_legacy_vsyscall_returns_to_caller' else 'ptrace'
 phase('test-'+str(i).zfill(2),'test',[str(Q/'retained-binaries'/(key+'.elf')),name,'--exact','--test-threads=1','--nocapture','-Z','unstable-options','--format=json'],selected=[name],harness=key,required_markers=markers.get(name,[]))
write_new(Q/'PHASES.json',dict(phases=phases,planned_phases=len(phases),planned_tests=len(names),component_only=True))
write_new(Q/'SELECTORS.json',dict(exact_declarations=len(names),library_declarations=len(all_lib),integration_declarations=1,ordered_names=names,all_nine_previous_declarations_retained=True,new_stop_declarations=7,new_stop_modes=8,legacy_mapping_required_no_early_return_credit=True))
env=dict(base_context['environment']);env.update(CARGO_TARGET_DIR=str(target),TMPDIR=str(Q/'tmp'),XDG_CACHE_HOME=str(Q/'cache'),GIT_CEILING_DIRECTORIES=str(W))
absent=[]
for parent in [R,*R.parents]:
 for name in ['config','config.toml']:
  path=parent/'.cargo'/name
  if path==Path('/home/newton/.cargo/config.toml'):continue
  require(not path.exists() and not path.is_symlink(),'unexpected Cargo config '+str(path));absent.append(str(path))
helper_names=['phase.py','common.py','cache_lease.py','observer/observer.py','observer/before_exec.py','observer/unit_reference.py','observer/source-inputs.json']
helper_rows=[]
for name in helper_names:
 old=C/'q-corrected'/name;new=Q/name;require(old.read_bytes()==new.read_bytes(),'runtime helper altered')
 helper_rows.append(dict(before=file_record(old),after=file_record(new),identical=True))
write_new(Q/'HELPER-CONTINUITY.json',dict(runtime_helpers=helper_rows,observer_algorithm_unchanged=True,phase_parser_unchanged=True,bounds_unchanged=True,lease_algorithm_unchanged=True))
inputs=external+[file_record(Q/n) for n in [*helper_names,'SETUP.json','HISTORY.json','PHASES.json','SELECTORS.json','HELPER-CONTINUITY.json','prepare.py','execute.py','retain_artifacts.py','bind_dependencies.py']]+[file_record(W/n) for n in ['SOURCE.patch','INJECTION-CORRECTION.patch','ORACLE-CHANGES.patch','SOURCE-MANIFEST.json','BASE-MANIFEST.json','SOURCE-ORIGIN.json','CONTINUITY.json','OWNER-SCM.json','PREPARATION-LIMITS.json','prepare_caller.py']]+[file_record(R/'Cargo.lock')]
inputs+=list(history[k] for k in history if isinstance(history[k],dict) and 'sha256' in history[k])
by_path={}
for row in inputs:require(row['path'] not in by_path or by_path[row['path']]==row,'conflicting caller input');by_path[row['path']]=row
inputs=list(by_path.values())
for row in inputs:check_file(row)
context=dict(sources=[dict(root=str(R),manifest=file_record(W/'SOURCE-MANIFEST.json'))],environment=env,scm=json_read(W/'OWNER-SCM.json')['state'],inputs=inputs,input_symlinks=aliases,absent_inputs=absent,target=dict(path=str(target),identity=[s.st_dev,s.st_ino,s.st_uid]),lease=lease_binding,limits=base_context['limits'])
write_new(Q/'CONTEXT.json',context)
plan=dict(context);plan['inputs']=[*inputs,file_record(Q/'CONTEXT.json')];row=phases[0]
plan.update(path=str(Q/'metadata-plan.json'),name='metadata',kind='metadata',argv=row['argv'],selected=[],artifact_selectors=[],output=str(Q/'observer/metadata'),control=str(Q/'controls/metadata'),scope='Fresh isolated clock/injection successor metadata; no result transfer, no test or parity credit')
write_new(Q/'metadata-plan.json',plan)
changes=[]
for old,new in [(C/'corrected-component-continuation-v1/bind_dependencies.py',Q/'bind_dependencies.py'),(C/'corrected-component-continuation-v1/retain_artifacts.py',Q/'retain_artifacts.py'),(C/'corrected-component-continuation-v1/prepare.py',Q/'prepare.py'),(C/'corrected-component-continuation-v1/execute.py',Q/'execute.py')]:
 changes.extend(difflib.unified_diff(old.read_text().splitlines(True),new.read_text().splitlines(True),fromfile='previous/'+old.name,tofile='current/'+new.name))
(Q/'CALLER-DELTA.patch').open('x').writelines(changes)
print(json.dumps(dict(planned_phases=len(phases),planned_tests=len(names),lib=len(all_lib),integration=1,metadata=file_record(Q/'metadata-plan.json'),private_target=context['target'],private_lease=lease_binding)))
