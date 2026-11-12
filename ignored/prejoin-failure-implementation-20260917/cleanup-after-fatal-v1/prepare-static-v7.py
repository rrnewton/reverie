from pathlib import Path
import copy,difflib,hashlib,json
A=Path(__file__).resolve().parent.parent;oldroot=A/'qualification-execution-v6';dest=A/'qualification-execution-v7';build=A/'qualification-build-v7';full=A/'futex-full-library-v3'
sha=lambda p:hashlib.sha256(Path(p).read_bytes()).hexdigest()
def write(p,v):
 with Path(p).open('x') as f:json.dump(v,f,indent=2);f.write('\n')
def entry(p):
 p=Path(p);s=p.stat();return dict(path=str(p),resolved_path=str(p.resolve(strict=True)),bytes=s.st_size,mode=s.st_mode&0o7777,sha256=sha(p))
assert json.loads((full/'run-1/summary.json').read_bytes())['status']=='passed'
dest.mkdir();bp=json.loads((build/'plan.json').read_bytes());old=json.loads((oldroot/'plan.json').read_bytes());p=copy.deepcopy(old)
oldout=p['observer_root'];newout=oldout.replace('qualification-execution-v6','qualification-execution-v7')
def replace(v):
 if isinstance(v,str):return v.replace(str(oldroot/'run-1'),str(dest/'run-1')).replace(oldout,newout)
 if isinstance(v,list):return [replace(x) for x in v]
 if isinstance(v,dict):return {k:replace(x) for k,x in v.items()}
 return v
p=replace(p)
for k in ['source_base','source_head','source_binding','source_manifest']:p[k]=bp[k]
p['execution']=['/usr/bin/python3','-B',str(dest/'launch.py')]
emitted=json.loads((build/'run-1/compiled-executables.json').read_bytes());p['artifacts']={'static-elf':emitted['static-elf']}
inv=build/'run-1/list-static-elf-inventory.json';names=json.loads(inv.read_bytes())['names'];assert len(names)==288
p['inventories']={'static-elf':{'path':str(inv),'count':len(names)}}
p['selected_tests']={'static-elf':old['selected_tests']['static-elf']}
p['stages']=[s for s in p['stages'] if s['artifact']=='static-elf'];assert len(p['stages'])==22
for s in p['stages']:
 s['payload'][s['payload'].index('--sha256')+1]=emitted['static-elf']['sha256'];s['argv']=s['argv'][:s['argv'].index('--log-bytes')+2]+s['payload']
p['completed_full_library']={'summary':str(full/'run-1/summary.json'),'outcomes':str(full/'run-1/native-outcomes.json'),'inventory':str(build/'run-1/list-lib-inventory.json'),'count':454,'already_covered_selected_vm':old['selected_tests']['lib']}
p['scope']='First original 22 static integration methods after the complete serial 454 library pass, on final committed 12d4ce8c. The full library already executed every earlier selected native and four focused VM method; these are not repeated here. Original static names, order, ten exec modes, assertions, admission, bounds and first-failure stop remain. Earlier failures remain retained. No Hermit INFO, repeat determinism or canonical parity claim.'
inputs={r['path']:r for r in old['inputs']}
for r in bp['inputs']:inputs[r['path']]=r
for path,r in list(inputs.items()):
 if sha(path)!=r['sha256']:
  assert path in [v['path'] for v in emitted.values()];inputs[path]=entry(path)
extras=[oldroot/'plan.json',oldroot/'launch.py',oldroot/'RESULT.json',oldroot/'REPORT.md',build/'plan.json',build/'build.py',build/'RESULT.json',build/'REPORT.md',build/'TERMINAL.json',A/'lint-v15/RESULT.json',A/'lint-v15/REPORT.md',A/'lint-v15/TERMINAL.json',full/'plan.json',full/'launch.py',full/'RESULT.json',full/'REPORT.md',full/'TERMINAL.json']
extras += [x for x in (build/'run-1').iterdir() if x.is_file()]
extras += [build/'run-1/retained-elf-v25'/x for x in ['binding.json','lib','static-elf']]
extras += [x for x in (full/'run-1').iterdir() if x.is_file()]
extras += [x for x in (full/'run-1/admissions').iterdir() if x.is_file()]
for st in bp['stages']:
 extras += [Path(st['out'])/x for x in ['stdout','stderr','result.json']]
fp=json.loads((full/'plan.json').read_bytes());extras += [Path(fp['stage']['out'])/x for x in ['stdout','stderr','result.json']]
for x in extras:inputs[str(x)]=entry(x)
p['inputs']=list(inputs.values());write(dest/'plan.json',p)
text=(oldroot/'launch.py').read_text().replace(sha(oldroot/'plan.json'),sha(dest/'plan.json'))
text=text.replace("    require(len(plan['selected_tests']['lib']) == 4 and\n            len(plan['selected_tests']['static-elf']) == 22, 'changed qualification population')", "    require(set(plan['selected_tests']) == {'static-elf'} and\n            len(plan['selected_tests']['static-elf']) == 22, 'changed qualification population')")
text=text.replace("    require(len(selected) == 26 and len(set(selected)) == 26 and\n            set(selected) == set(plan['selected_tests']['lib'] + plan['selected_tests']['static-elf']),", "    require(len(selected) == 22 and len(set(selected)) == 22 and\n            selected == plan['selected_tests']['static-elf'],")
needle="    root = Path(plan['run_root'])\n"
addition="    # The unchanged complete library already covers the selected VM methods.\n    full = plan['completed_full_library']\n    prior = json.loads(read_bounded(full['summary'], 1024**2))\n    outcomes = json.loads(read_bounded(full['outcomes'], 1024**2))\n    full_names = json.loads(read_bounded(full['inventory'], 1024**2))['names']\n    require(prior['status'] == 'passed' and prior['registered_count'] == full['count'] == 454\n            and prior['outcome_counts'] == {'ok': 454, 'failed': 0, 'ignored': 0},\n            'complete library prerequisite did not pass')\n    require(len(full['already_covered_selected_vm']) == 4 and\n            all(name in full_names for name in full['already_covered_selected_vm']),\n            'earlier focused VM population missing from full execution')\n"
assert needle in text;text=text.replace(needle,addition+needle,1)
# Use the parser's actual outcome keys as recorded; this assertion is deliberately bound to that record.
counts=json.loads((full/'run-1/summary.json').read_bytes())['outcome_counts']
assert counts.get('failed',0)==0 and counts.get('ignored',0)==0
text=text.replace("{'ok': 454, 'failed': 0, 'ignored': 0}",repr(counts))
with (dest/'launch.py').open('x') as f:f.write(text)
with (dest/'caller-increment.patch').open('x') as f:f.writelines(difflib.unified_diff((oldroot/'launch.py').read_text().splitlines(True),text.splitlines(True),fromfile='execution-v6/launch.py',tofile='execution-v7/launch.py'))
write(dest/'PREPARATION.json',dict(source_head=p['source_head'],plan_sha256=sha(dest/'plan.json'),caller_sha256=sha(dest/'launch.py'),actual_inventory_count=len(names),stages=len(p['stages']),full_prerequisite_sha256=sha(full/'run-1/summary.json'),scope=p['scope']))
print(json.dumps({'plan':sha(dest/'plan.json'),'caller':sha(dest/'launch.py'),'inputs':len(p['inputs'])}))
