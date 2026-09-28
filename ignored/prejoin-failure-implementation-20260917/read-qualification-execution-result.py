from pathlib import Path
import hashlib,json,runpy,sys
area=Path.cwd()/'ignored/prejoin-failure-implementation-20260917';version=sys.argv[1];p=area/version;plan=json.loads((p/'plan.json').read_text());helper=runpy.run_path(plan['helpers']['path'],run_name='readback_only');helper['check_inputs'](plan)
def h(x):return helper['digest'](x)
for artifact in plan['artifacts'].values():helper['check_executable'](artifact)
summary=json.loads((p/'run-1/summary.json').read_text());accepted={r['stage'] for r in summary['completed']};active=summary.get('active_stage') if summary['status']!='passed' else None
rows=[];unexecuted=[]
for s in plan['stages']:
 out=Path(s['out']);dispatch=p/'run-1'/(s['name']+'-dispatch.json')
 if s['name'] not in accepted and s['name']!=active:
  assert not out.exists() and not dispatch.exists();unexecuted.append({'stage':s['name'],'test':s['test']});continue
 r=json.loads((out/'result.json').read_text());stdout=helper['read_bounded'](out/'stdout',s['reader_limit_bytes']);stderr=helper['read_bounded'](out/'stderr',s['reader_limit_bytes']);print(s['name'],'STDOUT',stdout.decode(),'STDERR',stderr.decode())
 a=Path(s['admission_record']);exitpath=a.with_suffix('.exit.json')
 final=r.get('final_accounting');refusal=r.get('refusal_final_accounting')
 if s['name'] in accepted:
  assert r['accounting_complete'] and r['comparison_eligible'] and final['cgroup_empty'] and r['wrapper_exit_code']==0 and r['observer_error'] is None and r['stop_reason'] is None
  post=json.loads((p/'run-1'/(s['name']+'-service-post.json')).read_text());assert post['exit']==0 and post['properties']['ActiveState']=='inactive' and post['properties']['MainPID']=='0' and post['properties']['ControlGroup']==''
 else:post=None
 admission=json.loads(a.read_text());assert admission['api_version']==12 and admission['device']=='/dev/kvm';artifact=plan['artifacts'][s['artifact']];assert admission['test_binary']=={k:artifact[k] for k in ['path','bytes','mode','sha256']}
 rows.append({'stage':s['name'],'test':s['test'],'accepted':s['name'] in accepted,'observer_exit':json.loads((p/'run-1'/(s['name']+'-readback.json')).read_text())['observer_exit'],'wrapper_exit':r['wrapper_exit_code'],'accounting_complete':r['accounting_complete'],'comparison_eligible':r['comparison_eligible'],'observer_error':r['observer_error'],'stop_reason':r['stop_reason'],'wall_seconds':r['elapsed_seconds'],'final_accounting':final,'refusal_final_accounting':refusal,'result_path':str(out/'result.json'),'result_sha256':h(out/'result.json'),'stdout_bytes':len(stdout),'stdout_sha256':h(out/'stdout'),'stderr_bytes':len(stderr),'stderr_sha256':h(out/'stderr'),'admission_sha256':h(a),'admission_exit':json.loads(exitpath.read_text()),'post':post})
r={'status':summary['status'],'error':summary.get('error'),'active_stage':active,'accepted_count':len(accepted),'accepted_vm_count':sum(x.startswith('vm-') for x in accepted),'accepted_static_count':sum(x.startswith('static-elf-') for x in accepted),'unexecuted_count':len(unexecuted),'unexecuted':unexecuted,'observed':rows,'source_binding_sha256':h(plan['source_binding']),'plan_sha256':h(p/'plan.json'),'caller_sha256':h(p/'launch.py'),'launch_sha256':h(p/'run-1/launch.json'),'source_and_artifact_checks_after':'Full source/explicit inputs and both compiler-emitted executables still match. Prior record bytes retained.','scope':'Reverie selected real VM/static execution only; no Hermit strict INFO, determinism or canonical parity claim.'}
with (p/'RESULT.json').open('x') as f:json.dump(r,f,indent=2);f.write('\n')
print(json.dumps({k:v for k,v in r.items() if k not in ['observed','unexecuted']},indent=2));print('RESULT',h(p/'RESULT.json'))
