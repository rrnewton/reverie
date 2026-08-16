#!/usr/bin/env python3
"""Render the results table from captured data. Nothing here is hand-typed."""
import json
import sys

d = json.load(open("results-pass2.json"))
p1 = {r["cell"]: r for r in json.load(open("results-pass1.json"))}

rows = []
for r in d:
    cell = r["cell"]
    a = r["accept"]
    ao = a["observable"]
    for m in r["mutants"]:
        mo = m["observable"]
        # the observable the verdict reads is the exit status; the bite is proven
        # on the guest's own stdout+stderr under `hermit run`
        moved = (ao["rc"], ao["stdout"], ao["stderr"]) != (mo["rc"], mo["stdout"], mo["stderr"])
        gated = m["gate_replay"]["rc"] not in (0, "TIMEOUT")
        verdict = ("MUTATION-INERT" if not moved
                   else "EXIT-GATED" if gated
                   else "NOT-EXIT-GATED")
        rows.append({
            "cell": cell, "mut": m["label"], "family": m["family"],
            "anchor": m["anchor"].replace("\n", " ⏎ "),
            "replacement": m["replacement"].replace("\n", " ⏎ "),
            "note": m["note"],
            "acc_obs_rc": ao["rc"], "mut_obs_rc": mo["rc"],
            "acc_stdout": ao["stdout"], "mut_stdout": mo["stdout"],
            "acc_stderr_b": len(ao["stderr"]), "mut_stderr_b": len(mo["stderr"]),
            "mut_stderr": mo["stderr"].strip(),
            "acc_replay_rc": a["gate_replay"]["rc"], "mut_replay_rc": m["gate_replay"]["rc"],
            "acc_verify_rc": a["gate_verify"]["rc"], "mut_verify_rc": m["gate_verify"]["rc"],
            "acc_native_rc": a["native"]["rc"], "acc_native_stderr": a["native"]["stderr"].strip(),
            "verdict": verdict,
        })

out = []
w = out.append
w("cell                                       mutation      acc_replay mut_replay acc_verify mut_verify  verdict")
for r in rows:
    w(f"{r['cell']:42s} {r['mut']:13s} {str(r['acc_replay_rc']):>10s} "
      f"{str(r['mut_replay_rc']):>10s} {str(r['acc_verify_rc']):>10s} "
      f"{str(r['mut_verify_rc']):>10s}  {r['verdict']}")
w("")
w("OBSERVABLE DELTA (guest stdout+stderr under `hermit run --backend ptrace --strict`)")
for r in rows:
    w(f"{r['cell']}  {r['mut']}")
    w(f"    stdout {r['acc_stdout']!r} ({len(r['acc_stdout'])}B) -> {r['mut_stdout']!r} ({len(r['mut_stdout'])}B)")
    w(f"    stderr {r['acc_stderr_b']}B -> {r['mut_stderr_b']}B: {r['mut_stderr']!r}")
text = "\n".join(out)
open("TABLE.txt", "w").write(text + "\n")
print(text)
json.dump(rows, open("rows.json", "w"), indent=1)
