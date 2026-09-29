#!/usr/bin/env python3
"""Independent recount of the TFA kernel-7.1 campaign from its experiment.json files.

Read-only. Does NOT import the repository validator; counters are recomputed from
record fields so they can be compared with validate.py and RESULTS.md section 4.1.

Run from anywhere:
    python3 /tmp/sub-tfa2-030/gather/campaign_recount.py > /tmp/sub-tfa2-030/gather/campaign_recount.out 2>&1

Definitions (from experiments/tfa-flaky-tests_20260917/README.md "Discovery, confirmation,
and durable evidence"):
  S = samples with status VALID (one sample = one discovery tuple (cell, swept flag, value))
  C = samples with status CENSORED
  T = all samples (= S + C)
  R = events with kind HERMIT_EXECUTION and result.guest_started == true
  E = events with kind HERMIT_EXECUTION (whether or not the guest started)
Per-target union rule (RESULTS.md section 7): a canonical target is WORKING if any of
its records has current_outcome.kind == WORKING, else BLOCKED if any is BLOCKED, else
NOT-DETERMINABLE.
"""
import collections
import glob
import json
import os

REPO = "/home/newton/work/dev-hermit"
KERNEL71_GLOB = "experiments/tfa-kernel71-*_20260921"
SCHEMA_ROOT = "experiments/tfa-flaky-tests_20260917"

SKIP_FLAGS = {"--backend", "--strict", "--chaos", "--env", "--seed", "--rng-seed",
              "--sched-seed", "--fuzz-seed", "--verify", "--verify-strict",
              "--verify-allow", "--keep-logs"}


def hermit_options(argv):
    """Return the Hermit option segment between 'run' and the guest '--'."""
    try:
        i = argv.index("run")
    except ValueError:
        return []
    try:
        j = argv.index("--", i)
    except ValueError:
        j = len(argv)
    return argv[i + 1:j]


def control_summary(argv):
    """Scheduler/time controls: every Hermit option other than the common ones."""
    opts = hermit_options(argv)
    out = []
    k = 0
    while k < len(opts):
        tok = opts[k]
        if not tok.startswith("--"):
            k += 1
            continue
        base = tok.split("=", 1)[0]
        if base in SKIP_FLAGS or base in ("--mount", "--verify-log-dir", "--base-env"):
            k += 2 if (base in ("--env", "--seed", "--rng-seed", "--sched-seed", "--fuzz-seed",
                                "--verify-allow", "--backend") and "=" not in tok) else 1
            continue
        if "=" in tok:
            out.append(tok)
            k += 1
            continue
        nxt = opts[k + 1] if k + 1 < len(opts) else None
        if nxt is not None and not nxt.startswith("--"):
            out.append(f"{tok} {nxt}")
            k += 2
            continue
        out.append(tok)
        k += 1
    return out


def load(root):
    recs = []
    for p in sorted(glob.glob(os.path.join(REPO, root, "services/*/*/experiment.json"))):
        with open(p) as f:
            recs.append((os.path.relpath(p, REPO), json.load(f)))
    return recs


def seed_of(sample):
    s = sample.get("seed") or {}
    return s.get("flag"), s.get("value")


def per_record(d):
    samples = d.get("samples", [])
    events = d.get("events", [])
    S = sum(1 for s in samples if s.get("status") == "VALID")
    C = sum(1 for s in samples if s.get("status") == "CENSORED")
    T = len(samples)
    kinds = collections.Counter(e.get("kind") for e in events)
    hx = [e for e in events if e.get("kind") == "HERMIT_EXECUTION"]
    R = sum(1 for e in hx if (e.get("result") or {}).get("guest_started") is True)
    E = len(hx)
    roles = collections.Counter(e.get("measurement_role") for e in hx)
    return dict(S=S, C=C, T=T, R=R, E=E, kinds=dict(kinds), roles=dict(roles))


def describe_record(path, d):
    ident = d["identity"]
    pr = per_record(d)
    co = d.get("current_outcome") or {}
    lines = [f"### {path}"]
    lines.append(f"  target={ident['canonical_target']}  test={ident['test_name']}  mode={ident['build_mode_selector']}")
    lines.append(f"  attempt_state={d.get('attempt_state')}  plan_id={d['plan'].get('plan_id')}  sealed_at={d['plan'].get('sealed_at')}  plan_receipt={d.get('plan_receipt')}")
    md = d["plan"].get("measurement_design", {})
    lines.append(f"  design: purpose={md.get('purpose')} swept={md.get('swept_coordinate', {}).get('flag')} modes={md.get('swept_coordinate', {}).get('scheduler_modes')} "
                 f"basis={md.get('design_basis', {}).get('kind')} p={md.get('design_basis', {}).get('expected_hermit_fail_probability')} "
                 f"N={md.get('required_valid_seed_tuples')} cap={md.get('max_attempted_seed_tuples')} "
                 f"population={len(md.get('seed_population', {}).get('values', []))} stop={md.get('stop_rule')} fixed={md.get('fixed_seed_roots')}")
    fcs = [fc["failure_class_id"] for fc in d["plan"]["target_oracle"]["fail_classes"]]
    lines.append(f"  oracle={d['plan']['target_oracle']['oracle_id']} fail_classes={fcs} timeout_policy={d['plan']['target_oracle']['timeout_policy']}")
    for c in d.get("cells", []):
        lines.append(f"  cell={c['cell_id']} sha256={c['cell_sha256']} frozen_at={c.get('frozen_at')}")
        lines.append(f"    hermit_rev={c['hermit_source_revision']} hermit_bin={c['hermit_binary_sha256']} test_elf={c['test_artifact_sha256']}")
        lines.append(f"    controls={control_summary(c['hermit']['argv'])} pmu={c['pmu']} timing={c['timing']}")
        lines.append(f"    host={c['host']['hostname']} | {c['host']['kernel']} | {c['host']['cpu']} bounds={c['resource_bounds']}")
    lines.append(f"  counters: S={pr['S']} C={pr['C']} T={pr['T']} R={pr['R']} E(HERMIT_EXECUTION)={pr['E']} event_kinds={pr['kinds']} roles={pr['roles']}")
    ev_by_id = {e["event_id"]: e for e in d.get("events", [])}
    for s in d.get("samples", []):
        fl, v = seed_of(s)
        lines.append(f"  sample {s['sample_id']} seed={fl}={v} idx={s.get('selection_index')} status={s['status']} outcome={s.get('target_outcome')} class={s.get('failure_class_id')}")
        if s.get("censor_reason"):
            lines.append(f"      censor_reason: {s['censor_reason']}")
        for i in s.get("execution_event_ids", []):
            e = ev_by_id.get(i)
            if e is None:
                lines.append(f"      event {i}: MISSING")
                continue
            r = e.get("result") or {}
            lines.append(f"      event {i}: {e.get('measurement_role')} gs={r.get('guest_started')} exit={r.get('exit_code')} sig={r.get('signal')} "
                         f"to={r.get('timed_out')}/{r.get('timeout_origin')} sel={r.get('selected_test_executed_count')} oracle={r.get('target_oracle')} "
                         f"cls={r.get('failure_class_id')} harness={r.get('harness_status')}")
            if r.get("diagnostic"):
                lines.append(f"        diagnostic: {r['diagnostic']}")
    for m in d.get("matchers", []):
        lines.append(f"  matcher {m['matcher_id']} kind={m['kind']} events={m.get('execution_event_ids')} complete={m.get('complete')} same_outcome={m.get('same_target_outcome')} same_class={m.get('same_failure_class')} details={m.get('details')}")
    lines.append(f"  current_outcome.kind={co.get('kind')} classification_id={co.get('classification_id')} classified_at={co.get('classified_at')} history_len={len(d.get('outcome_history', []))}")
    if co.get("kind") == "WORKING":
        lines.append(f"    pass_witness={co.get('pass_witness')} fail_witness={co.get('fail_witness')}")
    for k in ("missing_or_ambiguous_facts", "pending_in_scope_measurements", "measurement_budget_exhausted"):
        if k in co:
            lines.append(f"    {k}: {co[k]}")
    return lines


def eff(kinds):
    if "WORKING" in kinds:
        return "WORKING"
    if "BLOCKED-WITH-NAMED-CAUSE" in kinds:
        return "BLOCKED-WITH-NAMED-CAUSE"
    return "NOT-DETERMINABLE"


def main():
    out = []
    roots = [os.path.relpath(r, REPO) for r in sorted(glob.glob(os.path.join(REPO, KERNEL71_GLOB)))]
    out.append(f"kernel71 roots found: {len(roots)}")
    all_recs = []
    for r in roots:
        for p, d in load(r):
            all_recs.append((r, p, d, per_record(d)))

    out.append("")
    out.append("== Per-root recount (A/W/B/U = distinct canonical targets within the root)")
    for r in roots:
        rr = [(p, d, pr) for (x, p, d, pr) in all_recs if x == r]
        tg = {d["identity"]["canonical_target"]: (d.get("current_outcome") or {}).get("kind") for p, d, pr in rr}
        tot = collections.Counter()
        for p, d, pr in rr:
            for k in ("S", "C", "T", "R", "E"):
                tot[k] += pr[k]
        out.append(f"{r}: records={len(rr)} A={len(tg)} W={sum(v == 'WORKING' for v in tg.values())} "
                   f"B={sum(v == 'BLOCKED-WITH-NAMED-CAUSE' for v in tg.values())} U={sum(v == 'NOT-DETERMINABLE' for v in tg.values())} "
                   f"S={tot['S']} C={tot['C']} T={tot['T']} R={tot['R']} E={tot['E']}")

    per_t = {}
    for r, p, d, pr in all_recs:
        t = d["identity"]["canonical_target"]
        e = per_t.setdefault(t, dict(records=0, S=0, C=0, T=0, R=0, E=0, kinds=[], roots=[]))
        e["records"] += 1
        for k in ("S", "C", "T", "R", "E"):
            e[k] += pr[k]
        e["kinds"].append((d.get("current_outcome") or {}).get("kind"))
        e["roots"].append(r.split("experiments/tfa-kernel71-")[1])

    out.append("")
    out.append("== Per-target union across all kernel71 roots")
    for t in sorted(per_t):
        e = per_t[t]
        out.append(f"{t}: effective={eff(e['kinds'])} records={e['records']} S={e['S']} C/T={e['C']}/{e['T']} R={e['R']} E={e['E']} per-record-kinds={e['kinds']} roots={e['roots']}")
    tot = collections.Counter()
    for e in per_t.values():
        for k in ("records", "S", "C", "T", "R", "E"):
            tot[k] += e[k]
    effs = collections.Counter(eff(e["kinds"]) for e in per_t.values())
    out.append("")
    out.append("== Campaign totals (kernel71, 13 roots, canonical-target union)")
    out.append(f"A={len(per_t)} W={effs['WORKING']} B={effs['BLOCKED-WITH-NAMED-CAUSE']} U={effs['NOT-DETERMINABLE']}")
    out.append(f"records={len(all_recs)} material_records={sum(1 for _, _, d, _ in all_recs if d.get('attempt_state') == 'MATERIAL')}")
    out.append(f"S={tot['S']} C={tot['C']} T={tot['T']} R(guest-started HERMIT_EXECUTION events)={tot['R']} E(all HERMIT_EXECUTION events)={tot['E']}")
    kinds_all = collections.Counter()
    for _, _, d, _ in all_recs:
        for ev in d.get("events", []):
            kinds_all[ev.get("kind")] += 1
    out.append(f"event kinds over all records: {dict(kinds_all)}")
    n_sh = n_abs = n_buck = 0
    for _, _, d, _ in all_recs:
        for ev in d.get("events", []):
            if ev.get("kind") != "HERMIT_EXECUTION":
                continue
            a = ev.get("argv") or []
            if a and a[0] == "bin/safehermit":
                n_sh += 1
            try:
                j = a.index("--", a.index("run"))
                if a[j + 1].startswith("/home/newton/work/tfa/fbsource/buck-out/"):
                    n_abs += 1
            except (ValueError, IndexError):
                pass
            if any(tok in ("buck", "buck2") for tok in a):
                n_buck += 1
    out.append(f"HERMIT_EXECUTION argv checks: begins-with-bin/safehermit={n_sh} guest-argv0-under-tfa-buck-out={n_abs} contains-buck-token={n_buck}")
    facts = {k: collections.Counter() for k in ("host", "hermit_rev", "hermit_bin", "target_rev", "backend", "strict_chaos_relax", "pmu", "bounds", "heuristic", "swept")}
    ncells = 0
    for _, _, d, _ in all_recs:
        for c in d.get("cells", []):
            ncells += 1
            facts["host"][(c["host"]["hostname"], c["host"]["kernel"], c["host"]["architecture"], c["host"]["cpu"])] += 1
            facts["hermit_rev"][c["hermit_source_revision"]] += 1
            facts["hermit_bin"][c["hermit_binary_sha256"]] += 1
            facts["target_rev"][c["target_source_revision"]] += 1
            facts["backend"][c["hermit"]["backend"]] += 1
            facts["strict_chaos_relax"][(c["hermit"]["strict"], c["hermit"]["chaos"], tuple(c["hermit"]["relaxations"]))] += 1
            facts["pmu"][json.dumps(c["pmu"], sort_keys=True)] += 1
            facts["bounds"][json.dumps(c["resource_bounds"], sort_keys=True)] += 1
            facts["heuristic"][c["hermit"]["scheduler_heuristic"]] += 1
            facts["swept"][c["hermit"]["swept_coordinate"]] += 1
    out.append(f"cells={ncells}")
    for k, v in facts.items():
        out.append(f"cell fact {k}: {dict(v)}")

    # Timing of HERMIT_EXECUTION events (started_at/finished_at are second-resolution UTC strings).
    import datetime
    import statistics

    def ts(x):
        return datetime.datetime.strptime(x, "%Y-%m-%dT%H:%M:%SZ")
    starts, ends = [], []
    dur_by_t = collections.defaultdict(list)
    for _, _, d, _ in all_recs:
        for ev in d.get("events", []):
            if ev.get("kind") != "HERMIT_EXECUTION" or not ev.get("started_at") or not ev.get("finished_at"):
                continue
            a, b = ts(ev["started_at"]), ts(ev["finished_at"])
            starts.append(a)
            ends.append(b)
            dur_by_t[d["identity"]["canonical_target"]].append((b - a).total_seconds())
    alld = [x for v in dur_by_t.values() for x in v]
    out.append(f"HERMIT_EXECUTION wall window (UTC): first started_at={min(starts)} last finished_at={max(ends)}; "
               f"sum of per-event (finished-started) = {sum(alld):.0f} s over {len(alld)} events "
               f"(note: a --verify invocation's two replicas can carry overlapping timestamps, so this sum can double-count)")
    for t in sorted(dur_by_t):
        v = dur_by_t[t]
        out.append(f"  {t}: events={len(v)} median={statistics.median(v):.0f}s min={min(v):.0f}s max={max(v):.0f}s")

    out.append("")
    out.append("== Schema root (experiments/tfa-flaky-tests_20260917), context only; not part of the kernel71 union")
    st = collections.Counter()
    srecs = load(SCHEMA_ROOT)
    for p, d in srecs:
        pr = per_record(d)
        for k in ("S", "C", "T", "R", "E"):
            st[k] += pr[k]
        hk = sorted({(c["host"]["kernel"], c["hermit_source_revision"][:12]) for c in d.get("cells", [])})
        out.append(f"{p}: kind={(d.get('current_outcome') or {}).get('kind')} S={pr['S']} C={pr['C']} T={pr['T']} R={pr['R']} E={pr['E']} kernel/hermit={hk}")
        for s in d.get("samples", []):
            out.append(f"    sample {s['sample_id']} status={s['status']} censor_reason: {s.get('censor_reason')}")
        for ev in d.get("events", []):
            r = ev.get("result") or {}
            out.append(f"    event {ev.get('event_id')}: kind={ev.get('kind')} exit={r.get('exit_code')} sig={r.get('signal')} gs={r.get('guest_started')} sel={r.get('selected_test_executed_count')} harness={r.get('harness_status')} diag={r.get('diagnostic')}")
    out.append(f"schema root totals: records={len(srecs)} S={st['S']} C={st['C']} T={st['T']} R={st['R']} E={st['E']}")

    out.append("")
    out.append("== Compact per-record summary (kernel71): seed->status/outcome/class, censor reasons, diagnostics")
    for r, p, d, pr in all_recs:
        co = d.get("current_outcome") or {}
        c0 = d["cells"][0] if d.get("cells") else None
        out.append(f"### {p}")
        out.append(f"  kind={co.get('kind')} classification_id={co.get('classification_id')} S={pr['S']} C={pr['C']} T={pr['T']} R={pr['R']} E={pr['E']} "
                   f"cell={c0['cell_id'] if c0 else None} swept={c0['hermit']['swept_coordinate'] if c0 else None} controls={control_summary(c0['hermit']['argv']) if c0 else None}")
        md = d["plan"]["measurement_design"]
        out.append(f"  design N={md['required_valid_seed_tuples']} cap={md['max_attempted_seed_tuples']} basis={md['design_basis']['kind']} population={md['seed_population']['values'][:3]}..{md['seed_population']['values'][-1:]} ({len(md['seed_population']['values'])})")
        seq = []
        for s in d.get("samples", []):
            fl, v = seed_of(s)
            tag = s["status"][0] + ":" + (s.get("target_outcome") or "-") + ("/" + s["failure_class_id"] if s.get("failure_class_id") else "")
            seq.append(f"{v}={tag}")
        out.append(f"  samples: {' '.join(seq)}")
        cr = collections.Counter(s.get("censor_reason") for s in d.get("samples", []) if s.get("status") == "CENSORED")
        for k, v in cr.most_common():
            out.append(f"  censor_reason x{v}: {k}")
        dg = collections.Counter()
        for e in d.get("events", []):
            rr = e.get("result") or {}
            dg[(e.get("kind"), rr.get("guest_started"), rr.get("exit_code"), rr.get("signal"), rr.get("timed_out"), rr.get("harness_status"), rr.get("target_oracle"), rr.get("diagnostic"))] += 1
        for k, v in dg.most_common():
            out.append(f"  event-result x{v}: kind={k[0]} gs={k[1]} exit={k[2]} sig={k[3]} timed_out={k[4]} harness={k[5]} oracle={k[6]} diag={k[7]}")
        if co.get("kind") == "WORKING":
            out.append(f"  pass_witness={co.get('pass_witness')} fail_witness={co.get('fail_witness')}")
        for k in ("missing_or_ambiguous_facts", "pending_in_scope_measurements", "measurement_budget_exhausted"):
            if k in co:
                out.append(f"  current_outcome.{k}: {co[k]}")
        out.append("")

    out.append("")
    out.append("== Per-record detail (kernel71)")
    for r, p, d, pr in all_recs:
        out.extend(describe_record(p, d))
        out.append("")
    print("\n".join(out))


if __name__ == "__main__":
    main()
