from pathlib import Path
import ast,hashlib,json,runpy
area=Path.cwd()/'ignored/prejoin-failure-implementation-20260917';build=area/'qualification-build-v6';prior=area/'qualification-build-v5';source=area/'source-v23-preparation'
def digest(path):
 h=hashlib.sha256()
 with Path(path).open('rb') as f:
  for b in iter(lambda:f.read(1024*1024),b''):h.update(b)
 return h.hexdigest()
def bind(path):
 p=Path(path);s=p.stat();return dict(path=str(p),resolved_path=str(p.resolve(strict=True)),bytes=s.st_size,mode=s.st_mode&0o7777,sha256=digest(p))
def put(path,obj):
 with path.open('x') as f:json.dump(obj,f,indent=2);f.write('\n')
new_artifacts=json.loads((build/'run-1/compiled-executables.json').read_text());old_artifacts=json.loads((prior/'run-1/compiled-executables.json').read_text());assert set(new_artifacts)=={'lib','static-elf'}
comparison={'old_source_binding':str(area/'source-v22-preparation/binding.json'),'new_source_binding':str(source/'binding.json'),'new_head':'9db60ab95587d4cb5e0438dfeca409471eb9baf5','Cargo_lock_sha256':digest(Path.cwd()/'Cargo.lock'),'candidate_files_and_patch_identical':True,'upstream_dependency_source_changed':'reverie/src/backend_stats.rs; all other upstream changes retained as recorded in FORWARD-READBACK.json.','executables':{},'decision':'Both actual ELFs changed; execute unchanged 44 native plus original 4 VM and 22 static methods. No execution is carried forward solely by source similarity.'}
for kind in new_artifacts:
 a,b=old_artifacts[kind],new_artifacts[kind];assert a['sha256']!=b['sha256']
 old_names=json.loads((prior/'run-1'/('list-'+kind+'-inventory.json')).read_text())['names'];new_names=json.loads((build/'run-1'/('list-'+kind+'-inventory.json')).read_text())['names'];assert old_names==new_names
 comparison['executables'][kind]={'old':a,'new':b,'size_increase':b['bytes']-a['bytes'],'same_inventory':True,'inventory_count':len(new_names),'old_retained':bind(prior/'run-1/retained-elf-v22'/kind),'new_retained':bind(build/'run-1/retained-elf-v23'/kind)}
put(build/'EXECUTABLE-COMPARISON.json',comparison)
extra=[build/'plan.json',build/'build.py',build/'RESULT.json',build/'REPORT.md',build/'EXECUTABLE-COMPARISON.json',build/'run-1/summary.json',build/'run-1/launch.json',build/'run-1/compiled-executables.json',build/'run-1/retained-elf-v23/binding.json',build/'run-1/retained-elf-v23/lib',build/'run-1/retained-elf-v23/static-elf']
for name in ['compile','list-lib','list-static-elf']:
 stage=next(x for x in json.loads((build/'plan.json').read_text())['stages'] if x['name']==name)
 extra.extend([Path(stage['out'])/'result.json',Path(stage['out'])/'stdout',Path(stage['out'])/'stderr',build/'run-1'/(name+'-readback.json'),build/'run-1'/(name+'-service-post.json')])
extra.extend([area/'lint-v14/RESULT.json',area/'lint-v14/REPORT.md',area/'cargo-v16/plan.json',area/'qualification-execution-v5/UNION-RESULT.json'])

def replace_inputs(plan):
 updated=[];known=set()
 for row in plan['inputs']:
  path=row['path'];new=path.replace(str(area/'source-v22-preparation'),str(source))
  if new.startswith(str(prior)+'/'):new=new.replace(str(prior),str(build)).replace('retained-elf-v22','retained-elf-v23')
  if new in [a['path'] for a in new_artifacts.values()]:row=bind(new)
  elif new!=path:row=bind(new)
  else:assert bind(path)==row,path
  if row['path'] not in known:updated.append(row);known.add(row['path'])
 for path in extra:
  if str(path) not in known:updated.append(bind(path));known.add(str(path))
 plan['inputs']=updated

# Original full 26-method qualification; the earlier partial continuation is
# not the selector source for a fresh final-head attempt.
old=area/'qualification-execution-v4';new=area/'qualification-execution-v6';new.mkdir();plan=json.loads((old/'plan.json').read_text())
for key in ['run_root','observer_root','tmpdir']:plan[key]=plan[key].replace('qualification-execution-v4','qualification-execution-v6')
plan['environment_fixed']['TMPDIR']=plan['environment_fixed']['TMPDIR'].replace('qualification-execution-v4','qualification-execution-v6');plan['execution']=['/usr/bin/python3','-B',str(new/'launch.py')]
plan['stages']=json.loads(json.dumps(plan['stages']).replace('qualification-execution-v4','qualification-execution-v6'))
plan['source_binding']=str(source/'binding.json');plan['source_manifest']=str(source/'tracked-source-manifest.json');plan['source_base']='526c21cf06ef9e5098ec9002b93e40e2022e798f';plan['source_head']='9db60ab95587d4cb5e0438dfeca409471eb9baf5';plan['artifacts']=new_artifacts
for kind in ['lib','static-elf']:plan['inventories'][kind]={'path':str(build/'run-1'/('list-'+kind+'-inventory.json')),'count':453 if kind=='lib' else 288}
for stage in plan['stages']:
 artifact=new_artifacts[stage['artifact']];stage['payload'][stage['payload'].index('--executable')+1]=artifact['path'];stage['payload'][stage['payload'].index('--sha256')+1]=artifact['sha256'];stage['argv']=stage['argv'][:stage['argv'].index('--log-bytes')+2]+stage['payload']
replace_inputs(plan);plan['scope']='First final-head execution of the unchanged original four VM and 22 static Tool methods, on committed 9db60ab9/source v23 and actual rebuilt ELFs. Original order, assertions, hardware admission, bounds and first-failure stop remain. Earlier v22 first attempts and accounting-refusal retry are preserved separately. No Hermit strict INFO, repeat determinism or canonical parity claim.'
for key in ['run_root','observer_root']:assert not Path(plan[key]).exists()
put(new/'plan.json',plan);text=(old/'launch.py').read_text().replace(digest(old/'plan.json'),digest(new/'plan.json'));ast.parse(text)
with (new/'launch.py').open('x') as f:f.write(text)
helpers=runpy.run_path(plan['helpers']['path'],run_name='preflight_only');helpers['check_inputs'](plan)
for a in new_artifacts.values():helpers['check_executable'](a)
put(new/'reservation.json',{'plan':bind(new/'plan.json'),'caller':bind(new/'launch.py'),'execution':plan['execution'],'inputs':len(plan['inputs']),'selected_count':26,'status':'Prepared, not executed.'})

# Reuse the already compiled and inventoried library for the exact native
# population, in one separately bounded non-VM invocation.
native=area/'native-forward-v1';native.mkdir();bp=json.loads((build/'plan.json').read_text());np=json.loads((area/'cargo-v16/plan.json').read_text());p=dict(bp)
p['run_root']=str(native/'run-1');p['tmpdir']=str(native/'run-1/tmp');p['observer_root']=bp['observer_root'].replace('reverie-qualification-build-v6','reverie-native-forward-v1');p['execution']=['/usr/bin/python3','-B',str(native/'launch.py')];p['environment_fixed']=dict(bp['environment_fixed'],TMPDIR=p['tmpdir']);p['selected_tests']=np['selected_tests'];p['required_count']=44;p['artifact']=new_artifacts['lib'];p['inventory']={'path':str(build/'run-1/list-lib-inventory.json'),'count':453}
step=dict(np['stages'][2]);step['out']=p['observer_root']+'/native';step['payload']=[p['artifact']['path'],'--exact',*p['selected_tests'],'--test-threads=1','--nocapture'];step['argv']=step['argv'][:step['argv'].index('--log-bytes')+2]+step['payload'];step['argv'][step['argv'].index('--out')+1]=step['out'];p['stages']=[step]
for key in ['expected_artifacts','expected_tests']:p.pop(key,None)
p['scope']='One exact 44-method native invocation of the actual final-head KVM library ELF. Existing compile and full 453 inventory are separately bound. No VM or guest instruction is selected and no guest parity claim follows.'
replace_inputs(p)
for key in ['run_root','observer_root']:assert not Path(p[key]).exists()
put(native/'plan.json',p)
text='''#!/usr/bin/python3
"""Run exactly the retained native population on an authenticated built ELF."""
from pathlib import Path
import hashlib,json,os,re,runpy,subprocess
HERE=Path(__file__).resolve().parent
PLAN_SHA256=PLAN_HASH

def main():
    raw=(HERE/'plan.json').read_bytes()
    assert hashlib.sha256(raw).hexdigest()==PLAN_SHA256
    plan=json.loads(raw)
    helper=Path(plan['helpers']['path'])
    assert hashlib.sha256(helper.read_bytes()).hexdigest()==plan['helpers']['sha256']
    functions=runpy.run_path(str(helper),run_name='reviewed_helpers_only')
    require,digest,read_bounded,write_new,check_executable=(functions[k] for k in ['require','digest','read_bounded','write_new','check_executable'])
    require(plan['execution']==['/usr/bin/python3','-B',str(HERE/'launch.py')],'wrong caller')
    require([s['name'] for s in plan['stages']]==['native'],'unexpected stage')
    selected=plan['selected_tests'];require(plan['required_count']==44 and len(selected)==44 and len(set(selected))==44,'changed native population')
    prior=json.loads(read_bounded(Path(plan['source_root'])/'ignored/prejoin-failure-implementation-20260917/cargo-v16/plan.json',1024**2))
    require(selected==prior['selected_tests'],'original native identities changed')
    inventory=json.loads(read_bounded(plan['inventory']['path'],1024**2))
    require(len(inventory['names'])==453 and inventory['count']==453 and len(set(inventory['names']))==453,'inventory changed')
    require(all(inventory['names'].count(name)==1 for name in selected),'missing selected native')
    stage=plan['stages'][0];artifact=plan['artifact']
    require(stage['payload']==[artifact['path'],'--exact',*selected,'--test-threads=1','--nocapture'],'changed native arguments')
    require(stage['argv'][stage['argv'].index('--log-bytes')+2:]==stage['payload'],'payload/argv mismatch')
    root=Path(plan['run_root'])
    for path in [root,Path(plan['observer_root'])]:require(not path.exists() and not path.is_symlink(),'retain prior output: '+str(path))
    functions['check_inputs'](plan);check_executable(artifact)
    root.mkdir(mode=0o700);Path(plan['tmpdir']).mkdir(mode=0o700)
    environment={key:os.environ[key] for key in plan['environment_keys'] if key in os.environ};environment.update(plan['environment_fixed'])
    write_new(root/'launch.json',dict(plan_sha256=PLAN_SHA256,caller_sha256=digest(__file__),execution=plan['execution'],cwd=str(Path.cwd()),environment=environment,source_binding=plan['source_binding'],artifact=artifact,selected=selected,scope=plan['scope']))
    try:
        functions['check_inputs'](plan);check_executable(artifact)
        write_new(root/'native-dispatch.json',dict(argv=stage['argv'],cwd=stage['cwd'],artifact=artifact,selected=selected))
        with (root/'native-observer.stdout').open('xb') as stdout,(root/'native-observer.stderr').open('xb') as stderr:
            process=subprocess.run(stage['argv'],cwd=stage['cwd'],env=environment,stdin=subprocess.DEVNULL,stdout=stdout,stderr=stderr)
        result_path=Path(stage['out'])/'result.json';result=json.loads(read_bounded(result_path,1024**2))
        write_new(root/'native-readback.json',dict(observer_exit=process.returncode,result_path=str(result_path),result_sha256=digest(result_path),result=result))
        functions['require_terminal'](result,process.returncode,stage,root,environment)
        raw_stdout=read_bounded(Path(stage['out'])/'stdout',stage['reader_limit_bytes']);raw_stderr=read_bounded(Path(stage['out'])/'stderr',stage['reader_limit_bytes']);text=raw_stdout.decode()
        summaries=re.findall(r'^test result: ok\\. (\\d+) passed; (\\d+) failed; (\\d+) ignored; (\\d+) measured; (\\d+) filtered out;',text,re.M)
        require(summaries==[('44','0','0','0','409')],'wrong native summary')
        passed=re.findall(r'^test (.+) \\.\\.\\. ok$',text,re.M)
        require(sorted(passed)==sorted(selected),'wrong passing native identities')
        functions['check_inputs'](plan);check_executable(artifact)
        record=dict(status='passed',selected=selected,summary_counts=summaries[0],stdout_sha256=hashlib.sha256(raw_stdout).hexdigest(),stderr_sha256=hashlib.sha256(raw_stderr).hexdigest(),cpu_nsec=result['final_accounting']['cpu_usage_nsec'],wall_seconds=result['elapsed_seconds'],scope=plan['scope'])
        write_new(root/'summary.json',record);print(json.dumps(record))
    except Exception as error:
        write_new(root/'summary.json',dict(status='failed',error=str(error),scope=plan['scope']))
        raise

if __name__=='__main__':main()
'''.replace('PLAN_HASH',repr(digest(native/'plan.json')))
ast.parse(text)
with (native/'launch.py').open('x') as f:f.write(text)
helpers['check_inputs'](p);helpers['check_executable'](p['artifact'])
put(native/'reservation.json',{'plan':bind(native/'plan.json'),'caller':bind(native/'launch.py'),'execution':p['execution'],'inputs':len(p['inputs']),'selected_count':44,'status':'Prepared, not executed.'})
print(json.dumps({'native':json.loads((native/'reservation.json').read_text()),'qualification':json.loads((new/'reservation.json').read_text()),'comparison':bind(build/'EXECUTABLE-COMPARISON.json')},indent=2))
