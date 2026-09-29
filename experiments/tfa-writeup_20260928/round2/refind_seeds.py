#!/usr/bin/env python3
"""TFA round 2: re-find PASS/FAIL seeds for the 3 known tests on current Hermit main.

Replays each campaign winning cell's exact Hermit argv (from the tracked experiment.json),
substituting only: the Hermit binary, the swept --sched-seed and the verify log dir (--keep-logs
is kept because --verify-log-dir requires it; logs are deleted after a deterministic trial). The
safehermit deadline is 300 s. The outcome classifier below is a marker regex; every failure was
then checked by hand against the declared signature (ROUND2.md section 3). Budget (coord2 decision #8, 2026-09-29): 6 seeds per test, 30 min wall per
test. One trial at a time, under nice/ionice (decision #3). Every run goes through bin/safehermit.
"""
import glob, json, os, re, subprocess, sys, time
ROOT = '/home/newton/work/dev-hermit'
HERMIT = sys.argv[1]
OUT = os.environ.get('REFIND_OUT', os.path.join(ROOT, 'ignored/tfa2-round2/runs'))
CASES = [
    # name, root, seeds (historical PASS, FAIL first), pass regex, fail regex
    ('thrift', 'tfa-kernel71-thrift-slowdown-continuation_20260921', [0, 1, 2, 3, 4, 5],
     r'\[       OK \] RequestInstrumentationTestWithFinishedDebugPayload\.snapshotStartedProcessingTest',
     r'InstrumentationTest\.cpp:836'),
    ('asyncssl', 'tfa-kernel71-asyncssl-slice5-continuation_20260921', [1, 11, 0, 2, 3, 4],
     r'\[       OK \] AsyncSSLSocketByteEventTest\.ObserverAttachedBeforeConnect',
     r'AsyncSSLSocketTest\.cpp:3797: Failure'),
    ('httpcoro', 'tfa-kernel71-httpcoro-midpoint-continuation_20260921', [3, 0, 1, 2, 4, 5],
     r'\[       OK \] HTTPDownstreamSessionTest/H2DownstreamSessionTest\.ResetRateLimit/h2',
     r'HTTPDownstreamCoroSessionTests\.cpp:1685\] Check failed: bodyEvent\.hasException\(\)'),
]
CAP_S = 30 * 60
DEADLINE_S = 300
only = sys.argv[2:] or [c[0] for c in CASES]
summary = []
for name, root, seeds, pass_re, fail_re in CASES:
    if name not in only:
        continue
    if os.environ.get('REFIND_SEEDS'):
        seeds = [int(x) for x in os.environ['REFIND_SEEDS'].split(',')]
    p = glob.glob(f'{ROOT}/experiments/{root}/services/*/*/experiment.json')[0]
    cell = json.load(open(p))['cells'][0]
    argv = cell['hermit']['argv']
    run_i = argv.index('run')
    hermit_args = list(argv[run_i:])  # --verify-log-dir requires --keep-logs; logs are deleted below when verify succeeds
    t_case = time.time()
    for seed in seeds:
        if time.time() - t_case > CAP_S:
            summary.append({'case': name, 'seed': seed, 'skipped': 'wall cap'})
            continue
        d = os.path.join(OUT, name, f'seed-{seed:02d}')
        os.makedirs(d, exist_ok=True)
        vlog = os.path.join(d, 'verify-logs'); os.makedirs(vlog, exist_ok=True)
        args = []
        for a in hermit_args:
            if a == '{SWEEP}':
                a = str(seed)
            elif a.startswith('--verify-log-dir='):
                a = '--verify-log-dir=' + vlog
            args.append(a)
        cmd = ['nice', '-n', '10', 'ionice', '-c3', f'{ROOT}/bin/safehermit',
               '--sh-report', os.path.join(d, 'safehermit.txt'), '--sh-deadline', str(DEADLINE_S),
               '--sh-max-log-bytes', '67108864', '--sh-disk-limit', '8G', HERMIT] + args
        open(os.path.join(d, 'argv.json'), 'w').write(json.dumps(cmd, indent=1))
        t0 = time.time()
        with open(os.path.join(d, 'stdout.txt'), 'wb') as so, open(os.path.join(d, 'stderr.txt'), 'wb') as se:
            rc = subprocess.call(cmd, stdout=so, stderr=se, cwd=ROOT)
        el = time.time() - t0
        out = open(os.path.join(d, 'stdout.txt'), errors='replace').read()
        err = open(os.path.join(d, 'stderr.txt'), errors='replace').read()
        both = out + err
        verify = ('deterministic' if 'Success: deterministic' in err else
                  'NONDETERMINISTIC' if 'nondeterministic' in err else 'unknown')
        n_pass = len(re.findall(pass_re, out))
        n_fail = len(re.findall(fail_re, both))
        outcome = ('FAIL' if n_fail else 'PASS' if (n_pass and rc == 0) else 'OTHER')
        row = {'case': name, 'seed': seed, 'rc': rc, 'elapsed_s': round(el, 1), 'verify': verify,
               'pass_markers': n_pass, 'fail_markers': n_fail, 'outcome': outcome}
        json.dump(row, open(os.path.join(d, 'result.json'), 'w'), indent=1)
        # verify logs are large; keep only on disagreement
        if verify == 'deterministic':
            subprocess.call(['rm', '-rf', vlog])
        summary.append(row)
        print(json.dumps(row), flush=True)
json.dump(summary, open(os.path.join(OUT, 'summary-' + '-'.join(only) + '.json'), 'w'), indent=1)
