import sys
from pathlib import Path

MUTATIONS = {
    "no-patched-fail": (
        '    if any(run.verdict == "FAIL" for run in patched):\n',
        "    if False:\n",
    ),
    "no-wall-evidence": (
        '    if returncode == 124 and report.get("unit_result") == "timeout" and not decided:\n',
        "    if returncode == 124 and not decided:\n",
    ),
    "no-phase-distinct": (
        '    if guests["unfixed"] == guests["patched"]:\n',
        "    if False:\n",
    ),
    "no-guest-check": (
        '    if guest.get("sha256") != guest_sha256:\n',
        "    if False:\n",
    ),
    "no-stdout-binding": (
        '        _bound_file(receipt.get("stdout"), run / "stdout.txt", "stdout")\n',
        "",
    ),
    "trust-recorded-label": (
        "    decided = _decided(case, argv, returncode, run)\n",
        '    decided = str(_object(receipt["classification"], "c").get("oracle") or "")\n',
    ),
    "no-failing-seed-rule": (
        "            reasons.append(\n                f\"seed {seed} FAILed unfixed but not every patched replay PASSed\"\n            )\n",
        "            pass\n",
    ),
    "no-witness-caveat": (
        "            if seed not in failing\n",
        "            if False\n",
    ),
}
MUTATIONS["no-artifact-binding"] = (
    "    if guests[\"unfixed\"] != case.artifact_sha256:\n",
    "    if False:\n",
)
path = Path(sys.argv[1])
old, new = MUTATIONS[sys.argv[2]]
text = path.read_text()
if text.count(old) != 1:
    sys.exit(f"mutation {sys.argv[2]} does not match exactly once")
path.write_text(text.replace(old, new))
