import difflib
import hashlib
import json
import pathlib
import re
import stat
import subprocess

D = pathlib.Path(__file__).resolve().parent
H = pathlib.Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-prejoin-main-20260917')
B = '8051335e87104f7cf832204f74920d38416393b2'
P = D / 'preview-v6'
assert not P.exists()
old = json.loads((D / 'PREVIEW-MANIFEST-v5.json').read_text())
sha = lambda b: hashlib.sha256(b).hexdigest()

def git(*args):
    return subprocess.check_output(['git', '-C', str(H), *args], timeout=15)

def write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open('xb') as f:
        f.write(data)

def record(path):
    data = path.read_bytes()
    return {'path': str(path), 'bytes': len(data), 'sha256': sha(data)}

originals = {}
for f in old['files']:
    data = pathlib.Path(f['preview_path']).read_bytes()
    assert sha(data) == f['sha256']
    originals[f['path']] = data
    write(P / f['path'], data)
    (P / f['path']).chmod(int(f['mode'], 8))

added = ['README.md', 'demos/README.md', 'ci/compat-envelope/README.md',
         'tests/backend-parity/README.md', 'tests/e2e/determinism-stress/README.md',
         'tests/reproducible-builds/README.md', 'tests/bin/README.md']
for name in added:
    data = git('show', f'{B}:{name}')
    originals[name] = data
    write(P / name, data)

def replace(name, before, after):
    p = P / name
    s = p.read_text()
    assert s.count(before) == 1, (name, before, s.count(before))
    p.write_text(s.replace(before, after))

replace('README.md', '''`--verify` compares captured status and output and applies the `Stripped`
comparison to selected Detcore scheduler messages. A successful result is a
useful diagnostic, but it is not strict determinism. Strict verification requires
`--verify-strict --verify-json REPORT.json`, `bitwise_parity: true`, and nonzero
compared-message counts.''', '''`--verify` compares captured status, output, and canonical INFO messages by
default. Numeric fields are preserved; only the defined log envelope and
explicitly marked address fields are canonicalized. Use `--verify-json REPORT.json`
to retain the typed verdict, and require a matched canonical comparison,
`bitwise_parity: true`, and equal positive compared-message counts before claiming
strict verification. `--verify-strict` remains an accepted compatibility spelling
for this default policy. Historical `Stripped` reports remain readable, but do
not establish the current canonical guarantee.''')
replace('README.md', 'measured post-0.2 `Stripped` envelope, build instructions, and explicit',
        'historical post-0.2 `Stripped` measurements, build instructions, and explicit')
replace('README.md', 'Runs the `Stripped` diagnostic over output, status, and selected logs; not strict determinism',
        'Compares output, status, and canonical INFO logs; retain `--verify-json` and check the typed verdict and nonzero counts')

replace('demos/README.md', '''positive compared-INFO-message counts on both sides. `--verify-strict` is
required for that: a plain `--verify` stays on the lossy Stripped comparator and
cannot establish L2. It needs `jq`, which the demo preflights before booting.''', '''positive compared-INFO-message counts on both sides. Plain `--verify` now uses
that canonical policy too; the demo retains `--verify-strict` as a compatibility
spelling. The typed verdict and counts, rather than flag selection alone,
establish what this invocation compared. It needs `jq`, which the demo preflights
before booting.''')
replace('demos/README.md', "Current Reverie's 1,000-RCB processor default panicked during verification Run 1. The",
        "The measured Reverie revision's 1,000-RCB processor default panicked during verification Run 1. The")
replace('demos/README.md', '''after nine minutes and was stopped; current-main Stripped verification therefore
remains blocked on practical PMU calibration.''', '''after nine minutes and was stopped. That historical Stripped-verification
attempt did not complete; it supplies no result for the current canonical
default or evidence that the PMU calibration issue has been resolved.''')

replace('ci/compat-envelope/README.md', '''itself. Bare `--verify` still uses the legacy Stripped comparator. These cells
therefore measure same-backend repeatability under the current contract; they
do not establish strict INFO-log determinism or cross-backend parity. The
scorecard says this directly and reports no cross-backend parity count until
the manifest has cells that really compare fresh ptrace and non-ptrace logs.''', '''itself. Bare `--verify` now selects canonical INFO comparison. A fresh typed
report must establish a matched comparison with `bitwise_parity: true` and equal
positive message counts; old Stripped results are not promoted by the new
default. These remain same-backend comparisons, not cross-backend parity. No
cross-backend parity count is justified until cells actually compare fresh
ptrace and non-ptrace logs.''')

replace('tests/backend-parity/README.md', '''## Current ratchet

The L1 ratchet (`--strict`, run three times, byte-identical stdout) and the
Stripped verification ratchet (`--strict --verify`, Hermit's double-run
comparison after selected numeric, address, path, and time fields are stripped)
are tracked separately. Stripped verification is not L2.''', '''## Recorded ratchets

The L1 ratchet (`--strict`, run three times, byte-identical stdout) and the
historical Stripped verification ratchet are recorded separately below. The
Stripped measurements used a double-run comparison that removed selected
numeric, address, path, and time fields; they are not L2. Current `--verify`
selects canonical INFO comparison and requires fresh typed evidence. The
policy change does not remeasure any row in these tables.''')
replace('tests/backend-parity/README.md', '''KVM now compares retained logs too. Plain `--verify` uses the Stripped policy on
every backend; `--verify-strict` selects canonical `BitwiseInfoV1` on every
backend. Whether a KVM cell matches is a measured property of that workload.''', '''KVM now compares retained logs too. Plain `--verify` selects canonical
`BitwiseInfoV1` on every backend; `--verify-strict` remains an accepted
compatibility spelling. Whether a KVM cell matches is a measured property of
that workload, established from the typed report and equal positive counts.''')
replace('tests/backend-parity/README.md', '''### Stripped verification (`--verify`)

Passing `--verify` adds a two-run comparison: the runner invokes
`hermit run --strict --verify --verify-allow both`. Plain `--verify` now uses
the Stripped retained-log comparison for ptrace, DBT, and KVM;
`--verify-strict` selects canonical `BitwiseInfoV1` for all three. The KVM
statuses above predate that change and came from the older guest-output and
exit-status comparison; they must not be reused as current log-comparison
results. A fresh matrix run must be judged from its typed `--verify-json`
report. The matrix still hard-codes KVM's expected tier, printed ratchet, and
observation description to `guest`; those three consumers must be updated
before the 28 cells can be remeasured under the current comparator.''', '''### Canonical verification (`--verify`)

Passing `--verify` adds a two-run comparison: the runner invokes
`hermit run --strict --verify --verify-allow both`. Plain `--verify` selects
canonical `BitwiseInfoV1` for ptrace, DBT, and KVM. `--verify-strict` remains a
compatibility spelling for the same policy. The historical KVM statuses above
came from the older guest-output and exit-status comparison; they must not be
reused as current log-comparison results. A fresh matrix run must be judged
from its typed `--verify-json` report. Historical `guest` and Stripped tier
decoding remains useful for old reports, but cannot substitute for a current
matched canonical verdict with `bitwise_parity: true` and equal positive
compared-message counts.''')
replace('tests/backend-parity/README.md', '''Enforce the Stripped verification ratchet on any backend by adding `--verify`
(it implies `--strict`); Hermit's double-run then asserts the recorded
verification kind per contract:''', '''Request canonical verification on any backend by adding `--verify` (it implies
`--strict`). Judge the fresh typed report under the current contract; the old
Stripped ratchet values above remain historical measurements:''')

replace('tests/e2e/determinism-stress/README.md', '''These scripts exercise the ptrace backend with Stripped verification, not L2.
Every test case invokes the exact verifier path:''', '''These scripts exercise the ptrace backend with the default canonical INFO
comparison. Every test case invokes the exact verifier path:''')
replace('tests/e2e/determinism-stress/README.md', '''Canonical strictness reports `relaxations=none`. Stripped strictness records two
stripped-prefix policy tokens:''', '''Canonical strictness reports `relaxations=none`. Historical Stripped reports
recorded two stripped-prefix policy tokens:''')
replace('tests/e2e/determinism-stress/README.md', '''twenty times for repeated Stripped stress evidence. Repetition does not promote
Stripped evidence to L4 because the underlying comparison is not L2. The
default is one Stripped comparison so the full targeted matrix remains
practical. Other controls are:''', '''twenty times. Each repetition needs its own valid comparison evidence; a
repetition count or success marker alone does not establish L4. Historical
Stripped results are not promoted by the default-policy change. The default is
one canonical comparison so the full targeted matrix remains practical. Other
controls are:''')
replace('tests/e2e/determinism-stress/README.md', '''completeness manifest. This shell suite therefore adds explicit Stripped
verification coverage at the CLI boundary rather than treating green unit
tests as complete end-to-end syscall coverage. It does not establish L2.''', '''completeness manifest. This shell suite exercises the CLI verifier boundary;
green unit tests are not complete end-to-end syscall coverage. Its marker-based
acceptance and textual summaries alone do not establish a typed nonempty
canonical verdict, and the policy change supplies no new measured result.''')

replace('tests/reproducible-builds/README.md', '''status, output, and Stripped execution logs. That comparison removes selected
numeric, address, path, and time fields; it is not an L2 comparison.''', '''status, output, and canonical INFO logs under the default policy. The runner
does not yet retain and validate the typed verification report, so its command
status alone must not be reported as a nonempty canonical-verification result.
The independent object-file comparison remains the artifact oracle.''')
replace('tests/reproducible-builds/README.md', '''  plus a Stripped `--strict --verify` run of the same compiler command. The
  direct object-file comparison, not bare `--verify`, supplies the bitwise
  artifact claim.''', '''  plus a default canonical `--strict --verify` invocation of the same compiler
  command. Typed-report consumption remains a separate runner obligation. The
  direct object-file comparison supplies the bitwise artifact claim.''')
replace('tests/bin/README.md', '''on Detcore's precise waiter queue. It passes Stripped verification, not L2
(ptrace backend, ERROR log level, `--debug-futex-mode polling`, no determinism
relaxations):''', '''on Detcore's precise waiter queue. The historical attempt below passed
Stripped verification, not L2 (ptrace backend, ERROR log level,
`--debug-futex-mode polling`, no determinism relaxations). Its banner does not
establish a result under the current canonical default:''')

replace('hermit-cli/src/bin/hermit/analyze/phases.rs',
        '/// A weaker log difference that does not expect certain lines to be conserved in preemption replay.',
        '/// Compare canonical INFO messages without dropping preemption-replay records.')
replace('hermit-cli/src/bin/hermit/analyze/phases.rs',
        '"    hermit log-diff --record-envelope=all-records-v1 \\\n+             --ignore-lines=CHAOSRAND {} {}"',
        '"    hermit log-diff --canonical-info --record-envelope=all-records-v1 {} {}"')

files = []
base_records = []
complete = []
increment = []
for name in sorted(originals):
    raw = git('show', f'{B}:{name}')
    blob = git('rev-parse', f'{B}:{name}').decode().strip()
    p = P / name
    now = p.read_bytes()
    files.append({'path': name, 'base_blob': blob, 'base_sha256': sha(raw),
                  'preview_path': str(p), 'bytes': len(now),
                  'mode': oct(stat.S_IMODE(p.stat().st_mode)),
                  'sha256': sha(now), 'changed': raw != now})
    base_records.append({'path': name, 'bytes': len(raw), 'sha256': sha(raw), 'git_blob': blob})
    for before, output in [(raw, complete), (originals[name], increment)]:
        if before != now:
            output.append(f'diff --git a/{name} b/{name}\n')
            output.extend(difflib.unified_diff(before.decode().splitlines(True), now.decode().splitlines(True),
                                             fromfile=f'a/{name}', tofile=f'b/{name}'))
write(D / 'candidate-v6.patch', ''.join(complete).encode())
write(D / 'v5-to-v6.patch', ''.join(increment).encode())

def dump(name, obj):
    write(D / name, (json.dumps(obj, indent=2) + '\n').encode())

dump('BASE-v6.json', {'repository': str(H), 'commit': B, 'files': base_records})
dump('PREVIEW-MANIFEST-v6.json', {
    'base_repository': str(H), 'base_commit': B, 'base_tree': old['base_tree'],
    'scope': 'owned source previews only; uncompiled, unexecuted, unlanded',
    'patch': record(D / 'candidate-v6.patch'), 'files': files})
print(json.dumps({'patch': record(D / 'candidate-v6.patch'), 'increment': record(D / 'v5-to-v6.patch'),
                  'files': len(files), 'changed': sum(f['changed'] for f in files)}, indent=2))
