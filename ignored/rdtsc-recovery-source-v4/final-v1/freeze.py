from pathlib import Path
import fcntl,hashlib,json,os
F=Path(__file__).resolve().parent;N=F.parent;Q=N/'qualification-v1';A=N/'qualification-result-v1'
def rec(p):
 p=Path(p);st=p.lstat();b=os.readlink(p).encode() if p.is_symlink() else p.read_bytes();r=dict(path=str(p),bytes=len(b),sha256=hashlib.sha256(b).hexdigest(),mode=st.st_mode&0o7777)
 if p.is_symlink():r.update(kind='symlink',target=os.readlink(p))
 return r
def write(n,v):
 with (F/n).open('x') as f:f.write(v if isinstance(v,str) else json.dumps(v,indent=2)+'\n')
results=json.loads((A/'RESULTS.json').read_text());assert results['all_qualified']
plan=json.loads((Q/'clippy-plan.json').read_text());binding=plan['lease'];p=Path(binding['path'])
fd=os.open(p,os.O_RDONLY|os.O_CLOEXEC|os.O_NOFOLLOW)
try:
 st=os.fstat(fd);assert [st.st_dev,st.st_ino,st.st_uid]==binding['identity']
 fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
 token=json.loads(os.pread(fd,4096,0));assert token['plan_sha256']==rec(Q/'clippy-plan.json')['sha256']
 completion=Path(binding['state_directory'])/(token['plan_sha256']+'.json');c=json.loads(completion.read_text())
 assert rec(c['result']['path'])==c['result']
 terminal=json.loads(Path(c['result']['path']).read_text());assert terminal['accepted'] and terminal['terminal_authenticated']
 observer=json.loads(Path(terminal['observer_result']['path']).read_text());assert observer['final_accounting']['cgroup_empty']
 assert observer['final_accounting']['properties']['MainPID']==0
 lease=dict(identity=binding['identity'],path=str(p),token=token,completion=rec(completion),last_result=rec(c['result']['path']),exclusive_nonblocking_available=True,terminal=True,no_token_write=True)
finally:os.close(fd)
lease['descriptor_closed']=True;write('LEASE-RELEASE.json',lease)
write('REPORT.md','''# V4 component qualification completed

All 40 planned phases were accepted on their first attempt: 31 exact declarations passed, zero failed or ignored, plus metadata, cold compile, format, four inventories, core/ptrace check and Clippy with -D warnings. All 31 raw stdout/stderr pairs were separately inspected; no skip or unexecuted-capability message appeared, including historical timestamp-19. Inventories are 510 library / 317 static ELF / 6 vmcall / 8 read-clock declarations, distinct from selection or physical guest counts.

The new loader-free Host-worker control passed its original exact two-root callback/status/output assertions together with both full timestamp-result checks, real clone, worker TID/post-RDTSC marker and clear-TID/futex completion. Prefix/register, LOCK/overlength, subscribed unrelated faults, single-step exact boundary, lifecycle, actual cleanup rejection and the unchanged direct/Tool fault neighbor all ran successfully. The cleanup negative's expected caught assertion panic and actual WithCleanup Debug value remain in raw stderr; the selected suite passed, and the aggregate was not flattened into a fault-only success.

Fresh Cargo JSON identifies all four emitted harnesses and the linked non-test KVM library as fresh=false. Immutable distinct-inode harness copies, source, lock, dependency inputs, loader/toolchain closure, exact plans, raw results, observer accounting and source/SCM readback are retained. Compilation emitted zero diagnostics. Total systemd-accounted aggregate CPU was 295.666952 seconds; summed payload elapsed time was 104.5281079816632 seconds and summed observer transport time was 230.06808288930915 seconds. These measure different boundaries. Original per-phase limits, two jobs, zero-diagnostic gate, selectors, order, skip audit and resource guards were unchanged. The lane's final terminal completion was authenticated; a nonblocking exclusive read-only acquisition succeeded and was closed without rewriting its token.

Live R HEAD remains 79516661bf82d30ab2967c71834a6d47447b76ee, branch codex/kvm-setitimer-20260918, clean product/index state unchanged. Its source base is tree-equivalent to landed 44fcb1955f44547f50d724fe8f7d718215fed446. V4 changes only the new test fixture; production is byte-identical to V3 and the separate H39ac build. Every historical failure remains a failure: V1 warning refusal, V2 fault-helper failure, predecessor fault-helper failure, V3 twelve-versus-two pthread failure, diagnostic-v1 compile refusal and diagnostic-v2 unchanged count failure. Actual loader RIP attribution explains the fixture correction; no old failed run was relabelled.

This is finite component evidence, not independent source approval or same-run parity. The completed Claude V3 source review requested changes; its F1-F3 will be investigated separately after this packet is frozen. Neither this passing suite nor the test-only V4 correction discharges those findings. The public CPL0/real-mode timestamp interception gap and full portable comparator outcome remain separate scope/evidence. The active native source review has not received concurrent Claude findings.
''')
write('TARGET.json',dict(source_target=rec(N/'TARGET.json'),source_manifest=rec(N/'SOURCE-MANIFEST.json'),source_patch=rec(N/'SOURCE.patch'),delta=rec(N/'DELTA.patch'),caller_delta=rec(N/'CALLER-DELTA.patch'),results=rec(A/'RESULTS.json'),raw_execution_audit=rec(A/'RAW-EXECUTION-AUDIT.json'),qualification_readback=rec(A/'READBACK.json'),artifacts=rec(Q/'retained-binaries/ARTIFACTS.json'),binary_binding=rec(Q/'retained-binaries/BINDING.json'),report=rec(F/'REPORT.md'),lease=rec(F/'LEASE-RELEASE.json'),qualified_phases=40,passed_declarations=31,source_review_approval=False,parity_qualified=False))
records={r['path']:r for r in json.loads((A/'INPUTS.json').read_text())['records']}
for p in list(A.iterdir())+list(F.iterdir()):
 if p.is_file():records[str(p)]=rec(p)
for row in records.values():assert rec(row['path'])==row
write('INPUTS.json',dict(records=list(records.values()),source_entries=2622))
write('READBACK.json',dict(target=rec(F/'TARGET.json'),results=rec(A/'RESULTS.json'),inputs=rec(F/'INPUTS.json'),report=rec(F/'REPORT.md'),records=len(records),all_inputs_unchanged=True,all_40_phases_accepted=True,all_31_tests_executed_without_skip=True,lease_closed=True,no_source_approval_or_parity_claim=True))
print(json.dumps({n:rec(F/n) for n in ['TARGET.json','REPORT.md','INPUTS.json','READBACK.json','LEASE-RELEASE.json']},indent=2))
