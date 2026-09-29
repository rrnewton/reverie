"""Apply one named mutation to followup.py's plain-text policy or snapshot use.

Usage: negctl-mutate-plaintext.py FILE NAME. Each mutation must match exactly once.
"""

import sys
from pathlib import Path

MUTATIONS = {
    "no-control-filter": (
        'unicodedata.category(ch)[0] != "C"',
        "True",
    ),
    "no-backtick-escape": (
        """.replace("`", "'")""",
        "",
    ),
    "no-length-cap": (
        "if len(line) > AGENT_TEXT_LIMIT:",
        "if False:",
    ),
    "no-text-fence": (
        '            "```text",\n',
        "",
    ),
    "reread-report": (
        "rank, evidence_gap, alternatives = _hypotheses(candidate)",
        "rank, evidence_gap, alternatives = _hypotheses(\n"
        "        validate_candidate_report(Path(str(report)), case.case_id)\n"
        "    )",
    ),
}

path, name = Path(sys.argv[1]), sys.argv[2]
old, new = MUTATIONS[name]
text = path.read_text()
if text.count(old) != 1:
    sys.exit(f"{name}: anchor matched {text.count(old)} times")
path.write_text(text.replace(old, new))
