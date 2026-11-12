#!/usr/bin/python3
"""Freeze actual partial qualification and refused startup; never repair a lease."""
import hashlib,json,os,shutil,stat,sys
from pathlib import Path
F=Path(__file__).resolve().parent;Q=F.parent
sys.path.insert(0,str(Q))
from common import check_file,digest,file_record,json_read,read,require,write_new
import phase
require(sys.flags.optimize==0,'unoptimized execution required')
seq=json_read(Q/'EXECUTION-RESULT.json');sel=json_read(Q/'SELECTORS.json')
require(len(seq['sequence'])==72 and len(seq['completed'])==17 and seq['error'],'wrong finite history')
require(seq['sequence'][17]=='neighbor-05' and seq['completed']==seq['sequence'][:17],'unexpected stop')
proof=json_read(F/'TERMINAL-LIFECYCLE.json');raw_cpu=proof['properties']['CPUUsageNSec']
require(raw_cpu=='[not set]' and proof['properties']['LoadState']=='not-found','unexpected late accounting')
write_new(F/'TERMINAL-READBACK.json',dict(original=file_record(F/'TERMINAL-LIFECYCLE.json'),cpu_accounting_available=False,cpu_usage_nsec=None,raw_cpu_usage=raw_cpu,correction='The preliminary collector boolean incorrectly treated [not set] as available. The raw query was preserved correctly; it supplies no CPU accounting. No original receipt or collector artifact changed.',terminal_lifecycle_only=True,old_lease_unresolved=True,no_original_result_mutation=True))
rows=[]
for name in seq['sequence'][:18]:
 plan=json_read(Q/(name+'-plan.json'));receipt=json_read(Q/'controls'/name/'result.json');observed=json_read(Q/'observer'/name/'result.json')
 require(digest(Q/(name+'-plan.json'))==receipt['plan_sha256'] and receipt['inputs_unchanged'],'plan/source mismatch')
 phase.check(plan)
 accepted=name!='neighbor-05';require(receipt['accepted'] is accepted,'unexpected verdict')
 row=dict(name=name,kind=plan['kind'],selected=plan['selected'],accepted=accepted,raw_status=receipt['raw_status'],plan=file_record(Q/(name+'-plan.json')),receipt=file_record(Q/'controls'/name/'result.json'),observer=file_record(Q/'observer'/name/'result.json'),stdout=file_record(Q/'observer'/name/'stdout'),stderr=file_record(Q/'observer'/name/'stderr'),cpu_seconds=None,observer_wall_seconds=observed['elapsed_seconds'],payload_wall_seconds=None)
 if accepted:
  require(receipt['raw_status']==0 and receipt['terminal_authenticated'] and observed['accounting_complete'] and observed['observer_error'] is None,'accepted accounting mismatch')
  final=observed['final_accounting'];p=final['properties'];require(final['cgroup_empty'] and p['MainPID']==0 and p['ControlGroup']=='' and p['ActiveState'] in ['inactive','failed'],'accepted terminal missing')
  row.update(cpu_seconds=final['cpu_usage_nsec']/1e9,payload_wall_seconds=receipt['payload_exit']['elapsed_seconds'])
  require(receipt['payload_exit']['reaped'] and receipt['payload_exit']['returncode']==0,'child not reaped')
  if plan['kind']=='test':
   raw=[json.loads(x)for x in read(Q/'observer'/name/'stdout').splitlines()];started=[x['name']for x in raw if x.get('type')=='test'and x.get('event')=='started'];ended=[x for x in raw if x.get('type')=='test'and x.get('event')in ['ok','failed','ignored']];summ=[x for x in raw if x.get('type')=='suite'and x.get('event')in ['ok','failed']]
   require(started==plan['selected'] and len(ended)==1 and ended[0]['name']==started[0] and ended[0]['event']=='ok','wrong raw exact test')
   require(len(summ)==1 and summ[0]['passed']==1 and summ[0]['failed']==summ[0]['ignored']==summ[0]['measured']==0,'test denominator changed')
   row['raw_test_summary']=summ[0]
 else:
  require(receipt['raw_status']==137 and not receipt['terminal_authenticated'] and not observed['accounting_complete'] and observed['final_accounting'] is None,'refusal changed')
  require(not read(Q/'observer'/name/'stdout') and not read(Q/'observer'/name/'stderr'),'refused phase output appeared')
  row['classification']='observer startup refused before authenticated payload release; no test outcome or CPU accounting'
 rows.append(row)
markers={}
for name,marker in [('capture-03','capture closed standard descriptors completed'),('capture-04','capture final owner cleanup completed'),('capture-05','capture setup EMFILE control completed')]:
 raw=read(Q/'observer'/name/'stderr').decode();require(raw.count(marker)==1 and raw.count('capture child status=exit status: 0')==1,'missing real child completion')
 markers[name]=dict(stderr=file_record(Q/'observer'/name/'stderr'),marker=marker,exact_marker_count=1,child_status=0,intentional_caught_unwind='intentional capture cleanup unwind' in raw)
write_new(F/'CHILD-CONTROLS.json',dict(markers=markers,scope='All three isolated child bodies reached their existing final markers under actual successful child status, including closed standard fds, root-owner normal/unwind and real EMFILE. Unchanged install EMFILE neighbor-20 remains unrun.'))
arts=json_read(Q/'retained-binaries/ARTIFACTS.json');binding=json_read(Q/'retained-binaries/BINDING.json')
for key,art in arts.items():
 require(art['cargo_artifact']['fresh'] is False,'artifact reuse');check_file(art['file'],executable=True)
for c in binding['copies']:
 require(c['distinct_inode'] and c['original']['sha256']==c['retained']['sha256'],'retention mismatch')
 require(Path(c['original']['path']).stat().st_ino!=Path(c['retained']['path']).stat().st_ino,'retained inode aliases')
linked=binding['linked_library'];require(linked['cargo_artifact']['fresh'] is False,'linked library reused');check_file(linked['file'])
p=F/'retained-linked-library.rlib'
with p.open('xb') as dst,Path(linked['file']['path']).open('rb') as src:shutil.copyfileobj(src,dst)
p.chmod(0o444);require(digest(p)==linked['file']['sha256'] and p.stat().st_ino!=Path(linked['file']['path']).stat().st_ino,'linked copy differs')
write_new(F/'ARTIFACT-READBACK.json',dict(original_binding=file_record(Q/'retained-binaries/BINDING.json'),retained_artifacts=file_record(Q/'retained-binaries/ARTIFACTS.json'),four_harnesses_fresh_false=True,linked_library_fresh_false=True,retained_linked_library=file_record(p),historical_linked_library=linked['file'],meaning='Actual accepted compile, immutable four harnesses and separate rlib copy; no credit for unrun selectors. Historical target paths are not immutable inputs.'))
inventories={key:json_read(Q/'controls'/('list-'+key)/'result.json')['readback']['count']for key in arts}
accepted_test_names=[r['name']for r in rows if r['accepted']and r['kind']=='test'];require(len(accepted_test_names)==10,'wrong executed test count')
unqualified=[n for n in sel['proposed_order']if n not in accepted_test_names];require(len(unqualified)==53 and unqualified[0]=='neighbor-05','wrong remaining tests')
result=dict(status='STOPPED AT FIRST OBSERVER STARTUP REFUSAL',caller=file_record(Q/'CALLER-TARGET.json'),source_target=file_record(Q.parent/'TARGET.json'),source_patch=file_record(Q.parent/'SOURCE.patch'),planned_phases=72,attempted_phases=18,accepted_phases=17,refused_phases=1,product_test_failures=0,unattempted_phases=54,planned_exact_tests=63,qualified_exact_tests=10,executed_exact_tests=10,still_unqualified_exact_tests=53,unattempted_exact_tests=52,failed_release_selector='neighbor-05',retries=0,inventories=inventories,accepted_phase_cpu_seconds=sum(r['cpu_seconds']for r in rows if r['cpu_seconds']is not None),total_cpu_seconds=None,missing_cpu_phases=['neighbor-05'],summed_observer_wall_seconds=sum(r['observer_wall_seconds']for r in rows),accepted_payload_wall_seconds=sum(r['payload_wall_seconds']for r in rows if r['payload_wall_seconds']is not None),exact_test_payload_wall_seconds=sum(r['payload_wall_seconds']for r in rows if r['kind']=='test'and r['accepted']),phases=rows,remaining_unqualified_selectors=unqualified,remaining_checks=['core-check','clippy'],old_lease_unresolved=True,terminal_lifecycle=file_record(F/'TERMINAL-READBACK.json'))
write_new(F/'RESULTS.json',result)
(F/'REPORT.md').write_text(f'''# Capture pipe identity V2: partial qualification and startup refusal

The fresh compile, format and four inventories qualified. All six new capture controls and four unchanged neighbors passed: **10 / 63 exact declarations**, with no failed or ignored product test. The next selector, neighbor-05, was refused during observer startup. The caller stopped once, without retry. Eighteen of 72 phases were attempted: 17 accepted and one refused; 54 later phases were not attempted. Fifty-three selectors still need qualification, including neighbor-05, plus core/ptrace check and Clippy. The original V1 compile errors remain separate failures.

The actual observer failed its first Unit.Id property query with sd_bus timeout after 0.250307802 s. Its original result has raw wrapper 137, no final accounting and terminal_authenticated=false. Source shows initial unit_properties precedes authenticate_service, acquire_before_exec and the sole exec authorization. The child requires that response before its receipt and fd-exec. No authorization record, child receipt, payload record or payload-exit exists; stdout and stderr are empty. Zero samples alone is not the basis for the no-payload conclusion. The missing-outcomes reader error is preserved as a consequence, not a failed libtest assertion.

Fresh read-only lifecycle proof found the exact named unit not-found/inactive/dead with MainPID zero and empty ControlGroup, all recorded processes absent and no wrapper-session member. The unit was already unloaded: its default Result=success is **not** the old run's result. CPUUsageNSec is `[not set]`. The preliminary collector misclassified that string in a boolean; TERMINAL-READBACK explicitly corrects the boolean while retaining the raw query and original artifact. No CPU value is reconstructed. The accepted phases consumed {result['accepted_phase_cpu_seconds']:.6f} CPU seconds; total run CPU is unknown. Summed observer wall over all attempts is {result['summed_observer_wall_seconds']:.6f} seconds. Ten exact-test payloads total {result['exact_test_payload_wall_seconds']:.6f} seconds.

The actual child streams show exactly one successful closed-standard-fd marker, root-owner cleanup marker (including the expected caught unwind) and real EMFILE setup marker. The latter exercises first pipe failure, second-stream failure after first preparation, and F_DUPFD_CLOEXEC failure after closing the unused endpoint. The unchanged install EMFILE neighbor-20 remains unrun. The static direct/Tool pipe identity control passed under mandatory KVM admission. All original assertions, warning gates, output rules, per-selector 30 CPU / 60 observer-wall bounds and the explicit 30-second test timeout remain unchanged.

Inventories are {inventories}. All four retained harnesses and the linked non-test KVM library were emitted fresh=false. The immutable rlib copy is now separately retained; historical target artifact paths remain metadata. Loader, source/lock, compiler, dependency, raw outcome and initial caller bindings are indexed. No old outcome transfers to this source. No Hermit guest/parity claim follows from component tests.

The old private lease is deliberately unresolved: its exact refused plan token remains, and there is no terminal completion record. A fresh exclusive read-only proof hold closed without token/completion mutation. Normal admission must continue to reject this lease. Any separately reviewed continuation must use a fresh private lease and retain this refusal. There was no recovery, rerun, source edit, bound change or SCM mutation.

Audit tooling limits: initial result-path lookup used controls-run-1 rather than actual controls and failed before reads; an exploratory observer grep was truncated and then replaced with complete bounded source ranges; an unrelated accidental nonexistent sed path failed; the first collector indexed the wrong call-array position and stopped before lease/query, then the corrected collector selected the actual Id row. These are retained audit mistakes, not product failures or evidence of qualification. Whole scheduler/timer/cancellation or backend parity approval is outside this packet.
''')
files={};links={};special=[];skipped=[]
def add(p):
 p=Path(p);s=p.lstat()
 if stat.S_ISREG(s.st_mode):files[str(p)]=file_record(p)
 elif stat.S_ISLNK(s.st_mode):links[str(p)]=dict(path=str(p),target=os.readlink(p))
 elif stat.S_ISSOCK(s.st_mode):special.append(dict(path=str(p),kind='completed_observer_socket_metadata_only',device=s.st_dev,inode=s.st_ino,uid=s.st_uid,mode=stat.S_IMODE(s.st_mode)))
 else:require(stat.S_ISDIR(s.st_mode),'unexpected output kind')
held=json_read(Q/'CALLER-INPUTS.json')
for r in held['files']:check_file(r);files[r['path']]=r
for r in held['symlinks']:require(Path(r['path']).is_symlink()and os.readlink(r['path'])==r['target'],'held alias drift');links[r['path']]=r
for r in json_read(Q/'dependency-inputs.json'):check_file(r);files[r['path']]=r
for r in json_read(Q/'dependency-symlinks.json'):require(Path(r['path']).is_symlink()and os.readlink(r['path'])==r['target'],'dependency alias drift');links[r['path']]=r
for p in Q.iterdir():
 if p.is_file()and p.name!='lane.lease':add(p)
for root in ['observer','controls','loader','launch','lease-state','retained-binaries']:
 for p in sorted((Q/root).rglob('*')):add(p)
for p in F.iterdir():add(p)
write_new(F/'INPUTS.json',dict(files=sorted(files.values(),key=lambda r:r['path']),symlinks=sorted(links.values(),key=lambda r:r['path']),socket_metadata=special,unexpanded_gitlinks=held['unexpanded_gitlinks'],excluded_mutable=['private target','private tmp/cache','raw unresolved lease inode; exact token/identity in TERMINAL-LIFECYCLE'],no_original_results_rewritten=True))
for r in files.values():check_file(r)
for r in links.values():require(Path(r['path']).is_symlink()and os.readlink(r['path'])==r['target'],'final alias drift')
write_new(F/'READBACK.json',dict(results=file_record(F/'RESULTS.json'),report=file_record(F/'REPORT.md'),inputs=file_record(F/'INPUTS.json'),terminal=file_record(F/'TERMINAL-READBACK.json'),artifacts=file_record(F/'ARTIFACT-READBACK.json'),child_controls=file_record(F/'CHILD-CONTROLS.json'),regular_files=len(files),symlinks=len(links),completed_socket_metadata=len(special),all_indexed_bytes_modes_unchanged=True,whole_qualification=False,passed_exact_tests=10,remaining_exact_tests=53,old_lease_unresolved=True,source_product_mutations=False))
print(json.dumps({n:file_record(F/n)for n in ['REPORT.md','RESULTS.json','INPUTS.json','READBACK.json']},indent=2))
