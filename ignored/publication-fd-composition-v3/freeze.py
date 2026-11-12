"""Freeze source-only packet; no builds, tests, cache/lease or SCM mutation."""
from pathlib import Path
import hashlib,json,os,re,stat,subprocess,difflib
D=Path(__file__).resolve().parent;R=D.parents[1];V2=D.parent/'publication-fd-composition-v2'
paths=['reverie-kvm/src/elf.rs','reverie-kvm/src/executor.rs','reverie-kvm/src/process_signal_publication.rs']
def record(p):
 b=p.read_bytes();return dict(path=str(p),bytes=len(b),mode=stat.S_IMODE(p.stat().st_mode),sha256=hashlib.sha256(b).hexdigest())
def put(name,value):
 with (D/name).open('x') as f:json.dump(value,f,indent=2);f.write('\n')
# Preserve the only comment refinement after the source-author formatter.
a=(D/'source-format-retained/after'/paths[0]).read_text();b=(D/'source'/paths[0]).read_text()
(D/'POST-FORMAT-COMMENT.patch').write_text(''.join(difflib.unified_diff(a.splitlines(keepends=True),b.splitlines(keepends=True),fromfile='formatted/'+paths[0],tofile='final/'+paths[0])))
assert (D/'source-format-retained/after'/paths[1]).read_bytes()==(D/'source'/paths[1]).read_bytes()
locations={}
queries={paths[0]:['pub stdin_entry_id:','fn insert_file','fn take_stdin','fn try_clone_for_fork_locked','fn inherit_process_state_locked'],paths[1]:['struct FileTableState','fn same_stdin_entry','fn update_from_elf','fn prepare_from_elf','fn install(&self','fn execute_accept','fn fork_child','fn thread_child_with_signal_observation','fn release_files_on_exit','fn replace_after_exec','fn execute(&mut self','fn close(state','fn native_loaded_state','fn inherited_stdin_','fn observe_unlocked_retirement_then','fn descriptor_retirement_install_error','fn descriptor_retirement_accept_cleanup','fn shared_file_table_install_emfile'], 'reverie-kvm/src/vm.rs':['loaded.stdin ='], 'reverie-kvm/src/runtime.rs':['loaded.stdin =']}
for rel,needles in queries.items():
 for i,line in enumerate((D/'source'/rel).read_text().splitlines(),1):
  if any(n in line for n in needles):locations.setdefault(rel,[]).append(dict(line=i,text=line.strip()))
put('SOURCE-MAP.json',locations)
# The whole old source manifest is held; only elf/executor changed in successor,
# and the baseline contains exactly the new test block in executor.
changed=[]
for row in json.loads((V2/'SOURCE-MANIFEST.json').read_text()):
 if row['kind']=='file':
  rel=row['relative']
  if (D/'source'/rel).read_bytes()!=(V2/'source'/rel).read_bytes():changed.append(rel)
  if rel!='reverie-kvm/src/executor.rs':assert (D/'baseline-source'/rel).read_bytes()==(V2/'source'/rel).read_bytes()
assert sorted(changed)==paths[:2]
continuity=json.loads((D/'LIVE-CONTINUITY.json').read_text())
env=dict(os.environ,GIT_OPTIONAL_LOCKS='0')
assert subprocess.check_output(['/usr/bin/git','-C',str(R),'rev-parse','HEAD'],env=env,timeout=30).decode().strip()==continuity['head']
assert record(Path(continuity['index']['path']))==continuity['index']
for r in continuity['live']:assert record(Path(r['path']))==r
# Index the bounded packet, including both complete source copies, all original
# helper bytes, before/after files and retained source-format records.
records=[];links=[]
for p in sorted(D.rglob('*')):
 if p.is_symlink():links.append(dict(path=str(p),target=os.readlink(p)))
 elif p.is_file():records.append(record(p))
for root,names in [(V2,['TARGET.json','SOURCE.patch','READBACK.json','CALLER_INPUTS.json']),(D.parent/'publication-fd-composition-qualification-v1/final-v1',['TARGET.json','REPORT.md','RESULTS.json','READBACK.json']),(D.parent/'publication-fd-materialization-plan-v1',['PLAN.md','READBACK.json'])]:
 for n in names:records.append(record(root/n))
put('INPUTS.json',dict(schema=1,records=records,symlinks=links,scope='Source-only successor and unchanged v2 predecessors; no successor results or review verdict'))
for r in records:assert record(Path(r['path']))==r
for r in links:assert os.readlink(r['path'])==r['target']
put('READBACK.json',dict(schema=1,inputs=record(D/'INPUTS.json'),authenticated_regular_records=len(records),authenticated_symlinks=len(links),changed_relative_to_v2=changed,source_patch=record(D/'SOURCE.patch'),successor_delta=record(D/'V2-TO-V3.patch'),baseline_test_patch=record(D/'BASELINE-TEST.patch'),live_continuity=record(D/'LIVE-CONTINUITY.json'),test_continuity=record(D/'TEST-CONTINUITY.json'),no_successor_build_test_or_guest=True,no_cache_lease_scm_network_mutation=True))
put('TARGET.json',dict(schema=1,base=continuity['head'],status='SOURCE-ONLY STDIN IDENTITY SUCCESSOR; baseline and qualification NOT EXECUTED; review/handoff pending',source_root=str(D/'source'),baseline_source_root=str(D/'baseline-source'),product_paths=paths,changed_from_v2=changed,source_patch=record(D/'SOURCE.patch'),v2_to_v3=record(D/'V2-TO-V3.patch'),baseline_patch=record(D/'BASELINE-TEST.patch'),source_manifest=record(D/'SOURCE-MANIFEST.json'),report=record(D/'REPORT.md'),ownership=record(D/'OWNERSHIP.md'),plan=record(D/'PLAN.md'),source_inputs=record(D/'SOURCE_INPUTS.json'),caller_inputs=record(D/'CALLER_INPUTS.json'),test_continuity=record(D/'TEST-CONTINUITY.json'),inputs=record(D/'INPUTS.json'),readback=record(D/'READBACK.json'),proposed_corrected_declarations=81,proposed_lib_declarations=72,proposed_static_declarations=9,proposed_other_corrected_phases=7,baseline_failure_observed=False,production_execution_performed=False,public_api_or_publication_activation=False))
for name in ['TARGET.json','SOURCE.patch','V2-TO-V3.patch','BASELINE-TEST.patch','REPORT.md','OWNERSHIP.md','PLAN.md','INPUTS.json','READBACK.json','CALLER_INPUTS.json','TEST-CONTINUITY.json']:print(name,record(D/name)['sha256'])
