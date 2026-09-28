import difflib
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess

R = Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-setitimer-reverie-20260918')
D = R / 'ignored/same-inode-ofd-control-v1'
BASE = '000c15a1161ea2d58749431b5ddaaa97f7aa37d5'
OLD = R / 'ignored/scalar-write-fd-qualification-v1/baseline'
ENV = dict(os.environ, GIT_NO_LAZY_FETCH='1', GIT_OPTIONAL_LOCKS='0')


def sha(data):
    return hashlib.sha256(data).hexdigest()


def record(path):
    data = path.read_bytes()
    return dict(path=str(path), bytes=len(data), mode=stat.S_IMODE(path.stat().st_mode), sha256=sha(data))


def git(*args):
    return subprocess.run(['git', '-c', 'core.fsmonitor=false', *args], cwd=R, env=ENV,
                          check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30).stdout


def write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open('xb') as output:
        output.write(data)
    assert path.read_bytes() == data
    return record(path)


def json_write(path, value):
    return write(path, (json.dumps(value, indent=2) + '\n').encode())


def diff(before, after, a, b):
    return ''.join(difflib.unified_diff(before.decode().splitlines(keepends=True),
                                       after.decode().splitlines(keepends=True),
                                       fromfile=a, tofile=b)).encode()


assert git('rev-parse', 'HEAD').decode().strip() == BASE
assert git('status', '--porcelain=v1', '--untracked-files=no') == b''
scm = dict(head=BASE, tree=git('rev-parse', 'HEAD^{tree}').decode().strip(),
           branch=git('branch', '--show-current').decode().strip(),
           index_listing_sha256=sha(git('ls-files', '--stage')),
           tracked_status='clean')
json_write(D / 'SCM-BEFORE.json', scm)
rel = 'reverie-kvm/src/executor.rs'
before = git('show', BASE + ':' + rel)
assert (R / rel).read_bytes() == before
fragment = (D / 'test-fragment.rs').read_bytes()
assert fragment.count(b'#[test]') == 1
anchor = b'    #[test]\n    fn fdinfo_dispatch_observes_current_owned_offset_flags_and_descriptor_cloexec() {'
assert before.count(anchor) == 1
offset = before.index(anchor)
after = before[:offset] + fragment + b'\n' + before[offset:]
assert after[:offset] + after[offset + len(fragment) + 1:] == before
before_record = write(D / 'before' / rel, before)
after_record = write(D / 'after' / rel, after)
patch = diff(before, after, 'a/' + rel, 'b/' + rel)
assert not any(line.startswith(b'-') and not line.startswith(b'---') for line in patch.splitlines())
patch_record = write(D / 'candidate.patch', patch)
source = []
for path in [rel, 'reverie-kvm/src/elf.rs', 'reverie-kvm/src/vm.rs',
             'reverie-kvm/src/lib.rs', 'reverie-kvm/Cargo.toml',
             'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml']:
    data = git('show', BASE + ':' + path)
    assert (R / path).read_bytes() == data
    bound = before_record if path == rel else write(D / 'context' / path, data)
    source.append(dict(relative_path=path, revision=BASE,
                       git_blob=git('rev-parse', BASE + ':' + path).decode().strip(),
                       live=record(R / path), retained=bound))

old_prepare = (OLD / 'prepare.py').read_text()
new_prepare = old_prepare
new_prepare = new_prepare.replace(
    "[cargo,'clippy','--offline','--locked','-p','reverie-kvm','--lib','--test','static_elf','--message-format=json','--','-D','warnings']",
    "[cargo,'clippy','--offline','--locked','-p','reverie-kvm','--lib','--message-format=json','--','-D','warnings']")
old_format = """        paths=['reverie-kvm/src/executor.rs', 'reverie-kvm/src/runtime.rs', 'reverie-kvm/tests/support/captured_write_signals.rs']
        actual=subprocess.check_output(['/usr/bin/git','-C',str(OWNER),'diff','HEAD','--name-only'],timeout=30).decode().splitlines()
        actual+=subprocess.check_output(['/usr/bin/git','-C',str(OWNER),'ls-files','--others','--exclude-standard','--','reverie/src','reverie-kvm/src','reverie-kvm/tests'],timeout=30).decode().splitlines()
        require(sorted(p for p in actual if p.endswith('.rs'))==paths,'changed Rust path set differs')
"""
new_format = """        paths=['reverie-kvm/src/executor.rs']
        require(REPO != OWNER, 'negative-before source must be an owned snapshot')
        expected=json_read(HERE/'SETUP.json')['test_source'];check_file(expected)
        inputs.append(expected)
        require(read(REPO/paths[0])==read(expected['path']), 'snapshot test differs from reviewed after source')
"""
assert old_format in new_prepare
new_prepare = new_prepare.replace(old_format, new_format)
new_prepare = new_prepare.replace(
    "[cargo,'test','--offline','--locked','-p','reverie-kvm','--lib','--test','static_elf','--no-run','--message-format=json']",
    "[cargo,'test','--offline','--locked','-p','reverie-kvm','--lib','--no-run','--message-format=json']")
new_prepare = new_prepare.replace(
    "artifact_selectors=[dict(id='lib',target='reverie_kvm',kind=['lib']),dict(id='static',target='static_elf',kind=['test'])]",
    "artifact_selectors=[dict(id='lib',target='reverie_kvm',kind=['lib'])]")
new_prepare = new_prepare.replace(
    "scope='Scalar write low32 descriptor component; separate baseline/corrected source. No Hermit timer fix or parity claim'",
    "scope='Same-inode OFD replacement negative-before unit control over unchanged production; no FIFO or runtime correction claim'")
assert new_prepare != old_prepare
write(D / 'caller/before/prepare.py', old_prepare.encode())
write(D / 'caller/after/prepare.py', new_prepare.encode())
write(D / 'caller-change.patch', diff(old_prepare.encode(), new_prepare.encode(),
                                    'a/prepare.py', 'b/prepare.py'))

runner = []
for path in ['prepare.py', 'phase.py', 'common.py', 'cache_lease.py', 'observer/observer.py',
             'observer/before_exec.py', 'observer/unit_reference.py', 'observer/source-inputs.json',
             'RUNNER_ORIGINS.json']:
    runner.append(dict(relative_path=path, original=record(OLD / path)))
    if path != 'prepare.py':
        write(D / 'caller/unchanged' / path, (OLD / path).read_bytes())
plans = []
for name in ['metadata', 'compile', 'list-lib', 'test-signalfd']:
    path = OLD / (name + '-plan.json')
    data = json.loads(path.read_text())
    plans.append(dict(original=record(path), kind=data['kind'], limits=data['limits']))
format_plan = R / 'ignored/scalar-write-fd-qualification-v1/corrected/format-plan.json'
fmt = json.loads(format_plan.read_text())
plans.append(dict(original=record(format_plan), kind=fmt['kind'], limits=fmt['limits']))

selectors = {
    'groups': {
        'test-replaced-description': {'artifact': 'lib', 'names': ['executor::tests::shared_file_table_reopen_same_inode_replaces_description']},
        'test-shared-alias-neighbor': {'artifact': 'lib', 'names': ['executor::tests::fdinfo_dispatch_retains_partial_records_and_shared_dup_seek_state']},
        'test-object-identity-neighbor': {'artifact': 'lib', 'names': ['executor::tests::forked_states_share_file_object_identity_namespace']},
    }
}
json_write(D / 'SELECTORS.json', selectors)
setup = dict(owner_slot=str(R), source_root=str(D / 'baseline-source'), base=BASE,
             purpose='One additive unit control over unchanged production; no live source writes',
             test_source=after_record)
json_write(D / 'SETUP.json', setup)
json_write(D / 'CALLER_PLAN.json', dict(
    execution_authorized=False, intended_qualification_directory=str(D / 'qualification'),
    source_snapshot=setup['source_root'], source_base=BASE,
    snapshot_rule='All tracked blobs/modes/symlinks exactly base, except the one complete reviewed after file; full fresh manifest before any phase.',
    caller_delta=record(D / 'caller-change.patch'), runner_origins=runner,
    unchanged_observer_sha256=record(OLD / 'observer/observer.py')['sha256'],
    phase_order=['metadata', 'compile', 'format', 'list-lib', 'test-shared-alias-neighbor', 'test-object-identity-neighbor', 'test-replaced-description'],
    original_limits=plans,
    toolchain='/home/newton/.rustup/toolchains/nightly-2026-07-29-x86_64-unknown-linux-gnu/bin',
    target=str(R / 'target/process-alarm-qualification-v1'),
    lease=str(R / 'ignored/timer-integration-20260918/lane.lease'),
    phase_invocation=['/usr/bin/python3', '-B', '<qualification>/phase.py', 'launch', '<exact plan path>', '<exact plan sha256>'],
    material_changes=['Compile/select only reverie-kvm library harness; no static guest target.',
                      'Format the one reviewed snapshot after file, bound by exact bytes, instead of checking unrelated live product diff.',
                      'Fresh source/SCM/toolchain/dependency/ELF/list/cache/lease identities and selectors; no historical executable/path receipt reused as current.',
                      'No observer, terminal accounting, acceptance parser, limits or negative-test failure classification change.'],
    negative_result_policy='Preserve raw101/failed assertion as accepted=false. Separately authenticate exact events, source and terminal accounting; no arbitrary failure is a success. Never change assertions after a before result.',
    held=True,
))
json_write(D / 'SOURCE_INPUTS.json', dict(base=scm, source=source,
                                        before=before_record, after=after_record, patch=patch_record,
                                        test_fragment=record(D / 'test-fragment.rs'),
                                        original_plan=record(R / 'ignored/fifo-read-probe-ownership-v1/REPORT.md')))
json_write(D / 'PATCH-RECONSTRUCTION.json', dict(
    base=BASE, path=rel, insertion_byte_offset=offset,
    fragment_bytes=len(fragment), separator_bytes=1,
    reconstruction='Deleting the exact inserted fragment plus one separator newline reproduces the complete before bytes.',
    before_sha256=sha(before), after_sha256=sha(after), patch_sha256=sha(patch),
    added_test_declarations=1, removed_lines=0, changed_production_bytes=0,
    cargo_rustfmt_tests_executed=False,
))
assert git('rev-parse', 'HEAD').decode().strip() == BASE
assert git('status', '--porcelain=v1', '--untracked-files=no') == b''
assert sha(git('ls-files', '--stage')) == scm['index_listing_sha256']
assert (R / rel).read_bytes() == before
json_write(D / 'SCM-AFTER.json', scm)
print(json.dumps(dict(patch=patch_record, after=after_record,
                      caller_delta=record(D / 'caller-change.patch'))))
