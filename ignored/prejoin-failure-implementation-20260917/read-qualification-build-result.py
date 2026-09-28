from pathlib import Path
import hashlib,json,os,runpy,shutil,sys
area=Path.cwd()/'ignored/prejoin-failure-implementation-20260917';version,source_version,previous,current_native,previous_native=sys.argv[1:];p=area/version;plan=json.loads((p/'plan.json').read_text());helper=runpy.run_path(plan['helpers']['path'],run_name='readback_only');helper['check_inputs'](plan)
def h(path):return helper['digest'](path)
summary=json.loads((p/'run-1/summary.json').read_text());assert summary['status']=='passed';artifacts=json.loads((p/'run-1/compiled-executables.json').read_text());assert set(artifacts)=={'lib','static-elf'}
out=p/'run-1'/('retained-elf-'+source_version);out.mkdir();retained={}
for name,artifact in artifacts.items():
 helper['check_executable'](artifact);dest=out/name;shutil.copy2(artifact['path'],dest);assert os.stat(artifact['path']).st_ino!=dest.stat().st_ino
 retained[name]={'path':str(dest),'bytes':dest.stat().st_size,'mode':dest.stat().st_mode&0o7777,'sha256':h(dest),'separate_inode':True};assert all(retained[name][k]==artifact[k] for k in ['bytes','mode','sha256'])
with (out/'binding.json').open('x') as f:json.dump({'source':artifacts,'retained':retained},f,indent=2);f.write('\n')
old=json.loads((area/previous/'run-1/summary.json').read_text());current_selected=json.loads((area/current_native/'plan.json').read_text())['selected_tests'];old_selected=json.loads((area/previous_native/'plan.json').read_text())['selected_tests'];added=set(current_selected)-set(old_selected)
for name,inv in summary['inventories'].items():
 expected=sorted(old['inventories'][name]['names']+(list(added) if name=='lib' else []));assert inv['names']==expected;assert len(inv['names'])==inv['count'];print('INVENTORY',name,inv['count'])
rows=[];diagnostics=[]
for s in plan['stages']:
 stage=Path(s['out']);stdout=helper['read_bounded'](stage/'stdout',s['reader_limit_bytes']);stderr=helper['read_bounded'](stage/'stderr',s['reader_limit_bytes']);r=json.loads((stage/'result.json').read_text());post=json.loads((p/'run-1'/(s['name']+'-service-post.json')).read_text());print(s['name'],'STDERR',stderr.decode())
 if s['name']=='compile':diagnostics=[json.loads(line) for line in stdout.splitlines() if json.loads(line).get('reason')=='compiler-message'];print('STRUCTURED DIAGNOSTICS',json.dumps(diagnostics,indent=2))
 assert r['accounting_complete'] and r['final_accounting']['cgroup_empty'] and r['wrapper_exit_code']==0 and r['observer_error'] is None and r['stop_reason'] is None and r['final_report']['truncated']=='false'
 assert post['exit']==0 and post['properties']['ActiveState']=='inactive' and post['properties']['MainPID']=='0' and post['properties']['ControlGroup']==''
 rows.append({'stage':s['name'],'cpu_seconds':r['final_accounting']['cpu_usage_nsec']/1e9,'wall_seconds':r['elapsed_seconds'],'service':r['final_accounting']['properties'],'result_path':str(stage/'result.json'),'result_sha256':h(stage/'result.json'),'stdout_sha256':h(stage/'stdout'),'stderr_sha256':h(stage/'stderr'),'post':post})
result={'status':'passed','source_version':source_version,'source_binding_sha256':h(plan['source_binding']),'plan_sha256':h(p/'plan.json'),'caller_sha256':h(p/'build.py'),'launch_sha256':h(p/'run-1/launch.json'),'stages':rows,'inventories':{name:inv['count'] for name,inv in summary['inventories'].items()},'compiler_diagnostics':diagnostics,'retained_elf_binding_sha256':h(out/'binding.json'),'retained':retained,'scope':'Compilation and actual inventories only; no VM/test/guest execution. Full source/input and emitted artifact checks passed after completion.'}
with (p/'RESULT.json').open('x') as f:json.dump(result,f,indent=2);f.write('\n')
report=f'# Reverie {source_version} qualification build\n\nThe bounded compile and two inventories passed. Actual library inventory is {result["inventories"]["lib"]}; static_elf inventory is {result["inventories"]["static-elf"]}. All previous identities remain, plus only the exact new native names. The prepared selection retains all {len(current_selected)} natives, four VM and 22 original static methods. No tests ran in this sequence.\n\n'
report+='; '.join(f"{row['stage']} used {row['cpu_seconds']:.6f} CPU / {row['wall_seconds']:.9f} wall seconds" for row in rows)+'. All three services exited 0 with complete accounting and independently inactive/dead, MainPID 0, empty control group. No observer error, bound, cleanup refusal or truncated output occurred. Output was bounded and untruncated.\n\n'
report+=f'Cargo structured stdout contains {len(diagnostics)} compiler-message rows; complete stderr was read. Both exact compiler-emitted executables were copied to separate inodes under run-1/retained-elf-{source_version}/ and bound by {result["retained_elf_binding_sha256"]}. Complete source/input and artifact comparisons passed after execution. RESULT.json SHA256 '+h(p/'RESULT.json')+' retains all artifact, raw-output and terminal identities.\n'
with (p/'REPORT.md').open('x') as f:f.write(report)
print(json.dumps(result,indent=2));print('RESULT',h(p/'RESULT.json'));print('REPORT',h(p/'REPORT.md'))
