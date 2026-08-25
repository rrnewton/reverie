#!/usr/bin/env python3
"""Decision-table tests for the port-time timeout gate.

The gate's whole value is that it refuses to bless a budget on a path that
cannot enforce it, and refuses an invented constant where the data does not
support one. Both refusals are bracketed here, in both directions.
"""
from __future__ import annotations

import io
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import port_gate as G


def step(name="test.x", **kw):
    s = {"name": name, "_manifest": "m.json"}
    s.update(kw)
    return s


def budgets(node="test.x", n=20, max_cpu=10.0, thin=False):
    return {node: {"node": node, "n_samples": n, "max_cpu_s": max_cpu,
                   "suggested_cpu_timeout": round(max_cpu * 1.5), "thin": thin}}


class EnforcementGateTest(unittest.TestCase):
    """Check 1: a budget on an unenforced path is inert, and must not be blessed."""

    def test_unenforced_path_beats_every_other_signal(self) -> None:
        """Even a perfectly derived cpu_timeout is NOT_ENFORCED off a boxed path.

        This ordering is the point of the gate. 'Declared but unenforced' is the
        exact state the work exists to leave, so it must not be reportable as a
        pass no matter how good the number is.
        """
        a = G.audit_node(step(cpu_timeout=15, timeout=600, hint={"est_duration_s": 8}),
                         budgets(), enforced=False)
        self.assertEqual(a.verdict, G.NOT_ENFORCED)
        self.assertIn("INERT", " ".join(a.notes))

    def test_unenforced_still_reports_the_carried_wall(self) -> None:
        """The 600s-on-an-8-second-node smell is the thing to re-derive at port
        time, so it must survive into the report rather than being masked."""
        a = G.audit_node(step(timeout=600, hint={"est_duration_s": 8}),
                         budgets(), enforced=False)
        self.assertEqual(a.wall_bloat_x, 75.0)
        self.assertIn("re-derive", " ".join(a.notes))


class DerivationTest(unittest.TestCase):
    """Check 2: derived, never guessed. UNSET is a valid answer."""

    def test_thin_data_yields_UNSET_not_a_number(self) -> None:
        a = G.audit_node(step(timeout=60), budgets(n=3), enforced=True)
        self.assertEqual(a.verdict, G.UNSET_OK)
        self.assertIsNone(a.derived_cpu_timeout)

    def test_invented_constant_on_thin_data_is_a_HARD_FAILURE(self) -> None:
        """A plausible-looking number where the data cannot support one is worse
        than UNSET: it reads as derived and nobody re-checks it."""
        a = G.audit_node(step(cpu_timeout=42, timeout=60), budgets(n=3), enforced=True)
        self.assertEqual(a.verdict, G.UNDERIVED)
        self.assertIn("invented constant", " ".join(a.notes))

    def test_missing_cpu_timeout_on_enforceable_path_with_data(self) -> None:
        a = G.audit_node(step(timeout=60), budgets(n=20, max_cpu=10.0), enforced=True)
        self.assertEqual(a.verdict, G.MISSING_CPU)
        self.assertEqual(a.derived_cpu_timeout, 15)

    def test_declared_must_equal_derived(self) -> None:
        a = G.audit_node(step(cpu_timeout=99, timeout=60), budgets(n=20, max_cpu=10.0),
                         enforced=True)
        self.assertEqual(a.verdict, G.UNDERIVED)

    def test_correctly_derived_passes(self) -> None:
        """The POSITIVE control -- a gate that never passes anything is useless."""
        a = G.audit_node(step(cpu_timeout=15, timeout=20, hint={"est_duration_s": 10}),
                         budgets(n=20, max_cpu=10.0), enforced=True)
        self.assertEqual(a.verdict, G.PASS)

    def test_derivation_anchors_on_MAX_not_p95(self) -> None:
        """The tail is what trips a ceiling, so the anchor must be the max."""
        b = budgets(n=20, max_cpu=100.0)
        b["test.x"]["p95_cpu_s"] = 10.0
        derived, n, _ = G.derive(b, "test.x")
        self.assertEqual(derived, 150)      # 100*1.5, not 10*1.5

    def test_derived_zero_is_floored_because_zero_means_DISABLED(self) -> None:
        """A node whose max_cpu is under ~0.33s derives to round(x*1.5)==0, and
        the scheduler enables the monitor only `if cpu_timeout > 0`. Emitting 0
        would silently DISABLE the ceiling while looking like a derived number --
        the exact 'declared but unenforced' shape, self-inflicted."""
        derived, n, note = G.derive(budgets(n=20, max_cpu=0.1), "test.x")
        self.assertEqual(derived, 1)
        self.assertIn("DISABLED", note)

    def test_correct_cpu_but_bloated_wall_is_still_flagged(self) -> None:
        a = G.audit_node(step(cpu_timeout=15, timeout=600, hint={"est_duration_s": 8}),
                         budgets(n=20, max_cpu=10.0), enforced=True)
        self.assertEqual(a.verdict, G.BLOATED)


class EnforcementDetectionTest(unittest.TestCase):
    """The enforcement verdict is READ FROM SOURCE, not from a table that rots."""

    def test_detects_ci_short_circuit_and_local_boxing(self) -> None:
        root = G._root()
        by_name = {p.name: p for p in G.enforcement_paths(root)}
        runner_lane = next(k for k in by_name if "run-dag.sh" in k)
        self.assertFalse(by_name[runner_lane].boxed)
        self.assertIn("GITHUB_ACTIONS", by_name[runner_lane].reason)
        local = next(k for k in by_name if k.startswith("local"))
        self.assertTrue(by_name[local].boxed)

    def test_routing_must_bind_to_the_exec_line_not_a_substring(self) -> None:
        """REGRESSION: run-node.sh's header comment describes the RETIRED jq+bash
        design and names safe-ci-dag-runner nine times. Keying "is it routed?" on
        that substring reports routed for a file that does not route -- a proxy
        with no causal link to execution. Bind to the exec line."""
        by_name = {p.name: p for p in G.enforcement_paths(G._root())}
        portable = next(k for k in by_name if "run-node.sh" in k)
        self.assertTrue(by_name[portable].routed)
        self.assertIn("--only", by_name[portable].reason)

    def test_portable_lane_is_ROUTED_but_NOT_BOXED(self) -> None:
        """The live half-ported state, and the reason the audit must fire NOW:
        the run --only port made manifest WALL timeouts enforceable for the first
        time, while --allow-cgroup-failure keeps cpu_timeout inert."""
        self.assertTrue(G.any_ci_path_routed(G._root()))
        self.assertFalse(G.any_ci_path_boxes(G._root()))

    def test_reads_renamed_dagrun_cgroup_source(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            source = root / "agent-utils/rs/dagrun/src/cgroup.rs"
            source.parent.mkdir(parents=True)
            source.write_text('var("CI"); var("GITHUB_ACTIONS");\n')

            by_name = {path.name: path for path in G.enforcement_paths(root)}
            runner_lane = next(name for name in by_name if "run-dag.sh" in name)

            self.assertIn("GITHUB_ACTIONS", by_name[runner_lane].reason)


class JoinTest(unittest.TestCase):
    """A join that silently matches nothing makes the whole gate vacuous."""

    def test_node_name_is_group_dot_job(self) -> None:
        self.assertEqual(G.node_name({"group": "check", "job": "reverie_pin"}),
                         "check.reverie_pin")

    def test_join_against_the_real_store_is_non_empty(self) -> None:
        """REGRESSION: keying on `name`/`id` (absent from these manifests) made
        EVERY store lookup miss, so every node reported "no samples" and the gate
        degraded to a uniform UNSET that reads as a clean result."""
        res = G.run_gate(G._root())
        self.assertGreater(res["derivable_now"], 0,
                           "no manifest node joined the budget store -- the gate is vacuous")


def run_as_selftest() -> int:
    suite = unittest.TestLoader().loadTestsFromModule(sys.modules[__name__])
    buf = io.StringIO()
    with redirect_stdout(buf):
        result = unittest.TextTestRunner(stream=buf, verbosity=2).run(suite)
    print(buf.getvalue())
    ok = result.wasSuccessful()
    print(f"port-gate selftest: {'PASS' if ok else 'FAIL'} ({result.testsRun} tests)")
    return 0 if ok else 1


if __name__ == "__main__":
    unittest.main()
