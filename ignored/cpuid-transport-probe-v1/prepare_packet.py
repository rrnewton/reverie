"""Prepare only the isolated probe/caller packet; never run Cargo or a VM."""
from pathlib import Path
import ast
import copy
import difflib
import hashlib
import json
import os
import shutil
import subprocess

N = Path(__file__).resolve().parent
R = N.parents[1]
P = N.parent / 'rdtsc-recovery-source-v6'
Q = N / 'qualification-v1'
S = R.parent / 'kvm-parent-reader-support-20260916'
VM = 'reverie-kvm/src/vm.rs'
PROBE = 'reverie-kvm/src/cpuid_transport_probe.rs'
NAME = 'vm::tests::cpuid_fault_transport_probe::actual_cpuid_faulting_preserves_instruction_and_is_owned_by_one_vcpu'
TARGET = R / 'target/cpuid-transport-probe-v1-cold-v1'

def record(path):
    path = Path(path)
    st = path.lstat()
    h = hashlib.sha256()
    if path.is_symlink():
        data = os.fsencode(os.readlink(path))
        h.update(data)
        result = dict(path=str(path), bytes=len(data), sha256=h.hexdigest(), mode=st.st_mode & 0o7777,
                      kind='symlink', target=os.readlink(path))
    else:
        with path.open('rb') as f:
            for chunk in iter(lambda: f.read(1024**2), b''):
                h.update(chunk)
        result = dict(path=str(path), bytes=st.st_size, sha256=h.hexdigest(), mode=st.st_mode & 0o7777)
    return result

def write(path, value):
    path = N / path
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open('x') as f:
        f.write(value if isinstance(value, str) else json.dumps(value, indent=2) + '\n')

def diff(before, after, rel):
    return ''.join(difflib.unified_diff(before.splitlines(True), after.splitlines(True),
                                      fromfile='v6/' + rel, tofile='probe/' + rel))

assert not TARGET.exists() and not TARGET.is_symlink()
assert (N/'source'/PROBE).is_file()
source = []
for row in json.loads((P/'SOURCE-MANIFEST.json').read_text()):
    current = copy.deepcopy(row)
    rel = row['relative']
    path = N/'source'/rel
    current['path'] = str(path)
    if row['kind'] == 'unexpanded_gitlink':
        assert path.is_dir() and not path.is_symlink() and not list(path.iterdir())
    else:
        old = record(P/'source'/rel)
        new = record(path)
        assert old['sha256'] == row['sha256'] and old['mode'] == new['mode']
        if rel == VM:
            current.update(sha256=new['sha256'], bytes=new['bytes'], changed_from_baseline=True)
        else:
            assert new['sha256'] == old['sha256'], rel
    source.append(current)
probe = record(N/'source'/PROBE)
source.append(dict(relative=PROBE, path=probe['path'], mode='100644', kind='file',
                   bytes=probe['bytes'], sha256=probe['sha256'], file_mode=probe['mode'],
                   baseline_blob_sha1=None, changed_from_baseline=True))
source.sort(key=lambda row: row['relative'])
assert len(source) == 2624
assert (N/'source/Cargo.lock').read_bytes() == (P/'source/Cargo.lock').read_bytes()
write('SOURCE-MANIFEST.json', source)
write('SOURCE-CONTINUITY.json', dict(predecessor=record(P/'TARGET.json'),
      predecessor_qualification=record(P/'final-v1/TARGET.json'), entries=len(source),
      unchanged_predecessor_entries=2622, changed_test_container=VM, added_test_module=PROBE,
      lock=record(N/'source/Cargo.lock'), production_behavior_unchanged=True,
      test_body_changes='Only test-only include and additive root-authored probe; diagnostic stream change disclosed separately.'))

Q.mkdir()
for name in ['prepare.py', 'phase.py', 'common.py', 'cache_lease.py', 'bind_dependencies.py',
             'admit_target.py', 'retain_artifacts.py', 'toolchain-standard-inputs.json']:
    shutil.copyfile(P/'qualification-v1'/name, Q/name)
(Q/'observer').mkdir()
for path in (P/'qualification-v1/observer').iterdir():
    if path.is_file() and (path.suffix == '.py' or path.name == 'source-inputs.json'):
        shutil.copyfile(path, Q/'observer'/path.name)
shutil.copyfile(P/'execute_phases.py', N/'execute_phases.py')

path = Q/'prepare.py'
text = path.read_text()
assert text.count('rdtsc-recovery-v6-cold-v1') == 2
text = text.replace('rdtsc-recovery-v6-cold-v1', TARGET.name)
text = text.replace("name in ('metadata','compile','list-lib','list-static','list-vmcall','list-read-clock','format','clippy','core-check')",
                    "name in ('metadata','compile','list-lib','format')")
start = text.index("    if name=='core-check':")
end = text.index("        require(REPO != OWNER", start)
text = text[:start] + "    if kind=='format':\n        paths=" + repr([VM, PROBE]) + '\n' + text[end:]
old = "argv=[cargo,'test','--offline','--locked','-p','reverie-kvm','--lib','--test','static_elf','--test','vmcall','--test','read_clock','--no-run','--message-format=json']"
new = "argv=[cargo,'test','--offline','--locked','-p','reverie-kvm','--lib','--no-run','--message-format=json']"
assert text.count(old) == 1
text = text.replace(old, new)
old = "artifact_selectors=[dict(id='lib',target='reverie_kvm',kind=['lib']),dict(id='static',target='static_elf',kind=['test']),dict(id='vmcall',target='vmcall',kind=['test']),dict(id='read-clock',target='read_clock',kind=['test'])]"
assert text.count(old) == 1
text = text.replace(old, "artifact_selectors=[dict(id='lib',target='reverie_kvm',kind=['lib'])]")
old = 'CPL3 timestamp recovery: exact 37 declarations and existing public vmcall/clock neighbors; no public real-mode interception, timer completion or 75-cell parity claim from component phases'
assert text.count(old) == 1
text = text.replace(old, 'One real CPL3 CPUID-fault transport probe; no callback implementation, accounting, scheduler, same-VM multi-vCPU or parity claim')
path.write_text(text)
path = Q/'admit_target.py'
text = path.read_text()
assert text.count('rdtsc-recovery-v6-cold-v1') == 1
path.write_text(text.replace('rdtsc-recovery-v6-cold-v1', TARGET.name))
path = Q/'retain_artifacts.py'
text = path.read_text()
start = text.index("linked=[r for r in rows")
end = text.index("originals=json_read", start)
text = text[:start] + text[end:]
text = text.replace("require(set(originals)=={'lib','static','vmcall','read-clock'},'wrong four harnesses')",
                    "require(set(originals)=={'lib'},'wrong library harness selection')")
text = text.replace("linked_library=dict(cargo_artifact=linked[0],file=linked_file),", '')
text = text.replace('Cargo fresh=false for all four harnesses and linked KVM library; not a test result',
                    'Cargo fresh=false for the complete KVM library test harness; single --lib compile has no separate integration-linked non-test rlib requirement; not a test result')
path.write_text(text)

setup = json.loads((P/'qualification-v1/SETUP.json').read_text())
setup.update(source_root=str(N/'source'), target=str(TARGET),
             source_files=[dict(relative=p, file=record(N/'source'/p)) for p in [VM, PROBE]],
             lock=record(N/'source/Cargo.lock'),
             purpose='Isolated real CPUID-fault transport feasibility probe only; no active implementation')
write('qualification-v1/SETUP.json', setup)
write('qualification-v1/source-manifest.json',
      [dict(path=r['relative'], mode=r['mode'], **(dict(sha256=r['sha256']) if 'sha256' in r else {})) for r in source])
write('qualification-v1/SELECTORS.json', dict(exact_declarations=1, groups={
    'probe-01': dict(artifact='lib', names=[NAME], source=PROBE,
                     purpose='Actual disabled, armed, independent, disarmed and unrelated-fault CPUID hardware path; no capability skip')
}))
binding = P/'qualification-v1/retained-binaries/BINDING.json'
prior = json.loads((P/'qualification-v1/retained-binaries/ARTIFACTS.json').read_text())
write('qualification-v1/PREDECESSOR-ELFS.json', dict(source=str(binding), sha256=record(binding)['sha256'],
       hashes={'lib': prior['lib']['file']['sha256']}, purpose='Refuse actual V6 harness bytes as newly compiled probe output; no V6 execution credit'))
write('RUN-ORDER.json', dict(phases=['metadata', 'compile', 'format', 'list-lib', 'probe-01'],
       metadata_followup='bind_dependencies.py', compile_followup='retain_artifacts.py',
       no_extra_retry_or_skipped_phase=True))

origins = []
caller = ''
for path in sorted(Q.rglob('*.py')):
    ast.parse(path.read_text())
    before = P/'qualification-v1'/path.relative_to(Q)
    same = before.read_bytes() == path.read_bytes()
    origins.append(dict(before=record(before), after=record(path), byte_identical=same))
    if not same:
        caller += diff(before.read_text(), path.read_text(), 'qualification-v1/' + str(path.relative_to(Q)))
ast.parse((N/'execute_phases.py').read_text())
origins.append(dict(before=record(P/'execute_phases.py'), after=record(N/'execute_phases.py'), byte_identical=True))
write('qualification-v1/RUNNER_ORIGINS.json', dict(records=origins,
       meaning='Original observer, phase, common, lease, dependency binder and outer driver unchanged. Three helpers adapt only target, explicit five-phase/one-artifact selection, format paths and truthful artifact scope.'))
write('CALLER-DELTA.patch', caller)
assert record(Q/'observer/observer.py')['sha256'] == '137c9b42c3f9e081db59954e0ef692ed861f488be7c2bb8a15ef595c271b2179'

states = {}
for label, args in [('head', ['rev-parse', 'HEAD']), ('branch', ['branch', '--show-current']), ('index', ['ls-files', '--stage'])]:
    raw = subprocess.check_output(['/usr/bin/git', '-C', str(R), *args], timeout=30,
                                  env={**os.environ, 'GIT_OPTIONAL_LOCKS': '0'})
    states[label] = hashlib.sha256(raw).hexdigest() if label == 'index' else raw.decode().strip()
write('PREPARATION-STATE.json', dict(scm=states, target_absent=not TARGET.exists(),
       no_cargo_test_vm_or_lease_execution=True, only_source_formatter_preparation=record(N/'format-preparation.json')))
print(json.dumps(dict(source_entries=len(source), selector=NAME, caller_delta=record(N/'CALLER-DELTA.patch'),
                     source_manifest=record(N/'SOURCE-MANIFEST.json'), no_execution=True), indent=2))
