from pathlib import Path

HERE = Path(__file__).parent
p = Path("/home/newton/work/tfa/fbsource/fbcode/testinfra/tfa/tests/test_prototype.py")
s = p.read_text()


def rep(old: str, new: str) -> None:
    global s
    assert s.count(old) == 1, old[:60]
    s = s.replace(old, new)


rep(
    "from testinfra.tfa.followup import (\n    followup_text,\n",
    "from testinfra.tfa.followup import (\n    AGENT_TEXT_LIMIT,\n    followup_text,\n",
)
rep(
    '            "proven): An invariant was violated before the fatal symptom.\\n"\n'
    '            "- Contradicting or unverified evidence the investigator reported: "\n'
    '            "not instrumented\\n"\n'
    '            "- Other ranked hypotheses, which these replays do not exclude:\\n"\n'
    '            "  - rank 1: race\\n",\n',
    '            "proven): An invariant was violated before the fatal symptom.\\n"\n'
    '            "- Investigator-written text, shown as plain text with each field "\n'
    '            "collapsed to one line and cut at 1000 characters:\\n"\n'
    '            "```text\\n"\n'
    '            "Contradicting or unverified evidence the investigator reported: "\n'
    '            "not instrumented\\n"\n'
    '            "Other ranked hypotheses, which these replays do not exclude:\\n"\n'
    '            "rank 1: race\\n"\n'
    '            "```\\n",\n',
)
anchor = "    def test_comment_gives_measured_commands_for_lowest_failing_seed(self) -> None:\n"
rep(anchor, (HERE / "new_followup_tests.py.txt").read_text() + anchor)
p.write_text(s)
