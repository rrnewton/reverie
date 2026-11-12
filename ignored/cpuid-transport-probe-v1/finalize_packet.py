"""Freeze actual probe observations and already terminal qualification evidence."""
from pathlib import Path
import fcntl
import hashlib
import json
import os
import re

N=Path(__file__).resolve().parent
Q=N/'qualification-v1'
A=N/'qualification-result-v1'
F=N/'final-v1'

def rec(path):
    path=Path(path);info=path.lstat();h=hashlib.sha256()
    if path.is_symlink():
        data=os.fsencode(os.readlink(path));h.update(data);size=len(data)
    else:
        size=info.st_size
        with path.open('rb')as stream:
            for chunk in iter(lambda:stream.read(1024**2),b''):h.update(chunk)
    value=dict(path=str(path),bytes=size,sha256=h.hexdigest(),mode=info.st_mode&0o7777)
    if path.is_symlink():value.update(kind='symlink',target=os.readlink(path))
    return value

def write(name,value):
    with (F/name).open('x')as f:f.write(value if isinstance(value,str)else json.dumps(value,indent=2)+'\n')

r=json.loads((A/'RESULTS.json').read_text())
assert r['all_qualified'] and r['attempted']==5 and r['passed_declarations']==1
assert r['failed_declarations']==r['ignored_declarations']==0 and r['no_skip_messages']
F.mkdir()
raw=Q/'observer/probe-01/stderr'
lines=raw.read_text().splitlines()
events=[]
for number,line in enumerate(lines,1):
    match=re.fullmatch(r'host feature: count=(\d+), platform_info=(0x[0-9a-f]+)',line)
    if match:
        events.append(dict(line=number,kind='host_feature',count=int(match[1]),platform_info=int(match[2],16)));continue
    match=re.fullmatch(r'(.+): get_msrs count=(\d+), entries=\[(.+)\]',line)
    if match:
        entries=[dict(index=int(a),reserved=int(b),data=int(c)) for a,b,c in re.findall(r'kvm_msr_entry \{ index: (\d+), reserved: (\d+), data: (\d+) \}',match[3])]
        assert len(entries)==2
        events.append(dict(line=number,kind='read_msrs',label=match[1],count=int(match[2]),entries=entries));continue
    match=re.fullmatch(r'(.+): set_msrs index=(0x[0-9a-f]+), value=(0x[0-9a-f]+), count=(\d+)',line)
    if match:
        events.append(dict(line=number,kind='write_msr',label=match[1],index=int(match[2],16),value=int(match[3],16),count=int(match[4])));continue
    match=re.fullmatch(r'(.+): vector=(\d+), guest_rip=(0x[0-9a-f]+), error=(\d+), rax=(0x[0-9a-f]+), rbx=(0x[0-9a-f]+), rcx=(0x[0-9a-f]+), rdx=(0x[0-9a-f]+)',line)
    assert match,('unparsed hardware diagnostic',number,line)
    events.append(dict(line=number,kind='fault',label=match[1],vector=int(match[2]),guest_rip=int(match[3],16),error=int(match[4]),
                       registers=dict(zip(['rax','rbx','rcx','rdx'],[int(match[i],16)for i in range(5,9)]))))
assert len(events)==18
host=[e for e in events if e['kind']=='host_feature'];reads=[e for e in events if e['kind']=='read_msrs'];writes=[e for e in events if e['kind']=='write_msr'];faults=[e for e in events if e['kind']=='fault']
assert len(host)==1 and host[0]['count']==1 and host[0]['platform_info']==0x80000000
assert len(reads)==6 and all(e['count']==2 and [x['index']for x in e['entries']]==[206,320]for e in reads)
assert [(e['label'],[x['data']for x in e['entries']])for e in reads]==[
 ('initial',[0x80000000,0]),('armed',[0x80000000,1]),('independent',[0x80000000,0]),
 ('first remains armed',[0x80000000,1]),('disarmed',[0x80000000,0]),('unrelated initial',[0x80000000,0])]
assert len(writes)==6 and all(e['count']==1 for e in writes)
assert [(e['index'],e['value'])for e in writes]==[(206,0x80000000),(320,1),(320,0),(206,0x80000000),(206,0x80000000),(320,1)]
assert [(e['label'],e['guest_rip'])for e in faults]==[
 ('unarmed CPUID reaches later HLT',0x20000c),('armed CPUID',0x20000a),
 ('independent CPUID reaches HLT',0x20000c),('disarmed CPUID reaches HLT',0x20000c),('unrelated CLI still faults',0x200000)]
assert all(e['vector']==13 and e['error']==0 for e in faults)
table=dict(rax=0x663,rbx=0,rcx=1,rdx=0x20100800)
assert all(faults[i]['registers']==table for i in [0,2,3])
assert faults[1]['registers']==dict(rax=0x80000001,rbx=0,rcx=0,rdx=0)
assert faults[4]['registers']==dict(rax=0,rbx=0,rcx=0,rdx=0)
write('HARDWARE-OBSERVATIONS.json',dict(raw_stderr=rec(raw),raw_stdout=rec(Q/'observer/probe-01/stdout'),
      current_kernel=list(os.uname()),all_raw_diagnostic_lines_accounted=True,events=events,
      actual_guest_entries=5,actual_separate_vm_backends=3,same_vm_multivcpu_test=False,
      support_bit_initially_present=True,explicit_support_write_verified=True,enable_transitions_measured=[0,1,0],
      cpuid_tool_callback_or_accounting_test=False))

path=Q/'probe-01-plan.json';plan=json.loads(path.read_text());binding=plan['lease']
fd=os.open(binding['path'],os.O_RDONLY|os.O_CLOEXEC|os.O_NOFOLLOW)
try:
    st=os.fstat(fd);assert [st.st_dev,st.st_ino,st.st_uid]==binding['identity']
    fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
    token=json.loads(os.pread(fd,4096,0));assert token['plan_sha256']==rec(path)['sha256']
    completion=Path(binding['state_directory'])/(token['plan_sha256']+'.json')
    c=json.loads(completion.read_text());assert rec(c['result']['path'])==c['result']
    terminal=json.loads(Path(c['result']['path']).read_text());assert terminal['terminal_authenticated']and terminal['accepted']
    assert rec(terminal['observer_result']['path'])==terminal['observer_result']
    observed=json.loads(Path(terminal['observer_result']['path']).read_text());final=observed['final_accounting']
    assert final['cgroup_empty'] and final['properties']['MainPID']==0 and final['properties']['ControlGroup']==''
    lease=dict(path=binding['path'],identity=binding['identity'],token=token,completion=rec(completion),
               exclusive_nonblocking_available=True,terminal=True,accepted=True,no_token_write=True)
finally:os.close(fd)
lease['descriptor_closed']=True;write('LEASE-RELEASE.json',lease)
inventory=json.loads((Q/'controls/list-lib/result.json').read_text())['readback']['count']
artifacts=json.loads((Q/'retained-binaries/ARTIFACTS.json').read_text());assert set(artifacts)=={'lib'}
artifact=artifacts['lib'];assert artifact['cargo_artifact']['fresh']is False
assert rec(artifact['file']['path'])==artifact['file']
assert artifact['file']['sha256']!=json.loads((Q/'PREDECESSOR-ELFS.json').read_text())['hashes']['lib']
write('REPORT.md',f'''# Actual CPUID transport probe passed

All five planned phases passed on their first attempt: locked metadata, cold library compile, formatting, actual inventory and the exact hardware probe. The declaration passed with zero failed/ignored cases and no raw skip or unexecuted message. Actual inventory is {inventory}; one declaration made five real guest entries across three separate VM backends. The probe's own libtest duration was 0.358927273 seconds and observed payload duration was 0.36125817405991256 seconds.

The system feature query returned exact count 1 and PLATFORM_INFO 0x80000000. Each of six vCPU state reads returned exactly two entries. Each of six writes returned exactly one. Initial support was already present, so the explicit support write verifies accepted state rather than a 0-to-1 support transition. The enable bit was measured at 0, then 1, then 0 after disarm; a second backend remained at 0 while the first remained at 1.

Armed CPUID produced genuine #GP(0) at its original RIP 0x20000a, with RAX=0x80000001, RCX=0, RBX=0 and RDX=0. Unarmed, independent and disarmed CPUID runs instead reached the following privileged HLT at 0x20000c, with the installed table tuple (0x663, 0, 1, 0x20100800). An independently armed CLI fixture retained its genuine #GP(0) at 0x200000. HARDWARE-OBSERVATIONS.json binds and accounts for all 18 raw diagnostic lines and every actual register/MSR value; raw streams are retained unchanged.

Compile emitted the whole-library test harness with fresh=false and no structured diagnostics. The retained ELF SHA256 is {artifact['file']['sha256']}, copied to a distinct inode and mode 0555. The complete source, actual lock/metadata and dependency closure, toolchain/loader, phase plans/results and terminal accounting were authenticated. Live source/index/HEAD remained unchanged. Aggregate CPU across the five phases is {r['total_cpu_seconds']} seconds; summed payload time is {r['total_payload_seconds']} seconds; summed observer transport time is {r['total_observer_transport_seconds']} seconds. These measurement boundaries are separate. No limits were changed. The final lease's accepted terminal completion was authenticated and a nonblocking exclusive read-only hold was closed without token mutation.

This measures the proposed MSR-controlled fault transport on the recorded running kernel only. It does not implement or qualify a CPUID Tool callback, callback return-register policy, one-time nondeterministic instruction accounting, lifecycle subscription handling, same-VM multi-vCPU isolation, ptrace/KVM parity or timestamp cancellation. The separate failed Detcore cancellation witness is not converted to success. No production source, commit, pin, PR or scheduler change occurred.
''')
write('TARGET.json',dict(source_target=rec(N/'TARGET.json'),source_manifest=rec(N/'SOURCE-MANIFEST.json'),
      source_patch=rec(N/'SOURCE.patch'),caller_delta=rec(N/'CALLER-DELTA.patch'),results=rec(A/'RESULTS.json'),
      raw_execution_audit=rec(A/'RAW-EXECUTION-AUDIT.json'),qualification_readback=rec(A/'READBACK.json'),
      hardware=rec(F/'HARDWARE-OBSERVATIONS.json'),binary_binding=rec(Q/'retained-binaries/BINDING.json'),
      artifacts=rec(Q/'retained-binaries/ARTIFACTS.json'),report=rec(F/'REPORT.md'),lease=rec(F/'LEASE-RELEASE.json'),
      qualified_phases=5,passed_declarations=1,inventory=inventory,transport_measured=True,
      production_callback_qualified=False,parity_qualified=False,Detcore_cancellation_qualified=False))
records={row['path']:row for row in json.loads((A/'INPUTS.json').read_text())['records']}
for path in [*A.iterdir(),*F.iterdir(),Path(__file__).resolve(),completion]:
    if path.is_file():records[str(path)]=rec(path)
for row in records.values():assert rec(row['path'])==row
write('INPUTS.json',dict(records=list(records.values()),source_entries=2624))
write('READBACK.json',dict(target=rec(F/'TARGET.json'),results=rec(A/'RESULTS.json'),inputs=rec(F/'INPUTS.json'),
      report=rec(F/'REPORT.md'),hardware=rec(F/'HARDWARE-OBSERVATIONS.json'),records=len(records),
      all_inputs_unchanged=True,all_five_phases_accepted=True,one_real_test_without_skip=True,
      lease_closed=True,no_callback_implementation_or_parity_claim=True))
print(json.dumps({name:rec(F/name)for name in ['TARGET.json','REPORT.md','HARDWARE-OBSERVATIONS.json','INPUTS.json','READBACK.json','LEASE-RELEASE.json']},indent=2))
