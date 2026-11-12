"""Authenticate and freeze preparation records only; no source/helper execution."""
from pathlib import Path
import ast
import hashlib
import json
import os

N = Path(__file__).resolve().parent
R = N.parents[1]
P = N.parent/'rdtsc-recovery-source-v6'
Q = N/'qualification-v1'
S = R.parent/'kvm-parent-reader-support-20260916'

def rec(path):
    path = Path(path)
    info = path.lstat()
    h = hashlib.sha256()
    if path.is_symlink():
        raw = os.fsencode(os.readlink(path)); h.update(raw)
        return dict(path=str(path), bytes=len(raw), sha256=h.hexdigest(), mode=info.st_mode & 0o7777,
                    kind='symlink', target=os.readlink(path))
    with path.open('rb') as f:
        for chunk in iter(lambda: f.read(1024**2), b''): h.update(chunk)
    return dict(path=str(path), bytes=info.st_size, sha256=h.hexdigest(), mode=info.st_mode & 0o7777)

def write(name, value):
    with (N/name).open('x') as f: json.dump(value, f, indent=2); f.write('\n')

probe = N/'source/reverie-kvm/src/cpuid_transport_probe.rs'
assert probe.read_text().count('eprintln!(') == 4
assert hashlib.sha256(probe.read_bytes().replace(b'eprintln!(', b'println!(')).hexdigest() == 'a3014ca5dc9235adb1d002cd29c23d7c507b9b7a5606ac4e27704eca89e6a077'
assert rec(N/'origin/probe.rs')['sha256'] == '492dc946c68955d2c94c6d7d560aa9a2cfd5252db96d7f5d3be5428d35c6bfae'
assert rec(N/'origin/probe.rs')['sha256'] == rec(S/'ignored/kvm-cpuid-transport-probe-source-v1/probe.rs')['sha256']
assert rec(N/'origin/PLAN.md')['sha256'] == rec(S/'ignored/kvm-cpuid-transport-probe-source-v1/PLAN.md')['sha256']
write('CONTROL-CONTINUITY.json', dict(original=rec(N/'origin/probe.rs'), final=rec(probe),
       formatter_preparation=rec(N/'format-preparation.json'),
       formatted_original_sha256='a3014ca5dc9235adb1d002cd29c23d7c507b9b7a5606ac4e27704eca89e6a077',
       final_after_reverting_exact_four_diagnostic_macros_equals_formatted_original=True,
       assertions_and_diagnostic_values_unchanged=True, original_existing_tests_unchanged=True,
       root_original_delta=rec(N/'PROBE-INTEGRATION.patch'), no_execution=True))

for path in [*Q.rglob('*.py'), N/'execute_phases.py', N/'prepare_packet.py', N/'freeze_packet.py']:
    ast.parse(path.read_text())
assert json.loads((N/'RUN-ORDER.json').read_text())['phases'] == ['metadata','compile','format','list-lib','probe-01']
assert len(json.loads((Q/'SELECTORS.json').read_text())['groups']) == 1
assert not Path(json.loads((Q/'SETUP.json').read_text())['target']).exists()
assert not (Q/'TARGET-ADMISSION.json').exists()
assert not (Q/'controls').exists()
assert not (N/'launch').exists()

registry = Path('/home/newton/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/kvm-ioctls-0.25.0')
write('SOURCE-READS.json', dict(scope='Source excerpts read, not a full-kernel or independent review of authored integration',
       complete_documents=[rec(N/'origin/probe.rs'),rec(N/'origin/PLAN.md'),rec(probe),
                           rec(N/'SOURCE.patch'),rec(N/'CALLER-DELTA.patch'),
                           rec(S/'ignored/kvm-cpuid-transport-research-v1/REPORT.md')],
       current_sources=[dict(file=rec(N/'source'/rel), sections=sections) for rel,sections in [
           ('reverie-kvm/src/vm.rs',['KvmBackend construction 1035-1100','static ELF installation 1327-1404','exception frame 2615-2638','test include 3671-3675','minimal_test_elf 6345-6383']),
           ('reverie-kvm/src/bootstrap.rs',['exception tables, gate, stub and frame shape 515-565']),
           ('reverie-kvm/src/clock.rs',['CountedVcpu Deref and lifecycle initialization 35-67'])]],
       resolved_ioctl_sources=[dict(file=rec(registry/'src/ioctls/vcpu.rs'), sections=['actual get_msrs/set_msrs methods and their count docs, 668-780']),
                               dict(file=rec(registry/'src/ioctls/system.rs'), sections=['feature index and system get_msrs methods, 440-542'])],
       no_hardware_or_vm_execution=True))

write('TARGET.json', dict(status='FROZEN PREPARATION; NO BUILD OR PROBE EXECUTION',
       source=str(N/'source'), source_manifest=rec(N/'SOURCE-MANIFEST.json'), source_patch=rec(N/'SOURCE.patch'),
       base_qualified_v6=rec(P/'final-v1/TARGET.json'), v6_source=rec(P/'SOURCE-MANIFEST.json'),
       original_root_probe=rec(N/'origin/probe.rs'), final_probe=rec(probe),
       probe_delta=rec(N/'PROBE-INTEGRATION.patch'), caller_delta=rec(N/'CALLER-DELTA.patch'),
       setup=rec(Q/'SETUP.json'), selectors=rec(Q/'SELECTORS.json'),
       runner_origins=rec(Q/'RUNNER_ORIGINS.json'), observer=rec(Q/'observer/observer.py'),
       plan=rec(N/'PLAN.md'), report=rec(N/'REPORT.md'), review=rec(N/'REVIEW.md'),
       phases=5, selected_declarations=1, planned_backends=3, planned_guest_entries=5,
       production_behavior_changed=False, execution_authorized=False))

records = {}
def add(path):
    path=Path(path)
    if path.is_file() or path.is_symlink(): records[str(path)]=rec(path)

for path in N.rglob('*'): add(path)
for row in json.loads((Q/'RUNNER_ORIGINS.json').read_text())['records']: add(row['before']['path'])
for row in json.loads((Q/'toolchain-standard-inputs.json').read_text())['files']:
    actual=rec(row['path']); assert actual == row; records[actual['path']]=actual
for row in json.loads((Q/'observer/source-inputs.json').read_text())['inputs']:
    actual=rec(Path(row['path']).resolve(strict=True)); assert actual['sha256']==row['sha256'] and actual['bytes']==row['bytes']
    add(row['path']); add(Path(row['path']).resolve(strict=True))
for rel in ['TARGET.json','SOURCE-MANIFEST.json','READBACK.json','final-v1/TARGET.json','final-v1/INPUTS.json','final-v1/READBACK.json',
            'qualification-result-v1/RESULTS.json','qualification-v1/SETUP.json','qualification-v1/observer/metadata/stdout',
            'qualification-v1/retained-binaries/BINDING.json','qualification-v1/retained-binaries/ARTIFACTS.json']:
    add(P/rel)
add(P/'source/reverie-kvm/src/vm.rs');add(P/'source/Cargo.lock')
old_artifacts=json.loads((P/'qualification-v1/retained-binaries/ARTIFACTS.json').read_text())
old=rec(old_artifacts['lib']['file']['path']); assert old==old_artifacts['lib']['file']; records[old['path']]=old
for dirname in ['kvm-cpuid-transport-probe-source-v1','kvm-cpuid-transport-research-v1','kvm-cpuid-feature-query-v1']:
    for path in (S/'ignored'/dirname).iterdir(): add(path)
for path in [registry/'src/ioctls/vcpu.rs',registry/'src/ioctls/system.rs']: add(path)
source=json.loads((N/'SOURCE-MANIFEST.json').read_text())
directories=[]
for row in source:
    path=N/'source'/row['relative']
    if row['kind']=='unexpanded_gitlink':
        assert path.is_dir() and not path.is_symlink() and not list(path.iterdir())
        directories.append(dict(path=str(path),kind=row['kind'],empty=True,mode=path.stat().st_mode & 0o7777))
    else:
        actual=rec(path);assert actual['sha256']==row['sha256']
for row in records.values(): assert rec(row['path']) == row
write('INPUTS.json',dict(records=list(records.values()), source_entries=len(source), directories=directories,
      meaning='Frozen preparation source/evidence. Future actual metadata/dependency/loader and result bindings are not invented here.'))
write('READBACK.json',dict(target=rec(N/'TARGET.json'),inputs=rec(N/'INPUTS.json'), records=len(records),
      symlinks=sum(row.get('kind')=='symlink' for row in records.values()), source_entries=len(source),
      declared_unexpanded_gitlinks=directories, all_records_authenticated=True, source_only=True,
      no_target_admission_or_lease_action=True, no_cargo_test_vm_execution=True,
      original_observer_phase_common_lease_and_outer_driver_unchanged=True,
      diagnostic_stream_delta_only_after_mechanical_formatting=True))
print(json.dumps({name:rec(N/name) for name in ['TARGET.json','SOURCE.patch','CALLER-DELTA.patch','REVIEW.md','REPORT.md','PLAN.md','INPUTS.json','READBACK.json']},indent=2))
