"""Attribute the per-obligation poll cost of `protocol.py watch` to subprocesses."""
import sys, time
from pathlib import Path
ROOT = Path('/home/newton/work/dev-hermit')
sys.path.insert(0, str(ROOT / 'ci-hub/remediation'))
import protocol, obligations

orig_run = protocol._run
log = []
def traced(cmd, **kw):
    t0 = time.monotonic()
    try:
        return orig_run(cmd, **kw)
    finally:
        log.append((time.monotonic() - t0, list(cmd)))
protocol._run = traced

store = obligations.default_store_path()
recs = obligations.latest_records(store)
targets = sorted(
    (r for r in recs.values() if r.get('overall_state') not in obligations.CLOSED_STATES),
    key=lambda r: (str(r.get('opened_at','')), r['obligation_id']),
)
which = sys.argv[1:] or [targets[0]['obligation_id'], targets[8]['obligation_id']]
for oid in which:
    log.clear()
    t0 = time.monotonic()
    protocol.poll_obligation(oid, store, deadline=None, redispatch=False)
    total = time.monotonic() - t0
    print(f'=== {oid} total={total:.2f}s subprocesses={len(log)} '
          f'accounted={sum(d for d,_ in log):.2f}s')
    for d, cmd in log:
        print(f'    {d:6.2f}s  {" ".join(str(c) for c in cmd)[:170]}')
