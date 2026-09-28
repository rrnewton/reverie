from pathlib import Path
import ast,difflib,hashlib,json
l=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918')
d=l/'ignored/kvm-cancellation-wake-v1'
predecessor=l/'ignored/kvm-entry-control-v1/run-unwind.py'
s=predecessor.read_text()
s=s.replace('import hashlib,json,os,pathlib,selectors,subprocess,sys,time','import hashlib,json,os,pathlib,re,selectors,shutil,subprocess,sys,time')
s=s.replace("out=root/'ignored/kvm-entry-control-v1'/sys.argv[1]", "owned=root/'ignored/kvm-cancellation-wake-v1'\ncontract=json.loads((owned/'CALLER-CONTRACT.json').read_text())\ndef digest(path):\n with open(path,'rb') as stream: return hashlib.file_digest(stream,'sha256').hexdigest()\ndef check_sources():\n for name,record in contract['source'].items():\n  path=root/name\n  assert path.stat().st_size==record['bytes'] and digest(path)==record['sha256'], name\n assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()==contract['base']\n assert subprocess.check_output(['git','branch','--show-current'],cwd=root,text=True).strip()==contract['branch']\ncheck_sources()\nout=owned/sys.argv[1]")
a=s.index("if phase in ['compile-3','compile-4']:")
b=s.index('\nenv=',a)
s=s[:a]+'''expected=[]
if phase=='compile':
 argv=[str(toolchain/'cargo'),'test','--offline','-p','reverie-kvm','--lib','--no-run','--message-format=json','-j','2','--locked','--features','native-test-support']
 wall=600
elif phase=='format-check':
 argv=[str(toolchain/'rustfmt'),'--check','--edition','2024','--config','skip_children=true','reverie-kvm/src/vm.rs']
 wall=30
elif phase in ['inventory',*contract['tests']]:
 binding=json.loads((owned/'retained/BINDING.json').read_text())
 executable=owned/'retained/reverie-kvm-tests'
 assert binding['source']==contract['source']
 assert executable.stat().st_size==binding['bytes'] and digest(executable)==binding['sha256']
 if phase=='inventory':
  argv=[str(executable),'--list']
 else:
  control=contract['tests'][phase]
  argv=[str(executable),*control['arguments']]
  expected=control['declarations']
 wall=60
else:
 raise SystemExit('unknown phase')
''' +s[b:]
s=s.replace("unit='kvm-entry-'+phase+'-'+str(os.getpid())", "unit='kvm-cancel-wake-'+phase+'-'+str(os.getpid())")
a=s.index('source={}\n');b=s.index("(out/'command.json')",a)
s=s[:a]+"source=contract['source']\n"+s[b:]
s=s.replace("'base':subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()", "'base':contract['base'],'expected_declarations':expected")
needle="(out/'result.json').write_text(json.dumps(result,indent=2)+'\\n')"
assert needle in s
s=s.replace(needle,'''check_sources()
result['source_unchanged']=True
(out/'result.json').write_text(json.dumps(result,indent=2)+'\\n')
if rc==0 and refusal is None:
 if phase=='compile':
  messages=[json.loads(line) for line in (out/'stdout').read_text().splitlines() if line.startswith('{')]
  assert any(row.get('reason')=='build-finished' and row.get('success') is True for row in messages)
  artifacts=[row for row in messages if row.get('reason')=='compiler-artifact' and row.get('target',{}).get('name')=='reverie_kvm' and row.get('profile',{}).get('test') is True and row.get('executable')]
  assert len(artifacts)==1, artifacts
  artifact=artifacts[0]
  origin=pathlib.Path(artifact['executable'])
  retained=owned/'retained'
  retained.mkdir()
  executable=retained/'reverie-kvm-tests'
  shutil.copyfile(origin,executable)
  executable.chmod(0o500)
  assert digest(origin)==digest(executable)
  binding={'path':str(executable),'origin':str(origin),'sha256':digest(executable),'bytes':executable.stat().st_size,'source':source,'compiler_artifact':artifact,'compile_result':str(out/'result.json')}
  (retained/'BINDING.json').write_text(json.dumps(binding,indent=2)+'\\n')
 elif phase=='inventory':
  names=[line.removesuffix(': test') for line in (out/'stdout').read_text().splitlines() if line.endswith(': test')]
  wanted=[name for control in contract['tests'].values() for name in control['declarations']]
  assert all(names.count(name)==1 for name in wanted), wanted
  prefix='vm::tests::cancellation_wake_tests::'
  assert sorted(name for name in names if name.startswith(prefix))==sorted(contract['tests']['new-tests']['declarations'])
  (out/'SELECTION.json').write_text(json.dumps({'discovered':len(names),'selected':wanted,'executed':False},indent=2)+'\\n')
 elif expected:
  stdout=(out/'stdout').read_text()
  successes=re.findall(r'^test (\\S+) \\.{3} ok$',stdout,re.M)
  assert sorted(successes)==sorted(expected), successes
  summary=f'test result: ok. {len(expected)} passed; 0 failed; 0 ignored; 0 measured;'
  assert summary in stdout, stdout
  assert not re.search(r'^test \\S+ \\.{3} (FAILED|ignored)',stdout,re.M)
  (out/'ACCEPTANCE.json').write_text(json.dumps({'expected':expected,'succeeded':successes,'raw_status':rc,'ignored':0,'failed':0,'source_unchanged':True},indent=2)+'\\n')''')
# Keep the predecessor's service/stream/timeout loop and environment unchanged.
ast.parse(s)
(d/'run.py').write_text(s)
(d/'CALLER-DELTA.patch').write_text(''.join(difflib.unified_diff(predecessor.read_text().splitlines(True),s.splitlines(True),fromfile=str(predecessor),tofile=str(d/'run.py'))))
plan=json.loads((d/'TEST-PLAN.json').read_text())
source={}
for n in ['reverie-kvm/src/vm.rs','reverie-kvm/src/entry.rs','reverie-kvm/src/entry/signal.rs','reverie-kvm/src/error.rs','reverie-kvm/src/lib.rs']:
 b=(l/n).read_bytes();source[n]={'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest()}
tests={}
for phase in plan['focused_phases']:
 name={'cancellation-wake-new':'new-tests','cancellation-existing-1':'exec-rearm','cancellation-existing-2':'poisoned-state','cancellation-existing-3':'late-start'}[phase['name']]
 tests[name]={'arguments':phase['argv'][1:],'declarations':phase['expected_exact_declarations']}
contract={'base':plan['base'],'branch':plan['branch'],'source':source,'tests':tests,'bounds':{'compile_wall_seconds':600,'inventory_and_each_test_wall_seconds':60,'format_wall_seconds':30,'cpu_quota':'200%','memory_max':'12G','memory_swap_max':0,'timeout_stop_seconds':10,'kill_mode':'control-group','stream_cap_bytes':16777216,'outer_deadline':'phase wall + 30 seconds'},'source_only_preparation':True}
(d/'CALLER-CONTRACT.json').write_text(json.dumps(contract,indent=2)+'\n')
plan['compile']['not_yet_bound']=['actual retained test ELF (produced by the admitted compile phase)']
plan['compile']['caller']=str(d/'run.py')
plan['compile']['source_binding']=str(d/'CALLER-CONTRACT.json')
plan['compile']['argv']=['/home/newton/.rustup/toolchains/nightly-2026-07-29-x86_64-unknown-linux-gnu/bin/cargo','test','--offline','-p','reverie-kvm','--lib','--no-run','--message-format=json','-j','2','--locked','--features','native-test-support']
plan.pop('proposed_per_phase_limits')
plan['concrete_limits']=contract['bounds']
plan['limits_note']='Reuse the supplied run-unwind.py compiler/test/service limits and offline locked cache exactly: CPU quota is a rate cap, not a cumulative CPU budget. Caller remains unlaunched pending root source/caller inspection.'
(d/'TEST-PLAN.json').write_text(json.dumps(plan,indent=2)+'\n')
commands=['cd '+str(l)]+['python3 '+str(d/'run.py')+' '+phase for phase in ['compile','inventory','new-tests','exec-rearm','poisoned-state','late-start','format-check']]
(d/'RUN-COMMANDS.txt').write_text('\n'.join(commands)+'\n')
print('Prepared seven separate bounded phases, unlaunched; exact source and seven test declarations bound.')
