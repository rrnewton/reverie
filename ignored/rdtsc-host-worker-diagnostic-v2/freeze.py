from pathlib import Path
import ast,copy,difflib,hashlib,json,os,shutil,stat
N=Path(__file__).resolve().parent;P=N.with_name('rdtsc-host-worker-diagnostic-v1');V=N.with_name('rdtsc-recovery-source-v3');R=N.parents[1];Q=N/'qualification-v1';O=P/'qualification-v2';C='reverie-kvm/tests/static_elf.rs'
def rec(p):
 p=Path(p);b=os.fsencode(os.readlink(p))if p.is_symlink()else p.read_bytes();return dict(path=str(p),bytes=len(b),mode=stat.S_IMODE(p.lstat().st_mode),sha256=hashlib.sha256(b).hexdigest(),**({'kind':'symlink','target':os.readlink(p)}if p.is_symlink()else{}))
def write(path,value):
 p=N/path;p.parent.mkdir(parents=True,exist_ok=True)
 with p.open('x')as f:f.write(value if isinstance(value,str)else json.dumps(value,indent=2)+'\n')
def diff(a,b,path,old='diagnostic-v1',new='diagnostic-v2'):return ''.join(difflib.unified_diff(a.splitlines(True),b.splitlines(True),fromfile=old+'/'+path,tofile=new+'/'+path))
before=(P/'source'/C).read_text();after=(N/'source'/C).read_text();assert 'serde_json'not in after
start='#[test]\nfn host_owned_timestamp_worker_keeps_native_execution()';assert before[before.index(start):]==after[after.index(start):]
write('DELTA.patch',diff(before,after,C));write('SOURCE.patch',diff((V/'source'/C).read_text(),after,C,'candidate-v3','diagnostic-v2'))
rows=[];changes=[]
for old in json.loads((P/'SOURCE-MANIFEST.json').read_text()):
 row=copy.deepcopy(old);p=N/'source'/row['relative'];row['path']=str(p)
 if row['kind']=='unexpanded_gitlink':assert p.is_dir()and not list(p.iterdir())
 else:
  actual=rec(p);assert actual['mode']==rec(P/'source'/row['relative'])['mode']
  if actual['sha256']!=old['sha256']:
   assert row['relative']==C;changes.append(C);row['sha256']=actual['sha256'];row['bytes']=actual['bytes']
 rows.append(row)
assert changes==[C];write('SOURCE-MANIFEST.json',rows)
Q.mkdir()
for p in O.iterdir():
 if p.is_file()and(p.suffix=='.py'or p.name in ['SELECTORS.json','toolchain-standard-inputs.json','PREDECESSOR-ELFS.json']):shutil.copyfile(p,Q/p.name)
(Q/'observer').mkdir()
for p in (O/'observer').iterdir():
 if p.is_file()and(p.suffix=='.py'or p.name=='source-inputs.json'):shutil.copyfile(p,Q/'observer'/p.name)
for name in ['prepare.py','admit_target.py']:
 p=Q/name;s=p.read_text();assert 'rdtsc-host-worker-diagnostic-v1-cold-v1'in s;p.write_text(s.replace('rdtsc-host-worker-diagnostic-v1-cold-v1','rdtsc-host-worker-diagnostic-v2-cold-v1'))
s=(P/'execute_qualification_v2.py').read_text();assert "Q=N/'qualification-v2'"in s;write('execute_phases.py',s.replace("Q=N/'qualification-v2'","Q=N/'qualification-v1'"));shutil.copyfile(P/'RUN-ORDER.json',N/'RUN-ORDER.json')
setup=json.loads((O/'SETUP.json').read_text());setup['source_root']=str(N/'source');setup['target']=str(R/'target/rdtsc-host-worker-diagnostic-v2-cold-v1');setup['source_files']=[dict(relative=C,file=rec(N/'source'/C))];setup['lock']=rec(N/'source/Cargo.lock');write('qualification-v1/SETUP.json',setup)
manifest=json.loads((O/'source-manifest.json').read_text())
for row in manifest:
 if row['path']==C:row['sha256']=rec(N/'source'/C)['sha256']
write('qualification-v1/source-manifest.json',manifest)
delta='';origins=[]
for p in Q.glob('*.py'):
 ast.parse(p.read_text());old=O/p.name;delta+=diff(old.read_text(),p.read_text(),'qualification/'+p.name);origins.append(dict(before=rec(old),after=rec(p),unchanged=old.read_bytes()==p.read_bytes()))
delta+=diff((P/'execute_qualification_v2.py').read_text(),(N/'execute_phases.py').read_text(),'execute_phases.py')
write('CALLER-DELTA.patch',delta);write('qualification-v1/RUNNER_ORIGINS.json',dict(sources=origins,predecessor=rec(O/'RUNNER_ORIGINS.json'),delta=rec(N/'CALLER-DELTA.patch'),observer=rec(Q/'observer/observer.py')))
write('SYMBOL-AUDIT.md','''New diagnostic symbols were checked before this successor freeze. reverie-kvm/Cargo.toml declares goblin, reverie-core (as reverie), futures, libc and the existing KVM crates; it does not declare serde_json. The successor uses no serde_json or new dependency. goblin::elf::Elf::parse is already used in this exact static_elf.rs fixture file at the lazy-loader control. Reverie Guest::regs, Guest::send_rpc, Tool::subscriptions and GlobalTool::receive_rpc are existing imported APIs with adjacent examples. Existing tuple RPC examples use Self::Request; Pid::as_raw and Rdtsc’s Copy/Debug derivations already support these records. All new formatting, String, Vec, iter, PathBuf, Command/status/argv access, environment, read/write and canonicalize operations use std and existing imports. No new import or Cargo manifest/lock change was made. This is source inspection, not a claim that the successor has compiled.\n''')
write('REPORT.md','''Diagnostic V2 fixes only the V1 recording-code compilation defect. Seven E0433 errors plus the compiler failure-note and actual raw101/accepted=false receipt remain in V1/compile-refusal-v1. No V1 diagnostic guest ran. This successor replaces unavailable serde_json with standard-library records, with no dependency addition or production edit.

Every callback is retained as a TSV row containing exact decimal PID, Debug enum type and hexadecimal u64 RIP; there is no filtering. Actual GCC arguments are retained as a NUL-terminated NUL-delimited byte sequence after requiring each argument’s existing UTF-8 spelling; argv cannot contain NUL. Actual cwd is retained separately. Status preserves optional raw code, success bool and full ExitStatus display, and raw compiler stdout/stderr stay separate. PT_INTERP and its canonical path have separate exact text files, with the actual interpreter bytes still retained. Only recording representation changed; capture points, all Tool behavior, original C and the exact two-call assertion are identical to diagnostic V1. Source reviewers must not treat diagnostic value as a passing test when the unchanged assertion fails.

The actual source symbol/dependency audit is in SYMBOL-AUDIT.md. The prior unexecuted scope-label correction and packaging refusals remain preserved. The candidate’s failed31-control run is unchanged. This packet is prepared only; no successor compiler or test has run.\n''')
plan=(P/'PLAN.md').read_text().replace('Finite diagnostic execution plan','Finite diagnostic V2 execution plan');plan+='\nUse this packet’s execute_phases.py and qualification-v1. The fresh target spelling is v2; all five phases, one selector and limits remain identical. Read callbacks.tsv, compiler.argv (NUL-delimited), compiler.cwd, compiler-status.txt, interpreter-path and interpreter-resolved-path as the exact records; no JSON dependency is assumed. Preserve the V1 raw101 compilation failure separately.\n';write('PLAN.md',plan)
write('TARGET.json',dict(status='PREPARED ONLY',predecessor=rec(P/'TARGET-CALLER-V2.json'),refusal=rec(P/'compile-refusal-v1/READBACK.json'),source=rec(N/'SOURCE-MANIFEST.json'),delta=rec(N/'DELTA.patch'),full_diagnostic_patch=rec(N/'SOURCE.patch'),caller_delta=rec(N/'CALLER-DELTA.patch'),setup=rec(Q/'SETUP.json'),selectors=rec(Q/'SELECTORS.json'),report=rec(N/'REPORT.md'),plan=rec(N/'PLAN.md'),symbol_audit=rec(N/'SYMBOL-AUDIT.md'),production_unchanged=True,host_fixture_body_identical_to_diagnostic_v1=True,original_two_call_assertion_unchanged=True,all_other_source_entries_unchanged=True))
files=[rec(p)for p in N.rglob('*')if p.is_file()or p.is_symlink()]
for p in [P/'TARGET-CALLER-V2.json',P/'READBACK.json',P/'compile-refusal-v1/READBACK.json',P/'compile-refusal-v1/INPUTS.json',P/'compile-refusal-v1/DIAGNOSTICS.json',P/'compile-refusal-v1/REPORT.md']:files.append(rec(p))
write('INPUTS.json',dict(records=files))
for row in files:assert rec(row['path'])==row
write('READBACK.json',dict(target=rec(N/'TARGET.json'),inputs=rec(N/'INPUTS.json'),records=len(files),source_entries=len(rows),execution=False))
print(json.dumps({n:rec(N/n)for n in ['TARGET.json','DELTA.patch','CALLER-DELTA.patch','READBACK.json']},indent=2))
