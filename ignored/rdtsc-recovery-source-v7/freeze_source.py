from pathlib import Path
import ast,difflib,hashlib,json,os,shutil
N=Path(__file__).resolve().parent;R=N.parent;V=R/'rdtsc-recovery-source-v6';Q=N/'qualification-v1';S=N/'source'
def rec(p):
 p=Path(p);return dict(path=str(p),bytes=p.stat().st_size,mode=p.stat().st_mode&0o7777,sha256=hashlib.sha256(p.read_bytes()).hexdigest())
def write(n,x):
 with (N/n).open('x')as f:f.write(x if isinstance(x,str)else json.dumps(x,indent=2)+'\n')
changed=['reverie-kvm/src/terminal_runtime_tests.rs','reverie-kvm/tests/support/timestamp_terminal.rs']
source=[]
for row in json.loads((V/'SOURCE-MANIFEST.json').read_text()):
 r=dict(row);r['path']=str(S/r['relative'])
 if r['mode']!='160000':
  p=Path(r['path']);digest=hashlib.sha256(os.fsencode(os.readlink(p))).hexdigest()if p.is_symlink()else rec(p)['sha256']
  if r['relative']in changed:r.update(sha256=digest,bytes=p.stat().st_size,changed_from_baseline=True)
  else:assert digest==row['sha256']
 source.append(r)
write('SOURCE-MANIFEST.json',source)
base=R/'publication-fd-composition-v3/source';paths=[r['relative']for r in source if r.get('changed_from_baseline')]
full=''.join(''.join(difflib.unified_diff(((base/r).read_text()if(base/r).exists()else'').splitlines(True),(S/r).read_text().splitlines(True),fromfile='a/'+r,tofile='b/'+r))for r in paths)
write('SOURCE.patch',full)
Q.mkdir(mode=0o700);(Q/'observer').mkdir(mode=0o700)
for name in ['prepare.py','phase.py','common.py','cache_lease.py','bind_dependencies.py','admit_target.py','retain_artifacts.py','toolchain-standard-inputs.json']:
 shutil.copy2(V/'qualification-v1'/name,Q/name)
for p in (V/'qualification-v1/observer').iterdir():
 if p.is_file()and(p.suffix=='.py'or p.name=='source-inputs.json'):shutil.copy2(p,Q/'observer'/p.name)
caller=''
for name,count in [('prepare.py',2),('admit_target.py',1)]:
 p=Q/name;a=p.read_text();assert a.count('rdtsc-recovery-v6-cold-v1')==count;b=a.replace('rdtsc-recovery-v6-cold-v1','rdtsc-recovery-v7-cold-v1')
 if name=='prepare.py':
  needle="'REVERIE_REQUIRE_KVM':'1'}";assert b.count(needle)==1
  b=b.replace(needle,"'REVERIE_REQUIRE_KVM':'1','RUSTUP_TOOLCHAIN':'nightly-2026-07-29','RUSTUP_AUTO_INSTALL':'0'}")
  b=b.replace('exact 37 declarations and existing public vmcall/clock neighbors','six finite V7 test declarations; retained V6 production and historical qualification')
 p.write_text(b);ast.parse(b);caller+=''.join(difflib.unified_diff(a.splitlines(True),b.splitlines(True),fromfile='v6/qualification-v1/'+name,tofile='v7/qualification-v1/'+name))
oldsel=json.loads((V/'qualification-v1/SELECTORS.json').read_text());groups={k:oldsel['groups'][k]for k in ['timestamp-32','timestamp-33','timestamp-34','timestamp-37']}
groups['timestamp-v7-worker-group']=dict(artifact='static',names=['timestamp_terminal::worker_timestamp_exit_group_terminates_leader_and_runs_hooks_once'],source=changed[1],origin='New N1 requested actual worker-origin group-exit control',purpose='Exact worker/leader Exit29 hooks and one process hook; no leader guest Exit17 or resumed timestamp')
groups['timestamp-v7-admission']=dict(artifact='lib',names=['runtime::terminal_tests::timestamp_exit_admission_preserves_returning_ordinary_injections'],source=changed[0],origin='New N2 source-correct KVM-free predicate control',purpose='Both exit forms admitted; write tail refused/ordinary allowed; exec/fork refused by both')
write('qualification-v1/SELECTORS.json',dict(exact_declarations=6,groups=groups,scope='Two new declarations plus four nearest unchanged controls; prior37 remain historical evidence, no repeated full-suite claim'))
setup=json.loads((V/'qualification-v1/SETUP.json').read_text());setup['source_root']=str(S);setup['target']=str(N.parents[1]/'target/rdtsc-recovery-v7-cold-v1');setup['source_files']=[dict(relative=r['relative'],file=rec(S/r['relative']))for r in setup['source_files']];setup['lock']=rec(S/'Cargo.lock');setup['purpose']='V7 additive timestamp terminal tests only; production byte-identical to V6';write('qualification-v1/SETUP.json',setup)
write('qualification-v1/source-manifest.json',[dict(path=r['relative'],mode=r['mode'],**(dict(sha256=r['sha256'])if'sha256'in r else{}))for r in source])
arts=json.loads((V/'qualification-v1/retained-binaries/ARTIFACTS.json').read_text());write('qualification-v1/PREDECESSOR-ELFS.json',dict(scope='Exact V6 qualified retained artifacts; no import as V7 evidence',hashes={k:v['file']['sha256']for k,v in arts.items()},records=[v['file']for v in arts.values()]))
origins=[]
for p in Q.rglob('*.py'):
 a=V/'qualification-v1'/p.relative_to(Q);origins.append(dict(before=rec(a),after=rec(p),byte_identical=a.read_bytes()==p.read_bytes()))
write('qualification-v1/RUNNER_ORIGINS.json',dict(records=origins,meaning='Only fresh target name, explicit pinned rustup dispatch, truthful finite scope; phase/parser/observer/lease/retention unchanged'))
write('CALLER-DELTA.patch',caller)
write('RUN-ORDER.json',dict(phases=['metadata','compile','format','core-check','clippy','list-lib','list-static','timestamp-v7-admission','timestamp-v7-worker-group','timestamp-32','timestamp-33','timestamp-34','timestamp-37'],declarations=6,phase_count=13))
print(json.dumps(dict(delta=rec(N/'DELTA.patch'),full_patch=rec(N/'SOURCE.patch'),caller_delta=rec(N/'CALLER-DELTA.patch'),source_entries=len(source)),indent=2))
