from pathlib import Path
import ast,copy,difflib,hashlib,json,os,re,shutil,stat
N=Path(__file__).resolve().parent;P=N.with_name('rdtsc-recovery-source-v3');Q=N/'qualification-v1';Q.mkdir(exist_ok=True);R=N.parents[1];C='reverie-kvm/tests/static_elf.rs'
def rec(p):
 p=Path(p);b=os.fsencode(os.readlink(p))if p.is_symlink()else p.read_bytes();return dict(path=str(p),bytes=len(b),mode=stat.S_IMODE(p.lstat().st_mode),sha256=hashlib.sha256(b).hexdigest(),**({'kind':'symlink','target':os.readlink(p)}if p.is_symlink()else{}))
def write(p,obj):
 p=N/p;p.parent.mkdir(parents=True,exist_ok=True)
 text=obj if isinstance(obj,str)else json.dumps(obj,indent=2)+'\n'
 if p.exists():assert p.read_text()==text;return
 with p.open('x')as f:f.write(text)
def diff(a,b,path):return ''.join(difflib.unified_diff(a.splitlines(True),b.splitlines(True),fromfile='v3/'+path,tofile='diagnostic/'+path))
before=(P/'source'/C).read_text();after=(N/'source'/C).read_text();write('SOURCE.patch',diff(before,after,C))
manifest=[];changed=[]
for old in json.loads((P/'SOURCE-MANIFEST.json').read_text()):
 row=copy.deepcopy(old);p=N/'source'/row['relative'];row['path']=str(p)
 if row['kind']=='unexpanded_gitlink':assert p.is_dir()and not list(p.iterdir())
 else:
  actual=rec(p);assert actual['mode']==rec(P/'source'/row['relative'])['mode']
  if actual['sha256']!=old['sha256']:
   assert row['relative']==C;changed.append(C);row['sha256']=actual['sha256'];row['bytes']=actual['bytes']
 manifest.append(row)
assert changed==[C];write('SOURCE-MANIFEST.json',manifest)
# All old test bodies remain byte-identical except the one selected diagnostic.
oldbodies=[]
for m in re.finditer(r'^#\[test\]\nfn (\w+)\(',before,re.M):
 start=m.start();end=start+re.search(r'^}\n',before[start:],re.M).end();body=before[start:end];name=m.group(1)
 if name=='host_owned_timestamp_worker_keeps_native_execution':
  afterstart=after.index('#[test]\nfn '+name+'(');afterend=afterstart+re.search(r'^}\n',after[afterstart:],re.M).end();newbody=after[afterstart:afterend]
  expected=body.replace('compile_c_program(&directory.0, "timestamp-host-worker", &source)','compile_host_timestamp_diagnostic(&directory.0, &source)').replace('run_static_elf_with_tool::<TimestampTool>(true, true)','run_static_elf_with_tool::<HostTimestampDiagnosticTool>(true, true)').replace('    assert_eq!(\n        log.calls(),','    log.retain();\n    assert_eq!(\n        log.calls(),')
  # Rustfmt wraps the changed long generic call, leaving all expressions intact.
  expected=expected.replace('    let (log, status, stdout, stderr) =\n        futures::executor::block_on(backend.run_static_elf_with_tool::<HostTimestampDiagnosticTool>(true, true))\n            .unwrap();', '    let (log, status, stdout, stderr) = futures::executor::block_on(\n        backend.run_static_elf_with_tool::<HostTimestampDiagnosticTool>(true, true),\n    )\n    .unwrap();')
  assert expected==newbody
  count_assert=body[body.index('    assert_eq!(\n        log.calls(),'):];assert newbody.endswith(count_assert)
 else:assert after.count(body)==1,name
 oldbodies.append(dict(name=name,sha256=hashlib.sha256(body.encode()).hexdigest(),unchanged=name!='host_owned_timestamp_worker_keeps_native_execution'))
write('TEST-CONTINUITY.json',dict(tests=oldbodies,selected='host_owned_timestamp_worker_keeps_native_execution',count_assertion_byte_identical=True,c_source_byte_identical=True,production_unchanged=True,original_timestamp_log_and_tool_unchanged=before[before.index('#[derive(Debug, Default)]\nstruct TimestampLog'):before.index('fn append_jne_failure')]in after))
for name in ['prepare.py','phase.py','common.py','cache_lease.py','bind_dependencies.py','admit_target.py','retain_artifacts.py','toolchain-standard-inputs.json']:
 shutil.copyfile(P/'qualification-v1'/name,Q/name)
(Q/'observer').mkdir()
for p in (P/'qualification-v1/observer').iterdir():
 if p.is_file()and(p.suffix=='.py'or p.name=='source-inputs.json'):shutil.copyfile(p,Q/'observer'/p.name)
selector=dict(groups={'host-worker-diagnostic':dict(artifact='static',names=['host_owned_timestamp_worker_keeps_native_execution'],source=C,purpose='Attribute every actual saved timestamp RIP; preserve the original exact two-call assertion even when it fails.')},exact_declarations=1)
write('qualification-v1/SELECTORS.json',selector)
order=dict(phases=['metadata','compile','format','list-static','host-worker-diagnostic'],metadata_followup='bind_dependencies.py',compile_followup='retain_artifacts.py',stop_first_unaccepted=True)
write('RUN-ORDER.json',order);shutil.copyfile(P/'execute_phases.py',N/'execute_phases.py')
target='rdtsc-host-worker-diagnostic-v1-cold-v1'
p=Q/'prepare.py';s=p.read_text().replace('rdtsc-recovery-v3-cold-v1',target)
s=s.replace("require(name in ('metadata','compile','list-lib','list-static','list-vmcall','list-read-clock','format','clippy','core-check') or name in json_read(HERE/'SELECTORS.json')['groups'],'unplanned phase')","require(name in ('metadata','compile','list-static','format','host-worker-diagnostic'),'unplanned phase')")
s=s.replace("'RUSTC':str(TOOL/'rustc'),'RUSTDOC':str(TOOL/'rustdoc'),'REVERIE_REQUIRE_KVM':'1'}","'RUSTC':str(TOOL/'rustc'),'RUSTDOC':str(TOOL/'rustdoc'),'REVERIE_REQUIRE_KVM':'1',\n         'REVERIE_TIMESTAMP_DIAGNOSTIC_DIR':str(HERE/'fixture-evidence')}")
s=s.replace("paths=['reverie-kvm/src/bootstrap.rs', 'reverie-kvm/src/cpuid.rs', 'reverie-kvm/src/lib.rs', 'reverie-kvm/src/runtime.rs', 'reverie-kvm/src/timestamp.rs', 'reverie-kvm/src/vm.rs', 'reverie-kvm/tests/static_elf.rs']","paths=['reverie-kvm/tests/static_elf.rs']")
s=s.replace("argv=[cargo,'test','--offline','--locked','-p','reverie-kvm','--lib','--test','static_elf','--test','vmcall','--test','read_clock','--no-run','--message-format=json']","argv=[cargo,'test','--offline','--locked','-p','reverie-kvm','--test','static_elf','--no-run','--message-format=json']")
s=s.replace("artifact_selectors=[dict(id='lib',target='reverie_kvm',kind=['lib']),dict(id='static',target='static_elf',kind=['test']),dict(id='vmcall',target='vmcall',kind=['test']),dict(id='read-clock',target='read_clock',kind=['test'])]","artifact_selectors=[dict(id='static',target='static_elf',kind=['test'])]")
s=s.replace("scope='CPL3 timestamp recovery: exact 31 declarations and existing public vmcall/clock neighbors; no public real-mode interception or full-backend claim'","scope='One-fixture RIP attribution only, retaining the original exact count assertion; not component qualification'")
p.write_text(s)
p=Q/'admit_target.py';s=p.read_text().replace('rdtsc-recovery-v3-cold-v1',target).replace("HERE.parent/'launch']","HERE.parent/'launch',HERE/'fixture-evidence']");p.write_text(s)
p=Q/'retain_artifacts.py';s=p.read_text().replace("{'lib','static','vmcall','read-clock'},'wrong four harnesses'","{'static'},'wrong diagnostic harness'").replace('all four harnesses and linked KVM library','diagnostic harness and linked KVM library');p.write_text(s)
setup=json.loads((P/'qualification-v1/SETUP.json').read_text());setup['source_root']=str(N/'source');setup['target']=str(R/'target'/target);setup['source_files']=[dict(relative=C,file=rec(N/'source'/C))];setup['lock']=rec(N/'source/Cargo.lock');setup['purpose']='One exact Host-owned worker diagnostic, no changed test oracle or production source';write('qualification-v1/SETUP.json',setup)
phase_manifest=json.loads((P/'qualification-v1/source-manifest.json').read_text())
for row in phase_manifest:
 if row['path']==C:row['sha256']=rec(N/'source'/C)['sha256']
write('qualification-v1/source-manifest.json',phase_manifest)
oldelf=json.loads((P/'qualification-v1/retained-binaries/ARTIFACTS.json').read_text())['static']['file']
write('qualification-v1/PREDECESSOR-ELFS.json',dict(source=rec(P/'qualification-v1/retained-binaries/BINDING.json'),hashes={'static':oldelf['sha256']},purpose='Refuse the failed V3 harness as newly compiled diagnostic source'))
caller='';origins=[]
for p in sorted(Q.glob('*.py')):
 old=P/'qualification-v1'/p.name;ast.parse(p.read_text());caller+=diff(old.read_text(),p.read_text(),'qualification-v1/'+p.name);origins.append(dict(before=rec(old),after=rec(p),unchanged=old.read_bytes()==p.read_bytes()))
write('CALLER-DELTA.patch',caller)
write('qualification-v1/RUNNER_ORIGINS.json',dict(sources=origins,observer_before=rec(P/'qualification-v1/observer/observer.py'),observer_after=rec(Q/'observer/observer.py'),material_delta=rec(N/'CALLER-DELTA.patch')))
write('REPORT.md','''Host-owned timestamp RIP diagnostic — preparation only

This is author investigation, not a correction or source approval. V3 timestamp-14 actually failed raw 101 with twelve root-Pid Tsc callbacks versus the original exact two. Status 0 and empty guest stdout/stderr were observed first. The extra instruction origins have not been measured; loader startup is a source lead, not the conclusion.

Only the isolated static_elf.rs test source changes. A private diagnostic Tool records every (Pid, Rdtsc, saved user RIP) through the same one RPC per callback and returns the identical sentinel. Existing TimestampTool/TimestampLog and their consumers are untouched. No callback is filtered, reordered, aggregated away or exempted. The original C source and all original guest assertions remain; the exact two-call vector is byte-identical. Diagnostic records are retained before that assertion, so if twelve recur this selector still returns a real failed result, not accepted evidence of correctness.

The fixture-specific compiler helper invokes the identical actual GCC command (-O2 -pthread), recording argv/cwd/status/stdout/stderr and retaining the actual generated C source and ELF before TestDirectory cleanup. It also retains the exact PT_INTERP pathname/canonical target/bytes used by that generated ELF. The phase environment and compiler/loader inputs remain bound. Raw output and generated artifacts will be hashed after terminal readback. The selected test uses an explicitly bound output directory, required empty before compiling. No stored executable or inferred compiler output is accepted.

Production and all other test bodies are unchanged. No component, native reference, Host-worker proof or same-run parity credit comes from preparation. Native probes are separate. Original V1 warning refusal, V2 fault helper failure, predecessor helper failure and V3 count failure remain intact.
''')
write('PLAN.md','''Finite diagnostic execution plan — requires root inspection before launch

Admit a new empty target using the existing lane lease and unchanged observer137c/common/phase/cache helpers. Explicit PYTHONOPTIMIZE=0 is required and optimize=0/debug=true must be recorded. Run metadata, bind its actual resolved dependencies, compile only the static_elf test harness, retain its fresh distinct-inode ELF and newly built linked KVM library, format-check the changed test file, list the actual static inventory, then run only host_owned_timestamp_worker_keeps_native_execution under --exact --test-threads=1 --nocapture with structured libtest output. No unrelated tests run, and this finite diagnostic does not replace the 31-control qualification requirement.

Bounds are unchanged: metadata and cold compile 600 aggregate CPU seconds / 900 wall / two jobs / offline locked; format, inventory and the single diagnostic 30 CPU / 60 wall. Memory 16 GiB, swap zero, stderr 16 MiB, stdout 64 MiB, phase reads 16 MiB and free floor 100 GiB. REQUIRE_KVM remains set and raw stdout/stderr require separate no-skip readback. No first refusal is retried. The expected count mismatch is still raw failure and accepted=false; diagnostic value does not relabel it.

After the actual selector, retain and hash all compiler records, C/ELF/interpreter bytes and full callback list. Query only these retained binaries using bounded readelf -W -l -s and objdump -d for concrete program/interpreter timestamp instruction addresses and nearby symbols; each query uses existing 30 CPU / 60 wall upper limits, at most 16 MiB stdout/stderr and exact executable/loader bindings. Those inspection commands execute no guest. Match saved RIPs against actual instruction bytes and source-bound ELF load-bias rules, stating any remaining mapping ambiguity. No source-only startup attribution and no replacement count of twelve is authorized. A corrected fixture contract must be proposed after attribution.
''')
write('TARGET.json',dict(status='PREPARED ONLY, NO COMPILE OR TEST EXECUTION',predecessor=rec(P/'TARGET.json'),failed_qualification=rec(P/'qualification-stop-v1/READBACK.json'),source_patch=rec(N/'SOURCE.patch'),source_manifest=rec(N/'SOURCE-MANIFEST.json'),test_continuity=rec(N/'TEST-CONTINUITY.json'),caller_delta=rec(N/'CALLER-DELTA.patch'),setup=rec(Q/'SETUP.json'),selectors=rec(Q/'SELECTORS.json'),plan=rec(N/'PLAN.md'),report=rec(N/'REPORT.md'),production_unchanged=True,original_exact_two_assertion=True,new_target=setup['target'],phase_count=5,test_declarations=1))
inputs=[]
for p in N.rglob('*'):
 if p.is_file()or p.is_symlink():inputs.append(rec(p))
for p in [P/'TARGET.json',P/'SOURCE-MANIFEST.json',P/'qualification-stop-v1/READBACK.json',P/'qualification-v1/controls/timestamp-14/result.json',P/'qualification-v1/observer/timestamp-14/stdout',P/'qualification-v1/observer/timestamp-14/stderr',P/'qualification-v1/retained-binaries/BINDING.json']:inputs.append(rec(p))
write('INPUTS.json',dict(records=inputs))
for row in inputs:assert rec(row['path'])==row
write('READBACK.json',dict(target=rec(N/'TARGET.json'),inputs=rec(N/'INPUTS.json'),records=len(inputs),source_entries=len(manifest),production_changes=0,source_changed_paths=[C],diagnostic_tests=1,executed=False))
print(json.dumps({x:rec(N/x)for x in ['TARGET.json','SOURCE.patch','CALLER-DELTA.patch','READBACK.json']},indent=2))
