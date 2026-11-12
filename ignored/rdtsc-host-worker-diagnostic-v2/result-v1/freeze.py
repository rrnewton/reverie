from pathlib import Path
import hashlib,json,os
N=Path(__file__).resolve().parent
D=N.parent; Q=D/'qualification-v1'
def rec(p):
 p=Path(p); st=p.lstat(); b=os.readlink(p).encode() if p.is_symlink() else p.read_bytes()
 r=dict(path=str(p),bytes=len(b),sha256=hashlib.sha256(b).hexdigest(),mode=st.st_mode&0o7777)
 if p.is_symlink():r.update(kind='symlink',target=os.readlink(p))
 return r
def write(n,v):
 with (N/n).open('x') as f:f.write(v if isinstance(v,str) else json.dumps(v,indent=2)+'\n')
phases=[]
for name in ['metadata','compile','format','list-static','host-worker-diagnostic']:
 p=Q/'controls'/name/'result.json'; d=json.loads(p.read_text()); obs=Q/'observer'/name/'result.json'; o=json.loads(obs.read_text())
 assert d['inputs_unchanged'] and d['terminal_authenticated']
 assert o['accounting_complete'] and o['final_accounting']['cgroup_empty']
 assert d['accepted']==(name!='host-worker-diagnostic')
 assert d['raw_status']==(101 if name=='host-worker-diagnostic' else 0)
 phases.append(dict(name=name,plan=rec(Q/(name+'-plan.json')),result=rec(p),observer=rec(obs),accepted=d['accepted'],raw_status=d['raw_status'],cpu_seconds=o['final_accounting']['cpu_usage_nsec']/1e9,payload_seconds=d['payload_exit']['elapsed_seconds'],observer_transport_seconds=d['transport']['elapsed_seconds']))
stdout=(Q/'observer/host-worker-diagnostic/stdout').read_text();stderr=(Q/'observer/host-worker-diagnostic/stderr').read_text()
events=[json.loads(line) for line in stdout.splitlines() if line.startswith('{')]
suite=events[-1];assert suite['passed']==0 and suite['failed']==1 and suite['ignored']==0
assert 'skip' not in (stdout+stderr).lower()
assert 'right: [(Pid(1), Tsc), (Pid(1), Tsc)]' in stderr
callbacks=(Q/'fixture-evidence/callbacks.tsv').read_text().splitlines();assert len(callbacks)==13
write('RESULTS.json',dict(qualification='FAILED: unchanged two-callback assertion; diagnostic attribution only',phases=phases,attempted=5,accepted=4,failed=1,selected_declarations=1,passed_declarations=0,failed_declarations=1,ignored_declarations=0,static_inventory=317,raw_suite=suite,no_skip_messages=True,all_terminal_authenticated=True,source_unchanged=True,attribution=rec(D/'attribution-v2/ATTRIBUTION.json'),attribution_readback=rec(D/'attribution-v2/READBACK.json')))
write('REPORT.md','''# Timestamp-14 diagnostic: failed oracle retained, callback origins measured

This is author diagnostic evidence, not source approval or a qualified component result. All five authorized phases ran once: metadata, cold compile, format and inventory passed; the one exact Host-worker selector failed raw 101 / accepted=false. The assertion remains exactly two root RDTSC callbacks and observed twelve. Libtest reports zero passed, one failed, zero ignored, with 316 other declarations filtered. No KVM-skip message appears. Guest status 0 and empty stdout/stderr assertions were reached and passed before the count assertion.

Every saved RIP was mapped to the retained actual executable/interpreter PT_LOAD executable segment and checked against exact instruction bytes and objdump boundaries. Ten callbacks are 0f31 instructions in the actual interpreter's _dl_start/dl_main, before the two main instructions at 0x401055 and 0x4010c2. The interpreter load bias is the current ELF loader's source-derived 0x1000000. All twelve RPC senders are task 1. The worker's retained code executes RDTSC at 0x4011f0, then returns its argument. There is no callback at that RIP; the successful pthread_join result 0x1234 and guest exit establish worker completion. This attributes the twelve measured callbacks without guessing a libc count or filtering the full callback log.

The original C fixture, exact compiler argv/status, emitted main ELF, resolved interpreter ELF, all callback records and bounded disassembly/readelf queries are retained. Attribution-v1's first read-only query refusal (ldd was asked about its own script) remains intact; attribution-v2 binds the script and queries the actual shell ELF before the same bounded inspections. There was no second guest attempt. Diagnostic-v1's earlier compile refusal for the unavailable serde_json dependency is retained; V2 uses the standard library and adds no dependency. No production file changed.

The compile consumed 111.592532 systemd-accounted aggregate CPU seconds, 60.506504140095785 payload seconds and 63.56781428400427 observer transport seconds. The selector consumed 2.666561 aggregate CPU seconds, 0.11600177688524127 payload seconds and 3.3604128470178694 transport seconds; these are different accounting domains. Original limits remain metadata/compile 600 CPU/900 wall with two jobs and each other phase 30 CPU/60 wall, with unchanged observer, memory/output/source/dependency/lease guards. Every phase completed terminal authentication and source readback.

A source-only successor will replace only this new unlanded pthread timestamp fixture with a loader-free static clone/CLEARTID ownership control. It must retain exactly two root callbacks, check both returned timestamp values, and prove the Host worker passed its own real RDTSC and exited before root completion. Existing pthread/join tests and all other candidate controls remain unchanged. The failed C fixture remains evidence; it is not renamed a pass. This diagnostic provides no full component, architectural, current-Hermit or same-run parity qualification.
''')
records={}
def add(p):
 p=Path(p)
 if p.is_file() or p.is_symlink():records[str(p)]=rec(p)
for row in json.loads((D/'INPUTS.json').read_text())['records']:
 r=rec(row['path']);assert r['sha256']==row['sha256'];add(row['path'])
for p in D.iterdir():
 if p.is_file():add(p)
for directory in [Q/'controls',Q/'observer',Q/'retained-binaries',Q/'fixture-evidence',D/'attribution-v1',D/'attribution-v2']:
 for p in directory.rglob('*'):add(p)
for p in Q.iterdir():
 if p.is_file():add(p)
for p in (D.parent/'rdtsc-host-worker-diagnostic-v1/compile-refusal-v1').rglob('*'):add(p)
for p in N.iterdir():add(p)
write('INPUTS.json',dict(records=list(records.values()),meaning='Actual failed diagnostic and complete attribution records; prior immutable source/caller inputs preserved'))
for row in records.values():assert rec(row['path'])==row
write('READBACK.json',dict(results=rec(N/'RESULTS.json'),report=rec(N/'REPORT.md'),inputs=rec(N/'INPUTS.json'),records=len(records),unchanged=True,accepted_phases=4,failed_phases=1,passed_tests=0,failed_tests=1,no_source_approval=True,no_component_qualification=True))
print(json.dumps({n:rec(N/n) for n in ['RESULTS.json','REPORT.md','INPUTS.json','READBACK.json']},indent=2))
