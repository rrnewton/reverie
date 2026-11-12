from pathlib import Path
import ast,copy,difflib,hashlib,json,os,shutil
N=Path(__file__).resolve().parent;P=N.parent/'rdtsc-recovery-source-v1';R=N.parents[1];Q=N/'qualification-v1'
def sha(b):return hashlib.sha256(b).hexdigest()
def rec(p):
 p=Path(p);s=p.lstat()
 if p.is_symlink(): b=os.readlink(p).encode();return dict(path=str(p),kind='symlink',target=os.readlink(p),bytes=len(b),sha256=sha(b),mode=s.st_mode&0o7777)
 b=p.read_bytes();return dict(path=str(p),kind='file',bytes=len(b),sha256=sha(b),mode=s.st_mode&0o7777)
def write(n,x):
 p=N/n;p.parent.mkdir(parents=True,exist_ok=True)
 with p.open('x')as f:f.write(x if isinstance(x,str)else json.dumps(x,indent=2)+'\n')
manifest=json.loads((P/'SOURCE-MANIFEST.json').read_text())
if not (N/'source').exists():shutil.copytree(P/'source',N/'source',symlinks=True)
changed='reverie-kvm/src/timestamp.rs';f=N/'source'/changed;before=(P/'source'/changed).read_bytes();old=b'        let mut memory = GuestMemory::new(0, 0x10_000).unwrap();';new=b'        let memory = GuestMemory::new(0, 0x10_000).unwrap();';assert before.count(old)==1;after=before.replace(old,new);f.write_bytes(after)
source=[];deltas=[]
for r in manifest:
 out=copy.deepcopy(r);out['path']=str(N/'source'/r['relative']);p=Path(out['path'])
 if r['kind']=='unexpanded_gitlink':assert p.is_dir() and not list(p.iterdir());source.append(out);continue
 actual=rec(p);assert actual['mode']==rec(P/'source'/r['relative'])['mode']
 if actual['sha256']!=r['sha256']:
  assert r['relative']==changed;deltas.append(dict(path=changed,before=rec(P/'source'/changed),after=actual));out['bytes']=actual['bytes'];out['sha256']=actual['sha256']
 source.append(out)
assert len(deltas)==1
write('SOURCE-MANIFEST.json',source)
write('DELTA.patch',''.join(difflib.unified_diff(before.decode().splitlines(keepends=True),after.decode().splitlines(keepends=True),fromfile='v1/'+changed,tofile='v2/'+changed)))
full=(P/'SOURCE.patch').read_bytes();assert full.count(b'+'+old)==1;write('SOURCE.patch',full.replace(b'+'+old,b'+'+new).decode())
write('SOURCE-CONTINUITY.json',dict(base='79516661bf82d30ab2967c71834a6d47447b76ee',landed_equivalent_base='44fcb1955f44547f50d724fe8f7d718215fed446',base_tree='7620fe83f486d665d9d09d4f09f0e93636b862e4',predecessor_target=rec(P/'TARGET.json'),entries=len(source),exact_changed_entries=deltas,all_other_bytes_modes_links_unchanged=True,correction='Remove only unnecessary mut in a unit-control binding; all assertions and execution semantics unchanged. No warning allow or test skip.'))
Q.mkdir()
for name in ['prepare.py','phase.py','common.py','cache_lease.py','bind_dependencies.py','admit_target.py','retain_artifacts.py','SELECTORS.json','toolchain-standard-inputs.json','PREDECESSOR-ELFS.json']:
 shutil.copyfile(P/'qualification-v1'/name,Q/name)
(Q/'observer').mkdir()
for p in (P/'qualification-v1/observer').iterdir():
 if p.is_file() and (p.suffix=='.py' or p.name=='source-inputs.json'):shutil.copyfile(p,Q/'observer'/p.name)
caller_delta=''
for name,count in [('prepare.py',2),('admit_target.py',1)]:
 p=Q/name;t=p.read_text();assert t.count('rdtsc-recovery-v1-cold-v1')==count;t2=t.replace('rdtsc-recovery-v1-cold-v1','rdtsc-recovery-v2-cold-v1');p.write_text(t2)
 caller_delta+=''.join(difflib.unified_diff(t.splitlines(keepends=True),t2.splitlines(keepends=True),fromfile='v1/qualification-v1/'+name,tofile='v2/qualification-v1/'+name))
write('CALLER-DELTA.patch',caller_delta)
setup=json.loads((P/'qualification-v1/SETUP.json').read_text());setup['source_root']=str(N/'source');setup['target']=str(R/'target/rdtsc-recovery-v2-cold-v1')
for row in setup['source_files']:
 r=rec(N/'source'/row['relative']);row['file']={k:r[k]for k in ['path','bytes','mode','sha256']}
r=rec(N/'source/Cargo.lock');setup['lock']={k:r[k]for k in ['path','bytes','mode','sha256']}
write('qualification-v1/SETUP.json',setup)
phase_manifest=json.loads((P/'qualification-v1/source-manifest.json').read_text())
for r in phase_manifest:
 if r['path']==changed:r['sha256']=sha(after)
write('qualification-v1/source-manifest.json',phase_manifest)
origins=[]
for p in sorted(Q.rglob('*')):
 if p.is_file() and p.suffix=='.py':
  ast.parse(p.read_text());oldfile=P/'qualification-v1'/p.relative_to(Q);origins.append(dict(before=rec(oldfile),after=rec(p),byte_identical=oldfile.read_bytes()==p.read_bytes()))
write('qualification-v1/RUNNER_ORIGINS.json',dict(sources=origins,material_delta=rec(N/'CALLER-DELTA.patch'),policy='Only two target-path locations in prepare and one in admission change. No observer, phase/common/lease, bounds/selector/result or artifact-admission logic change.',executed=False))
for name in ['execute_phases.py','RUN-ORDER.json']:shutil.copyfile(P/name,N/name)
write('REPORT.md','''# Timestamp source V2: one compile-warning correction

V1 cold compile produced raw 0 in 62.031 seconds, but its zero-diagnostic qualification correctly refused the new `timestamp.rs:239` unused_mut warning. Terminal accounting was complete and source remained unchanged. No tests, format, core/ptrace check or Clippy ran. The full original receipt and four unqualified ELF copies remain under V1 `qualification-refusal-v1`; they are not V2 qualification.

V2 removes only `mut` from that unit-control local binding. The complete 2,622-entry source copy is otherwise identical, including every assertion, historical control, fixture, production byte, mode and symlink. `DELTA.patch` is the exact V1-to-V2 change; `SOURCE.patch` is the full seven-path candidate against the same 79516661 / landed-equivalent 44fcb195 base. The correction changes no production mechanism or accepted behavior.

The prepare-only caller uses a new empty target name. Its complete material delta is two target-path locations in prepare.py and one in admit_target.py. SETUP binds the new snapshot and exact new test-file hash; its phase manifest differs in that one hash. All 30 selectors, 39 phases, observer137c, phase/common/lease/dependency/artifact-retention logic and bounds are unchanged. No source timestamp manipulation, cache import, warning exemption or retry of a failed test is used. No target admission, lease claim, compilation/test or reviewer launch has occurred for V2.

Original Claude setup remains preserved and unlaunched. A successor attachment must bind this exact delta and the V1 raw-0 warning refusal, followed by actual V2 qualification. Current Hermit39ac callback context is the separate retained source context already supplied; no same-run Hermit outcome is claimed. Root is separately preparing native architecture probes. The complete 75-cell same-run comparison and its unchanged full comparator remain required for a future parity claim.
''')
write('PLAN.md',f'''# V2 finite execution plan — root inspection pending

Follow the exact V1 CALLER-PLAN, RUN-ORDER, CONTROLS and PLAN at `{P}`, with source snapshot `{N/'source'}` and new target `{setup['target']}`. All 30 names and all 39 phases are unchanged. No target is created by this preparation. Before execution root inspects SOURCE-CONTINUITY and CALLER-DELTA and authorizes this target/caller.

Use explicit `PYTHONOPTIMIZE=0 /usr/bin/python3 -B`, record optimize0/debugtrue, then admit the empty target, metadata, fresh dependency binding, compile and accepted-artifact retention, followed by remaining phases. Stop at the first unaccepted phase and retain its raw result. Build/check/metadata retain 600 aggregate CPU / 900 wall; lists/tests/format retain 30 CPU / 60 wall, existing inner wrappers, two jobs, offline/locked, strict diagnostics and actual KVM requirement. Memory, swap, stdout/stderr, disk floor, source/SCM/ELF/loader and lease checks remain unchanged. No cache or source mutations occur during preparation. R live source/head/branch/index must remain unchanged during execution.

Preserve the V1 original warning refusal and distinguish its four emitted copies from any actual V2 artifacts. New Cargo output must identify fresh=false for all four harnesses and the linked non-test KVM library; qualified execution consumes retained distinct-inode copies only. Inventory is separate from selected outcomes. No component result establishes native fault equivalence or same-run 75-cell parity. No external reviewer may launch before real results are bound and root authorizes the final packet.
''')
write('TARGET.json',dict(status='IMMUTABLE V2 SOURCE/CALLER PREPARATION; NO V2 EXECUTION',source=str(N/'source'),authorship_base='79516661bf82d30ab2967c71834a6d47447b76ee',landed_tree_equivalent_base='44fcb1955f44547f50d724fe8f7d718215fed446',base_tree='7620fe83f486d665d9d09d4f09f0e93636b862e4',predecessor=rec(P/'TARGET.json'),source_manifest=rec(N/'SOURCE-MANIFEST.json'),source_patch=rec(N/'SOURCE.patch'),v1_to_v2_delta=rec(N/'DELTA.patch'),caller_delta=rec(N/'CALLER-DELTA.patch'),setup=rec(Q/'SETUP.json'),source_continuity=rec(N/'SOURCE-CONTINUITY.json'),original_refusal=rec(P/'qualification-refusal-v1/READBACK.json'),selectors=rec(Q/'SELECTORS.json'),observer=rec(Q/'observer/observer.py'),new_target=setup['target'],limits_selectors_oracles_unchanged=True))
records=[]
for p in sorted(N.rglob('*')):
 if p.is_symlink() or p.is_file():records.append(rec(p))
# Bind retained original evidence by content without rewriting it.
for p in [P/'TARGET.json',P/'INPUTS.json',P/'READBACK.json',P/'PLAN.md',P/'CALLER-PLAN.md',P/'CONTROLS.json',P/'COMPLETE_READS.json',*sorted((P/'qualification-refusal-v1').rglob('*'))]:
 if p.is_file():records.append(rec(p))
write('INPUTS.json',dict(records=records,source_entries=len(source),preparation_only=True))
for r in records:assert rec(r['path'])==r
write('READBACK.json',dict(status='Source/caller successor frozen for root inspection; not execution authorization',records=len(records),target=rec(N/'TARGET.json'),inputs=rec(N/'INPUTS.json'),delta=rec(N/'DELTA.patch'),caller_delta=rec(N/'CALLER-DELTA.patch'),unchanged_helpers=sum(r['byte_identical']for r in origins),modified_helpers=[r['after']['path']for r in origins if not r['byte_identical']],source_entries=len(source),no_execution=True))
print(json.dumps({n:rec(N/n)for n in ['TARGET.json','SOURCE.patch','DELTA.patch','CALLER-DELTA.patch','INPUTS.json','READBACK.json']},indent=2))
