from pathlib import Path
import ast,hashlib,json,runpy,re
r=Path.cwd();a=r/'ignored/prejoin-failure-implementation-20260917';out=a/'qualification-build-v1';out.mkdir()
def digest(p):return hashlib.sha256(Path(p).read_bytes()).hexdigest()
def bind(p):
 p=Path(p);s=p.stat();return dict(path=str(p),resolved_path=str(p.resolve(strict=True)),bytes=s.st_size,mode=s.st_mode&0o7777,sha256=digest(p))
old=json.loads((a/'cargo-v9/plan.json').read_text());inputs=[]
for row in old['inputs']:
 oldpath=row['path'];path=oldpath.replace('source-v12-preparation','source-v13-preparation');new=bind(path)
 if path==oldpath:assert new==row,path
 inputs.append(new)
helper=a/'cargo-v9/launch.py';inputs.append(bind(helper))
leader=re.findall(r'#\[test\]\s*fn ([a-zA-Z_0-9]+)\(', (r/'reverie-kvm/tests/support/leader_exit.rs').read_text())
assert len(leader)==17,len(leader)
static_tests=sorted(['leader_exit::'+name for name in leader]+[
'exec_worker_error_diagnostic::exec_worker_error_still_consumes_root_and_process_hooks',
 'terminal_fork::injected_exec_failure_joins_live_fork','terminal_fork::backend_exec_failure_joins_live_fork',
 'terminal_fork::injected_exec_failure_preserves_child_and_owner_errors','terminal_fork::backend_exec_failure_preserves_child_and_owner_errors'])
lib_tests=sorted(old['selected_tests']+[
'vm::tests::real_fork_and_thread_cancel_keep_status_while_terminal_hook_is_returning',
'vm::tests::real_fork_and_thread_wrappers_restore_both_capture_modes',
'vm::tests::page_fault_action_restores_complete_stopped_context',
'vm::tests::nested_host_fork_failure_uses_descendant_process_and_worker_identity'])
assert len(set(static_tests))==22 and len(set(lib_tests))==41
observer_root=str(Path(old['observer_root']).parent/'reverie-qualification-build-v1')
env=dict(old['environment_fixed']);env['TMPDIR']=str(out/'run-1/tmp')
p={k:old[k] for k in ['schema','source_root','source_base','source_head','observer','observer_sha256','environment_keys','service_memory_max_bytes','service_swap_max_bytes','optional_cargo_configs','locked_dependency_file']}
p.update(source_binding=str(a/'source-v13-preparation/binding.json'),source_manifest=str(a/'source-v13-preparation/tracked-source-manifest.json'),inputs=inputs,helpers={'path':str(helper),'sha256':digest(helper)},environment_fixed=env,target_dir=old['target_dir'],reuse_owned_target_cache=True,run_root=str(out/'run-1'),tmpdir=env['TMPDIR'],observer_root=observer_root,execution=['/usr/bin/python3','-B',str(out/'build.py')],scope='Compile the Reverie library and unchanged static_elf integration target, then record two actual inventories. No native, VM or guest test execution.',expected_tests={'lib':lib_tests,'static-elf':static_tests},expected_artifacts=[{'id':'lib','manifest':str(r/'reverie-kvm/Cargo.toml'),'name':'reverie_kvm','kind':['lib'],'test':True},{'id':'static-elf','manifest':str(r/'reverie-kvm/Cargo.toml'),'name':'static_elf','kind':['test'],'test':True}])
cargo=old['stages'][0]['payload'][0]
stages=[]
for name,artifact,payload,cpu,wall,cap in [
 ('compile',None,[cargo,'test','--locked','--offline','-p','reverie-kvm','--lib','--test','static_elf','--no-run','--message-format=json'],600000000,900,16777216),
 ('list-lib','lib',['<verified-compiled-test-executable>','--list','--format','terse'],5000000,15,1048576),
 ('list-static-elf','static-elf',['<verified-compiled-test-executable>','--list','--format','terse'],5000000,15,1048576)]:
 dest=str(Path(observer_root)/name);argv=['/usr/bin/python3','-B',p['observer'],'--out',dest,'--cpu-usec',str(cpu),'--wall-seconds',str(wall),'--log-bytes',str(cap)]+payload
 stages.append(dict(name=name,artifact=artifact,payload=payload,argv=argv,cwd=str(r),out=dest,cpu_usec=cpu,wall_seconds=wall,stderr_limit_bytes=cap,reader_limit_bytes=cap))
p['stages']=stages
for k in ['run_root','observer_root']:assert not Path(p[k]).exists()
(out/'plan.json').write_text(json.dumps(p,indent=2)+'\n')
# Keep the previously reviewed multi-artifact build/list engine; remove only
# Hermit-specific artifact and feature conditions and select both real R targets.
h=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-prejoin-hermit-20260917/ignored/prejoin-failure-implementation-20260917/qualification-build-v2/build.py')
s=h.read_text().replace('0583d6c4cc1c1a778f0d1c27cf52395741b62cfeff1c535fb5a397c31fb5d513',digest(out/'plan.json'))
s=s.replace("        for repo in plan['repositories']:\n            functions['check_inputs'](dict(repo, inputs=plan['inputs'],\n                                          optional_cargo_configs=plan['optional_cargo_configs']))", "        functions['check_inputs'](plan)")
s=s.replace("            require('kvm-execution-tests' in row['features'] and\n                    'kvm-native-test-support' not in row['features'], 'wrong qualification features')", "            require(row['features'] == ['default'], 'unexpected Reverie qualification features')")
start=s.index("        required = {row['id']")
end=s.index('        return artifacts',start)
s=s[:start]+"        require(set(artifacts) == {'lib', 'static-elf'}, 'missing qualification executable')\n"+s[end:]
s=s.replace("['compile', 'list-hermit', 'list-cli', 'list-kvm-harder']", "['compile', 'list-lib', 'list-static-elf']")
s=s.replace("    require(len(set(plan['original_cli_tests'])) == 24, 'changed original KVM CLI population')\n", "    require(len(plan['expected_tests']['lib']) == 41 and len(plan['expected_tests']['static-elf']) == 22, 'changed prepared identity counts')\n")
s=s.replace("Path(plan['repositories'][0]['source_root']) / 'target'", "Path(plan['source_root']) / 'target'")
s=s.replace("repositories=plan['repositories'], scope=plan['scope']", "source_binding=plan['source_binding'], scope=plan['scope']")
start=s.index("                if step['artifact'] == 'hermit':")
end=s.index("                inventories[step['artifact']]",start)
s=s[:start]+"                require(all(listed.count(name) == 1 for name in plan['expected_tests'][step['artifact']]), 'selected identity absent or duplicated')\n"+s[end:]
s=s.replace("set(plan['test_artifacts'])", "{'lib', 'static-elf'}")
s=s.replace("              recurring_kvm_selection=plan['original_cli_tests'] + [plan['setup_test']],\n              recurring_kvm_count=len(plan['original_cli_tests']) + 1,\n",'')
ast.parse(s);(out/'build.py').write_text(s)
f=runpy.run_path(str(helper),run_name='preflight');f['check_inputs'](p)
record={'plan':bind(out/'plan.json'),'caller':bind(out/'build.py'),'expected_tests':p['expected_tests'],'scope':p['scope'],'outputs':{s['name']:s['out'] for s in stages},'preflight':'Full live source/input checks passed; no execution.'};(out/'reservation.json').write_text(json.dumps(record,indent=2)+'\n');print(json.dumps({k:v for k,v in record.items() if k!='expected_tests'},indent=2))
