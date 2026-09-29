import sys
from pathlib import Path

MUTATIONS = {
    "no-oracle": (
        "        _check_oracle(case, outcome, argv, classification.get(\"returncode\"), run)\n",
        "        pass\n",
    ),
    "no-repeat-check": (
        "            if key in report:\n                raise RelocationError(f\"safehermit report repeats {key}: {run}\")\n            report[key] = value\n",
        "            report.setdefault(key, value)\n",
    ),
    "no-binary-check": (
        "    if report.get(\"binary_sha256\") != case.hermit_sha256:\n        raise RelocationError(f\"safehermit ran a Hermit not in the manifest: {run}\")\n",
        "",
    ),
    "no-censored-oracle-refusal": (
        "        raise RelocationError(f\"CENSORED attempt satisfies the {decided} oracle: {run}\")\n",
        "        pass\n",
    ),
    "no-realpath": (
        "    for candidate in (path, os.path.realpath(path)):\n",
        "    for candidate in (path,):\n",
    ),
}
path = Path(sys.argv[1])
old, new = MUTATIONS[sys.argv[2]]
text = path.read_text()
if text.count(old) != 1:
    sys.exit(f"mutation {sys.argv[2]} does not match exactly once")
path.write_text(text.replace(old, new))
