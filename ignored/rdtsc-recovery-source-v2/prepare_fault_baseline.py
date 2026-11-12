from pathlib import Path
import hashlib,json,shutil,subprocess,importlib.util,os
R=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-setitimer-reverie-20260918');V=R/'ignored/rdtsc-recovery-source-v2';OLD=R/'ignored/publication-fd-stdin-cold-qualification-v1/qualification-v1';N=R/'ignored/rdtsc-fault-baseline-v1';Q=N/'qualification-v1';Q.mkdir(parents=True,mode=0o700)
def rec(p):p=Path(p);b=p.read_bytes();return dict(path=str(p),bytes=len(b),mode=p.stat().st_mode&0o7777,sha256=hashlib.sha256(b).hexdigest())
def write(p,x):
 with Path(p).open('x')as f:f.write(x if isinstance(x,str)else json.dumps(x,indent=2)+'\n')
source=R/'ignored/publication-fd-composition-v3/source'
for name in ['phase.py','common.py','cache_lease.py'] :shutil.copyfile(V/'qualification-v1'/name,Q/name)
(Q/'observer').mkdir()
for p in (V/'qualification-v1/observer').iterdir():
 if p.is_file()and(p.suffix=='.py'or p.name=='source-inputs.json'):shutil.copyfile(p,Q/'observer'/p.name)
write(Q/'SETUP.json',dict(source_root=str(source),scope='One unchanged predecessor exception neighbor; no build/source change'))
for sub in ['controls','tmp','cache','loader','retained-binaries']:(Q/sub).mkdir(mode=0o700)
old_binding=json.loads((OLD/'retained-binaries/BINDING.json').read_text());r=next(r for r in old_binding['copies']if r['cargo_artifact']['target']['name']=='static_elf');original=Path(r['retained']['path']);assert rec(original)==r['retained'];copy=Q/'retained-binaries/static.elf';shutil.copyfile(original,copy);copy.chmod(0o555);a=rec(copy);assert a['sha256']==r['retained']['sha256']and a['bytes']==r['retained']['bytes'];assert original.stat().st_ino!=copy.stat().st_ino
inventory=json.loads((OLD/'controls/list-static/result.json').read_text());name='static_elf_faults_are_reported_by_direct_and_tool_runtimes';assert inventory['accepted']and inventory['terminal_authenticated']and inventory['readback']['names'].count(name)==1
old_plan=json.loads((OLD/'list-static-plan.json').read_text());plan=json.loads(json.dumps(old_plan));plan.update(path=str(Q/'baseline-fault-plan.json'),name='baseline-fault',kind='test',argv=[str(copy),name,'--exact','--test-threads=1','--nocapture','-Z','unstable-options','--format=json'],selected=[name],output=str(Q/'observer/baseline-fault'),control=str(Q/'controls/baseline-fault'),scope='One unchanged qualified predecessor585 neighbor; preserve actual fault/helper mismatch and do not claim unexecuted PF/GP branches')
plan['environment']['TMPDIR']=str(Q/'tmp');plan['environment']['XDG_CACHE_HOME']=str(Q/'cache')
plan['scm']={}
for k,args in [('head',['rev-parse','HEAD']),('branch',['branch','--show-current']),('index',['ls-files','--stage'])]:
 raw=subprocess.check_output(['/usr/bin/git','-C',str(R),*args],timeout=30);plan['scm'][k]=hashlib.sha256(raw).hexdigest()if k=='index'else raw.decode().strip()
# Reuse the exact reviewed loader-binding function in a new owned helper file.
template=(V/'qualification-v1/prepare.py').read_text();prefix=template[:template.index('def main():')];write(Q/'loader_helpers.py',prefix)
import sys;sys.path.insert(0,str(Q));spec=importlib.util.spec_from_file_location('baseline_loader',Q/'loader_helpers.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
more,links=m.loader('baseline-static',a,plan['environment']);plan['inputs']+=more;plan['input_symlinks']+=links
for p in [Q/'SETUP.json',*Q.glob('*.py'),* (Q/'observer').glob('*.py'),Q/'observer/source-inputs.json',copy,OLD/'retained-binaries/BINDING.json',OLD/'controls/compile/result.json',OLD/'controls/list-static/result.json',source/'reverie-kvm/src/failure.rs',source/'reverie-kvm/src/error.rs',source/'reverie-kvm/src/runtime.rs',source/'reverie-kvm/tests/static_elf.rs']:plan['inputs'].append(rec(p))
plan['inputs']=list({r['path']:r for r in plan['inputs']}.values());plan['input_symlinks']=list({r['path']:r for r in plan['input_symlinks']}.values())
assert plan['limits']==old_plan['limits'] and plan['limits']['aggregate_cpu_usec']==30000000 and plan['limits']['wall_seconds']==60
assert plan['environment']['REVERIE_REQUIRE_KVM']=='1'
write(Q/'baseline-fault-plan.json',plan)
write(N/'PLAN.md','''# One unchanged predecessor fault control — prepared, not launched

Run exactly `static_elf_faults_are_reported_by_direct_and_tool_runtimes` from the retained source-qualified composition-v3 / landed-tree-equivalent585 static ELF. The actual old inventory lists it once. The executable is a distinct-inode mode0555 content-identical copy; current loader binding and complete original source/tool/dependency/compile receipt inputs remain bound. No Cargo or source compilation occurs. The original observed phase/common/lease/observer code and 30 CPU / 60 wall bounds are unchanged; only phase paths, exact selector and temporary directories change. Fresh SCM state is bound.

This test first runs direct UD2 and then Tool UD2, both using the old exact top-level GuestException helper. Later PF/GP cases must not be credited if an earlier assertion fails. Its old KVM-open path prints a skip and returns rather than consulting REVERIE_REQUIRE_KVM; therefore this evidence requires independent raw stderr readback with no `skipping KVM exception test`, or it is explicitly unexecuted regardless of libtest's event. An actual reached panic/raw101 cannot be confused with that skip. No assertion, fixture or library byte was changed.

After root inspects the exact plan, use PYTHONOPTIMIZE=0 /usr/bin/python3 -B qualification-v1/phase.py launch <absolute plan> <exact SHA>. Retain raw result/stdio/accounting and terminal lease completion. No retry, build, production fix, callback success or unrun branch is inferred. This preparation does not execute the test or claim a lease.
''')
write(N/'SOURCE-DIAGNOSIS.md','''# Existing error ownership contract

At candidate runtime.rs:2593 the Tool entry creates RunFailure and a FailureContext. finish_tool_process at2174–2175 publishes the original execution error before cleanup; vm.rs:1023–1027 calls FailureContext::publish; failure.rs:76–115 records the first Arc cause and returns Error::SharedFailure. runtime.rs:2631–2633 then uses RunFailure::complete, whose error.rs:302–412 completion preserves secondary cleanup/context and returns SharedFailure for a lone primary. Error::SharedFailure's Display is exactly its cause. Error::primary deliberately strips SharedFailure **and** SignalEffects/WorkerFailure/WithCleanup/Cleanup/ExecWorkerTeardown and would be too permissive for this exact fault-only test.

The actual timestamp-10 stderr names vector6/RIP0x200000/CR2zero, yet its top-level assert_invalid_opcode match fails. The exact source path explains a SharedFailure envelope around the expected fault; a Debug value was not emitted, so this note distinguishes source-derived wrapper identification from a directly logged enum. No signal ledger or cleanup suffix appears in Display. The baseline measurement is still pending and will determine whether this old helper mismatch is reproduced before any timestamp delta. No GP branch success is claimed.

Any proposed test adaptation must retain exact fault values and reject every extra cleanup, worker or signal-effects wrapper rather than calling primary(). A candidate narrow shape check could accept only the documented SharedFailure directly containing GuestException for Tool results, with direct results retaining their original top-level contract. No adaptation has been applied or approved here.
''')
write(N/'BINDING.json',dict(source='Frozen composition-v3; root proved identical to79516661/landed44fcb195 tree7620fe83',old_binding=rec(OLD/'retained-binaries/BINDING.json'),original_artifact=r['retained'],retained=a,original_inventory=rec(OLD/'controls/list-static/result.json'),plan=rec(Q/'baseline-fault-plan.json'),source_manifest=plan['source_manifest'],observer=rec(Q/'observer/observer.py'),unchanged_helpers={n:rec(Q/n)for n in ['phase.py','common.py','cache_lease.py']},raw_skip_readback_required=True,no_execution=True))
files=[rec(p)for p in sorted(N.rglob('*'))if p.is_file()];write(N/'READBACK.json',dict(files=files,all_inputs_authenticated=all(rec(r['path'])==r for r in files),plan=rec(Q/'baseline-fault-plan.json'),no_test_or_lease_execution=True));print(json.dumps(dict(plan=rec(Q/'baseline-fault-plan.json'),readback=rec(N/'READBACK.json'))))
