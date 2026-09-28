from pathlib import Path
import fcntl,hashlib,json,os
N=Path(__file__).resolve().parent;Q=N/'qualification-v1';A=N/'qualification-result-v1';F=N/'final-v1'
def rec(p):
 p=Path(p);st=p.lstat();h=hashlib.sha256()
 if p.is_symlink():b=os.fsencode(os.readlink(p));h.update(b);size=len(b)
 else:
  size=st.st_size
  with p.open('rb')as f:
   for b in iter(lambda:f.read(1024**2),b''):h.update(b)
 r=dict(path=str(p),bytes=size,sha256=h.hexdigest(),mode=st.st_mode&0o7777)
 if p.is_symlink():r.update(kind='symlink',target=os.readlink(p))
 return r
def write(n,v):
 with (F/n).open('x')as f:f.write(v if isinstance(v,str)else json.dumps(v,indent=2)+'\n')
r=json.loads((A/'RESULTS.json').read_text());assert r['all_qualified'] and r['attempted']==46 and r['passed_declarations']==37 and r['failed_declarations']==r['ignored_declarations']==0
assert r['no_skip_messages'];F.mkdir()
p=Q/'clippy-plan.json';plan=json.loads(p.read_text());binding=plan['lease'];fd=os.open(binding['path'],os.O_RDONLY|os.O_CLOEXEC|os.O_NOFOLLOW)
try:
 st=os.fstat(fd);assert [st.st_dev,st.st_ino,st.st_uid]==binding['identity'];fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB);token=json.loads(os.pread(fd,4096,0));assert token['plan_sha256']==rec(p)['sha256'];completion=Path(binding['state_directory'])/(token['plan_sha256']+'.json');c=json.loads(completion.read_text());terminal=json.loads(Path(c['result']['path']).read_text());assert terminal['terminal_authenticated'] and terminal['accepted'];obs=json.loads(Path(terminal['observer_result']['path']).read_text());assert obs['final_accounting']['cgroup_empty'] and obs['final_accounting']['properties']['MainPID']==0
 lease=dict(path=binding['path'],identity=binding['identity'],token=token,completion=rec(completion),exclusive_nonblocking_available=True,terminal=True,accepted=True,no_token_write=True)
finally:os.close(fd)
lease['descriptor_closed']=True;write('LEASE-RELEASE.json',lease)
inventories={k:len(json.loads((Q/'controls'/('list-'+k)/'result.json').read_text())['readback']['names'])for k in ['lib','static','vmcall','read-clock']}
write('REPORT.md',f'''# V6 component qualification completed

All 46 planned phases were accepted on their first attempt. All 37 exact declarations passed, with zero failures/ignored/unattempted. Raw stdout/stderr from every selected control was separately read for KVM skip/unexecuted messages; none appeared, including historical timestamp-19. Actual inventories are {inventories}, distinct from selection and physical guest modes.

The actual terminal controls passed tail Exit23/ExitGroup29, ordinary Exit23/ExitGroup29, real EIO consuming cleanup, four nonterminal tail refusals with unchanged guest-memory marker and exact error shape, and real sibling RPC/exit ordering. The public non-ELF loop cleared both hardware/software interception states in both modes while executing RDTSC and the following vmcall. These are twelve modes within four new declarations. The existing signal-guard and terminal-cancellation/IntoGuest controls passed their retained assertions. Old clock/branch/RPC, Host-worker exact callback, lifecycle, fault, prefix, register, cleanup, vmcall and read-clock selections also passed. Actual cleanup/refusal Debug output remains in raw stderr and is not erased or flattened into a success cause.

The compile emitted all four harnesses and the linked non-test candidate KVM library with fresh=false and no structured diagnostics. Distinct-inode immutable copies, source/lock/dependency/toolchain/loader inputs, every actual plan/observer/result and source/SCM readback are bound. Format, core/ptrace Cargo check and Clippy -D warnings passed. Aggregate measured CPU is {r['total_cpu_seconds']} seconds; summed payload time is {r['total_payload_seconds']} seconds; summed observer transport time is {r['total_observer_transport_seconds']} seconds. These are different measurement boundaries. Per-phase limits and two-job configuration were unchanged. Final terminal lease completion was authenticated and a nonblocking exclusive read-only acquisition was closed without token mutation.

V5's raw101 E0050 compile remains a failure, with zero tests executed and unqualified emitted library copies preserved. Its first report-assembly count assumption is separately disclosed. V6 changed only the missed private test implementation's ignored request parameter; production and all test bodies/semantic assertions are byte-identical to V5. The added exact terminal-cancellation neighbor is unchanged. All earlier timestamp/compiler/fault/pthread/diagnostic failures remain bound history. Nothing was retried or relabelled.

This is bounded component evidence, not an independent source approval, F3 CPU-policy verdict or full ptrace/KVM parity. It does not execute current Detcore's ThreadExited path or prove cancellation before/after its timestamp charge/clock RPC. That true composition witness remains separate work; successful synthetic-Tool/backend ordering is not substituted for it. The public CPL0 interception gap is unchanged. Both initial source reviews requested changes and no approval transfers from these passes. Live source/index/HEAD remains unchanged at the recorded 79516661 base; no commit, pin or PR action occurred.
''')
write('TARGET.json',dict(source_target=rec(N/'TARGET.json'),source_manifest=rec(N/'SOURCE-MANIFEST.json'),source_patch=rec(N/'SOURCE.patch'),delta=rec(N/'DELTA.patch'),caller_delta=rec(N/'CALLER-DELTA.patch'),results=rec(A/'RESULTS.json'),raw_execution_audit=rec(A/'RAW-EXECUTION-AUDIT.json'),qualification_readback=rec(A/'READBACK.json'),artifacts=rec(Q/'retained-binaries/ARTIFACTS.json'),binary_binding=rec(Q/'retained-binaries/BINDING.json'),report=rec(F/'REPORT.md'),lease=rec(F/'LEASE-RELEASE.json'),qualified_phases=46,passed_declarations=37,actual_inventories=inventories,source_review_approval=False,parity_qualified=False,Detcore_point_coverage=False))
records={x['path']:x for x in json.loads((A/'INPUTS.json').read_text())['records']}
for p in list(A.iterdir())+list(F.iterdir()):
 if p.is_file():records[str(p)]=rec(p)
for row in records.values():assert rec(row['path'])==row
write('INPUTS.json',dict(records=list(records.values()),source_entries=2623))
write('READBACK.json',dict(target=rec(F/'TARGET.json'),results=rec(A/'RESULTS.json'),inputs=rec(F/'INPUTS.json'),report=rec(F/'REPORT.md'),records=len(records),all_inputs_unchanged=True,all_46_phases_accepted=True,all_37_tests_executed_without_skip=True,lease_closed=True,no_source_approval_or_parity_claim=True))
print(json.dumps({n:rec(F/n)for n in ['TARGET.json','REPORT.md','INPUTS.json','READBACK.json','LEASE-RELEASE.json']},indent=2))
