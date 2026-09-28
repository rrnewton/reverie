from pathlib import Path
import hashlib,json,os,runpy,shutil,sys
area=Path.cwd()/'ignored/prejoin-failure-implementation-20260917'
version,source_version,previous=sys.argv[1:]
p=area/version;plan=json.loads((p/'plan.json').read_text());helper=runpy.run_path(str(p/'launch.py'),run_name='readback_only')
helper['check_inputs'](plan)
def h(path):return helper['digest'](path)
summary=json.loads((p/'run-1/summary.json').read_text());assert summary['status']=='passed'
artifact=json.loads((p/'run-1/compiled-executable.json').read_text());helper['check_executable'](artifact)
out=p/'run-1'/('retained-elf-'+source_version);out.mkdir();target=out/'lib';shutil.copy2(artifact['path'],target)
assert os.stat(artifact['path']).st_ino!=target.stat().st_ino
binding={'source':artifact,'retained':{'path':str(target),'bytes':target.stat().st_size,'mode':target.stat().st_mode&0o7777,'sha256':h(target)},'separate_inode':True}
assert all(binding['retained'][key]==artifact[key] for key in ['bytes','mode','sha256'])
with (out/'binding.json').open('x') as f:json.dump(binding,f,indent=2);f.write('\n')
rows=[];diagnostics=[]
for step in plan['stages']:
 root=Path(step['out']);stdout=helper['read_bounded'](root/'stdout',step['reader_limit_bytes']);stderr=helper['read_bounded'](root/'stderr',step['reader_limit_bytes']);result=json.loads((root/'result.json').read_text());post=json.loads((p/'run-1'/(step['name']+'-service-post.json')).read_text())
 assert result['accounting_complete'] and result['final_accounting']['cgroup_empty']
 assert result['final_accounting']['properties']['ActiveState']=='inactive' and result['final_accounting']['properties']['MainPID']==0 and result['final_accounting']['properties']['ControlGroup']==''
 assert post['exit']==0 and post['properties']['ActiveState']=='inactive' and post['properties']['MainPID']=='0' and post['properties']['ControlGroup']==''
 assert result['observer_error'] is None and result['stop_reason'] is None and result['final_report']['truncated']=='false'
 if step['name']=='compile':
  diagnostics=[json.loads(line) for line in stdout.splitlines() if json.loads(line).get('reason')=='compiler-message'];print('STRUCTURED DIAGNOSTICS',json.dumps(diagnostics,indent=2))
 elif step['name']=='native':print('NATIVE STDOUT',stdout.decode())
 print(step['name'],'STDERR',stderr.decode())
 rows.append({'stage':step['name'],'cpu_seconds':result['final_accounting']['cpu_usage_nsec']/1e9,'wall_seconds':result['elapsed_seconds'],'result_path':str(root/'result.json'),'result_sha256':h(root/'result.json'),'service':result['final_accounting']['properties'],'accounting_complete':result['accounting_complete'],'cgroup_empty':result['final_accounting']['cgroup_empty'],'observer_error':result['observer_error'],'stop_reason':result['stop_reason'],'stdout':{'bytes':len(stdout),'sha256':h(root/'stdout')},'stderr':{'bytes':len(stderr),'sha256':h(root/'stderr')},'post':post})
raw=Path(plan['stages'][1]['out'])/'stdout';names=[s[:-6] for s in raw.read_text().splitlines() if s.endswith(': test')]
prior_plan=json.loads((area/previous/'plan.json').read_text());prior_raw=Path(prior_plan['stages'][1]['out'])/'stdout';prior=[s[:-6] for s in prior_raw.read_text().splitlines() if s.endswith(': test')]
additions=set(plan['selected_tests'])-set(prior_plan['selected_tests'])
assert set(prior)<=set(names) and set(names)-set(prior)==additions
r={'status':'passed','source_version':source_version,'source_binding_sha256':h(plan['source_binding']),'source_manifest_sha256':h(plan['source_manifest']),'plan_sha256':h(p/'plan.json'),'caller_sha256':h(p/'launch.py'),'launch_sha256':h(p/'run-1/launch.json'),'selected_count':plan['required_count'],'listed_total':len(names),'inventory_additions':sorted(additions),'compiler_message_count':len(diagnostics),'compiler_diagnostics':diagnostics,'stages':rows,'retained_elf_binding':str(out/'binding.json'),'retained_elf_binding_sha256':h(out/'binding.json'),'source_check_after':'Complete comparison passed before source changes; no separate full after snapshot is claimed.','limits':'All outputs bounded and untruncated; unchanged observer limits and actual service accounting.','scope':'Selected native execution only; no complete-repair, VM/static, Hermit, determinism or parity claim. See adjacent REPORT.md for source-version limitations.'}
with (p/'RESULT.json').open('x') as f:json.dump(r,f,indent=2);f.write('\n')
print(json.dumps(r,indent=2));print('RESULT SHA256',h(p/'RESULT.json'));print('ELF binding SHA256',h(out/'binding.json'))
