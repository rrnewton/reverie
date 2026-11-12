#!/usr/bin/env python3
"""Offline source mapping only. Never imports or executes product programs."""
import hashlib
import json
import pathlib
import re
import shlex
import subprocess

V = pathlib.Path(__file__).parent
D = V.parent / 'm2-next-increment-20260917'
R = pathlib.Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-prejoin-main-20260917')
BASE = '8051335e87104f7cf832204f74920d38416393b2'

def dump(name, value):
    destination = V / name
    assert not destination.exists(), destination
    destination.write_text(json.dumps(value, indent=2) + '\n')

def git(*args):
    return subprocess.check_output(['git', *args], cwd=R)

def copy_source(path):
    data = git('show', BASE + ':' + path)
    destination = V / 'source' / path
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists():
        assert destination.read_bytes() == data
    else:
        destination.write_bytes(data)
    return data

for path in ['tests/helpers.bzl', 'detcore/BUCK', 'ci/cargo-guest-binaries.rs',
             'hermit-cli/tests/common/hermit_binary.rs', 'ci/run-with-hermit-e2e-artifact.sh',
             'ci/configure-build-jobs.sh', 'ci/nextest-binaries.rs',
             'ci/nextest-timeout-config.rs', 'ci/nextest-test-results.rs',
             'ci/prepare-rust-scripts.sh', 'rust-toolchain.toml']:
    copy_source(path)

graph = json.loads((V / 'source/ci/dag/validate.json').read_text())
tags = {
    'test.regular_crates', 'test.hermit_unit', 'test.detcore_unit',
    'test.detcore_misc', 'test.detcore_parallel', 'test.hermit_integration',
    'test.cli', 'test.hermit_modes', 'test.app_strict_verify', 'test.command_strict_verify',
    'test.isolated_detcore_workdir', 'test.isolated_dbt_workdir',
    'quick.verify_smoke', 'quick.record_replay_smoke',
    'super.record_replay_matrix_diagnostic', 'super.chaos_hello_race_verification_diagnostic',
    'super.weekly_portable_chaos_cases', 'super.weekly_ignored_portable_chaos_cases',
    'super.weekly_relaxed_default_mode_cases', 'super.pmu_buck_chaos_cases',
    'super.pmu_analyze_hello_race_stress_calibrated_skid',
    'super.network_syscall_determinism_diagnostic', 'super.post_fork_scheduling_diagnostics',
    'privileged-test.pmu_buck_chaos_cases', 'privileged-only-test.pmu_buck_chaos_cases',
    'privileged-test.cli_kvm', 'privileged-only-test.cli_kvm',
    'lint.rustfmt', 'lint.clippy', 'build.workspace', 'build.workspace_in_pinned_root',
    'build.rust_scripts', 'build.rust_scripts_in_pinned_root',
    'build.e2e_artifact', 'build.e2e_artifact_in_pinned_root',
    'setup.nextest', 'super.build_workspace', 'quick.build',
}
tags.update('super.weekly_pmu_parallel_memory_diagnostic_mem_race_' + x + '_detcore'
            for x in ['bottom', 'default', 'middle', 'top'])
selected = []
for node in graph['steps']:
    tag = node['group'] + '.' + node['job']
    if tag not in tags and tag.removesuffix('_on_host') not in tags:
        continue
    payload = node['cmd']
    if payload.startswith('./ci/hermetic/run-in-pinned-root.sh '):
        words = shlex.split(payload)
        boundary = words.index('--')
        suffix = words[boundary + 1:]
        assert len(suffix) == 5 and suffix[:2] == ['bash', '-c'] and suffix[3] == 'bash'
        payload = suffix[4]
    selected.append({'tag': tag, 'complete_committed_step': node,
                     'decoded_execution_payload': payload,
                     'inventory_status': 'source declaration only; no list or test execution',
                     'source': 'ci/dag/validate.json at ' + BASE})
dump('COMMANDS.json', {
    'base': BASE,
    'scope': 'Existing committed commands and resource limits, not a new execution plan or authorization',
    'run_node': {'argv_shape': ['./ci/run-node.sh', '<portable|privileged>', '<group.job>[,<group.job>...]'],
                 'restriction': 'Hosted selection only; omits external dependencies and requires their prepared outputs. Trailing command replacement arguments are refused.'},
    'dag_defaults': {k: v for k, v in graph.items() if k != 'steps'},
    'nodes': selected,
})

caller_paths = [
    'detcore/tests/misc/mod.rs', 'detcore/tests/misc/vfork.rs',
    'detcore/tests/parallelism/mod.rs', 'detcore/tests/stats/mod.rs',
    'hermit-cli/tests/cli.rs', 'hermit-cli/tests/hermit_modes.rs',
    'hermit-cli/tests/record_replay.rs', 'hermit-cli/tests/analyze.rs',
    'hermit-cli/tests/stress_suite.rs', 'hermit-cli/tests/verification_report_cli.rs',
    'hermit-cli/tests/verification_report_consumers.rs', 'hermit-verify/tests/cli.rs',
    'tests/BUCK', 'tests/helpers.bzl',
]
source_declarations = []
anchors = []
for path in caller_paths:
    lines = (V / 'source' / path).read_text().splitlines()
    current_function = None
    declarations = []
    for number, line in enumerate(lines, 1):
        match = re.match(r'\s*(?:pub )?(?:async )?fn ([A-Za-z_0-9]+)', line)
        if match:
            current_function = match.group(1)
            declarations.append({'line': number, 'name': current_function,
                                 'preceding_attributes_or_context': lines[max(0, number-5):number-1]})
        if re.search(r'det_test_fn|det_test_cmd|make_det_test_variants|hermit-verify|trace-replay|chaos-replay|hermit_analyze|verify_replay__', line):
            anchors.append({'path': path, 'line': number, 'enclosing_or_preceding_function': current_function, 'text': line})
    source_declarations.append({'path': path, 'functions': declarations})
dump('CALLER-ANCHORS.json', {'base': BASE, 'warning': 'Function association is lexical; macros and cfg attributes are not expanded. Not an emitted test inventory.', 'anchors': anchors})
dump('SOURCE-DECLARATIONS.json', {'base': BASE, 'scope': 'All function declarations in mapped fixture/test sources, including helpers; not a test count or inventory', 'files': source_declarations})

# Bind literal source fixtures, including table entries joined to tests/c.
# This intentionally does not claim C include, linker, installed-tool, or Cargo dependency closure.
fixture_refs = {}
for path in caller_paths:
    lines = (V / 'source' / path).read_text().splitlines()
    for number, line in enumerate(lines, 1):
        for value in re.findall(r'"([^"\n]+\.(?:c|rs|json|sh))"', line):
            candidates = [value]
            if '/' not in value and value.endswith('.c'):
                candidates = ['tests/c/' + value]
            for candidate in candidates:
                if candidate.startswith(('tests/', 'flaky-tests/', 'detcore/', 'hermit-cli/')):
                    fixture_refs.setdefault(candidate, []).append({'caller': path, 'line': number, 'literal': value})
for path in ['hermit-cli/test-resources/flaky_cas_sequence_schedules-passing.json',
             'hermit-cli/test-resources/flaky_cas_sequence_schedules-failing.json',
             'tests/chaos/lock_granularity.c', 'tests/util/pmu_skid.c',
             'tests/c/simple/nanosleep-threads-simple.c']:
    fixture_refs.setdefault(path, []).append({'caller': 'explicit source mapping', 'line': None})
fixture_records = []
for path, references in sorted(fixture_refs.items()):
    p = subprocess.run(['git', 'show', BASE + ':' + path], cwd=R, capture_output=True)
    if p.returncode:
        fixture_records.append({'path': path, 'references': references, 'status': 'literal candidate not found at immutable base', 'diagnostic': p.stderr.decode()})
        continue
    data = copy_source(path)
    fixture_records.append({'path': path, 'references': references, 'status': 'source bytes retained',
                            'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest(),
                            'blob': git('rev-parse', BASE + ':' + path).decode().strip()})
dump('FIXTURE-SOURCES.json', {'base': BASE, 'scope': 'Literal source fixture mapping; does not resolve generated guests, all includes, external tools, dynamic libraries, or Cargo package closure', 'files': fixture_records})

records = []
for path in sorted((V / 'source').rglob('*')):
    if not path.is_file():
        continue
    rel = str(path.relative_to(V / 'source'))
    data = path.read_bytes()
    original = git('show', BASE + ':' + rel)
    assert data == original, rel
    records.append({'path': rel, 'copy': str(path), 'bytes': len(data),
                    'sha256': hashlib.sha256(data).hexdigest(),
                    'blob': git('rev-parse', BASE + ':' + rel).decode().strip()})
dump('SOURCE-MANIFEST.json', {'base': BASE, 'scope': 'Every retained immutable source copy, byte-checked against Git objects', 'files': records})

packet = json.loads((D / 'PREVIEW-MANIFEST-v5.json').read_text())
checks = []
for record in packet['files']:
    actual = pathlib.Path(record['preview_path']).read_bytes()
    assert len(actual) == record['bytes'] and hashlib.sha256(actual).hexdigest() == record['sha256']
    checks.append({'path': record['path'], 'sha256': record['sha256'], 'matched': True})
dump('M2-PREVIEW-READBACK.json', {'base': BASE, 'preview_manifest': str(D / 'PREVIEW-MANIFEST-v5.json'), 'checks': checks})
print(json.dumps({'nodes': len(selected), 'retained_source_files': len(records),
                  'fixture_records': len(fixture_records), 'unresolved_literal_candidates': [x['path'] for x in fixture_records if x['status'] != 'source bytes retained'],
                  'preview_checks': len(checks)}, indent=2))
