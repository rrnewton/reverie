from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import service_result  # noqa: E402


class ServiceResultTest(unittest.TestCase):
    def classify(self, text: str) -> dict[str, object]:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "validate.log"
            log.write_text(text)
            return service_result.from_log(log)

    def test_genuine_pass_requires_status_and_exit_agreement(self) -> None:
        result = self.classify(
            "   862 test(s) executed, 350 filtered\n"
            "   nodes: 55 executed, 0 failed, 0 skipped in 9m12s at -j 16\n"
            "FINAL_VALIDATE_STATUS: PASSED\n"
            "# ci-hub/validate-lock tool COST ACTUAL wall=552.0s cpu=1.0s "
            "cpu_user=0.5s cpu_system=0.5s exit=0\n"
        )

        self.assertEqual("completed", result["state"])
        self.assertEqual("success", result["result"])
        self.assertEqual(0, result["exit_code"])
        self.assertEqual(55, result["executed_nodes"])
        self.assertEqual(862, result["executed_tests"])
        self.assertTrue(service_result.is_evidenced_terminal(result))

    def test_sigterm_exit_is_named_killed_and_preserved_as_143(self) -> None:
        result = self.classify(
            "validate-lock: supervisor received signal 15\n"
            "# ci-hub/validate-lock tool COST ACTUAL wall=50.197s cpu=0.2s "
            "cpu_user=0.1s cpu_system=0.1s exit=143\n"
        )

        self.assertEqual("killed", result["state"])
        self.assertEqual("terminated", result["result"])
        self.assertEqual(143, result["exit_code"])
        self.assertEqual(15, result["signal"])

    def test_direct_signal_description_is_also_named_killed(self) -> None:
        result = self.classify(
            "# ci-hub/validate-lock tool COST ACTUAL wall=2.0s cpu=0.1s "
            "cpu_user=0.1s cpu_system=0.0s exit=signal:9\n"
        )

        self.assertEqual("killed", result["state"])
        self.assertEqual(137, result["exit_code"])
        self.assertEqual(9, result["signal"])

    def test_exit_zero_without_status_is_unknown(self) -> None:
        result = self.classify(
            "   0 test(s) executed, 0 filtered\n"
            "   nodes: 0 executed, 0 failed, 0 skipped in 0s at -j 16\n"
            "# ci-hub/validate-lock tool COST ACTUAL wall=0.1s cpu=0.1s "
            "cpu_user=0.1s cpu_system=0.0s exit=0\n"
        )

        self.assertEqual("unknown", result["state"])
        self.assertEqual("unknown", result["result"])
        self.assertEqual(0, result["exit_code"])
        self.assertEqual(0, result["executed_nodes"])
        self.assertEqual("final validate status is absent", result["detail"])

    def test_collected_units_synthetic_success_is_not_exit_evidence(self) -> None:
        properties = {
            "LoadState": "not-found",
            "ActiveState": "inactive",
            "ExecMainCode": "0",
            "ExecMainStatus": "0",
            "Result": "success",
            "InvocationID": "",
        }

        result = service_result.classify(service_result.RunEvidence(), properties=properties)

        # `unknown`, not `not-run`. This test's whole thesis is that a collected
        # unit's synthetic success IS NOT EVIDENCE -- so the honest answer is
        # that the classifier cannot tell, never a positive claim that the run
        # did not happen.
        self.assertEqual("unknown", result["state"])
        self.assertEqual("unknown", result["result"])
        self.assertIsNone(result["exit_code"])
        self.assertEqual("no-exit-evidence", result["result_source"])

    def test_unknown_is_not_terminal_so_it_is_re_derived_not_frozen(self) -> None:
        """The one state that means "I cannot tell" must never be durable.

        Freezing it is the defect: an ignorant row gets preserved and re-served
        instead of being re-derived once the log or the unit can answer.
        """
        unknown = service_result.classify(service_result.RunEvidence())

        self.assertEqual("unknown", unknown["state"])
        self.assertFalse(service_result.is_evidenced_terminal(unknown))

    def test_absence_is_not_terminal_even_when_exit_is_known(self) -> None:
        result = self.classify(
            "   0 test(s) executed, 0 filtered\n"
            "   nodes: 0 executed, 0 failed, 0 skipped in 0s at -j 16\n"
            "# ci-hub/validate-lock tool COST ACTUAL wall=0.1s cpu=0.1s "
            "cpu_user=0.1s cpu_system=0.0s exit=0\n"
        )

        self.assertEqual("unknown", result["state"])
        self.assertFalse(service_result.is_evidenced_terminal(result))

    def test_could_not_run_is_named_and_uses_exit_75(self) -> None:
        result = self.classify(
            "validate refused by: stale base\n"
            "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n"
            "# ci-hub/validate-lock tool COST ACTUAL wall=0.1s cpu=0.1s "
            "cpu_user=0.1s cpu_system=0.0s exit=75\n"
        )

        self.assertEqual("completed", result["state"])
        self.assertEqual("no-result", result["result"])
        self.assertEqual("COULD_NOT_RUN", result["final_validate_status"])
        self.assertTrue(service_result.is_evidenced_terminal(result))

    def test_earlier_quoted_status_does_not_override_final_status(self) -> None:
        result = self.classify(
            "fixture says FINAL_VALIDATE_STATUS: PASSED in prose\n"
            "FINAL_VALIDATE_STATUS: COULD_NOT_RUN\n"
            "quoted block follows:\n"
            "FINAL_VALIDATE_STATUS: FAILED\n"
            "# ci-hub/validate-lock tool COST ACTUAL wall=0.1s cpu=0.1s "
            "cpu_user=0.1s cpu_system=0.0s exit=1\n"
        )

        self.assertEqual("completed", result["state"])
        self.assertEqual("failure", result["result"])
        self.assertEqual("FAILED", result["final_validate_status"])

    def test_status_exit_disagreement_is_unknown(self) -> None:
        result = self.classify(
            "FINAL_VALIDATE_STATUS: PASSED\n"
            "# ci-hub/validate-lock tool COST ACTUAL wall=0.1s cpu=0.1s "
            "cpu_user=0.1s cpu_system=0.0s exit=1\n"
        )

        self.assertEqual("unknown", result["state"])
        self.assertEqual("unknown", result["result"])
        self.assertIn("requires exit 0, got 1", result["detail"])

    def test_status_signal_disagreement_is_unknown_not_killed(self) -> None:
        result = self.classify(
            "FINAL_VALIDATE_STATUS: PASSED\n"
            "# ci-hub/validate-lock tool COST ACTUAL wall=0.1s cpu=0.1s "
            "cpu_user=0.1s cpu_system=0.0s exit=143\n"
        )

        self.assertEqual("unknown", result["state"])
        self.assertIn("requires exit 0, got 143", result["detail"])

    def test_loaded_systemd_signal_is_fail_closed_fallback(self) -> None:
        properties = {
            "LoadState": "loaded",
            "ActiveState": "failed",
            "ExecMainCode": "killed",
            "ExecMainStatus": "15",
            "Result": "signal",
            "InvocationID": "fixture-invocation",
        }

        result = service_result.classify(service_result.RunEvidence(), properties=properties)

        self.assertEqual("killed", result["state"])
        self.assertEqual(143, result["exit_code"])


if __name__ == "__main__":
    unittest.main()
