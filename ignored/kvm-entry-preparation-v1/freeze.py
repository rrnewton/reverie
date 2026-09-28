import difflib
import hashlib
import json
import re
from pathlib import Path

D = Path(__file__).resolve().parent
L = D.parent.parent

def record(path):
    data = path.read_bytes()
    return dict(path=str(path), bytes=len(data), sha256=hashlib.sha256(data).hexdigest())

def write(name, value):
    (D / name).write_text(json.dumps(value, indent=2) + '\n')

proof = {'test_suffixes': [], 'unchanged_functions': [], 'product_execution': False}
for name in ['vm', 'memory', 'bootstrap', 'runtime']:
    before = (D / f'BASE-{name}.rs').read_text()
    after_path = D / ('PROPOSED-runtime.rs' if name == 'runtime' else f'{name}.rs')
    after = after_path.read_text()
    if name != 'runtime':
        assert (L / f'reverie-kvm/src/{name}.rs').read_bytes() == after_path.read_bytes()
    old_tests = before[before.index('\nmod tests {'):]
    new_tests = after[after.index('\nmod tests {'):]
    edits = [op for op in difflib.SequenceMatcher(None, old_tests.splitlines(True), new_tests.splitlines(True), autojunk=False).get_opcodes() if op[0] != 'equal']
    assert all(op[0] == 'insert' for op in edits), (name, edits)
    old_count = len(re.findall(r'^\s*#\[test\]', before, re.M))
    new_count = len(re.findall(r'^\s*#\[test\]', after, re.M))
    assert new_count - old_count == (1 if name in ['vm', 'memory'] else 0)
    proof['test_suffixes'].append(dict(file=name, original_declarations=old_count, candidate_declarations=new_count, only_insertions=edits, all_original_test_suffix_bytes_preserved=True))
    assert after.count('#[allow(') == before.count('#[allow('), name

base_vm = (D / 'BASE-vm.rs').read_text()
new_vm = (D / 'vm.rs').read_text()
for marker in ['    pub(crate) fn capture(\n', '    pub(crate) fn capture_for_action(\n', '    pub(crate) fn completed_syscall_boundary_registers(\n', '    async fn park_process_action(\n']:
    start = base_vm.index(marker)
    end = base_vm.index('\n    }\n', start) + len('\n    }\n')
    body = base_vm[start:end]
    assert body in new_vm, marker
    proof['unchanged_functions'].append(dict(marker=marker.strip(), bytes=len(body.encode()), sha256=hashlib.sha256(body.encode()).hexdigest()))

calls = []
for path in (L / 'reverie-kvm/src').rglob('*.rs'):
    text = path.read_text()
    for number, line in enumerate(text.splitlines(), 1):
        if re.search(r'\bset_syscall_return_park\b', line):
            calls.append(dict(path=str(path), line=number, text=line))
assert not calls, calls
proof['removed_blocking_park_helper_current_callers'] = calls
proof['runtime_proposal'] = {'applied_by_author': False, 'coordinator_reported_application': True, 'base': record(D/'BASE-runtime.rs'), 'proposal': record(D/'PROPOSED-runtime.rs'), 'patch': record(D/'RUNTIME-CALLER.patch')}
write('PRESERVATION.json', proof)
write('TEST-PLAN.json', {
    'status': 'proposed; no compiler or test execution in this increment',
    'compile': 'one composed offline locked no-run library build, owned by coordinator after interfaces settle',
    'new_exact_names': [
        'memory::tests::entry_copy_tests::read_preparation_runs_once_with_one_token_and_preserves_poison',
        'vm::tests::action_parent_captures_yield_for_close_and_keep_stop_and_validation'],
    'focused_existing_neighbor': 'vm::tests::completed_syscall_boundary_accepts_only_the_two_transport_rip_forms',
    'old_controls': 'preserved byte-for-byte; no repeat requested for already passed controls without a new concern',
    'expected': {'new_declarations': 2, 'new_vm_modes': 8, 'ignored': 0, 'new_controls_kvm_run': 0},
    'prerequisite': 'VM control requires /dev/kvm and actual vCPU register ioctls; it must not skip on unavailable KVM',
    'bounds': 'coordinator must bind exact composed source, retained executable, original finite per-phase runner limits, input/terminal checks before execution; this is not execution admission',
    'format': record(D/'FORMAT.json')})

files = sorted(p for p in D.iterdir() if p.is_file() and p.name != 'TARGET.json')
write('TARGET.json', {
    'kind': 'prepared asynchronous parent capture source increment',
    'base': '91110d249ffd8957267d71fab8c83d9636105efe',
    'owned_paths': ['reverie-kvm/src/vm.rs', 'reverie-kvm/src/memory.rs', 'reverie-kvm/src/bootstrap.rs'],
    'runtime': 'proposal only from author; coordinator reports applying exact patch',
    'product_execution': False,
    'source_ownership_released_after_freeze': True,
    'records': [record(p) for p in files]})
print(json.dumps({'target': record(D/'TARGET.json'), 'preservation': record(D/'PRESERVATION.json'), 'source': [record(D/f'{n}.rs') for n in ['vm','memory','bootstrap']], 'runtime_patch': record(D/'RUNTIME-CALLER.patch'), 'test_counts': proof['test_suffixes']}, indent=2))
