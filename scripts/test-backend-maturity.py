#!/usr/bin/env python3
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree.

"""Check the validator's decisions without building or substituting a backend.

The shell functions and final aggregation come from the actual validator. Only
runtime-command outcomes are controlled here. Positive cases prevent blanket
refusal from satisfying the regression tests.
"""

import argparse
import hashlib
import json
import re
import shlex
import subprocess
import tempfile
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('--source', type=Path, default=Path(__file__).with_name('validate-backend-maturity.sh'))
parser.add_argument('--output', type=Path)
args = parser.parse_args()
source = args.source.read_text()
temporary = None
if args.output is None:
    temporary = tempfile.TemporaryDirectory(prefix='reverie-maturity-test-')
    args.output = Path(temporary.name) / 'controls'
args.output.mkdir()
functions = '\n'.join(re.findall(r'(?ms)^[a-z_][a-z_0-9]*\(\) \{\n.*?^\}\n', source))
minimum = source[source.index('declare -A MINIMUM_LEVEL=('):source.index('\nsanitize()')]
footer = source[source.index("printf '\\nMaximum defensible maturity"):source.index('\nverify_source final')]
records = []

def execute(name, script, expected, required=()):
    path = args.output / (name + '.sh')
    path.write_text(script)
    result = subprocess.run(['bash', str(path)], capture_output=True, text=True, timeout=10)
    path.with_suffix('.stdout').write_text(result.stdout)
    path.with_suffix('.stderr').write_text(result.stderr)
    missing = [marker for marker in required if marker not in result.stdout]
    records.append(dict(name=name, status=result.returncode, expected=expected, missing=missing,
                        satisfied=result.returncode == expected and not missing))

setup = '''set -uo pipefail
declare -A LEVEL_OUTCOME=() MAXIMUM_LEVEL=()
BACKEND_LIST=(ptrace kvm dbt sabre liteinst e9patch)
PARTIAL_SELECTION=0
SKIP_PREPARE=0
PREPARED_OUTCOME=pass
REPORT=controlled-rows
'''
rows = '''
for backend in "${BACKEND_LIST[@]}"; do
    for level in B0 B1 B1.5; do
        LEVEL_OUTCOME["$backend:$level"]=pass
    done
done
'''
def aggregate(name, alteration, expected):
    execute(name, setup + minimum + functions + rows + alteration + '\n' + footer + '\nexit "$overall"\n', expected)

aggregate('all-pass', '', 0)
for backend in ('ptrace', 'kvm', 'dbt', 'sabre', 'liteinst', 'e9patch'):
    for level in ('B0', 'B1', 'B1.5'):
        for outcome, expected in (('fail', 1), ('unmeasurable', 2)):
            aggregate(f'{backend}-{level}-{outcome}', f'LEVEL_OUTCOME["{backend}:{level}"]={outcome}', expected)
        aggregate(f'{backend}-{level}-missing', f'unset "LEVEL_OUTCOME[{backend}:{level}]"', 2)
aggregate('fail-before-unavailable', 'LEVEL_OUTCOME[ptrace:B1.5]=fail\nLEVEL_OUTCOME[e9patch:B1]=unmeasurable', 1)
aggregate('fail-after-unavailable', 'LEVEL_OUTCOME[ptrace:B1]=unmeasurable\nLEVEL_OUTCOME[dbt:B1.5]=fail', 1)
aggregate('partial-selection', 'PARTIAL_SELECTION=1\nBACKEND_LIST=(ptrace)', 2)
aggregate('skipped-preparation', 'SKIP_PREPARE=1', 2)

stub = r'''
REPEATS=2
TARGET_DIR=/unused
PROFILE=debug
declare -A CASE_INFRASTRUCTURE_DENIED=() CASE_EXECUTION_FAILED=() CASE_DEPENDENCY=()
run_cargo_test() { return 0; }
cargo_artifact() { printf '/unused/preload\n'; }
record() { printf 'ROW:%s:%s:%s:%s:%s\n' "$1" "$2" "$3" "$8" "${14}"; }
run_case() {
    local label=$1 status=0 denied=0
    CURRENT_CASE=$label
    case "$label" in
        liteinst-counter1-*)
            printf 'liteinst-counter1\n' > "$WORK_DIR/$label.stdout"
            printf 'Total system calls in process tree: 79\n' > "$WORK_DIR/$label.stderr"
            if [[ ${COUNTER_FAIL:-0} == 1 ]]; then status=1; fi
            ;;
        liteinst-counter2-*)
            printf 'liteinst-counter2\n' > "$WORK_DIR/$label.stdout"
            printf 'Total system calls in process tree: 79, from 1 processes, 1 thread(s).\n' > "$WORK_DIR/$label.stderr"
            ;;
        liteinst-strace-*)
            printf 'liteinst-strace\n' > "$WORK_DIR/$label.stdout"
            printf 'write(1, "liteinst-strace", 16) = 16\n' > "$WORK_DIR/$label.stderr"
            ;;
        liteinst-process-tree)
            : > "$WORK_DIR/$label.stdout"
            printf '%s\n' "$TREE_OUTPUT" > "$WORK_DIR/$label.stderr"
            status=$TREE_STATUS
            denied=$TREE_DENIED
            ;;
        *) return 90 ;;
    esac
    printf '%s' "$status" > "$WORK_DIR/$label.status"
    CASE_EXECUTION_FAILED[$label]=$((status != 0 && denied == 0))
    CASE_INFRASTRUCTURE_DENIED[$label]=$denied
    return "$status"
}
measure_liteinst
'''
for name, status, denied, output, counter_fail, expected in (
    ('tree-good', 0, 0, 'Total system calls in process tree: 184, from 2 processes, 2 thread(s).', 0, 'pass'),
    ('tree-wrong-totals', 0, 0, 'Total system calls in process tree: 79, from 1 processes, 1 thread(s).', 0, 'fail'),
    ('tree-missing-totals', 0, 0, '', 0, 'fail'),
    ('tree-timeout', 124, 0, '', 0, 'fail'),
    ('tree-failure', 1, 0, '', 0, 'fail'),
    ('tree-launch-denied', 126, 1, '', 0, 'unmeasurable'),
    ('counter-failure-tree-denied', 126, 1, '', 1, 'fail'),
):
    work = args.output / name
    work.mkdir()
    config = f'WORK_DIR={shlex.quote(str(work.resolve()))}\nTREE_STATUS={status}\nTREE_DENIED={denied}\nTREE_OUTPUT={shlex.quote(output)}\nCOUNTER_FAIL={counter_fail}\n'
    execute(name, 'set -uo pipefail\n' + functions + config + stub, 0, [f'ROW:liteinst:B1.5:{expected}:'])

dbt_stub = r'''
REPEATS=2
TARGET_DIR=/unused
PROFILE=debug
DBT_DRRUN=/unused/drrun
DBT_CLIENT=/unused/client.so
DBT_HOME=/unused/dynamorio
declare -A CASE_INFRASTRUCTURE_DENIED=() CASE_EXECUTION_FAILED=() CASE_DEPENDENCY=()
compile_fixture() { return 0; }
record() { printf 'ROW:%s:%s:%s:%s:%s\n' "$1" "$2" "$3" "$8" "${14}"; }
run_case() {
    local label=$1
    CURRENT_CASE=$label
    case "$label" in
        dbt-b1)
            cp "$WORK_DIR/dbt-chaos.txt" "$WORK_DIR/$label.stdout"
            for ((j=1; j<=10; j++)); do
                printf 'chaos [pid 3 n %s] read(3, 0x1234, 1) = 1\n' "$j"
            done > "$WORK_DIR/$label.stderr"
            ;;
        dbt-counter1-*)
            printf 'dbt-counter1\n' > "$WORK_DIR/$label.stdout"
            printf 'counter1-global syscalls=79\n' > "$WORK_DIR/$label.stderr"
            ;;
        dbt-counter2-*)
            printf 'dbt-counter2\n' > "$WORK_DIR/$label.stdout"
            printf 'Total system calls in process tree: 79, from 1 processes, 1 thread(s).\n' > "$WORK_DIR/$label.stderr"
            ;;
        dbt-strace-*)
            printf 'dbt-strace\n' > "$WORK_DIR/$label.stdout"
            cp "$TRACE_FILE" "$WORK_DIR/$label.stderr"
            ;;
        *) return 90 ;;
    esac
    printf '0' > "$WORK_DIR/$label.status"
    CASE_EXECUTION_FAILED[$label]=0
    CASE_INFRASTRUCTURE_DENIED[$label]=0
    return 0
}
measure_dbt
'''

for name, text, expected in (
    ('semantic-write', '[dbt strace pid 3] write(1, 0x1234, 11) = ?\n', 0),
    ('semantic-write-with-other-events', '[dbt strace pid 3] getpid() = ?\n[dbt strace pid 3] write(1, 0x1234, 11) = ?\n', 0),
    ('diagnostic-token', 'warning: dbt strace failed\n', 1),
    ('unrelated-syscall', '[dbt strace pid 3] getpid() = ?\n', 1),
    ('wrong-write-fd', '[dbt strace pid 3] write(2, 0x1234, 11) = ?\n', 1),
    ('wrong-write-length', '[dbt strace pid 3] write(1, 0x1234, 10) = ?\n', 1),
    ('null-write-buffer', '[dbt strace pid 3] write(1, NULL, 11) = ?\n', 1),
    ('zero-write-buffer', '[dbt strace pid 3] write(1, 0x0, 11) = ?\n', 1),
    ('diagnostic-event-substring', 'warning: [dbt strace pid 3] write(1, 0x1234, 11) = ?\n', 1),
    ('unreported-return-shape', '[dbt strace pid 3] write(1, 0x1234, 11) = 11\n', 1),
):
    trace = args.output / (name + '.trace')
    trace.write_text(text)
    work = args.output / name
    work.mkdir()
    config = f'WORK_DIR={shlex.quote(str(work.resolve()))}\nTRACE_FILE={shlex.quote(str(trace.resolve()))}\n'
    outcome = 'pass' if expected == 0 else 'fail'
    execute(name, 'set -uo pipefail\n' + functions + config + dbt_stub, 0,
            ['ROW:dbt:B1:pass:', f'ROW:dbt:B1.5:{outcome}:'])

result = dict(source=str(args.source), source_sha256=hashlib.sha256(source.encode()).hexdigest(),
              passed=sum(r['satisfied'] for r in records), total=len(records), records=records)
(args.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps({k:v for k,v in result.items() if k != 'records'}))
for record in records:
    if not record['satisfied']:
        print('FAILED', record['name'], 'actual', record['status'], 'expected', record['expected'],
              'missing output', record['missing'])
raise SystemExit(0 if all(r['satisfied'] for r in records) else 1)
