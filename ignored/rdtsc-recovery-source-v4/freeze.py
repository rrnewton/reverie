from pathlib import Path
import ast,copy,difflib,hashlib,json,os,re,shutil,collections
N=Path(__file__).resolve().parent; P=N.parent/'rdtsc-recovery-source-v3'; R=N.parents[1]
D=N.parent/'rdtsc-host-worker-diagnostic-v2'; Q=N/'qualification-v1'
BASE=N.parent/'publication-fd-composition-v3/source'; CHANGED='reverie-kvm/tests/static_elf.rs'
NAME='host_owned_timestamp_worker_keeps_native_execution'
def rec(p):
 p=Path(p);st=p.lstat();b=os.readlink(p).encode() if p.is_symlink() else p.read_bytes()
 r=dict(path=str(p),bytes=len(b),sha256=hashlib.sha256(b).hexdigest(),mode=st.st_mode&0o7777)
 if p.is_symlink():r.update(kind='symlink',target=os.readlink(p))
 return r
def write(n,v):
 p=N/n;p.parent.mkdir(parents=True,exist_ok=True)
 with p.open('x') as f:f.write(v if isinstance(v,str) else json.dumps(v,indent=2)+'\n')
def delta(a,b,p,old='v3',new='v4'):
 return ''.join(difflib.unified_diff(a.splitlines(True),b.splitlines(True),fromfile=old+'/'+p,tofile=new+'/'+p))
before=(P/'source'/CHANGED).read_text();after=(N/'source'/CHANGED).read_text()
start=before.index('#[test]\nfn '+NAME+'()');end=before.index('\nfn prefixed_timestamp_register_program',start)
newstart=after.index('fn host_owned_timestamp_program()');newend=after.index('\nfn prefixed_timestamp_register_program',newstart)
assert after[:newstart]+before[start:end]+after[newend:]==before
assert (N/'OLD-PTHREAD-FIXTURE.rs').read_text()==before[start:end]
(N/'NEW-HOST-FIXTURE.rs').write_text(after[newstart:newend])
oldassert=before[before.index('    let (log, status, stdout, stderr)',start):end]
assert after.count(oldassert)==1
write('DELTA.patch',delta(before,after,CHANGED))
source=[];changed=[]
for row in json.loads((P/'SOURCE-MANIFEST.json').read_text()):
 new=copy.deepcopy(row);new['path']=str(N/'source'/row['relative']);path=Path(new['path'])
 if row['kind']=='unexpanded_gitlink':assert path.is_dir() and not list(path.iterdir())
 else:
  old=rec(P/'source'/row['relative']);now=rec(path);assert old['sha256']==row['sha256'] and old['mode']==now['mode']
  if now['sha256']!=row['sha256']:
   assert row['relative']==CHANGED;changed.append(dict(relative=CHANGED,before=old,after=now));new.update(sha256=now['sha256'],bytes=now['bytes'])
 source.append(new)
assert len(source)==2622 and len(changed)==1
write('SOURCE-MANIFEST.json',source)
setup=json.loads((P/'qualification-v1/SETUP.json').read_text());paths=[r['relative'] for r in setup['source_files']]
write('SOURCE.patch',''.join(delta((BASE/p).read_text() if (BASE/p).exists() else '',(N/'source'/p).read_text(),p,'a','b') for p in paths))
body_records=[]
for m in re.finditer(r'^#\[test\]\nfn (\w+)\(',before,re.M):
 name=m.group(1);rest=before[m.start():];finish=re.search(r'^}\n',rest,re.M);assert finish
 body=rest[:finish.end()]
 if name!=NAME:assert after.count(body)==1,name
 body_records.append(dict(name=name,sha256=hashlib.sha256(body.encode()).hexdigest(),bytes=len(body.encode()),unchanged=name!=NAME))
write('TEST-BODY-CONTINUITY.json',dict(predecessor_tests=body_records,replaced_new_unlanded_fixture=NAME,unchanged_test_bodies=sum(r['unchanged'] for r in body_records),all_other_file_text_identical=True,exact_log_status_and_output_assertions_unchanged=True,production_changed=[]))
write('SOURCE-CONTINUITY.json',dict(predecessor=rec(P/'TARGET.json'),source_entries=len(source),changed=changed,production_unchanged=True,all_other_bytes_modes_and_links_unchanged=True,recovering_original_fixture_exactly_recovers_whole_v3_test_file=True))
Q.mkdir()
for name in ['prepare.py','phase.py','common.py','cache_lease.py','bind_dependencies.py','admit_target.py','retain_artifacts.py','SELECTORS.json','toolchain-standard-inputs.json','PREDECESSOR-ELFS.json']:
 shutil.copyfile(P/'qualification-v1'/name,Q/name)
(Q/'observer').mkdir()
for p in (P/'qualification-v1/observer').iterdir():
 if p.is_file() and (p.suffix=='.py' or p.name=='source-inputs.json'):shutil.copyfile(p,Q/'observer'/p.name)
caller=''
for name,count in [('prepare.py',2),('admit_target.py',1)]:
 p=Q/name;old=p.read_text();assert old.count('rdtsc-recovery-v3-cold-v1')==count
 new=old.replace('rdtsc-recovery-v3-cold-v1','rdtsc-recovery-v4-cold-v1');p.write_text(new);caller+=delta(old,new,'qualification-v1/'+name)
old=(Q/'SELECTORS.json').read_text();selection=json.loads(old)
selection['groups']['timestamp-14']['origin']='V4 replacement of new unlanded fixture after actual RIP attribution'
selection['groups']['timestamp-14']['purpose']='Loader-free static clone/CLEARTID: both root RDTSC results exact, actual worker RDTSC followed by its TID/completion marker and real exit, exactly two root callbacks and zero worker callbacks; no count widening or callback filtering.'
(Q/'SELECTORS.json').write_text(json.dumps(selection,indent=2)+'\n');caller+=delta(old,(Q/'SELECTORS.json').read_text(),'qualification-v1/SELECTORS.json')
write('CALLER-DELTA.patch',caller)
shutil.copyfile(P/'RUN-ORDER.json',N/'RUN-ORDER.json');shutil.copyfile(P/'execute_phases.py',N/'execute_phases.py')
assert len(json.loads((N/'RUN-ORDER.json').read_text())['phases'])==40 and selection['exact_declarations']==31
setup['source_root']=str(N/'source');setup['target']=str(R/'target/rdtsc-recovery-v4-cold-v1')
for row in setup['source_files']:row['file']={k:v for k,v in rec(N/'source'/row['relative']).items() if k in ['path','bytes','mode','sha256']}
setup['lock']={k:v for k,v in rec(N/'source/Cargo.lock').items() if k in ['path','bytes','mode','sha256']}
write('qualification-v1/SETUP.json',setup)
manifest=json.loads((P/'qualification-v1/source-manifest.json').read_text())
for row in manifest:
 if row['path']==CHANGED:row['sha256']=hashlib.sha256(after.encode()).hexdigest()
write('qualification-v1/source-manifest.json',manifest)
origins=[]
for p in sorted(Q.rglob('*.py')):
 ast.parse(p.read_text());old=P/'qualification-v1'/p.relative_to(Q)
 origins.append(dict(before=rec(old),after=rec(p),byte_identical=old.read_bytes()==p.read_bytes()))
write('qualification-v1/RUNNER_ORIGINS.json',dict(records=origins,meaning='Only fresh target spelling changes in helpers; one selector descriptive metadata update; no executable scope, count, ordering, guards or bounds change.'))
controls=[]
for phase,group in selection['groups'].items():
 text=(N/'source'/group['source']).read_text();leaf=group['names'][0].rsplit('::',1)[-1];matches=list(re.finditer(r'\bfn '+re.escape(leaf)+r'\(',text));assert len(matches)==1
 controls.append(dict(phase=phase,artifact=group['artifact'],name=group['names'][0],source=rec(N/'source'/group['source']),line=text[:matches[0].start()].count('\n')+1,origin=group['origin'],purpose=group['purpose'],executed=False,cpu_seconds=30,wall_seconds=60))
write('CONTROLS.json',dict(declarations=len(controls),controls=controls,actual_inventory_pending=True))
write('SYMBOL-AUDIT.md','''# Source preparation symbol audit

Only static_elf.rs changes. The new local helper uses existing Vec, u64/i32 conversion/byte methods and existing test helpers append_jne_failure, patch_stats_jump, append_stats_exit and static_elf. Existing libc dependency supplies the same CLONE_* flags already used by clone_thread_program and the Linux EAGAIN/EINTR constants already used elsewhere in the workspace. No new crate, import, API or feature is used. LOAD_ADDRESS, MEMORY_SIZE and RDTSC_SENTINEL already exist in this file. Existing TimestampTool, TimestampLog and the complete final assertions are reused unchanged. The dated rustfmt was used only to format this new isolated source during author preparation; there was no Cargo, compiler, test, guest, target admission or lease action.

The child-TID/result/done words are distinct addresses at LOAD_ADDRESS+0x1800/+0x1808/+0x1810; the child stack is +0x1900 through +0x1f00. This is the existing static ELF's separate zero-filled data page, with the same clone3/CLEARTID layout used by neighboring controls. An author assertion prevents appended code/clone_args from extending to that page. Every relative branch is patched through the existing signed displacement helper; appended clone_args is 8-byte aligned and its pointer is patched before execution can reach it. A negative clone result branches to exit_group(1). The parent only accepts wake 0, EAGAIN or EINTR and rechecks the actual shared TID; every other futex return fails. The worker writes its returned TID and 0x1234 only after its real RDTSC, then calls SYS_exit. The parent requires both exact values after observing clear-TID zero and checks the second full timestamp value. There is no arbitrary delay, host scheduling tolerance or callback filtering.
''')
write('REPORT.md','''# V4: loader-free Host-worker timestamp ownership control

Author preparation only: no V4 compiler, test, guest, inventory or source-review result is claimed. All production bytes are identical to V3 (and V2). Only the new unlanded host_owned_timestamp_worker_keeps_native_execution fixture changes, with one private bytecode helper. Replacing that region with its retained old body exactly recovers the entire V3 static_elf.rs file. Every existing pthread/join test and every other timestamp/neighbor control remains byte-identical. The final status, empty-output and exact two-root-callback assertions are byte-identical.

V3's pthread fixture actually failed with twelve root callbacks. The separately authorized diagnostic retained the unchanged count assertion and failed again raw 101. Actual saved RIPs, actual GCC ELF/interpreter bytes and disassembly prove ten interpreter _dl_start/dl_main instructions followed by the two explicit main instructions. The worker executed RDTSC at 0x4011f0 and returned the joined argument with no callback at that RIP. Those failures and all twelve records remain bound evidence. There is no change to expected twelve, >=2, log filtering or claim that the old test passed.

The replacement tests the intended consumer ownership boundary without a loader. The root's first actual RDTSC must return both exact sentinel words. A real clone3 creates a Host-owned worker with the existing CLONE_THREAD/shared-memory/CLEARTID flags. The root records the returned child TID and uses actual FUTEX_WAIT/clear-TID completion. The worker unconditionally executes RDTSC, then records its actual gettid result and completion marker 0x1234, and performs SYS_exit. The root cannot pass until the child TID has cleared and both marker and TID match. The second root RDTSC must again return both exact words. The complete global callback vector must still be exactly [(Pid(1), Tsc), (Pid(1), Tsc)]; a worker callback remains an extra element and fails. A trap without a consumer, clone/exit failure, missing worker execution/completion or wrong Tool result cannot satisfy these checks.

The actual glibc clone3/pthread-style FUTEX_WAIT/CLEARTID neighbor remains timestamp-22 unchanged, including all four dispatch modes. Existing pthread tests elsewhere are preserved rather than replaced. This correction replaces only our unlanded C fixture whose assumed root instruction census was disproved by actual measurement. No accepted product behavior is narrowed, no skip or tolerance is added, and no limits/comparator/gate change.

The exact 31-selector/40-phase qualification plan is unchanged. A new empty target and fresh source-bound ELFs are required; helper changes are only the target spelling, and selector-14's descriptive provenance/purpose is updated honestly. V1 warning refusal, V2 fault-helper failure, unchanged predecessor fault failure, V3 timestamp-14 failure, diagnostic-v1 compile refusal and diagnostic-v2 count failure remain historical failures. V3's thirteen passes and H39ac's separate successful build are not V4 test credit. Architectural native probes and the full same-run portable comparator remain separate evidence; no parity claim follows from this control.
''')
write('PLAN.md',f'''# V4 finite qualification plan — not launched

Candidate {N/'source'}; proposed new empty target {setup['target']}. No cache or lease has been touched. Root must inspect this exact source/caller successor before execution. Follow original CALLER-PLAN and unchanged RUN-ORDER: 40 phases, 31 exact declarations. Explicit PYTHONOPTIMIZE=0 and optimize=0/debug=true for assertion-using Python drivers. Ordinary new target admission, existing owned lease, metadata, fresh dependency closure, compile, fresh emitted ELF retention, then remaining phases. Stop on the first unaccepted phase; retain its actual raw result and all earlier attempts.

Original two-job offline/locked toolchain, 600 CPU/900 wall metadata/compile/check, 30 CPU/60 wall other phases, 16 GiB/no swap, 16 MiB stderr/64 MiB stdout/16 MiB read bound and 100 GiB free floor remain unchanged. Require fresh=false on all four harnesses and the linked KVM library; run retained immutable distinct-inode copies only. Actual inventories remain separate from the 31 selected declarations. Read every selected raw stdout/stderr for any KVM skip or unexecuted capability, including historical timestamp-19. Libtest ok alone is insufficient.

The static ownership fixture must succeed with exactly two root callbacks, real worker RDTSC completion, matching child TID, marker 0x1234, clear-TID completion and exact root timestamp words. No replacement expected count, ignored branch or filtered record. Existing timestamp-22 remains the full unchanged glibc clone3/futex/clear-TID neighbor. Every other selected prefix/register/fault/clock/lifecycle/cleanup/vmcall control remains in its prior relative position. The V3 failed suite and diagnostic failures are not relabelled. No new H39ac build is required for this test-only delta; its production blob continuity must be stated if its already-built binary is used by the separately authorized official runner.
''')
prior=[P/'qualification-stop-v1/READBACK.json',D/'result-v1/READBACK.json',D.parent/'rdtsc-host-worker-diagnostic-v1/compile-refusal-v1/READBACK.json']
write('TARGET.json',dict(status='FROZEN V4 SOURCE/CALLER PREPARATION ONLY; UNCOMPILED AND UNRUN',source=str(N/'source'),authorship_base='79516661bf82d30ab2967c71834a6d47447b76ee',landed_tree_equivalent_base='44fcb1955f44547f50d724fe8f7d718215fed446',base_tree='7620fe83f486d665d9d09d4f09f0e93636b862e4',predecessor=rec(P/'TARGET.json'),source_manifest=rec(N/'SOURCE-MANIFEST.json'),source_patch=rec(N/'SOURCE.patch'),delta=rec(N/'DELTA.patch'),caller_delta=rec(N/'CALLER-DELTA.patch'),source_continuity=rec(N/'SOURCE-CONTINUITY.json'),test_continuity=rec(N/'TEST-BODY-CONTINUITY.json'),setup=rec(Q/'SETUP.json'),selectors=rec(Q/'SELECTORS.json'),observer=rec(Q/'observer/observer.py'),prior_refusals=[rec(p) for p in prior],declarations=31,phases=40,production_unchanged=True,execution_authorized=False))
records={}
def add(p):
 p=Path(p)
 if p.is_file() or p.is_symlink():records[str(p)]=rec(p)
for p in N.rglob('*'):add(p)
for p in [P/'TARGET.json',P/'INPUTS.json',P/'READBACK.json',P/'PLAN.md',P/'SOURCE-MANIFEST.json',N.parent/'rdtsc-recovery-source-v1/CALLER-PLAN.md']+prior:add(p)
for index in [P/'qualification-stop-v1/INPUTS.json',D/'result-v1/INPUTS.json']:
 add(index)
 for row in json.loads(index.read_text())['records']:
  now=rec(row['path']);assert now['sha256']==row['sha256'];add(row['path'])
write('INPUTS.json',dict(records=list(records.values()),source_entries=len(source),status='SOURCE AND CALLER PREPARATION ONLY'))
for row in records.values():assert rec(row['path'])==row
write('READBACK.json',dict(target=rec(N/'TARGET.json'),inputs=rec(N/'INPUTS.json'),source_entries=len(source),records=len(records),exact_changed_paths=[CHANGED],production_unchanged=True,all_other_test_bodies_identical=True,final_assertions_identical=True,selectors_and_order_unchanged=True,all_original_limits_unchanged=True,no_execution=True))
print(json.dumps({n:rec(N/n) for n in ['TARGET.json','SOURCE.patch','DELTA.patch','CALLER-DELTA.patch','REPORT.md','PLAN.md','INPUTS.json','READBACK.json']},indent=2))
