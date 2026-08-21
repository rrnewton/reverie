#!/usr/bin/env python3
"""Bracket the stable-background admission gate's rejection attribution.

THE DEFECT THIS GUARDS AGAINST. The gate compares (pid, start_ticks) identity
sets and rejected an arm whenever ANY of them disappeared. Eight unowned
orphans sat on this box on 2026-08-20, aged 15 to 66 hours, belonging to agents
long gone. Any one exiting mid-arm discarded the arm, and the message --
`background PIDs DISAPPEARED: ['592293:58477089']` -- was indistinguishable
from real contamination. Fifty-seven minutes of measurement produced one
admissible arm and the rejections could not be attributed.

WHAT MUST NOT REGRESS IS THE OTHER HALF. Census equality is what makes an arm
trustworthy and it has caught real contamination, including an agent's own
unreaped processes. So the tests below bracket BOTH directions: a known unowned
orphan leaving is background, and everything else still rejects.

Lives in scripts/ because `make lint`'s py-unittest gate discovers only
`scripts/test_*.py`; a test beside the tool would never run.
"""
from __future__ import annotations

import importlib.util
import os
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
GATE = ROOT / "experiments/mttcg-vcpu-concurrency_20260819/admit.py"


def load_gate(me: str | None):
    """Import the gate with DG_AGENT_NAME set, since ME is read at import."""
    previous = os.environ.get("DG_AGENT_NAME")
    if me is None:
        os.environ.pop("DG_AGENT_NAME", None)
    else:
        os.environ["DG_AGENT_NAME"] = me
    try:
        spec = importlib.util.spec_from_file_location("admit_gate", GATE)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module
    finally:
        if previous is None:
            os.environ.pop("DG_AGENT_NAME", None)
        else:
            os.environ["DG_AGENT_NAME"] = previous


def proc(pid, start, comm="qemu-system-x86", ppid=1, agent=None, cpu=0):
    return {
        f"{pid}:{start}": dict(pid=pid, comm=comm, state="S", ppid=ppid,
                               cpu_ticks=cpu, uid=os.getuid(), agent=agent)
    }


def census(procs, t=0.0, kern=0, errs=()):
    return dict(t=t, procs=dict(procs), load=["0", "0", "0"], pressure="x",
                kernel_warn_lines=kern, scan_errors=list(errs),
                total_cpu_ticks=0)


class AdmissionAttributionTest(unittest.TestCase):
    def setUp(self):
        self.assertTrue(GATE.is_file(), f"gate missing at {GATE}")
        self.gate = load_gate("mine")

    def test_a_known_unowned_orphan_exiting_does_not_reject(self):
        """The whole point: background leaving is not contamination."""
        before = census({**proc(111, 900, agent="gone-agent"),
                         **proc(222, 901, agent="mine", ppid=5000)}, t=0.0)
        after = census({**proc(222, 901, agent="mine", ppid=5000)}, t=10.0)
        bad, info = self.gate.compare(before, after)
        self.assertEqual(bad, [], f"a known orphan exit rejected the arm: {bad}")

    def test_that_exit_is_reported_rather_than_silently_swallowed(self):
        """Admitting is not the same as saying nothing: it moved host load."""
        before = census({**proc(111, 900, comm="hermit", agent="gone-agent")}, t=0.0)
        after = census({}, t=10.0)
        _, info = self.gate.compare(before, after)
        joined = " ".join(info["other_agents_orphans"])
        self.assertIn("EXITED-DURING-ARM", joined)
        self.assertIn("111:900", joined)
        self.assertIn("comm=hermit", joined)
        self.assertIn("gone-agent", joined)

    def test_an_untagged_orphan_counts_as_unowned(self):
        """`systemd-run --user` does not inherit DG_AGENT_NAME, so an untagged
        experiment process is more likely unowned, not less."""
        before = census({**proc(333, 902, agent=None)}, t=0.0)
        after = census({}, t=10.0)
        bad, _ = self.gate.compare(before, after)
        self.assertEqual(bad, [], f"an untagged orphan exit rejected: {bad}")

    def test_a_new_process_appearing_still_rejects(self):
        """The half that must not regress."""
        before = census({}, t=0.0)
        after = census({**proc(444, 903, agent="someone")}, t=10.0)
        bad, _ = self.gate.compare(before, after)
        self.assertTrue(any("APPEARED" in b for b in bad), bad)

    def test_a_parented_process_disappearing_still_rejects(self):
        """Someone's live run ending mid-arm is contamination, not background."""
        before = census({**proc(555, 904, ppid=4242, agent="someone")}, t=0.0)
        after = census({}, t=10.0)
        bad, _ = self.gate.compare(before, after)
        self.assertTrue(any("DISAPPEARED" in b for b in bad), bad)

    def test_KNOWN_GAP_a_process_orphaned_during_the_arm_is_not_rejected(self):
        """KNOWN LIMITATION, ASSERTED SO IT IS DISCOVERABLE RATHER THAN FIXED.

        A process present in BOTH censuses whose ppid changes to 1 during the
        arm -- its parent died mid-measurement -- is NOT rejected. The gate
        compares key sets of (pid, start_ticks), so it sees arrivals and
        departures but never notices that a SURVIVOR was reparented. A parent
        exiting mid-arm is a real change in background and the gate is blind
        to it.

        THIS PREDATES the unowned-orphan exclusion above; it was found by
        writing these tests, not introduced by them. It is deliberately NOT
        fixed: rejecting on mid-arm reparenting is a TIGHTENING, tightening
        changes which experimental arms count, and that is a measurement
        semantics decision for the owner rather than something to slip in
        beside a bug fix. A benign wrapper exiting would begin discarding arms.

        This test therefore pins CURRENT behaviour. If someone deliberately
        tightens the gate, this test SHOULD fail, and its failure is the
        signal that the decision was taken -- flip the assertion then.
        """
        before = census({**proc(666, 905, comm="hermit", ppid=4242, agent="someone")}, t=0.0)
        after = census({**proc(666, 905, comm="hermit", ppid=1, agent="someone")}, t=10.0)
        bad, info = self.gate.compare(before, after)
        self.assertEqual(bad, [], "the gate began rejecting mid-arm reparenting; "
                                  "if that was deliberate, flip this assertion")
        self.assertIn("someone:666(hermit)", info["other_agents_orphans"],
                      "the reparented process is not even reported")

    def test_my_own_unreaped_orphan_still_rejects(self):
        """Self-contamination: three agents did this on 2026-08-20."""
        before = census({}, t=0.0)
        after = census({**proc(777, 906, agent="mine")}, t=10.0)
        bad, _ = self.gate.compare(before, after)
        self.assertTrue(any("MY OWN UNREAPED ORPHAN" in b for b in bad), bad)

    def test_every_rejection_names_the_process_not_just_its_pid(self):
        """A rejection nobody can attribute is the defect this file exists for."""
        before = census({**proc(888, 907, comm="hermit", ppid=4242, agent="someone")}, t=0.0)
        after = census({**proc(999, 908, comm="qemu-system-x86", agent="other")}, t=10.0)
        bad, _ = self.gate.compare(before, after)
        joined = " ".join(bad)
        for expected in ("comm=hermit", "888:907", "comm=qemu-system-x86", "999:908"):
            self.assertIn(expected, joined)

    def test_kernel_warnings_and_scan_errors_still_reject(self):
        """Unrelated guards must survive the change."""
        bad, _ = self.gate.compare(census({}, kern=0), census({}, kern=3))
        self.assertTrue(any("soft-lockup" in b or "CSD" in b for b in bad), bad)
        bad, _ = self.gate.compare(census({}, errs=()), census({}, errs=("1:OSError",)))
        self.assertTrue(any("scan errors" in b for b in bad), bad)


if __name__ == "__main__":
    unittest.main()
