from pathlib import Path
import copy,difflib,hashlib,json,runpy,subprocess,os,time
A=Path(__file__).resolve().parent.parent;root=A.parent.parent;oldroot=A/'futex-full-library-v1';dest=A/'futex-full-library-v3';dest.mkdir()
sha=lambda p:hashlib.sha256(Path(p).read_bytes()).hexdigest()
def write(p,v):
 with Path(p).open('x') as f:json.dump(v,f,indent=2);f.write('\n')
def entry(p):
 p=Path(p);s=p.stat();return dict(path=str(p),resolved_path=str(p.resolve(strict=True)),bytes=s.st_size,mode=s.st_mode&0o7777,sha256=sha(p))
build=A/'qualification-build-v7';bp=json.loads((build/'plan.json').read_bytes());old=json.loads((oldroot/'plan.json').read_bytes());p=copy.deepcopy(old)
oldout=p['observer_root'];newout=oldout.replace('futex-full-library-v1','futex-full-library-v3')
def replace(v):
 if isinstance(v,str):return v.replace(str(oldroot/'run-1'),str(dest/'run-1')).replace(oldout,newout)
 if isinstance(v,list):return [replace(x) for x in v]
 if isinstance(v,dict):return {k:replace(x) for k,x in v.items()}
 return v
p=replace(p)
for k in ['source_base','source_head','source_binding','source_manifest']:p[k]=bp[k]
p['execution']=['/usr/bin/python3','-B',str(dest/'launch.py')]
p['artifacts']={'lib':json.loads((build/'run-1/compiled-executables.json').read_bytes())['lib']}
inv=build/'run-1/list-lib-inventory.json';names=json.loads(inv.read_bytes())['names'];assert len(names)==454
prior=json.loads((A/'qualification-build-v6/run-1/list-lib-inventory.json').read_bytes())['names'];new='vm::tests::cancelled_child_cleanup_retains_real_errors_and_ordinary_cancellation';assert set(names)==set(prior)|{new}
p['inventories']={'lib':{'path':str(inv),'count':len(names)}};p['registered_tests']=names;p['registered_count']=len(names)
p['previous_full_inventory']=old['inventories']['lib']['path'];p['new_library_methods']=[new]
p['previously_landed_base']=p.pop('landed_commit');p['prior_landing_tree_identity_record']=p.pop('landing_tree_identity_record')
p['scope']='First full serial library execution of final committed 12d4ce8c cleanup correction. All 453 prior methods plus the actual one new regression method; original 427 retained. Actual API 12 admission and REVERIE_REQUIRE_KVM=1 required. Same original resource limits and every assertion/deadline. Prior 450/3 and historical 426/1 remain retained. No separate selected-native/four-VM rerun, Hermit execution or canonical parity claim.'
s=p['stage'];s['payload'][s['payload'].index('--sha256')+1]=p['artifacts']['lib']['sha256'];s['argv']=s['argv'][:s['argv'].index('--log-bytes')+2]+s['payload']
inputs={r['path']:r for r in old['inputs']}
for r in bp['inputs']:inputs[r['path']]=r
changed=[]
for path,r in list(inputs.items()):
 if sha(path)!=r['sha256']:
  assert path in [v['path'] for v in json.loads((build/'run-1/compiled-executables.json').read_bytes()).values()];changed.append({'prior':r,'current':entry(path)});inputs[path]=entry(path)
extras=[oldroot/'plan.json',oldroot/'launch.py',oldroot/'RESULT.json',oldroot/'REPORT.md',oldroot/'TERMINAL-READBACK.json',oldroot/'run-1/native-outcomes.json',oldroot/'run-1/source-after.json',build/'plan.json',build/'build.py',build/'RESULT.json',build/'REPORT.md',build/'TERMINAL.json',A/'lint-v15/RESULT.json',A/'lint-v15/REPORT.md',A/'lint-v15/TERMINAL.json']
extras += [x for x in (build/'run-1').iterdir() if x.is_file()]
extras += [build/'run-1/retained-elf-v25'/x for x in ['binding.json','lib','static-elf']]
for st in bp['stages']:
 extras += [Path(st['out'])/x for x in ['stdout','stderr','result.json']]
for x in extras:inputs[str(x)]=entry(x)
p['inputs']=list(inputs.values());write(dest/'plan.json',p)
text=(oldroot/'launch.py').read_text().replace(sha(oldroot/'plan.json'),sha(dest/'plan.json'))
text=text.replace("plan['registered_count'] == 453", "plan['registered_count'] == 454")
needle="    require(len(old['registered_tests']) == len(set(old['registered_tests'])) == 427 and\n"
addition="    previous = json.loads(read_bounded(plan['previous_full_inventory'], 1024**2))['names']\n    require(len(previous) == len(set(previous)) == 453 and\n            plan['new_library_methods'] == [\n                'vm::tests::cancelled_child_cleanup_retains_real_errors_and_ordinary_cancellation'] and\n            set(names) == set(previous) | set(plan['new_library_methods']),\n            'prior full population or new regression changed')\n"
assert needle in text;text=text.replace(needle,addition+needle)
text=text.replace("landed_commit=plan['landed_commit']", "previously_landed_base=plan['previously_landed_base']")
with (dest/'launch.py').open('x') as f:f.write(text)
with (dest/'caller-increment.patch').open('x') as f:f.writelines(difflib.unified_diff((oldroot/'launch.py').read_text().splitlines(True),text.splitlines(True),fromfile='full-v1/launch.py',tofile='full-v3/launch.py'))
write(dest/'PREPARATION.json',dict(source_head=p['source_head'],plan_sha256=sha(dest/'plan.json'),caller_sha256=sha(dest/'launch.py'),actual_inventory_count=len(names),changed_prior_live_inputs=changed,first_attempt=True,scope=p['scope']))
print(json.dumps({'plan':sha(dest/'plan.json'),'caller':sha(dest/'launch.py'),'inputs':len(p['inputs'])}))
