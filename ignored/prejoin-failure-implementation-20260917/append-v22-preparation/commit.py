from pathlib import Path
import datetime,hashlib,json,os,runpy,subprocess
slot=Path.cwd();area=slot/'ignored/prejoin-failure-implementation-20260917';out=area/'append-v22-preparation';oldhead='d99853df1ab677863f149e6c81dfa2d1147f886d'
def git(*args):return subprocess.check_output(['git',*args])
def digest(data):return hashlib.sha256(data).hexdigest()
def put(name,obj):
 with (out/name).open('x') as f:json.dump(obj,f,indent=2);f.write('\n')
def run(name,argv):
 with (out/(name+'.stdout')).open('xb') as stdout,(out/(name+'.stderr')).open('xb') as stderr:
  result=subprocess.run(argv,stdout=stdout,stderr=stderr)
 record={'argv':argv,'exit':result.returncode,'stdout_sha256':digest((out/(name+'.stdout')).read_bytes()),'stderr_sha256':digest((out/(name+'.stderr')).read_bytes())};put(name+'.json',record)
 if result.returncode:raise RuntimeError(record)
 return record
plan=json.loads((area/'qualification-execution-v5/plan.json').read_text());f=runpy.run_path(plan['helpers']['path'],run_name='check_only');f['check_inputs'](plan)
for art in plan['artifacts'].values():f['check_executable'](art)
assert json.loads((area/'qualification-execution-v5/UNION-RESULT.json').read_text())['accepted_original_method_count']==26
assert git('rev-parse','HEAD').decode().strip()==oldhead
assert git('branch','--show-current').decode().strip()=='codex/kvm-proc-fd-identity-20260917'
assert not git('diff','--cached','--name-only').strip()
for name in ['rebase-merge','rebase-apply']:assert not Path(git('rev-parse','--git-path',name).decode().strip()).exists()
owned=['reverie-kvm/src/error.rs','reverie-kvm/src/executor.rs','reverie-kvm/src/failure.rs','reverie-kvm/src/runtime.rs','reverie-kvm/src/runtime/failure_tests.rs','reverie-kvm/src/runtime/native_test_support.rs','reverie-kvm/src/vm.rs','reverie/src/tool.rs']
assert git('diff','--name-only').decode().splitlines()==owned
run('diff-check',['git','diff','--check'])
append=git('diff','--no-ext-diff','--binary',oldhead)
with (out/'append.patch').open('xb') as file:file.write(append)
manifest=json.loads(Path(plan['source_manifest']).read_text());expected={}
for row in manifest:
 path=slot/row['path']
 if row['mode']=='160000':obj=row['git_blob']
 else:
  data=os.fsencode(os.readlink(path)) if row['mode']=='120000' else path.read_bytes()
  assert digest(data)==row['sha256'],row['path']
  obj=hashlib.sha1(b'blob '+str(len(data)).encode()+b'\0'+data).hexdigest()
 expected[row['path']]={'mode':row['mode'],'object':obj}
put('expected-tree.json',expected)
terminals=[]
for version in ['cargo-v16','lint-v13','qualification-build-v5','qualification-execution-v4','qualification-execution-v5']:
 p=json.loads((area/version/'plan.json').read_text())
 for stage in p['stages']:
  file=Path(stage['out'])/'result.json'
  if not file.exists():continue
  result=json.loads(file.read_text());unit=result['authenticated_service']['unit'];argv=['systemctl','--user','show',unit,'--property=LoadState,ActiveState,SubState,MainPID,ControlGroup'];done=subprocess.run(argv,capture_output=True,text=True,timeout=5);props=dict(line.split('=',1) for line in done.stdout.splitlines() if '=' in line)
  assert done.returncode==0 and props['ActiveState']=='inactive' and props['MainPID']=='0' and props['ControlGroup']=='',unit
  terminals.append({'attempt':version,'unit':unit,'argv':argv,'exit':done.returncode,'stdout':done.stdout,'stderr':done.stderr})
ancestors=set();pid=os.getpid()
while pid and pid not in ancestors:
 ancestors.add(pid)
 try:pid=int(next(x for x in Path('/proc',str(pid),'status').read_text().splitlines() if x.startswith('PPid:')).split()[1])
 except (FileNotFoundError,ProcessLookupError):break
matches=[];target=str(slot/'target')
for ent in Path('/proc').iterdir():
 if not ent.name.isdecimal() or int(ent.name) in ancestors:continue
 try:
  argv=(ent/'cmdline').read_bytes().replace(b'\0',b' ').decode(errors='replace');cwd=str((ent/'cwd').readlink())
  if target in argv or cwd.startswith(target):matches.append({'pid':int(ent.name),'argv':argv,'cwd':cwd})
 except (FileNotFoundError,ProcessLookupError,PermissionError):continue
put('before-commit.json',{'time':datetime.datetime.now(datetime.timezone.utc).isoformat(),'head':oldhead,'owned':owned,'append_sha256':digest(append),'source_binding_sha256':digest(Path(plan['source_binding']).read_bytes()),'expected_tree_sha256':digest((out/'expected-tree.json').read_bytes()),'target_matches':matches,'terminal_readbacks':terminals,'message_sha256':digest((out/'commit-message.txt').read_bytes())})
assert not matches,matches
run('stage',['git','add','--',*owned])
assert git('diff','--cached','--name-only').decode().splitlines()==owned
run('commit',['git','commit','-F',str(out/'commit-message.txt')])
head=git('rev-parse','HEAD').decode().strip();tree=git('rev-parse','HEAD^{tree}').decode().strip();assert git('rev-parse','HEAD^').decode().strip()==oldhead
actual={}
for line in git('ls-tree','-rz','--full-tree',head).split(b'\0'):
 if not line:continue
 meta,path=line.split(b'\t',1);mode,kind,obj=meta.decode().split();actual[path.decode()]={'mode':mode,'object':obj}
assert actual==expected
assert not git('diff','--name-only').strip() and not git('diff','--cached','--name-only').strip()
message=git('show','-s','--format=%B',head).decode();assert message.rstrip()==(out/'commit-message.txt').read_text().rstrip()
put('COMMIT-READBACK.json',{'head':head,'tree':tree,'parent':oldhead,'branch':git('branch','--show-current').decode().strip(),'all_source_entries_equal':len(expected),'changed_paths':owned,'source_binding_sha256':digest(Path(plan['source_binding']).read_bytes()),'expected_tree_sha256':digest((out/'expected-tree.json').read_bytes()),'message_sha256':digest(message.encode()),'tracked_clean':True,'public_push':False})
print((out/'COMMIT-READBACK.json').read_text());print('READBACK_SHA256',digest((out/'COMMIT-READBACK.json').read_bytes()))
