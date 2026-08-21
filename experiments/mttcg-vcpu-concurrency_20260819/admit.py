#!/usr/bin/env python3
"""STABLE-BACKGROUND admission gate. Exits non-zero to REJECT an arm.

Counts are not enough: the PR465 A/B logged a constant qemu=6/hermit=2 across
every arm and was still inadmissible, because the *identity* of the background
changed and host CPU pressure moved. This compares identity sets keyed by
(pid, start_ticks) -- start_ticks defeats PID reuse, which matters because PID
wrap is close on this box.

  capture <file>            write a census
  compare <before> <after>  gate a completed arm; rc 0 admit, rc 1 reject
"""
import os, sys, json, time

MYUID = os.getuid()
# Only MY orphans are mine to reap; another agent's orphan is a REPORT, not a kill.
ME = os.environ.get('DG_AGENT_NAME')
INTERESTING = ('qemu-system', 'hermit', 'spin2', 'spin3', 'racer', 'order')

def agent_of(pid):
    try:
        for e in open(f'/proc/{pid}/environ', 'rb').read().split(b'\0'):
            if e.startswith(b'DG_AGENT_NAME='):
                return e.decode('utf8', 'replace').split('=', 1)[1]
    except OSError:
        pass
    return None

def capture():
    procs, errs = {}, []
    total = 0
    for pd in os.listdir('/proc'):
        if not pd.isdigit():
            continue
        try:
            st = open(f'/proc/{pd}/stat').read()
            comm = st.split('(')[1].rsplit(')', 1)[0]
            tail = st.rsplit(')', 1)[1].split()
            state, ppid, start = tail[0], int(tail[1]), int(tail[19])
            cpu = int(tail[11]) + int(tail[12])
        except (OSError, IndexError, ValueError) as exc:
            errs.append(f'{pd}:{type(exc).__name__}')
            continue
        try:
            uid = os.stat(f'/proc/{pd}').st_uid
        except OSError:
            continue
        if not any(comm.startswith(k) for k in INTERESTING):
            continue
        total += cpu
        procs[f'{pd}:{start}'] = dict(pid=int(pd), comm=comm, state=state,
                                      ppid=ppid, cpu_ticks=cpu, uid=uid,
                                      agent=agent_of(pd) if uid == MYUID else None)
    la = open('/proc/loadavg').read().split()[:3]
    try:
        pres = open('/proc/pressure/cpu').read().strip().splitlines()[0]
    except OSError:
        pres = 'unavailable'
    try:
        dm = open('/dev/kmsg', 'rb', 0)
        dm.close(); kern = None
    except OSError:
        kern = None
    import subprocess
    try:
        out = subprocess.run(['dmesg'], capture_output=True, text=True, timeout=20).stdout
        kern = sum(1 for l in out.splitlines()
                   if any(k in l.lower() for k in ('csd:', 'rcu_sched', 'rcu_preempt',
                                                   'soft lockup', 'softlockup')))
    except Exception:
        kern = -1
    return dict(t=time.time(), procs=procs, load=la, pressure=pres,
                kernel_warn_lines=kern, scan_errors=errs,
                total_cpu_ticks=total)

def describe(census, key):
    """pid:start alone cannot be attributed. Say what it was and whose."""
    v = census['procs'].get(key)
    if not v:
        return key
    return (f"{key}(comm={v['comm']} ppid={v['ppid']} "
            f"agent={v['agent'] or 'untagged'})")


def unowned_orphan_before(a, key):
    """Was this already somebody's abandoned process when the arm STARTED?

    ppid==1 and not mine. `agent is None` counts: a process launched through
    `systemd-run --user` does not inherit DG_AGENT_NAME, so an untagged
    experiment process is MORE likely to be unowned, not less -- and the
    INTERESTING filter means anything captured here is an experiment process,
    not a system daemon.

    Judged from the BEFORE census on purpose. A process that becomes an orphan
    DURING the arm is a parent dying mid-measurement, which is contamination and
    must still reject.
    """
    v = a['procs'].get(key)
    return bool(v) and v['ppid'] == 1 and v['agent'] != ME


def compare(a, b):
    bad = []
    info_orphans = []
    ka, kb = set(a['procs']), set(b['procs'])
    gone, new = sorted(ka - kb), sorted(kb - ka)
    # AN UNOWNED ORPHAN EXITING IS BACKGROUND, NOT CONTAMINATION, AND REJECTING
    # ON IT THREW AWAY GOOD ARMS. Eight such processes sat on this box on
    # 2026-08-20, aged 15 to 66 hours, belonging to agents that were long gone;
    # any one of them exiting mid-arm discarded the arm, and the rejection read
    # exactly like real contamination, so the agent whose arm died could not
    # tell which had happened. Fifty-seven minutes of measurement that night
    # yielded one admissible arm and nobody could attribute the rest.
    # This does NOT loosen the gate for anything owned: a departure that was not
    # already an unowned orphan at the BEFORE census still rejects, and an
    # arrival always rejects.
    orphan_exits = [k for k in gone if unowned_orphan_before(a, k)]
    real_gone = [k for k in gone if not unowned_orphan_before(a, k)]
    if real_gone:
        bad.append('background PIDs DISAPPEARED: '
                   + ', '.join(describe(a, k) for k in real_gone))
    if new:
        bad.append('background PIDs APPEARED: '
                   + ', '.join(describe(b, k) for k in new))
    if orphan_exits:
        # Reported, never silent: it changes host load mid-arm even when it does
        # not invalidate the arm, and a reader judging a marginal result needs
        # to know it happened.
        info_orphans.extend('EXITED-DURING-ARM ' + describe(a, k)
                            for k in orphan_exits)
    # my own orphans -- three agents contaminated themselves with these tonight
    mine = [v for v in b['procs'].values()
            if v['ppid'] == 1 and ME and v['agent'] == ME]
    for v in mine:
        bad.append(f"MY OWN UNREAPED ORPHAN: pid={v['pid']} comm={v['comm']} "
                   f"ppid={v['ppid']} agent={v['agent']}")
    others = [v for v in b['procs'].values()
              if v['ppid'] == 1 and v['agent'] and v['agent'] != ME]
    if others:
        who = sorted({v['agent'] for v in others})
        # Not a rejection by itself: it is background, and stable background is
        # admissible. It is reported so its OWNER can reap it.
        info_orphans.extend(f"{v['agent']}:{v['pid']}({v['comm']})" for v in others)
    if b['kernel_warn_lines'] != a['kernel_warn_lines']:
        bad.append(f"new kernel CSD/RCU/soft-lockup lines: "
                   f"{a['kernel_warn_lines']} -> {b['kernel_warn_lines']}")
    if len(b['scan_errors']) > len(a['scan_errors']):
        bad.append(f"new scan errors: {len(a['scan_errors'])} -> {len(b['scan_errors'])}")
    # background CPU burned by UNRELATED processes during the arm
    dt = max(1e-9, b['t'] - a['t'])
    hz = os.sysconf('SC_CLK_TCK')
    shared = ka & kb
    burn = sum(b['procs'][k]['cpu_ticks'] - a['procs'][k]['cpu_ticks']
               for k in shared if not b['procs'][k]['agent'])
    cores = burn / hz / dt
    return bad, dict(other_agents_orphans=info_orphans, seconds=round(dt, 1), background_cores=round(cores, 3),
                     load_before=a['load'], load_after=b['load'],
                     n_background=len(shared))

if __name__ == '__main__':
    if sys.argv[1] == 'capture':
        json.dump(capture(), open(sys.argv[2], 'w'), indent=1)
        print(f'census -> {sys.argv[2]}')
    else:
        a = json.load(open(sys.argv[2])); b = json.load(open(sys.argv[3]))
        bad, info = compare(a, b)
        print('ADMISSION', json.dumps(info))
        for x in bad: print('  REJECT:', x)
        print('VERDICT:', 'REJECT' if bad else 'ADMIT')
        sys.exit(1 if bad else 0)
