#!/usr/bin/env python3
"""Brackets for the pre-replacement liveness probe, both directions.

The safeguard has TWO failure modes and this file must pin both:

  FALSE KILL  -- a live agent reported dead gets replaced. The 2026-08-08
                 incident. Guarded by every `refuse` case below.
  NO HEALING  -- the safeguard is so cautious that a genuinely dead agent is
                 never replaced. Equally a failure, and the easier one to ship
                 by accident. Guarded by every `verified-dead` case.

The live-process fixture at the bottom spawns OUR OWN child carrying a
throwaway `DG_AGENT_NAME`, so nothing here can touch a real agent, and the
probe itself never kills anything -- it reads /proc and prints. That is the
same inert-fixture discipline used for the closure gateway: the artifact under
test is not itself an authorization to do the dangerous thing.
"""

from __future__ import annotations

import importlib.util
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).resolve().parents[1] / "agent_liveness_probe.py"
SPEC = importlib.util.spec_from_file_location("agent_liveness_probe", MODULE_PATH)
assert SPEC and SPEC.loader
probe = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = probe
SPEC.loader.exec_module(probe)


def plant(
    root: Path,
    pid: int,
    *,
    agent: str | None,
    state: str = "S",
    comm: str = "claude",
    readable: bool = True,
) -> None:
    """Create a synthetic /proc/<pid>. `readable=False` omits environ entirely,
    which is the same OSError path a real unreadable environ takes."""
    entry = root / str(pid)
    entry.mkdir()
    (entry / "stat").write_text(f"{pid} ({comm}) {state} 1 1 0 0 -1 0 0 0\n")
    (entry / "comm").write_text(comm + "\n")
    if readable:
        payload = "PATH=/usr/bin\0"
        if agent is not None:
            payload += f"{probe.IDENTITY_VAR}={agent}\0"
        (entry / "environ").write_bytes(payload.encode())


class SyntheticProcTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)

    # ---- FALSE KILL side: these must all refuse -------------------------

    def test_live_agent_refuses(self) -> None:
        plant(self.root, 100, agent="victim")
        plant(self.root, 101, agent="bystander")
        verdict = probe.verify("victim", root=self.root)
        self.assertEqual("alive", verdict.state)
        self.assertEqual(probe.REFUSED_ALIVE, verdict.rc)
        self.assertFalse(verdict.may_replace)

    def test_stopped_agent_is_alive_not_dead(self) -> None:
        """A SIGSTOPped agent is the land-lock census case. `T` is not death."""
        plant(self.root, 100, agent="victim", state="T")
        plant(self.root, 101, agent="bystander")
        self.assertEqual("alive", probe.verify("victim", root=self.root).state)

    def test_broken_probe_cannot_authorize_a_kill(self) -> None:
        """Zero agents of ANY name means the detector is blind, not the fleet empty.

        This is the branch that stops a renamed env var or a container without
        host /proc from authorizing every replacement at once.
        """
        plant(self.root, 100, agent=None)
        plant(self.root, 101, agent=None)
        verdict = probe.verify("victim", root=self.root)
        self.assertEqual("unverifiable", verdict.state)
        self.assertEqual(probe.UNVERIFIABLE, verdict.rc)
        self.assertIn("this probe is", verdict.reason)

    def test_unreadable_agent_comm_still_blocks(self) -> None:
        """Pins the NARROWNESS of the AGENT_COMMS exclusion.

        Widening that set widens a hole in a safety check, so an unreadable
        process that looks like an agent CLI must keep forcing UNVERIFIABLE.
        """
        plant(self.root, 101, agent="bystander")
        plant(self.root, 102, agent=None, comm="claude", readable=False)
        verdict = probe.verify("victim", root=self.root)
        self.assertEqual("unverifiable", verdict.state)
        self.assertEqual(1, verdict.scan.blind_spot)

    def test_scan_error_refuses(self) -> None:
        verdict = probe.verify("victim", root=self.root / "does-not-exist")
        self.assertEqual("unverifiable", verdict.state)
        self.assertEqual(probe.UNVERIFIABLE, verdict.rc)

    # ---- NO HEALING side: these must all permit -------------------------

    def test_absent_agent_is_replaceable(self) -> None:
        plant(self.root, 101, agent="bystander")
        plant(self.root, 102, agent="other")
        verdict = probe.verify("victim", root=self.root)
        self.assertEqual("verified-dead", verdict.state)
        self.assertEqual(probe.VERIFIED_DEAD, verdict.rc)
        self.assertTrue(verdict.may_replace)

    def test_zombie_only_agent_is_replaceable(self) -> None:
        plant(self.root, 100, agent="victim", state="Z")
        plant(self.root, 101, agent="bystander")
        verdict = probe.verify("victim", root=self.root)
        self.assertEqual("verified-dead", verdict.state)

    def test_non_agent_unreadable_processes_do_not_block(self) -> None:
        """The measured real case: systemd / sd-pam / sshd / gnome-keyring.

        Counting these forced UNVERIFIABLE on every genuine death, i.e. it
        disabled self-healing outright.
        """
        plant(self.root, 101, agent="bystander")
        for pid, comm in ((200, "systemd"), (201, "sshd"), (202, "gnome-keyring-d")):
            plant(self.root, pid, agent=None, comm=comm, readable=False)
        verdict = probe.verify("victim", root=self.root)
        self.assertEqual("verified-dead", verdict.state)
        self.assertEqual(0, verdict.scan.blind_spot)
        self.assertEqual(3, verdict.scan.unreadable_non_agent)

    def test_unknown_comm_is_not_excluded(self) -> None:
        """An unreadable comm must not buy an exclusion from the safety check."""
        plant(self.root, 101, agent="bystander")
        entry = self.root / "300"
        entry.mkdir()
        (entry / "stat").write_text("300 (x) S 1 1 0 0 -1 0 0 0\n")
        self.assertEqual("unverifiable", probe.verify("victim", root=self.root).state)

    # ---- BLANK SUBJECT: the observed fail-open ---------------------------
    # Measured 2026-08-10 from the ORC surface: the probe was called with an
    # empty `$1` and returned `may_replace: true` for EVERY query while the scan
    # itself was healthy (265 processes, blind_spot 0, another agent seen
    # alive). Every self-check passed; the verdict was simply not ABOUT anybody.
    # These cases must stay refusals no matter how good the scan looks.

    def test_empty_name_refuses_even_on_a_perfect_scan(self) -> None:
        """The exact reproduction: healthy scan, live agents visible, blank name.

        Without the guard this is `verified-dead` + `may_replace: true`, because
        a blank name matches nothing and "nothing matched over a complete scan"
        is the permit branch. A vacuous absence must never authorize a kill.
        """
        plant(self.root, 101, agent="bystander")
        plant(self.root, 102, agent="other")
        verdict = probe.verify("", root=self.root)
        self.assertEqual("unverifiable", verdict.state)
        self.assertEqual(probe.UNVERIFIABLE, verdict.rc)
        self.assertFalse(verdict.may_replace)
        self.assertEqual(probe.NO_NAME_REASON, verdict.reason)

    def test_whitespace_name_refuses(self) -> None:
        """`" "` is a present argument and a real shell can produce it."""
        plant(self.root, 101, agent="bystander")
        for blank in (" ", "\t", "\n", "   "):
            with self.subTest(blank=repr(blank)):
                verdict = probe.verify(blank, root=self.root)
                self.assertEqual("unverifiable", verdict.state)
                self.assertFalse(verdict.may_replace)

    def test_blank_name_does_not_even_scan(self) -> None:
        """Coverage must report "did not look", not a complete sweep.

        A blank-name refusal that carried `own_uid_total: 265` would invite a
        reader to conclude the scan had cleared the subject.
        """
        plant(self.root, 101, agent="bystander")
        verdict = probe.verify("", root=self.root)
        self.assertEqual(0, verdict.scan.own_uid_total)
        self.assertEqual((), verdict.scan.matches)

    def test_pure_judge_also_refuses_a_blank_name(self) -> None:
        """The guard lives on the authority, not only on the `verify` wrapper.

        A caller that assembles its own Scan -- the exact thing that makes
        `judge` reusable -- must not be able to route around the check.
        """
        scan = probe.Scan(
            own_uid_total=265,
            distinct_agents_seen=frozenset({"bystander"}),
        )
        verdict = probe.judge("", scan)
        self.assertEqual("unverifiable", verdict.state)
        self.assertFalse(verdict.may_replace)

    def test_a_real_name_on_that_same_scan_still_permits(self) -> None:
        """POSITIVE control for the guard: it must not have disabled healing.

        Same Scan object as the negative case above, differing ONLY in the name.
        Without this, the four refusals above would also be satisfied by a probe
        that had started refusing everything.
        """
        scan = probe.Scan(
            own_uid_total=265,
            distinct_agents_seen=frozenset({"bystander"}),
        )
        verdict = probe.judge("victim", scan)
        self.assertEqual("verified-dead", verdict.state)
        self.assertTrue(verdict.may_replace)

    def test_rendered_blank_verdict_echoes_the_empty_name_and_refuses(self) -> None:
        """The JSON a caller actually reads must not say `may_replace: true`."""
        import json

        verdict = probe.verify("", root=self.root)
        payload = json.loads(probe._render("", verdict, as_json=True))
        self.assertEqual("", payload["agent"])
        self.assertFalse(payload["may_replace"])
        self.assertEqual(probe.UNVERIFIABLE, payload["rc"])

    def test_missing_argument_exits_unverifiable(self) -> None:
        """argparse's usage-error code and UNVERIFIABLE must not drift apart.

        A caller passing no argument at all is refused only because argparse
        happens to exit 2. That equals UNVERIFIABLE today; pin it so a future
        switch to a custom parser cannot silently turn it into 0.
        """
        with self.assertRaises(SystemExit) as caught:
            probe.main([])
        self.assertEqual(probe.UNVERIFIABLE, caught.exception.code)

    def test_cli_with_empty_name_returns_unverifiable(self) -> None:
        """End to end through `main`, the path the ORC script actually takes."""
        self.assertEqual(
            probe.UNVERIFIABLE,
            probe.main(["", "--json", "--proc-root", str(self.root)]),
        )



def _reap(process) -> None:
    """Kill one of OUR OWN children by exact pid. Never a pattern, never a name."""
    try:
        process.kill()
        process.wait(timeout=10)
    except Exception:
        pass

class LiveProcessBracketTest(unittest.TestCase):
    """The real bracket: one throwaway name, two liveness states, same code path.

    Uses a genuine spawned process and a genuine /proc scan rather than a
    synthetic tree, so it exercises the environ read that the whole safeguard
    rests on. The child is OUR OWN (we hold its pid), and it is signalled by
    exact pid -- never a pattern kill, per Hard Invariant 15.
    """

    def test_live_then_dead_flips_the_verdict(self) -> None:
        name = f"probe-fixture-{os.getpid()}"
        canary = f"probe-canary-{os.getpid()}"

        # A CANARY carrying a DIFFERENT agent identity, alive for the whole
        # bracket. It supplies the precondition the self-check above demands:
        # with at least one agent visible, "no process carries <name>" means the
        # subject is dead rather than that the probe is blind. Without it this
        # test asserted `verified-dead` in an environment where the probe is
        # CORRECT to refuse, so it failed on every runner that hosts no agents.
        # This adds a precondition; it does not relax the assertion, which is
        # still exactly `verified-dead`.
        watcher = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(120)"],
            env={**os.environ, probe.IDENTITY_VAR: canary},
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        self.addCleanup(_reap, watcher)

        # A real process that merely sleeps, carrying the throwaway identity.
        child = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(120)"],
            env={**os.environ, probe.IDENTITY_VAR: name},
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        try:
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if probe.verify(name).state == "alive":
                    break
                time.sleep(0.1)

            alive = probe.verify(name)
            self.assertEqual("alive", alive.state, alive.reason)
            self.assertEqual(probe.REFUSED_ALIVE, alive.rc)
            self.assertIn(child.pid, [m.pid for m in alive.scan.live_matches])
        finally:
            # Our own child, by exact pid.
            child.kill()
            child.wait(timeout=10)

        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if probe.verify(name).state != "alive":
                break
            time.sleep(0.1)

        dead = probe.verify(name)
        self.assertEqual("verified-dead", dead.state, dead.reason)
        self.assertEqual(probe.VERIFIED_DEAD, dead.rc)
        self.assertTrue(dead.may_replace)

    def test_a_real_live_agent_is_never_replaceable(self) -> None:
        """Whichever agent is running this test is, by construction, alive."""
        me = os.environ.get(probe.IDENTITY_VAR)
        if not me:
            self.skipTest(f"{probe.IDENTITY_VAR} not set in this environment")
        verdict = probe.verify(me)
        self.assertEqual("alive", verdict.state, verdict.reason)
        self.assertFalse(verdict.may_replace)


if __name__ == "__main__":
    unittest.main()
