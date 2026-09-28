import hashlib,json,os,pathlib,selectors,subprocess,sys,time
root=pathlib.Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918')
out=root/'ignored/kvm-entry-owner-composition-v1'/sys.argv[1]
out.mkdir()
toolchain=pathlib.Path('/home/newton/.rustup/toolchains/nightly-2026-07-29-x86_64-unknown-linux-gnu/bin')
phase=sys.argv[1]
if phase.startswith('compile-'):
 argv=[str(toolchain/'cargo'),'test','--offline','-p','reverie-kvm','--lib','--no-run','--message-format=json','-j','2']
 argv+=['--locked','--features','native-test-support']
 wall=600
elif phase == 'inventory-1':
 argv=[str(root/'ignored/kvm-entry-owner-composition-v1/retained-lib-3'),'--list']
 wall=30
elif phase.startswith('test-'):
 names={
  'test-owner-3':'entry::owner::tests::',
  'test-entry-owner-3':'entry::captured_owner_is_visible_before_wake_outside_registry_lock',
  'test-memory-retired-3':'memory::tests::entry_copy_tests::retired_memory_origin_retains_cause_without_expired_publisher',
  'test-cleanup-drain-3':'runtime::tests::unstarted_cleanup_retains_pending_future_and_real_errors',
 }
 argv=[str(root/'ignored/kvm-entry-owner-composition-v1/retained-lib-3')]
 if phase != 'test-owner-3': argv+=['--exact']
 argv += [names[phase], '--test-threads=1', '--nocapture']
 wall=60
elif phase.startswith('format-'):
 argv=[str(toolchain/'rustfmt'),'--edition','2024','--config','skip_children=true','reverie-kvm/src/entry.rs','reverie-kvm/src/clock.rs','reverie-kvm/src/vm.rs','reverie-kvm/src/runtime.rs','reverie-kvm/src/cpuid_runtime_tests.rs','reverie-kvm/src/entry/owner.rs','reverie-kvm/src/memory.rs','reverie-kvm/src/bootstrap.rs','reverie-kvm/src/executor.rs']
 wall=30
else:
 raise SystemExit('unknown phase')
env={'HOME':'/home/newton','PATH':str(toolchain)+':/usr/bin:/bin','CARGO_HOME':'/home/newton/.cargo','CARGO_TARGET_DIR':str(root/'target/parity-composition-v1-cold-v1'),'RUSTC':str(toolchain/'rustc'),'RUSTDOC':str(toolchain/'rustdoc'),'CARGO_BUILD_JOBS':'2','CARGO_NET_OFFLINE':'true','CARGO_TERM_COLOR':'never','LANG':'C','LC_ALL':'C','TZ':'UTC','RUSTUP_TOOLCHAIN':'nightly-2026-07-29','RUSTUP_AUTO_INSTALL':'0','REVERIE_REQUIRE_KVM':'1'}
unit='kvm-entry-owner-'+phase+'-'+str(os.getpid())
command=['/usr/bin/systemd-run','--user','--wait','--pipe','--collect','--unit',unit,'--property=CPUQuota=200%','--property=MemoryMax=12G','--property=MemorySwapMax=0','--property=LimitCORE=0','--property=RuntimeMaxSec='+str(wall),'--property=TimeoutStopSec=10','--property=KillMode=control-group','--working-directory='+str(root),'/usr/bin/env','-i']+[k+'='+v for k,v in env.items()]+argv
source={}
for n in ['reverie-kvm/src/'+n for n in ['entry.rs','entry/owner.rs','entry/signal.rs','error.rs','lib.rs','clock.rs','vm.rs','runtime.rs','memory.rs','cpuid_runtime_tests.rs','failure.rs','bootstrap.rs','executor.rs']]+['Cargo.lock']:
 b=(root/n).read_bytes(); source[n]={'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest()}
elf_before = {'path':argv[0], 'sha256':hashlib.sha256(pathlib.Path(argv[0]).read_bytes()).hexdigest(), 'bytes':pathlib.Path(argv[0]).stat().st_size} if phase.startswith(('test-','inventory-')) else None
(out/'command.json').write_text(json.dumps({'argv':argv,'environment':env,'service':command,'source':source,'executable_before':elf_before,'wall_seconds':wall,'stream_cap_bytes':16777216,'base':subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()},indent=2)+'\n')
started=time.monotonic(); p=subprocess.Popen(command,cwd=root,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
sel=selectors.DefaultSelector(); streams={}; counts={}
for name,f in [('stdout',p.stdout),('stderr',p.stderr)]:
 sel.register(f,selectors.EVENT_READ,name); streams[name]=(out/name).open('xb'); counts[name]=0
refusal=None
while sel.get_map():
 for key,_ in sel.select(1):
  b=os.read(key.fileobj.fileno(),65536)
  if not b: sel.unregister(key.fileobj); continue
  name=key.data; counts[name]+=len(b)
  if counts[name] > 16777216 and refusal is None:
   refusal='output limit'; subprocess.run(['/usr/bin/systemctl','--user','stop',unit],check=False)
  if counts[name] <= 16777216: streams[name].write(b)
 if time.monotonic()-started>wall+30 and refusal is None:
  refusal='outer deadline'; subprocess.run(['/usr/bin/systemctl','--user','stop',unit],check=False)
for f in streams.values(): f.close()
rc=p.wait(); result={'raw_status':rc,'elapsed_seconds':time.monotonic()-started,'stream_bytes':counts,'refusal':refusal}
(out/'result.json').write_text(json.dumps(result,indent=2)+'\n')
result['terminal_service']=subprocess.run(['/usr/bin/systemctl','--user','show',unit,'--property=LoadState,ActiveState,SubState,ControlGroup,MainPID,Result,ExecMainCode,ExecMainStatus,CPUUsageNSec'],capture_output=True,text=True,timeout=10).stdout
result['executable_after']={'path':argv[0],'sha256':hashlib.sha256(pathlib.Path(argv[0]).read_bytes()).hexdigest(),'bytes':pathlib.Path(argv[0]).stat().st_size} if elf_before else None
result['source_after']={n:{'bytes':(root/n).stat().st_size,'sha256':hashlib.sha256((root/n).read_bytes()).hexdigest()} for n in source}
(out/'result.json').write_text(json.dumps(result,indent=2)+'\n')
result['identity_stable']=(elf_before == result['executable_after'] and (phase.startswith('format-') or source == result['source_after']))
(out/'result.json').write_text(json.dumps(result,indent=2)+'\n')
if phase.startswith('test-'):
 import re
 summary=re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;', (out/'stdout').read_text())
 expected=8 if phase == 'test-owner-3' else 1
 result['selection']={'summaries':summary,'expected_passed':expected}
 if summary != [(str(expected),'0','0')]: refusal='selected test count/result mismatch';result['refusal']=refusal
(out/'result.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(result)); print((out/'stderr').read_text()[-10000:])
sys.exit(rc if rc else (1 if refusal or not result['identity_stable'] else 0))
