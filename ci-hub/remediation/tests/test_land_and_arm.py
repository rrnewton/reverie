from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from argparse import Namespace
from pathlib import Path
from unittest import mock

REMEDIATION = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REMEDIATION))

import land_and_arm

SHA = "a" * 40


class LandAndArmTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.intent_dir = self.root / "intents"
        self.store = self.root / "obligations.jsonl"
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        subprocess.run(
            [
                "git",
                "-C",
                str(self.root),
                "remote",
                "add",
                "origin",
                "https://github.com/rrnewton/hermit.git",
            ],
            check=True,
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def args(self) -> Namespace:
        return Namespace(
            repo="rrnewton/hermit",
            pr=123,
            source=self.root,
            land_mode="admin",
            actor="test-lander",
            command_timeout=5,
            observe_timeout=5,
            github_wait_seconds=5,
            poll_seconds=1,
            store=self.store,
            intent_dir=self.intent_dir,
            command=["--", "/bin/true"],
        )

    def intent(self) -> dict:
        path = land_and_arm._intent_path(self.intent_dir, "rrnewton/hermit", 123)
        return json.loads(path.read_text())

    def test_success_records_intent_before_land_and_arms_merged_sha(self) -> None:
        observed_states: list[str] = []

        def run_command(_command, _timeout):
            observed_states.append(self.intent()["state"])
            return 0

        with mock.patch.object(
            land_and_arm, "_run_land_command", side_effect=run_command
        ), mock.patch.object(
            land_and_arm, "observe_merged_sha", return_value=SHA
        ), mock.patch.object(
            land_and_arm, "arm_sha", return_value=(0, "obligation-1")
        ):
            self.assertEqual(land_and_arm.run(self.args()), 0)
        self.assertEqual(observed_states, ["prepared"])
        self.assertEqual(self.intent()["state"], "armed")
        self.assertEqual(self.intent()["landed_sha"], SHA)
        self.assertEqual(self.intent()["obligation_id"], "obligation-1")

    def test_failed_land_is_not_armed(self) -> None:
        with mock.patch.object(
            land_and_arm, "_run_land_command", return_value=7
        ), mock.patch.object(
            land_and_arm, "_pr_state", return_value=("OPEN", None)
        ), mock.patch.object(
            land_and_arm, "arm_sha"
        ) as arm:
            self.assertEqual(land_and_arm.run(self.args()), 7)
        arm.assert_not_called()
        self.assertEqual(self.intent()["state"], "land-command-failed")

    def test_nonzero_merge_command_still_arms_when_pr_actually_merged(self) -> None:
        with mock.patch.object(
            land_and_arm, "_run_land_command", return_value=1
        ), mock.patch.object(
            land_and_arm, "_pr_state", return_value=("MERGED", SHA)
        ), mock.patch.object(
            land_and_arm, "arm_sha", return_value=(0, "obligation-after-race")
        ):
            self.assertEqual(land_and_arm.run(self.args()), 0)
        self.assertEqual(self.intent()["state"], "armed")
        self.assertEqual(self.intent()["obligation_id"], "obligation-after-race")

    def test_recovery_arms_a_merge_left_between_merge_and_arm(self) -> None:
        args = self.args()
        intent = land_and_arm._new_intent(args, ["/bin/true"])
        path = land_and_arm._intent_path(self.intent_dir, args.repo, args.pr)
        land_and_arm._atomic_json(path, intent)
        with mock.patch.object(
            land_and_arm, "_pr_state", return_value=("MERGED", SHA)
        ), mock.patch.object(
            land_and_arm, "arm_sha", return_value=(0, "obligation-recovered")
        ):
            self.assertEqual(land_and_arm.recover_intent(path, observe_timeout=1), 0)
        recovered = self.intent()
        self.assertEqual(recovered["state"], "armed")
        self.assertEqual(recovered["obligation_id"], "obligation-recovered")

    def test_crash_after_open_recovers_one_local_runner_and_one_watcher(self) -> None:
        args = self.args()
        intent = land_and_arm._new_intent(args, ["/bin/true"])
        intent.update(
            # Simulate the old bug's cache: OPEN existed, so the intent was
            # incorrectly called armed even though no producer was launched.
            state="armed",
            landed_sha=SHA,
            obligation_id="crash-after-open",
            merged_at="2026-08-05T00:00:00Z",
        )
        path = land_and_arm._intent_path(self.intent_dir, args.repo, args.pr)
        land_and_arm._atomic_json(path, intent)
        opened = land_and_arm.obligations.create_obligation(
            repo=args.repo,
            landed_sha=SHA,
            land_mode=args.land_mode,
            verification_policy=intent["verification_policy"],
            actor=args.actor,
            obligation_id="crash-after-open",
            path=self.store,
        )
        self.assertEqual(opened["launch"]["state"], "pending")
        land_and_arm.obligations.transition(
            "crash-after-open",
            "crash-injected-before-spawn",
            {
                "launch": {"state": "repairable", "launcher_pid": None},
                "local": {
                    "state": "starting",
                    "launch_token": "abandoned-token",
                    "source": str(args.source),
                },
            },
            self.store,
        )
        observed_intent_states: list[str] = []

        def register_spawn(arguments, _log_path):
            observed_intent_states.append(self.intent()["state"])
            arguments = list(arguments)
            token = arguments[arguments.index("--launch-token") + 1]
            store = Path(arguments[arguments.index("--store") + 1])
            if arguments[0] == "_local-run":
                land_and_arm.protocol._register_local_runner(
                    "crash-after-open", token, store, pid=201
                )
                return 201
            land_and_arm.protocol._register_watcher(
                "crash-after-open", token, store, pid=202
            )
            return 202

        with mock.patch.object(
            land_and_arm.protocol, "resolve_landed_sha", return_value=SHA
        ), mock.patch.object(
            land_and_arm.protocol,
            "_spawn_detached",
            side_effect=register_spawn,
        ) as spawn, mock.patch.object(
            land_and_arm.protocol,
            "_watcher_process_identity",
            return_value={
                "host": "test-host",
                "boot_id": "test-boot",
                "start_ticks": 2020,
            },
        ), mock.patch.object(
            land_and_arm.protocol, "_pid_alive", return_value=True
        ), mock.patch.object(
            land_and_arm.protocol, "ensure_github_verification"
        ):
            self.assertEqual(land_and_arm.recover_intent(path, observe_timeout=1), 0)
            # A duplicate/restarted recovery consumes the durable launch instead
            # of launching a second verifier or watcher.
            self.assertEqual(land_and_arm.recover_intent(path, observe_timeout=1), 0)

        self.assertEqual(spawn.call_count, 2)
        self.assertEqual(observed_intent_states, ["merged-unarmed", "merged-unarmed"])
        record = land_and_arm.obligations.get_record("crash-after-open", self.store)
        with mock.patch.object(land_and_arm.protocol, "_pid_alive", return_value=True):
            self.assertTrue(land_and_arm.protocol.obligation_launch_durable(record))
        self.assertEqual(record["watcher"]["state"], "running")
        self.assertEqual(record["watcher"]["pid"], 202)
        self.assertEqual(self.intent()["state"], "armed")
        self.assertEqual(self.intent()["obligation_id"], "crash-after-open")
        self.assertEqual(len(land_and_arm.obligations.latest_records(self.store)), 1)

    def test_prepare_then_complete_supports_self_wrapped_lander(self) -> None:
        args = self.args()
        with mock.patch.object(
            land_and_arm, "observe_merged_sha", return_value=SHA
        ), mock.patch.object(
            land_and_arm, "arm_sha", return_value=(0, "obligation-external")
        ):
            self.assertEqual(land_and_arm.prepare(args), 0)
            complete_args = Namespace(
                repo=args.repo,
                pr=args.pr,
                intent_dir=args.intent_dir,
                observe_timeout=1,
            )
            self.assertEqual(land_and_arm.complete(complete_args), 0)
        self.assertEqual(self.intent()["state"], "armed")
        self.assertEqual(self.intent()["command"], ["external-bounded-lander"])

    def test_cross_repo_source_mismatch_is_refused_before_intent_or_land(self) -> None:
        args = self.args()
        args.repo = "rrnewton/reverie"
        path = land_and_arm._intent_path(args.intent_dir, args.repo, args.pr)
        with mock.patch.object(land_and_arm, "_run_land_command") as land:
            with self.assertRaisesRegex(
                land_and_arm.protocol.ProtocolError, "not required repository"
            ):
                land_and_arm.run(args)
        land.assert_not_called()
        self.assertFalse(path.exists())

    def test_reverie_source_identity_is_persisted_in_write_ahead_intent(self) -> None:
        source = self.root / "reverie-source"
        subprocess.run(["git", "init", "-q", str(source)], check=True)
        subprocess.run(
            [
                "git",
                "-C",
                str(source),
                "remote",
                "add",
                "origin",
                "git@github.com:rrnewton/reverie.git",
            ],
            check=True,
        )
        args = self.args()
        args.repo = "rrnewton/reverie"
        args.source = source
        intent = land_and_arm._new_intent(args, ["/bin/true"])
        self.assertEqual(intent["source"], str(source.resolve()))
        self.assertEqual(intent["verification_policy"]["repo"], args.repo)
        self.assertEqual(
            intent["verification_policy"]["github"]["required_positive_count"], 2
        )


if __name__ == "__main__":
    unittest.main()
