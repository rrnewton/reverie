from pathlib import Path
import ast,copy,difflib,hashlib,json,os,re,shutil
N=Path(__file__).resolve().parent;P=N.parent/'rdtsc-recovery-source-v4';R=N.parents[1];Q=N/'qualification-v1'
BASE=N.parent/'publication-fd-composition-v3/source'
S=R.parent/'kvm-parent-reader-support-20260916'
H=R.parent/'kvm-replay-prerequisites-20260918/ignored/rdtsc-h39ac-same-run-v3/composition/hermit'
NEW='reverie-kvm/tests/support/timestamp_terminal.rs'
CHANGED=['reverie-kvm/src/runtime.rs','reverie-kvm/src/vm.rs','reverie-kvm/tests/static_elf.rs',NEW]
def rec(p):
 p=Path(p);st=p.lstat();h=hashlib.sha256()
 if p.is_symlink():b=os.fsencode(os.readlink(p));h.update(b);size=len(b)
 else:
  size=st.st_size
  with p.open('rb') as f:
   for b in iter(lambda:f.read(1024*1024),b''):h.update(b)
 row=dict(path=str(p),bytes=size,sha256=h.hexdigest(),mode=st.st_mode&0o7777)
 if p.is_symlink():row.update(kind='symlink',target=os.readlink(p))
 return row
def write(name,value):
 p=N/name;p.parent.mkdir(parents=True,exist_ok=True)
 with p.open('x') as f:f.write(value if isinstance(value,str) else json.dumps(value,indent=2)+'\n')
def delta(before,after,path,a='v4',b='v5'):
 return ''.join(difflib.unified_diff(before.splitlines(True),after.splitlines(True),fromfile=a+'/'+path,tofile=b+'/'+path))
source=[];changed=[]
for row in json.loads((P/'SOURCE-MANIFEST.json').read_text()):
 new=copy.deepcopy(row);new['path']=str(N/'source'/row['relative']);p=Path(new['path'])
 if row['kind']=='unexpanded_gitlink':assert p.is_dir() and not list(p.iterdir())
 else:
  old=rec(P/'source'/row['relative']);now=rec(p);assert old['sha256']==row['sha256'] and old['mode']==now['mode']
  if now['sha256']!=row['sha256']:
   assert row['relative'] in CHANGED;changed.append(dict(relative=row['relative'],before=old,after=now));new.update(sha256=now['sha256'],bytes=now['bytes'],changed_from_baseline=True)
 source.append(new)
assert len(source)==2622 and {r['relative'] for r in changed}==set(CHANGED[:-1])
r=rec(N/'source'/NEW);source.append(dict(relative=NEW,path=r['path'],mode='100644',kind='file',bytes=r['bytes'],sha256=r['sha256'],file_mode=r['mode'],baseline_blob_sha1=None,changed_from_baseline=True))
source.sort(key=lambda r:r['relative']);write('SOURCE-MANIFEST.json',source)
write('DELTA.patch',''.join(delta((P/'source'/p).read_text() if (P/'source'/p).exists() else '',(N/'source'/p).read_text(),p)for p in CHANGED))
setup=json.loads((P/'qualification-v1/SETUP.json').read_text());paths=[r['relative'] for r in setup['source_files']]+[NEW]
write('SOURCE.patch',''.join(delta((BASE/p).read_text() if (BASE/p).exists() else '',(N/'source'/p).read_text(),p,'a','b')for p in paths))
write('SOURCE-CONTINUITY.json',dict(predecessor=rec(P/'TARGET.json'),source_entries=2623,changed_existing=changed,added=rec(N/'source'/NEW),all_other_bytes_modes_links_unchanged=True,production_change_only='reverie-kvm/src/runtime.rs; vm.rs delta is cfg(test)',no_cpuid_decoder_clock_or_public_api_change=True))
# Existing test declarations retain their full bodies, except mechanical private
# signature adaptation in one signal guard unit control. ClockTimestampTool's
# helper assertion is separately disclosed, not hidden behind body continuity.
bodies=[]
for name in CHANGED[:-1]:
 before=(P/'source'/name).read_text();after=(N/'source'/name).read_text()
 for m in re.finditer(r'(?m)^(\s*)#\[test\]\n\s*fn (\w+)\(',before):
  indent=m.group(1).split('\n')[-1];rest=before[m.start():];end=re.search(r'(?m)^'+re.escape(indent)+r'}\n',rest);assert end,(name,m.group(2))
  body=rest[:end.end()];same=body in after
  assert same or m.group(2)=='signal_hook_tail_injection_is_rejected_before_every_executor_side_effect',(name,m.group(2))
  bodies.append(dict(source=name,name=m.group(2),sha256=hashlib.sha256(body.encode()).hexdigest(),unchanged=same))
write('TEST-BODY-CONTINUITY.json',dict(declarations=bodies,changed_existing_unit='signal_hook_tail_injection_is_rejected_before_every_executor_side_effect: same assertions; explicit returning Write request supplied to private method',changed_helper='ClockTimestampTool: obsolete unlanded ordinary ExitGroup refusal replaced by forbidden execve refusal; original fork/getpid/clock/result oracles retained',old_exit_contract_replaced_by='new actual ordinary and tail Exit/ExitGroup terminal status/cleanup/no-continuation controls',new_declarations=4))
Q.mkdir()
helpers=['prepare.py','phase.py','common.py','cache_lease.py','bind_dependencies.py','admit_target.py','retain_artifacts.py','SELECTORS.json','toolchain-standard-inputs.json','PREDECESSOR-ELFS.json']
for name in helpers:shutil.copyfile(P/'qualification-v1'/name,Q/name)
(Q/'observer').mkdir()
for p in (P/'qualification-v1/observer').iterdir():
 if p.is_file() and (p.suffix=='.py' or p.name=='source-inputs.json'):shutil.copyfile(p,Q/'observer'/p.name)
caller=''
for name,count in [('prepare.py',2),('admit_target.py',1)]:
 p=Q/name;old=p.read_text();assert old.count('rdtsc-recovery-v4-cold-v1')==count
 new=old.replace('rdtsc-recovery-v4-cold-v1','rdtsc-recovery-v5-cold-v1')
 if name=='prepare.py':
  new=new.replace("'reverie-kvm/tests/static_elf.rs']","'reverie-kvm/tests/static_elf.rs', 'reverie-kvm/tests/support/timestamp_terminal.rs']")
  assert new.count("'reverie-kvm/tests/support/timestamp_terminal.rs'")==1
  new=new.replace('exact 31 declarations','exact 36 declarations')
 p.write_text(new);caller+=delta(old,new,'qualification-v1/'+name)
selection=json.loads((Q/'SELECTORS.json').read_text())
selection['groups']['timestamp-13']['purpose']='Current clock/RPC and exact returning getpid/fork refusal retained; obsolete unlanded exit refusal replaced by exec refusal, with actual ordinary/tail exit coverage in timestamp-32.'
added=[('32','static','timestamp_terminal::terminal_exit_preserves_status_and_consuming_cleanup',NEW,'Five actual modes: tail Exit23/ExitGroup29, ordinary Exit23/ExitGroup29, and real EIO cleanup; exact status, one callback and both consuming hooks, no continuation.'),('33','static','timestamp_terminal::nonterminal_tails_refuse_before_side_effects',NEW,'Four real timestamp tail refusals: uname/getpid/fork/exec; exact ENOSYS/full failure shape and unchanged guest-memory marker/no child/image or resumed callback.'),('34','static','timestamp_terminal::worker_terminal_rpc_precedes_real_sibling_exit_group',NEW,'Real Tool-owned worker timestamp RPC and terminal Exit0 precede leader actual ExitGroup17; exact exits and callback count. Backend control, not Detcore clock coverage.'),('35','lib','vm::tests::non_elf_tool_loop_disarms_prior_timestamp_interception','reverie-kvm/src/vm.rs','Both actual CR4.TSD entry states, real-mode RDTSC plus real vmcall/HLT; one syscall, no timestamp callback, cleared ownership. No CPL0 interception claim.'),('36','lib','runtime::tests::signal_hook_tail_injection_is_rejected_before_every_executor_side_effect','reverie-kvm/src/runtime.rs','Existing signal-hook side-effect/refusal control with unchanged assertions; private predicate now receives exact request.')]
for number,artifact,name,path,purpose in added:selection['groups']['timestamp-'+number]=dict(artifact=artifact,names=[name],source=path,origin='V5 new actual control' if number!='36' else 'existing guard control, mechanical private-signature adaptation',purpose=purpose)
selection['exact_declarations']=36
old=(Q/'SELECTORS.json').read_text();(Q/'SELECTORS.json').write_text(json.dumps(selection,indent=2)+'\n');caller+=delta(old,(Q/'SELECTORS.json').read_text(),'qualification-v1/SELECTORS.json')
old=(Q/'PREDECESSOR-ELFS.json').read_text();binding=P/'qualification-v1/retained-binaries/BINDING.json';artifacts=json.loads((P/'qualification-v1/retained-binaries/ARTIFACTS.json').read_text());previous=dict(source=str(binding),sha256=rec(binding)['sha256'],hashes={k:v['file']['sha256']for k,v in artifacts.items()},purpose='Refuse all four actual V4 harness bytes as new V5 compiler output; no V4 test credit.')
(Q/'PREDECESSOR-ELFS.json').write_text(json.dumps(previous,indent=2)+'\n');caller+=delta(old,(Q/'PREDECESSOR-ELFS.json').read_text(),'qualification-v1/PREDECESSOR-ELFS.json')
order=json.loads((P/'RUN-ORDER.json').read_text());at=order['phases'].index('timestamp-14');order['phases'][at:at]=['timestamp-'+a[0] for a in added]
assert len(order['phases'])==45 and len(selection['groups'])==36
write('RUN-ORDER.json',order);caller+=delta((P/'RUN-ORDER.json').read_text(),(N/'RUN-ORDER.json').read_text(),'RUN-ORDER.json');write('CALLER-DELTA.patch',caller)
shutil.copyfile(P/'execute_phases.py',N/'execute_phases.py')
setup['source_root']=str(N/'source');setup['target']=str(R/'target/rdtsc-recovery-v5-cold-v1')
setup['source_files']=[dict(relative=p,file=rec(N/'source'/p))for p in paths];setup['lock']=rec(N/'source/Cargo.lock')
write('qualification-v1/SETUP.json',setup)
manifest=[dict(path=r['relative'],mode=r['mode'],**(dict(sha256=r['sha256'])if 'sha256'in r else {}))for r in source]
write('qualification-v1/source-manifest.json',manifest)
origins=[]
for p in sorted(Q.rglob('*.py')):
 ast.parse(p.read_text());old=P/'qualification-v1'/p.relative_to(Q)
 origins.append(dict(before=rec(old),after=rec(p),byte_identical=old.read_bytes()==p.read_bytes()))
write('qualification-v1/RUNNER_ORIGINS.json',dict(records=origins,meaning='Material helpers: new empty target spelling, one added format path and truthful scope count only. SELECTORS adds four declarations plus one existing guard neighbor; prior hash exclusion advances to actual V4 harnesses. Observer, bounds, lease, phase parsing and retention code unchanged.'))
controls=[]
for phase,group in selection['groups'].items():
 text=(N/'source'/group['source']).read_text();leaf=group['names'][0].rsplit('::',1)[-1];matches=list(re.finditer(r'\bfn '+re.escape(leaf)+r'\(',text));assert len(matches)==1
 controls.append(dict(phase=phase,artifact=group['artifact'],name=group['names'][0],source=rec(N/'source'/group['source']),line=text[:matches[0].start()].count('\n')+1,origin=group['origin'],purpose=group['purpose'],executed=False,cpu_seconds=30,wall_seconds=60))
write('CONTROLS.json',dict(declarations=36,new_declarations=4,additional_existing_declarations=1,controls=controls,actual_inventory_pending=True,new_guest_modes=12,mode_count_not_a_test_or_execution_count=True))
write('PLAN.md',f'''# V5 finite qualification caller — preparation only

Source {N/'source'}. Proposed new empty target {setup['target']}. No target/cache/lease admission or phase has run. Root must inspect this exact successor first. Use unchanged observer and ordinary admission/lease sequence, actual optimize=0/debug=true with PYTHONOPTIMIZE=0, metadata -> dependency binding -> compile -> fresh artifact retention -> remaining RUN-ORDER. There are 45 phases and 36 exact declarations: the original 31, four new declarations, and one existing signal-hook refusal neighbor. Original declarations retain relative order. Stop at the first unaccepted phase, preserve raw status/streams and all prior attempts. No retry, skip, warning allowance or expected failure reclassification.

Original two jobs, offline/locked toolchain, 600 CPU/900 wall metadata/compile/check and 30 CPU/60 wall other phases, 16 GiB/no swap, 16 MiB stderr/64 MiB stdout/16 MiB phase read and 100 GiB free-space floor are unchanged. The format path set adds the new module; no path is removed. Every candidate source entry, including the new module and real ignored lock, is bound by the phase source checks. Fresh target is required; Cargo fresh=false for all four emitted harnesses and linked KVM library; exact source manifest/package IDs and unchanged inputs before/after; distinct-inode retained copies and loader closure. All four V4 harness hashes are forbidden as new output. Actual inventories are determined from the new ELFs, not assumed from the added test count.

Retain all 36 selected raw stdout/stderr and separately audit no KVM skip/unexecuted messages, especially historical timestamp-19. Four new declarations represent twelve actual guest modes (five exit/cleanup, four refusal, one sibling, two public-entry); report only modes actually reached. The existing signal guard unit is not VM qualification. EIO/refusal controls must assert actual complete typed errors and mandatory unchanged effects, not treat a nonzero test process as passing.

Successful component controls cannot establish current-Hermit's before/after-charge cancellation points, full portable comparator parity, F3 CPU policy or source approval. The source-derived witness plan remains a separate not-yet-launched composition task. A new Hermit build is needed for V5 production; the retained H39ac/V3 binary is historical evidence only for this correction.
''')
write('SYMBOL-AUDIT.md','''# Preparation symbol and path audit

No new Cargo dependency or public API. SyscallInfo is Copy; SyscallRequest derives exact equality and is already used throughout runtime. The private predicate changes only its request parameter; all trait implementations and call sites were searched. Exit/ExitGroup completion uses existing executor.take_exit, request_guest_thread_group_exit and ToolProcessExit conversion. New static tests import existing parent Tool/Guest/GlobalRPC/ExitStatus/Errno/Subscription/MemoryAccess/ThreadOwnership/bytecode helpers; futures::channel::oneshot is already an available direct dependency. ToolRunCompletion is publicly reexported by reverie-kvm/src/lib.rs. All request/config/response types use existing serializable primitives and ExitStatus. No serde_json dependency or fabricated error constructor is introduced.

The sibling clone3 arguments are eleven u64 words, eight-byte aligned, flags 0x50f00, stack at LOAD_ADDRESS+0x1900 with length 0x600 within the existing static ELF data page. The child and failure branch operands are patched by the existing signed-displacement helper. Both clone failure and wrong virtual child ID take explicit failure exit_group(78). The child contains two timestamp instructions: resuming the cancelled one causes an extra callback and fails. RPC gates hold no mutex across await. Real terminal/error supervision is invoked through the retained public completion API.

The F2 control is inside cfg(test), uses actual KVM state/production setter and public run_with_tool with existing () GlobalTool. It executes a two-byte RDTSC prefix followed by install_syscall's real vmcall/HLT program, asserts exact request/count and both hardware/software ownership bits. It does not claim CPL0 traps or safe reuse of an already loaded ELF as real mode. Rustfmt from the bound nightly was used only for isolated author formatting; no Cargo/compiler/test/guest was executed.
''')
reviews=[S/'ignored/kvm-rdtsc-claude-review-v2-20260918/REVIEW.md',S/'ignored/kvm-rdtsc-native-review-v1-20260918/01-source.final.txt',S/'ignored/kvm-rdtsc-claude-review-v2-20260918/ROOT-FOLLOWUP-FACTS.md']
primaries=[H/p for p in ['detcore/src/lib.rs','detcore/src/tool_global.rs','detcore/src/syscalls/time.rs','detcore/src/syscalls/threads.rs']]
write('PRIMARY-SOURCE-INPUTS.json',dict(records=[rec(p)for p in primaries+reviews],author_analysis=True,external_reviews_remain_changes_requested=True,actual_H39ac_before_after_charge_coverage_pending=True))
write('TARGET.json',dict(status='FROZEN V5 SOURCE/CALLER PREPARATION ONLY; UNCOMPILED AND UNRUN',source=str(N/'source'),authorship_base='79516661bf82d30ab2967c71834a6d47447b76ee',landed_tree_equivalent_base='44fcb1955f44547f50d724fe8f7d718215fed446',base_tree='7620fe83f486d665d9d09d4f09f0e93636b862e4',predecessor=rec(P/'TARGET.json'),predecessor_actual_results=rec(P/'final-v1/TARGET.json'),source_manifest=rec(N/'SOURCE-MANIFEST.json'),source_patch=rec(N/'SOURCE.patch'),delta=rec(N/'DELTA.patch'),caller_delta=rec(N/'CALLER-DELTA.patch'),report=rec(N/'REPORT.md'),plan=rec(N/'PLAN.md'),setup=rec(Q/'SETUP.json'),selectors=rec(Q/'SELECTORS.json'),observer=rec(Q/'observer/observer.py'),declarations=36,phases=45,execution_authorized=False,public_api_changed=False,F3_policy_unchanged=True,Detcore_point_coverage_pending=True))
records={}
def add(p):
 p=Path(p)
 if p.is_file()or p.is_symlink():records[str(p)]=rec(p)
for p in N.rglob('*'):add(p)
for p in primaries+reviews+[P/'TARGET.json',P/'INPUTS.json',P/'READBACK.json',P/'PLAN.md',P/'SOURCE-MANIFEST.json',P/'final-v1/TARGET.json',P/'final-v1/READBACK.json',P/'qualification-result-v1/RESULTS.json',P/'qualification-result-v1/RAW-EXECUTION-AUDIT.json',N.parent/'rdtsc-review-response-design-v1/REPORT.md',N.parent/'rdtsc-review-response-design-v1/INPUTS.json',N.parent/'rdtsc-review-response-design-v1/READBACK.json',N.parent/'rdtsc-recovery-source-v1/CALLER-PLAN.md',binding]:add(p)
# Preserve the complete already-frozen prior source/failure evidence closure.
for row in json.loads((P/'INPUTS.json').read_text())['records']:
 now=rec(row['path']);assert now['sha256']==row['sha256'] and now['bytes']==row['bytes'];records[now['path']]=now
write('INPUTS.json',dict(records=list(records.values()),source_entries=2623,status='SOURCE/CALLER PREPARATION ONLY'))
for row in records.values():assert rec(row['path'])==row
write('READBACK.json',dict(target=rec(N/'TARGET.json'),inputs=rec(N/'INPUTS.json'),records=len(records),source_entries=2623,changed_paths=CHANGED,all_other_source_unchanged=True,old_test_bodies_and_failures_preserved=True,obsolete_exit_refusal_change_disclosed=True,ordinary_and_tail_exit_controls_added=True,original_limits_helpers_observer_preserved=True,no_execution=True))
print(json.dumps({p:rec(N/p)for p in ['TARGET.json','SOURCE.patch','DELTA.patch','CALLER-DELTA.patch','REPORT.md','PLAN.md','INPUTS.json','READBACK.json']},indent=2))
