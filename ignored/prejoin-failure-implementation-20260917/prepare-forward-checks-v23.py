from pathlib import Path
import ast,hashlib,json,runpy
area=Path.cwd()/'ignored/prejoin-failure-implementation-20260917';source=area/'source-v23-preparation'
def digest(path):
 h=hashlib.sha256()
 with Path(path).open('rb') as f:
  for b in iter(lambda:f.read(1024*1024),b''):h.update(b)
 return h.hexdigest()
def bind(path):
 p=Path(path);s=p.stat();return dict(path=str(p),resolved_path=str(p.resolve(strict=True)),bytes=s.st_size,mode=s.st_mode&0o7777,sha256=digest(p))
def put(path,obj):
 with path.open('x') as f:json.dump(obj,f,indent=2);f.write('\n')
extra=[source/'committed-copies.json',source/'prior-evidence.json',source/'REVIEW-PROMPT.md',area/'append-v22-preparation/FORWARD-READBACK.json',area/'append-v22-preparation/COMMIT-READBACK.json',area/'qualification-build-v5/RESULT.json',area/'qualification-build-v5/run-1/retained-elf-v22/binding.json',area/'qualification-build-v5/run-1/retained-elf-v22/lib',area/'qualification-build-v5/run-1/retained-elf-v22/static-elf',area/'qualification-execution-v5/UNION-RESULT.json',area/'qualification-execution-v5/REPORT.md']
records=[]
for previous,current,caller in [('qualification-build-v5','qualification-build-v6','build.py'),('lint-v13','lint-v14','launch.py')]:
 old=area/previous;new=area/current;new.mkdir();plan=json.loads((old/'plan.json').read_text())
 old_source=str(area/'source-v22-preparation')
 for key in ['run_root','observer_root','tmpdir']:plan[key]=plan[key].replace(previous,current)
 plan['environment_fixed']['TMPDIR']=plan['environment_fixed']['TMPDIR'].replace(previous,current)
 plan['execution']=['/usr/bin/python3','-B',str(new/caller)]
 plan['source_binding']=str(source/'binding.json');plan['source_manifest']=str(source/'tracked-source-manifest.json');plan['source_base']='526c21cf06ef9e5098ec9002b93e40e2022e798f';plan['source_head']='9db60ab95587d4cb5e0438dfeca409471eb9baf5'
 for row in plan['inputs']:
  if row['path'].startswith(old_source+'/'):
   replacement=bind(row['path'].replace(old_source,str(source)));row.clear();row.update(replacement)
  else:assert bind(row['path'])==row,row['path']
 paths={row['path'] for row in plan['inputs']}
 for path in extra:
  if str(path) not in paths:plan['inputs'].append(bind(path));paths.add(str(path))
 plan['stages']=json.loads(json.dumps(plan['stages']).replace(previous,current))
 if current=='lint-v14':
  prior_all=json.loads((area/'lint-v5/plan.json').read_text())['stages'][1]['payload'];step=plan['stages'][1]
  step['payload']=prior_all;step['argv']=step['argv'][:step['argv'].index('--log-bytes')+2]+prior_all
  plan['scope']='Read-only workspace formatting and --workspace --all-targets --all-features Clippy on committed forward head 9db60ab9, source v23. No native, VM or guest execution. Default-feature v22 evidence remains distinct.'
 else:plan['scope']='Compile the final committed forward head 9db60ab9, source v23, with the unchanged default-feature reverie-kvm library/static_elf targets and two inventories. All 44 native, 4 VM and 22 static identities remain required. Compare actual emitted ELFs and dependency inputs with retained qualified v22 before deciding affected repeated execution. No test execution in this plan.'
 for s in plan['stages']:assert s['payload']==s['argv'][s['argv'].index('--log-bytes')+2:]
 for key in ['run_root','observer_root']:assert not Path(plan[key]).exists()
 put(new/'plan.json',plan)
 text=(old/caller).read_text();assert text.count(digest(old/'plan.json'))==1;text=text.replace(digest(old/'plan.json'),digest(new/'plan.json'));ast.parse(text)
 with (new/caller).open('x') as f:f.write(text)
 functions=runpy.run_path(plan['helpers']['path'],run_name='preflight_only');functions['check_inputs'](plan)
 record={'plan':bind(new/'plan.json'),'caller':bind(new/caller),'source_binding':bind(source/'binding.json'),'execution':plan['execution'],'inputs':len(plan['inputs']),'status':'Prepared only; no execution.','unchanged_limits':True}
 put(new/'reservation.json',record);records.append(record)
print(json.dumps(records,indent=2))
