from pathlib import Path
import hashlib,json
N=Path(__file__).resolve().parent;R=N.parent/'rdtsc-recovery-source-v4';H=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-replay-prerequisites-20260918/ignored/rdtsc-h39ac-same-run-v3/composition/hermit');S=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-parent-reader-support-20260916')
def rec(p):
 b=p.read_bytes();return dict(path=str(p),bytes=len(b),sha256=hashlib.sha256(b).hexdigest(),mode=p.stat().st_mode&0o7777)
def write(n,v):
 with (N/n).open('x') as f:json.dump(v,f,indent=2);f.write('\n')
paths=[R/'source'/p for p in ['reverie-kvm/src/runtime.rs','reverie-kvm/src/parked_signal_runtime.rs','reverie-kvm/src/executor.rs','reverie-kvm/src/elf.rs','reverie-kvm/src/vm.rs','reverie-kvm/src/cpuid.rs','reverie-kvm/src/timestamp.rs','reverie-kvm/src/syscall.rs','reverie-kvm/tests/static_elf.rs','reverie-kvm/tests/vmcall.rs','reverie-ptrace/src/task.rs','reverie-syscalls/src/syscalls.rs']]
paths += [H/p for p in ['detcore/src/lib.rs','detcore/src/tool_global.rs','detcore/src/cpuid.rs','detcore/src/syscalls/time.rs','detcore/src/syscalls/threads.rs']]
paths += [S/'ignored/kvm-rdtsc-claude-review-v2-20260918'/p for p in ['REVIEW.md','ROOT-FOLLOWUP-FACTS.md']]
paths += [S/'ignored/kvm-rdtsc-native-probes-v2/probe.c',R/'TARGET.json',R/'final-v1/TARGET.json',R/'final-v1/READBACK.json',R/'qualification-result-v1/RESULTS.json',N/'REPORT.md',N/'freeze.py']
records=[rec(p) for p in paths]
write('INPUTS.json',dict(records=records,kind='Author source design; no independent source verdict',sources='Exact frozen V4 and H39ac composition; no live product paths'))
for row in records:assert rec(Path(row['path']))==row
write('READBACK.json',dict(report=rec(N/'REPORT.md'),inputs=rec(N/'INPUTS.json'),records=len(records),source_unchanged=True,no_execution=True,f1_f2_implementation_next='new isolated V5',f3_policy_changes_authorized=False))
print(json.dumps({n:rec(N/n) for n in ['REPORT.md','INPUTS.json','READBACK.json']},indent=2))
