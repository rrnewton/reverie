"""Freeze completed qualification evidence only; never launch a phase or mutate source."""
from pathlib import Path
import fcntl, hashlib, json, os, shutil, stat, subprocess, sys

N = Path(__file__).resolve().parent
R = N.parents[1]
Q = N/'qualification-v1'
D = R/'ignored/publication-fd-composition-v2'
F = N/'final-v1'
assert not F.exists()

def rec(p):
    p = Path(p); s = p.lstat()
    assert stat.S_ISREG(s.st_mode), p
    h = hashlib.sha256()
    with p.open('rb') as f:
        for b in iter(lambda:f.read(1024*1024), b''): h.update(b)
    return dict(path=str(p), bytes=s.st_size, mode=stat.S_IMODE(s.st_mode), sha256=h.hexdigest())

def load(p): return json.loads(Path(p).read_text())
def put(p, value):
    with p.open('x') as f: json.dump(value, f, indent=2); f.write('\n')
def check(row):
    actual = rec(row['path'])
    assert all(actual[k] == value for k,value in row.items()), row
    return actual

selection = load(Q/'SELECTORS.json')
groups = selection['groups']
names = [(g['artifact'],name)for g in groups.values() for name in g['names']]
assert len(names) == len(set(names)) == 78
phases = ['metadata','compile','format','list-lib','list-static','core-check','clippy',*groups]
assert len(phases) == 85
assert sorted(p.parent.name for p in (Q/'controls').glob('*/result.json')) == sorted(phases)
results=[]; executions=[]; cpu_ns=0; observed_wall=0.; test_payload=0.; libtest_seconds=0.
for name in phases:
    planpath=Q/(name+'-plan.json'); plan=load(planpath)
    resultpath=Q/'controls'/name/'result.json'; result=load(resultpath)
    assert result['plan_sha256']==rec(planpath)['sha256']
    assert result['accepted'] and result['terminal_authenticated'] and result['inputs_unchanged']
    assert result['raw_status']==0 and result['payload_exit']['returncode']==0 and result['payload_exit']['reaped']
    check(result['observer_result']); observed=load(result['observer_result']['path'])
    accounting=observed['final_accounting']; props=accounting['properties']
    assert observed['accounting_complete'] and accounting['cgroup_empty']
    assert props['MainPID']==0 and props['ControlGroup']=='' and props['ActiveState'] in ['inactive','failed']
    cpu_ns+=accounting['cpu_usage_nsec']; observed_wall+=result['transport']['elapsed_seconds']
    limits=plan['limits']; large=plan['kind']in ['metadata','compile','check']
    assert limits==dict(aggregate_cpu_usec=600000000 if large else 30000000,
        wall_seconds=900 if large else 60,lethal_stderr_bytes=16777216,
        live_stdout_samples_bytes=67108864,phase_read_bytes=16777216,
        memory_bytes=17179869184,swap_bytes=0,free_floor_bytes=107374182400)
    item=dict(name=name,kind=plan['kind'],plan=rec(planpath),result=rec(resultpath),
        observer_result=rec(result['observer_result']['path']),
        raw_stdout=rec(Q/'observer'/name/'stdout'),raw_stderr=rec(Q/'observer'/name/'stderr'),
        accepted=True,raw_status=0,cpu_seconds=accounting['cpu_usage_nsec']/1e9,
        observer_wall_seconds=result['transport']['elapsed_seconds'],
        payload_seconds=result['payload_exit']['elapsed_seconds'],limits=limits)
    if name in groups:
        rows=[json.loads(line)for line in (Q/'observer'/name/'stdout').read_text().splitlines()]
        starts=[r['name']for r in rows if r.get('type')=='test'and r.get('event')=='started']
        ends=[r for r in rows if r.get('type')=='test'and r.get('event')in ['ok','failed','ignored']]
        suites=[r for r in rows if r.get('type')=='suite'and r.get('event')in ['ok','failed']]
        assert starts==groups[name]['names'] and len(ends)==1 and ends[0]['name']==starts[0] and ends[0]['event']=='ok'
        assert len(suites)==1 and suites[0]['passed']==1 and suites[0]['failed']==suites[0]['ignored']==suites[0]['measured']==0
        item.update(artifact=groups[name]['artifact'],names=starts,summary=suites[0])
        executions.extend((groups[name]['artifact'],n)for n in starts)
        test_payload+=result['payload_exit']['elapsed_seconds'];libtest_seconds+=suites[0].get('exec_time',0.)
    results.append(item)
assert sorted(executions)==sorted(names)
inventories={artifact:load(Q/'controls'/('list-'+artifact)/'result.json')['readback']['count']for artifact in ['lib','static']}
for artifact in inventories:
    rows=[json.loads(line)for line in (Q/'observer'/('list-'+artifact)/'stdout').read_text().splitlines()]
    listed=[r['name']for r in rows if r.get('type')=='test'];assert len(listed)==len(set(listed))==inventories[artifact]
    assert all(listed.count(name)==1 for a,name in names if a==artifact)

# Authenticate immutable retained actual artifacts before final records or cache release.
binary=load(Q/'retained-binaries/BINDING.json')
for row in binary['copies']:
    check(row['retained']);check(row['cargo_file'])
    assert row['retained']['sha256']==row['cargo_file']['sha256']
    assert Path(row['retained']['path']).read_bytes()[:4]==b'\x7fELF'
for row in load(D/'SOURCE-MANIFEST.json'):
    p=D/'source'/row['relative']
    if row['kind']=='file':check({k:row[k]for k in ['path','bytes','sha256']})
    elif row['kind']=='symlink':assert p.is_symlink()and os.readlink(p)==row['target']
    else:assert p.is_dir()and not list(p.iterdir())
live=load(D/'LIVE-CONTINUITY.json')['after']
for row in live['live']+[live['index']]:check(row)
for key,args in [('head',['rev-parse','HEAD']),('branch',['branch','--show-current'])]:
    p=subprocess.run(['/usr/bin/git','-C',str(R),*args],capture_output=True,
        env={**os.environ,'GIT_OPTIONAL_LOCKS':'0'},timeout=30)
    assert p.returncode==0 and p.stdout.decode().strip()==live[key]

F.mkdir(mode=0o700)
shutil.copytree(D/'source',F/'source',symlinks=True)
for name in ['SOURCE.patch','PUBLISHER-TO-COMPOSED.patch','FD-TO-COMPOSED.patch','V1-TO-V2.patch',
             'OWNERSHIP.md','SOURCE-MAP.json','TEST-SOURCE-CONTINUITY.json','RECONSTRUCTION.json']:
    shutil.copy2(D/name,F/name)
manifest=[]
for row in load(D/'SOURCE-MANIFEST.json'):
    item=dict(row)
    if row['kind']=='file':
        actual=rec(F/'source'/row['relative'])
        item.update({k:actual[k]for k in ['path','bytes','sha256']})
        item['file_mode']=actual['mode'];assert item['sha256']==row['sha256']
    manifest.append(item)
put(F/'FULL-SOURCE-MANIFEST.json',manifest)
put(F/'SOURCE_INPUTS.json',dict(base=live['head'],actual_source_is_uncommitted_snapshot=True,
    reviewed_target=rec(D/'TARGET.json'),source_patch=rec(F/'SOURCE.patch'),full_manifest=rec(F/'FULL-SOURCE-MANIFEST.json'),
    source_lock=rec(F/'source/Cargo.lock'),live_state_unchanged=live,setup=rec(Q/'SETUP.json'),
    actual_cargo_artifacts=rec(Q/'controls/compile/artifacts.json'),retained_artifacts=rec(Q/'retained-binaries/BINDING.json')))

publisher=R/'ignored/process-publication-implementation-v1/frozen-v1'
fd=R/'ignored/same-inode-ofd-repair-v2/final-v3'
baseline=R/'ignored/same-inode-ofd-control-v1/qualification'
predecessors=dict(publisher_first_attempt=load(publisher/'RESULTS.json')['first_attempt'],
    fd_first_attempt=load(fd/'RESULTS.json')['first_attempt'],
    stale_ofd_baseline_result=rec(baseline/'controls/test-replaced-description/result.json'),
    stale_ofd_baseline_stdout=rec(baseline/'observer/test-replaced-description/stdout'),
    stale_ofd_baseline_stderr=rec(baseline/'observer/test-replaced-description/stderr'),
    stale_ofd_baseline_artifacts=rec(baseline/'ELF-RETENTION.json'),
    baseline_report=rec(baseline/'REPORT.md'),
    component_results=[rec(publisher/'RESULTS.json'),rec(fd/'RESULTS.json')])
assert load(predecessors['stale_ofd_baseline_result']['path'])['raw_status']==101
assert not load(predecessors['stale_ofd_baseline_result']['path'])['accepted']
put(F/'PREDECESSORS.json',predecessors)
put(F/'RESULTS.json',dict(schema=1,source=rec(F/'SOURCE_INPUTS.json'),qualification_directory=str(Q),
    attempted_phases=85,qualified_phases=85,failed_phases=0,
    tests=dict(selected=78,executed=78,passed=78,failed=0,ignored=0,lib=69,static=9,new_retirement_poison=5),
    actual_inventories=inventories,phases=results,total_cpu_seconds=cpu_ns/1e9,
    summed_observer_wall_seconds=observed_wall,test_payload_seconds=test_payload,libtest_reported_seconds=libtest_seconds,
    retained_elfs=rec(Q/'retained-binaries/BINDING.json'),preserved_predecessors=rec(F/'PREDECESSORS.json')))
report=f'''The frozen composition-v2 qualified all 85 reviewed phases: 78 exact test declarations passed (69 lib, nine static), with zero failures or ignored tests, plus metadata, compile, format, both lists, core/ptrace check and strict Clippy. The five new retirement/poison controls passed. Actual inventories were {inventories['lib']} library and {inventories['static']} static declarations; those larger inventories were discovered, not executed in full.

The tested source is the isolated combined patch 41dd4f6af0ee2b4364b36254f67d5988113dd2b04ccfff10d50012280a64ce1a on base {live['head']}. It is not a committed or live-worktree qualification. The full copied source and patch are bound here; live product, branch and index stayed unchanged. Both actual Cargo ELFs were retained before cache reuse, with complete artifact, loader, source and list bindings. Compiler and Clippy produced zero structured diagnostics. Actual source reviews and SCM remain the coordinator's responsibility.

Aggregate observed CPU was {cpu_ns/1e9:.6f} seconds. Summed observer wall time was {observed_wall:.6f} seconds, including startup and authentication. Exact-test payload time was {test_payload:.6f} seconds; libtest reported {libtest_seconds:.6f} seconds. These are component measurements, not production performance or a whole-DAG receipt. The nine unchanged VM declarations retain their original physical mode loops and native comparisons; no separate physical-run count is inferred from declaration receipts.

Every phase retained its reviewed bounds: 600 CPU / 900 wall for metadata, compile and checks; 30 CPU / 60 wall for format, lists and each exact test; 16 GiB memory, zero swap, 16 MiB stderr, 64 MiB maintained stdout, original read cap and 100 GiB floor. Builds used two jobs, offline/locked nightly-2026-07-29. KVM-required admission and original inner 30-second VM deadlines remained enabled. The observer/parser/lease helpers were unchanged. Fresh SETUP mode fields supplied required binding data without relaxing check_file; the binder matched the snapshot-aware entry already present in reviewed CALLER_INPUTS.

Fresh metadata resolved 288 packages; the reviewed dependency walk selected 155 packages and bound 7,083 external files. Another 64 pinned-sysroot library/component inputs were bound before compilation. The package-only binder output is retained separately. Every attempted phase had authenticated terminal accounting and unchanged source/input checks.

Earlier failures remain original evidence: the publisher's raw-0 warning refusal, the FD repair's formatting failure, and the unchanged-source stale-OFD raw-101 assertion. PREDECESSORS.json binds these, their retained artifacts and original reports. No failure was relabelled, assertion changed, selector removed, budget widened or old component pass substituted for this run.

This qualifies the inactive source only. It activates no publisher, Tool hook, selector or timer/wait/child caller. Stale local signalfd carrier resolution outside install, ignored-alarm observation, actual selection authorization, driver terminal notification and child provenance/staging remain separate obligations. It is not a FIFO implementation, general host-blocking correction, timer repair or cross-backend determinism result.
'''
(F/'REPORT.md').write_text(report)

records={}
for root in [F,Q,N/'launch']:
    for p in root.rglob('*'):
        if p.is_file()and not p.is_symlink():records[str(p)]=rec(p)
for p in [N/'DEPLOYMENT.json',N/'CALLER-AUTHENTICATION.json',N/'FRESH-INPUT-BINDING.json',Path(__file__),
          D/'TARGET.json',D/'READBACK.json',D/'SOURCE_INPUTS.json',D/'CALLER_INPUTS.json']:
    records[str(p)]=rec(p)
for row in load(Q/'dependency-inputs.json'):records[row['path']]=check(row)
for row in load(D/'INPUTS-INITIAL.json')['publisher']+load(D/'INPUTS-INITIAL.json')['fd']:
    records[row['path']]=check(row)
def add_bound(value):
    if isinstance(value,dict):
        if all(k in value for k in ['path','bytes','sha256']):
            records[value['path']]=check(value)
        else:
            for x in value.values():add_bound(x)
    elif isinstance(value,list):
        for x in value:add_bound(x)
add_bound(predecessors)
for name in [predecessors['publisher_first_attempt']['retained_elfs']['path'],
             predecessors['fd_first_attempt']['retained_elf']['path'],
             predecessors['stale_ofd_baseline_artifacts']['path']]:
    def retained_only(value):
        if isinstance(value,dict):
            for key,item in value.items():
                if key=='retained'and isinstance(item,dict)and 'sha256'in item:
                    records[item['path']]=check(item)
                else:retained_only(item)
        elif isinstance(value,list):
            for item in value:retained_only(item)
    retained_only(load(name))
# Historical mutable compiler paths are represented by their immutable retained ELF copies.
put(F/'INPUTS.json',dict(schema=1,records=list(records.values()),record_count=len(records),
    source_manifest=rec(F/'FULL-SOURCE-MANIFEST.json'),results=rec(F/'RESULTS.json'),
    artifact_provenance=rec(Q/'retained-binaries/BINDING.json'),
    historical_compiler_paths_are_not_immutable_inputs=True))

# All source, results, raw receipts and retained binaries above are now immutable.
# The phase caller already closed its owned lease descriptors after terminal completion.
last=max(results,key=lambda row:Path(row['result']['path']).stat().st_mtime_ns)
lastplan=load(last['plan']['path']);binding=lastplan['lease']
fdno=os.open(binding['path'],os.O_RDWR|os.O_CLOEXEC|os.O_NOFOLLOW)
try:
    st=os.fstat(fdno);assert [st.st_dev,st.st_ino,st.st_uid]==binding['identity']
    fcntl.flock(fdno,fcntl.LOCK_EX|fcntl.LOCK_NB)
    token=json.loads(os.pread(fdno,4096,0))
    assert token['plan_sha256']==last['plan']['sha256']
    completion=load(Path(binding['state_directory'])/(token['plan_sha256']+'.json'))
    check(completion['result']);assert completion['result']['path']==last['result']['path']
    assert load(completion['result']['path'])['terminal_authenticated']
finally:
    os.close(fdno)
put(F/'LEASE-RELEASE.json',dict(path=binding['path'],identity=binding['identity'],exclusive_lock_available=True,
    check_descriptor_closed=True,no_lease_state_mutation=True,last_terminal_result=last['result'],
    retained_artifacts_before_release=rec(Q/'retained-binaries/BINDING.json'),immutable_results=rec(F/'RESULTS.json')))
readback=[rec(F/name)for name in ['REPORT.md','RESULTS.json','INPUTS.json','SOURCE_INPUTS.json',
    'FULL-SOURCE-MANIFEST.json','SOURCE.patch','PREDECESSORS.json','LEASE-RELEASE.json']]
for row in records.values():check(row)
put(F/'READBACK.json',dict(records=readback,all_input_records_authenticated=len(records),
    live_unchanged=live,product_execution_scope='Exactly reviewed 85 component phases',
    no_source_SCM_or_caller_change=True))
put(F/'TARGET.json',dict(schema=1,base=live['head'],source_target=rec(D/'TARGET.json'),
    source_patch=rec(F/'SOURCE.patch'),full_source_manifest=rec(F/'FULL-SOURCE-MANIFEST.json'),
    report=rec(F/'REPORT.md'),results=rec(F/'RESULTS.json'),inputs=rec(F/'INPUTS.json'),
    readback=rec(F/'READBACK.json'),lease_release=rec(F/'LEASE-RELEASE.json'),
    qualified_phases=85,passed_test_declarations=78,actual_inventories=inventories,
    status='FINITE QUALIFICATION COMPLETE; final independent source/runtime review and SCM pending'))
print(json.dumps({name:rec(F/name)for name in ['TARGET.json','REPORT.md','RESULTS.json','INPUTS.json','READBACK.json']},indent=2))
