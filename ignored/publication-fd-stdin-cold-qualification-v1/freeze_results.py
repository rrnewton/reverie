"""Authenticate and freeze completed evidence; never launch phases or change source."""
from pathlib import Path
import fcntl, hashlib, json, os, shutil, stat, subprocess
N=Path(__file__).resolve().parent
R=N.parents[1]
Q=N/'qualification-v1'
D=R/'ignored/publication-fd-composition-v3'
B=R/'ignored/publication-fd-stdin-baseline-v1'
U=R/'ignored/publication-fd-stdin-qualification-v1'
F=N/'final-v1'
assert not F.exists()
def rec(p):
    p=Path(p);s=p.lstat();assert stat.S_ISREG(s.st_mode),p
    h=hashlib.sha256()
    with p.open('rb')as f:
        for data in iter(lambda:f.read(1048576),b''):h.update(data)
    return dict(path=str(p),bytes=s.st_size,mode=stat.S_IMODE(s.st_mode),sha256=h.hexdigest())
def load(p):return json.loads(Path(p).read_text())
def put(p,v):
    with p.open('x')as f:json.dump(v,f,indent=2);f.write('\n')
def check(row):
    actual=rec(row['path']);assert all(actual[k]==v for k,v in row.items()),row
    return actual
selection=load(Q/'SELECTORS.json')['groups']
phases=load(N/'RUN-ORDER.json')['phases']
assert len(phases)==len(set(phases))==88
names=[(g['artifact'],name)for g in selection.values()for name in g['names']]
assert len(names)==len(set(names))==81
assert sum(a=='lib'for a,n in names)==72
assert sorted(p.parent.name for p in (Q/'controls').glob('*/result.json'))==sorted(phases)
results=[];executions=[];cpu_ns=0;wall=0.;payload=0.;libtest=0.
for name in phases:
    planp=Q/(name+'-plan.json');plan=load(planp);rp=Q/'controls'/name/'result.json';r=load(rp)
    assert r['plan_sha256']==rec(planp)['sha256']
    assert r['accepted']and r['terminal_authenticated']and r['inputs_unchanged']
    assert r['raw_status']==0 and r['payload_exit']['returncode']==0 and r['payload_exit']['reaped']
    check(r['observer_result']);o=load(r['observer_result']['path']);a=o['final_accounting'];p=a['properties']
    assert o['accounting_complete']and a['cgroup_empty']and p['MainPID']==0 and p['ControlGroup']==''and p['ActiveState']in['inactive','failed']
    large=plan['kind']in['metadata','compile','check']
    assert plan['limits']==dict(aggregate_cpu_usec=600000000 if large else 30000000,wall_seconds=900 if large else 60,lethal_stderr_bytes=16777216,live_stdout_samples_bytes=67108864,phase_read_bytes=16777216,memory_bytes=17179869184,swap_bytes=0,free_floor_bytes=107374182400)
    cpu_ns+=a['cpu_usage_nsec'];wall+=r['transport']['elapsed_seconds']
    row=dict(name=name,kind=plan['kind'],plan=rec(planp),result=rec(rp),observer_result=rec(r['observer_result']['path']),raw_stdout=rec(Q/'observer'/name/'stdout'),raw_stderr=rec(Q/'observer'/name/'stderr'),accepted=True,raw_status=0,cpu_seconds=a['cpu_usage_nsec']/1e9,observer_wall_seconds=r['transport']['elapsed_seconds'],payload_seconds=r['payload_exit']['elapsed_seconds'],limits=plan['limits'])
    if name in selection:
        events=[json.loads(line)for line in(Q/'observer'/name/'stdout').read_text().splitlines()]
        started=[x['name']for x in events if x.get('type')=='test'and x.get('event')=='started']
        ended=[x for x in events if x.get('type')=='test'and x.get('event')in['ok','failed','ignored']]
        suites=[x for x in events if x.get('type')=='suite'and x.get('event')in['ok','failed']]
        assert started==selection[name]['names']and len(ended)==1 and ended[0]['name']==started[0]and ended[0]['event']=='ok'
        assert len(suites)==1 and suites[0]['passed']==1 and suites[0]['failed']==suites[0]['ignored']==suites[0]['measured']==0
        row.update(artifact=selection[name]['artifact'],names=started,summary=suites[0]);executions.extend((selection[name]['artifact'],n)for n in started);payload+=r['payload_exit']['elapsed_seconds'];libtest+=suites[0].get('exec_time',0.)
    results.append(row)
assert sorted(executions)==sorted(names)
inventory={}
for a in ['lib','static']:
    raw=[json.loads(line)for line in(Q/'observer'/('list-'+a)/'stdout').read_text().splitlines()]
    listed=[x['name']for x in raw if x.get('type')=='test'];assert len(listed)==len(set(listed))
    assert all(listed.count(n)==1 for kind,n in names if kind==a)
    assert load(Q/'controls'/('list-'+a)/'result.json')['readback']['count']==len(listed)
    inventory[a]=len(listed)
for row in load(Q/'retained-binaries/BINDING.json')['copies']:
    check(row['retained']);check(row['cargo_file']);assert row['cargo_artifact']['fresh']is False
    assert row['retained']['sha256']==row['cargo_file']['sha256']
assert load(N/'LINKED-LIBRARY-PROVENANCE.json')['linked_library']['fresh']is False
for row in load(B/'qualification-v1/retained-binaries/BINDING.json')['copies']:check(row['retained'])
for row in load(U/'qualification-v1/retained-binaries/BINDING.json')['copies']:check(row['retained'])
for row in load(D/'SOURCE-MANIFEST.json'):
    p=D/'source'/row['relative']
    if row['kind']=='file':check({k:row[k]for k in ['path','bytes','sha256']})
    elif row['kind']=='symlink':assert p.is_symlink()and os.readlink(p)==row['target']
    else:assert p.is_dir()and not list(p.iterdir())
live=load(D/'LIVE-CONTINUITY.json')
for row in live['live']+[live['index']]:check(row)
for key,args in [('head',['rev-parse','HEAD']),('branch',['branch','--show-current'])]:
    result=subprocess.run(['/usr/bin/git','-C',str(R),*args],capture_output=True,env={**os.environ,'GIT_OPTIONAL_LOCKS':'0'},timeout=30)
    assert result.returncode==0 and result.stdout.decode().strip()==live[key]
F.mkdir(mode=0o700)
shutil.copytree(D/'source',F/'source',symlinks=True)
for name in ['SOURCE.patch','V2-TO-V3.patch','BASELINE-TEST.patch','NEW-TESTS.rs','OWNERSHIP.md','TEST-CONTINUITY.json','SOURCE-MAP.json']:
    shutil.copy2(D/name,F/name)
manifest=[]
for row in load(D/'SOURCE-MANIFEST.json'):
    item=dict(row)
    if row['kind']=='file':
        actual=rec(F/'source'/row['relative']);assert actual['sha256']==row['sha256']
        item.update({k:actual[k]for k in ['path','bytes','sha256']});item['file_mode']=actual['mode']
    manifest.append(item)
put(F/'FULL-SOURCE-MANIFEST.json',manifest)
put(F/'SOURCE_INPUTS.json',dict(base=live['head'],source_is_isolated_uncommitted_snapshot=True,qualified_source_root=str(D/'source'),copied_source_root=str(F/'source'),reviewed_target=rec(D/'TARGET.json'),source_patch=rec(F/'SOURCE.patch'),full_manifest=rec(F/'FULL-SOURCE-MANIFEST.json'),source_lock=rec(F/'source/Cargo.lock'),live_unchanged=live,setup=rec(Q/'SETUP.json'),retained_artifacts=rec(Q/'retained-binaries/BINDING.json'),linked_library=rec(N/'LINKED-LIBRARY-PROVENANCE.json')))
put(F/'PREDECESSORS.json',dict(previous_component_failures=rec(R/'ignored/publication-fd-composition-qualification-v1/final-v1/PREDECESSORS.json'),v2_qualification=rec(R/'ignored/publication-fd-composition-qualification-v1/final-v1/TARGET.json'),stdin_baseline=rec(B/'final-v1/READBACK.json'),stdin_baseline_results=rec(B/'final-v1/RESULTS.json'),stdin_baseline_retained_elfs=rec(B/'qualification-v1/retained-binaries/BINDING.json'),reused_artifact_refusal=rec(U/'build-provenance-refusal-v1/READBACK.json'),reused_artifact_original_result=rec(U/'qualification-v1/controls/compile/result.json'),reused_artifact_retained_elfs=rec(U/'qualification-v1/retained-binaries/BINDING.json')))
put(F/'RESULTS.json',dict(schema=1,source=rec(F/'SOURCE_INPUTS.json'),qualification_directory=str(Q),attempted_phases=88,qualified_phases=88,failed_phases=0,tests=dict(selected=81,executed=81,passed=81,failed=0,ignored=0,lib=72,static=9,new_stdin=3),actual_inventories=inventory,phases=results,total_cpu_seconds=cpu_ns/1e9,summed_observer_wall_seconds=wall,test_payload_seconds=payload,libtest_reported_seconds=libtest,retained_elfs=rec(Q/'retained-binaries/BINDING.json'),preserved_predecessors=rec(F/'PREDECESSORS.json')))
(F/'REPORT.md').write_text(f'''Composition-v3 passed all 88 corrected qualification phases: 81 exact declarations (72 library and nine static), with zero failures or ignored tests, plus metadata, compile, format, both actual lists, core/ptrace check and strict Clippy. The identical inherited-stdin epoll control failed on unchanged v2 production at its first MOD result: actual -2 (ENOENT), required 0; it now passes on the correction. The baseline receipt remains raw 101 and accepted=false, and its later assertions/capture-on iteration were not executed. All three new stdin controls, both adapted retirement-error controls and the unchanged real-EMFILE test passed early in the corrected run.\n\nThe qualified full patch is b51dc00c98699b65621112f6f3aa672df5492fcb3dd1ef85bf8c7c7b9b9ff94e on base {live['head']}. This is an isolated source qualification; live product, HEAD and index remained unchanged. Both selected harnesses and the linked non-test reverie_kvm library were freshly compiled in an initially empty dedicated target. Their actual Cargo records bind the source paths, and the retained harnesses differ from the known baseline binaries. Complete source copies, patch, caller, loader, lists and raw results are bound here.\n\nA prior corrected-source compile returned raw 0/accepted=true but reused both baseline ELFs (fresh=true). It is explicitly unusable corrected-source evidence. No test was run against that attempt. Its original receipt, fingerprint/mtime/dep-info evidence and suspect ELFs remain immutable. The reviewed recovery changed exactly two target-directory path occurrences in prepare.py, with no copied artifacts, source-mtime changes, RUSTFLAGS salt, parser change or cache deletion. The separate negative baseline and prior component failures also remain bound in PREDECESSORS.json; no prior pass substitutes for the final run.\n\nActual inventories were {inventory['lib']} library and {inventory['static']} static declarations, discovered rather than executed in full. Aggregate observed CPU across the 88 phases was {cpu_ns/1e9:.6f} seconds; summed observer wall time was {wall:.6f} seconds. Exact-test payload time was {payload:.6f} seconds; libtest reported {libtest:.6f} seconds. Nine static declarations retain their existing physical mode loops and native comparisons; declaration totals are not VM-launch totals. These are component measurements, not a full-DAG receipt or production performance comparison.\n\nAll original bounds remained: 600 CPU / 900 wall for metadata, compile and checks; 30 CPU / 60 wall for format, lists and exact tests; 16 GiB memory, zero swap, 16 MiB stderr, 64 MiB maintained stdout, original 16 MiB read cap, 100 GiB free floor and two jobs. Cargo was offline/locked on nightly-2026-07-29. REVERIE_REQUIRE_KVM=1, original inner 30-second VM deadlines and the real-EMFILE child limit were unchanged. Every final phase has authenticated terminal accounting and unchanged source/input checks.\n\nNo assertion, selector, comparison, error branch, tolerance, memory bound or lint gate was weakened. The install-error test still permits exactly two successful clones and observes exactly two retirees; actual stale ordinary entries force the third failure. The accept control still fails its second install after host accept, with exact A1/A2 state, once-only sentinel action, accepted socket, both-lock availability and peer EOF evidence. The real EMFILE control retains its original setup and oracle.\n\nThis remains an inactive publication prerequisite and descriptor-entry correction. It activates no publisher, Tool hook, selector, timer/wait/child caller or FIFO machinery. Stale local signalfd readiness outside install, ignored-alarm observation, receiver authorization, driver-terminal notification and child provenance/staging remain separate obligations. It is not a timer repair, general host-blocking solution or cross-backend determinism result. Independent source review and SCM remain the coordinator's responsibility.\n''')
records={};symlinks={}
for root in [F,Q,N/'launch',D,B,U]:
    for p in root.rglob('*'):
        if p.is_symlink():symlinks[str(p)]=dict(path=str(p),target=os.readlink(p))
        elif p.is_file():records[str(p)]=rec(p)
for p in N.iterdir():
    if p.is_file():records[str(p)]=rec(p)
for row in load(Q/'dependency-inputs.json'):records[row['path']]=check(row)
# Historical Cargo output paths stay historical metadata; immutable retained copies are bound above.
put(F/'INPUTS.json',dict(files=list(records.values()),symlinks=list(symlinks.values()),historical_compiler_paths_not_revalidated=True))
# Final immutable results precede the normal non-mutating lease release proof.
last=load(Q/'clippy-plan.json');binding=last['lease'];fd=os.open(binding['path'],os.O_RDWR|os.O_CLOEXEC|os.O_NOFOLLOW)
try:
    fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB);s=os.fstat(fd);assert [s.st_dev,s.st_ino,s.st_uid]==binding['identity']
    token=json.loads(os.pread(fd,4096,0));assert token['plan_sha256']==rec(Q/'clippy-plan.json')['sha256']
    completion=Path(binding['state_directory'])/(token['plan_sha256']+'.json');c=load(completion);check(c['result']);assert c['result']['path']==str(Q/'controls/clippy/result.json')
    release=dict(binding=binding,token=token,completion=rec(completion),result=c['result'],exclusive_available=True,lease_bytes_mutated=False,terminal_authenticated=True,accepted=True)
finally:os.close(fd)
release['descriptor_closed']=True;put(F/'LEASE-RELEASE.json',release)
put(F/'READBACK.json',dict(results=rec(F/'RESULTS.json'),report=rec(F/'REPORT.md'),inputs=rec(F/'INPUTS.json'),source_inputs=rec(F/'SOURCE_INPUTS.json'),lease_release=rec(F/'LEASE-RELEASE.json'),records=len(records),symlinks=len(symlinks),frozen=True))
put(F/'TARGET.json',dict(schema=1,base=live['head'],source_root=str(F/'source'),qualified_source_root=str(D/'source'),source_patch=rec(F/'SOURCE.patch'),source_inputs=rec(F/'SOURCE_INPUTS.json'),results=rec(F/'RESULTS.json'),report=rec(F/'REPORT.md'),inputs=rec(F/'INPUTS.json'),readback=rec(F/'READBACK.json'),predecessors=rec(F/'PREDECESSORS.json'),status='Qualified inactive composition-v3; independent source approval and SCM pending'))
print(json.dumps(dict(directory=str(F),target=rec(F/'TARGET.json'),results=rec(F/'RESULTS.json'),inputs=rec(F/'INPUTS.json'),readback=rec(F/'READBACK.json'),cpu_seconds=cpu_ns/1e9,observer_seconds=wall,test_payload_seconds=payload),indent=2))
