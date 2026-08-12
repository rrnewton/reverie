#!/usr/bin/env python3
"""Bounded, dependency-isolated smoke tests for every ci-hub command."""

from __future__ import annotations

import json
import os
import resource
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CI_HUB = ROOT / "ci-hub/ci-hub"
WALL_SECONDS = 15
# End-to-end semantic brackets span a complete authority lifecycle rather than
# one bounded ci-hub operation. The receipt bracket takes about 31s; the gate
# rotation bracket creates local repositories and drives ten mutation events.
# Keep ordinary command bounds strict and give only complete brackets enough
# time to report their real pass/fail result.
RECEIPT_BRACKET_WALL_SECONDS = 90
# The eligibility bracket drives ~30 negative and 4 positive cases through the
# real `ci-hub validate-status` binary against the on-disk ledger, so its cost
# tracks host load rather than any one operation. Measured 2026-08-11 on
# devbig014 at load ~21: 17.4s, 27.9s, 82.9s, 128.8s across four consecutive
# runs. The former 15s bound sat BELOW even the fastest observation, so this
# bracket could only ever time out -- it reported nothing for either leg, which
# is how three unbacked `locally-validated` labels survived unobserved. A bound
# that can never be met is not a strict bound, it is a disabled test.
ELIGIBILITY_BRACKET_WALL_SECONDS = 300
CPU_SECONDS = 5


def _limit_cpu() -> None:
    resource.setrlimit(resource.RLIMIT_CPU, (CPU_SECONDS, CPU_SECONDS))


class OperationalBoundsTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        # The shard measures command behavior, not a cold rustc bootstrap. Build
        # the rust-script front door before applying the per-command 5s CPU box;
        # subsequent --force checks still detect changed #[path] modules but use
        # Cargo's no-op path when the binary is current.
        subprocess.run(
            [str(CI_HUB), "--help"],
            cwd=ROOT,
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
            timeout=120,
        )

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.temp = Path(self.temporary.name)
        self.bin = self.temp / "bin"
        self.bin.mkdir()
        self.stall = self._script("stall", "exec sleep 30")
        self.succeed = self._script("succeed", "exit 0")
        self._script(
            "with-proxy",
            'exec "' + str(self.stall) + '" "$@"',
        )
        self._script(
            "tg",
            "printf '%s\\n' "
            '\'{"id":"task","title":"test","owner":"worker","tags":[]}\' '
            "'(1 rows)'",
        )
        self.snapshot = self.temp / "agents.json"
        self.snapshot.write_text(
            json.dumps([{"name": "worker", "status": "busy", "current_task": "task"}])
        )
        self.store = self.temp / "obligations.jsonl"

        # BIND A GRAPH so `active-work` can actually answer.
        #
        # The `tg` stub above is not enough any more, and that is the whole
        # reason this test went red. ci-hub now refuses an UNBOUND TaskGraph
        # read before it ever reaches `tg`: with TG_DB_PATH unset it returns
        # rc 2 `taskgraph-query-unavailable` rather than letting tg's implicit
        # `tasks` default report an empty fleet as clean. That refusal is
        # CORRECT -- it is a could-not-measure, not a failure -- so the fix is to
        # give the subcommand something to measure, NOT to widen the expected rc
        # set. Widening would put a silent pass exactly where a correct
        # could-not-measure is, which is the defect this repo has spent the
        # session removing; a green bought that way is worse than the red.
        #
        # `validate()` requires only a readable sqlite file carrying a `tasks`
        # relation; the row CONTENT comes from the stubbed `tg`, so an empty
        # schema is sufficient and keeps the fixture hermetic.
        #
        # Binding explicitly also removes a hidden environmental dependency:
        # `self.env` copies os.environ, so this test's outcome silently depended
        # on whether the RUNNER happened to have TG_DB_PATH set. It passed for
        # anyone with a graph bound and failed in CI without one, which is why
        # it surfaced late.
        self.graph = self.temp / "taskgraph.db"
        connection = sqlite3.connect(self.graph)
        try:
            connection.executescript(
                "CREATE TABLE tasks ("
                " local_id TEXT PRIMARY KEY, title TEXT, owner TEXT,"
                " status TEXT, tags TEXT);"
                "CREATE TABLE task_notes ("
                " id INTEGER PRIMARY KEY, task_id TEXT, content TEXT);"
            )
            connection.commit()
        finally:
            connection.close()

        self.env = os.environ.copy()
        self.env.update(
            {
                "TG_DB_PATH": str(self.graph),
                "PATH": f"{self.bin}:{self.env['PATH']}",
                "CI_HUB_TOOL_COST_ACTIVE": "1",
                "CI_HUB_MAIN_HEALTH_TIMEOUT": "0.1",
                "CI_HUB_MAIN_HEALTH_DEADLINE": "0.2",
                "CI_HUB_PR_STATUS_TIMEOUT": "0.1",
                "CI_HUB_PR_STATUS_DEADLINE": "0.2",
                "CI_HUB_REMEDIATION_NETWORK_TIMEOUT": "0.1",
                "CI_HUB_AGGREGATE_TMPDIRS": str(self.temp),
                "DEV_HERMIT_PARENT": str(self.temp),
                "TMPDIR": str(self.temp),
            }
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _script(self, name: str, body: str) -> Path:
        path = self.bin / name
        path.write_text(f"#!/usr/bin/env bash\nset -euo pipefail\n{body}\n")
        path.chmod(0o755)
        return path

    def run_bounded(
        self,
        *args: str,
        expected: set[int] = {0},
        env: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        started = time.monotonic()
        process = subprocess.Popen(
            [str(CI_HUB), *args],
            cwd=ROOT,
            env=self.env if env is None else env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
            preexec_fn=_limit_cpu,
        )
        try:
            stdout, stderr = process.communicate(timeout=WALL_SECONDS)
        except subprocess.TimeoutExpired as error:
            os.killpg(process.pid, signal.SIGKILL)
            process.communicate()
            self.fail(f"ci-hub {' '.join(args)} exceeded {WALL_SECONDS}s: {error}")
        result = subprocess.CompletedProcess(
            process.args,
            process.returncode,
            stdout,
            stderr,
        )
        elapsed = time.monotonic() - started
        self.assertLess(elapsed, WALL_SECONDS)
        self.assertIn(
            result.returncode,
            expected,
            msg=f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}",
        )
        return result

    def test_could_not_measure_stays_distinguishable_from_success(self) -> None:
        """The bracket: an UNBOUND graph must NOT be able to read as success.

        This is the assertion that makes binding a graph above a real fix rather
        than a way to make the red go away. If someone later "fixes" a future
        failure of this suite by widening the expected rc set to {0, 2}, or by
        letting an unbound read fall back to tg's implicit `tasks` default, this
        test fails -- because rc 2 and rc 0 would stop being distinguishable and
        an unmeasurable fleet would render as a clean one.

        Same shape as the github-main repair: absence must alarm distinctly from
        green, never quietly join it.
        """
        unbound = dict(self.env)
        unbound.pop("TG_DB_PATH", None)

        measured = self.run_bounded(
            "active-work", "--agent-snapshot", str(self.snapshot), "--json",
            expected={0},
        )
        unmeasurable = self.run_bounded(
            "active-work", "--agent-snapshot", str(self.snapshot), "--json",
            expected={2}, env=unbound,
        )

        self.assertNotEqual(
            measured.returncode, unmeasurable.returncode,
            "could-not-measure collapsed into the success code")
        self.assertIn("taskgraph-query-unavailable", unmeasurable.stdout)
        # And it must not dress the refusal up as a measured, empty fleet.
        self.assertNotIn('"state": "clean"', unmeasurable.stdout)
        self.assertIn('"state": "unknown"', unmeasurable.stdout)

    def test_every_subcommand_dispatches_within_wall_and_cpu_bounds(self) -> None:
        tick_env = self.env | {"CI_HUB_AGENT_TOOL": str(self.succeed)}
        stall_env = self.env | {"CI_HUB_AGENT_TOOL": str(self.stall)}
        cases = (
            (
                ("active-work", "--agent-snapshot", str(self.snapshot), "--json"),
                {0},
                self.env,
            ),
            (("main-health", "--repo", "rrnewton/dev-hermit"), {2}, self.env),
            (
                (
                    "hosted-status",
                    "--repo",
                    "rrnewton/hermit",
                    "--sha",
                    "a" * 40,
                    "--json",
                ),
                {2, 4},
                self.env,
            ),
            (("pr-status", "--repo", "rrnewton/dev-hermit"), {2}, stall_env),
            (("tick",), {0}, tick_env),
            (
                (
                    "arm-land",
                    "not-a-sha",
                    "--source",
                    str(ROOT),
                    "--no-dispatch",
                    "--store",
                    str(self.store),
                ),
                {2},
                self.env,
            ),
            (("obligations", "--store", str(self.store)), {0}, self.env),
            (
                ("obligations", "--actionable", "--store", str(self.store)),
                {0},
                self.env,
            ),
            (
                (
                    "inherit-obligations",
                    "--agent",
                    "test-lander",
                    "--session",
                    "test-session",
                    "--store",
                    str(self.store),
                ),
                {0},
                self.env,
            ),
            (
                (
                    "record-obligation-wake",
                    "--target",
                    "test-lander",
                    "--source",
                    "test",
                    "--store",
                    str(self.store),
                ),
                {0},
                self.env,
            ),
            (
                ("watch-obligations", "--once", "--store", str(self.store)),
                {0},
                self.env,
            ),
            (
                (
                    "cancel-watcher",
                    "missing",
                    "--reason",
                    "test cancellation",
                    "--store",
                    str(self.store),
                ),
                {2},
                self.env,
            ),
            (
                (
                    "resolve-obligation",
                    "missing",
                    "--kind",
                    "revert",
                    "--ref",
                    "a" * 40,
                    "--store",
                    str(self.store),
                ),
                {2},
                self.env,
            ),
            (("refresh-history", "--", "--help"), {0}, self.env),
            (("history", "--", "--help"), {0}, self.env),
            (("local-history", "--json", "--since", "2999-01-01"), {0}, self.env),
            (
                ("receipt-digest", "--sha", "a" * 40),
                {2},
                self.env,
            ),
            (
                (
                    "newest-green",
                    "--no-fetch",
                    "--ledger",
                    str(self.temp / "missing-ledger.jsonl"),
                    "--cache",
                    str(self.temp / "newest-green-cache.json"),
                ),
                # 2 is the explicit UNVERIFIABLE result when a product checkout
                # is uninitialized or its history is shallow; 4 is the normal
                # full-history no-evidence result exercised in the full workspace.
                {2, 4},
                self.env,
            ),
            (
                (
                    "first-bad",
                    "missing.cell",
                    "--no-fetch",
                    "--ledger",
                    str(self.temp / "missing-ledger.jsonl"),
                ),
                {4},
                self.env,
            ),
            (
                ("load-probe", "--sample-seconds", "0.1", "--top", "1"),
                {0, 1},
                self.env,
            ),
            (
                (
                    "runner-health",
                    "--repo",
                    "rrnewton/dev-hermit",
                    "--limit",
                    "1",
                    "--sample",
                    "0",
                    "--gh",
                    "/bin/false",
                ),
                {2},
                self.env,
            ),
        )
        for args, expected, environment in cases:
            with self.subTest(command=args[0]):
                self.run_bounded(*args, expected=expected, env=environment)

    def test_composite_health_returns_explicit_partial_result_on_stalls(self) -> None:
        # Isolate the obligations store: the composite `health` precedence ranks an
        # OPEN speculative-land obligation (print_obligations -> 1) above the stall's
        # degraded-2, so reading the live ignored/ci-hub/obligations.jsonl makes this
        # test fail whenever the real fleet has an open obligation. Point it at the
        # empty per-test store, exactly as the remediation sibling below does.
        env = self.env | {
            "CI_HUB_AGENT_TOOL": str(self.stall),
            "CI_HUB_OBLIGATIONS_STORE": str(self.store),
        }
        result = self.run_bounded(
            "health",
            "--repo",
            "rrnewton/dev-hermit",
            expected={2},
            env=env,
        )
        output = result.stdout + result.stderr
        self.assertIn("DEGRADED", output)
        self.assertIn("UNAVAILABLE", output)
        self.assertGreaterEqual(output.count("PARTIAL RESULT"), 2)

    def test_composite_health_surfaces_unacknowledged_remediation(self) -> None:
        self.store.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "obligation_id": "owed-remediation",
                    "repo": "rrnewton/hermit",
                    "landed_sha": "a" * 40,
                    "opened_at": "2026-08-03T00:00:00Z",
                    "overall_state": "remediation_required",
                    "local": {"state": "red"},
                    "github": {"state": "running"},
                    "recommendation": {"action": "revert"},
                    "remediation": {
                        "state": "triggered",
                        "dispatch": {"state": "sent_unacknowledged"},
                    },
                }
            )
            + "\n"
        )
        env = self.env | {
            "CI_HUB_AGENT_TOOL": str(self.stall),
            "CI_HUB_OBLIGATIONS_STORE": str(self.store),
        }
        result = self.run_bounded("health", expected={2}, env=env)
        output = result.stdout + result.stderr
        self.assertIn("Speculative-land obligations: REMEDIATION REQUIRED", output)
        self.assertIn("dispatch=sent_unacknowledged", output)

    def test_land_lock_full_protocol_is_bounded(self) -> None:
        env = self.env | {"CI_HUB_LANDING_LOCK": str(self.temp / "landing.lock")}
        self.assertIn("FREE", self.run_bounded("land-lock", "status", env=env).stdout)
        self.run_bounded(
            "land-lock",
            "acquire",
            "--agent",
            "ci-shard",
            "--pr",
            "test",
            "--wait",
            "0",
            "--hold",
            "30",
            env=env,
        )
        self.run_bounded(
            "land-lock",
            "renew",
            "--agent",
            "ci-shard",
            "--hold",
            "60",
            env=env,
        )
        self.assertIn(
            "ci-shard", self.run_bounded("land-lock", "status", env=env).stdout
        )
        self.run_bounded("land-lock", "release", "--agent", "ci-shard", env=env)
        self.run_bounded(
            "land-lock",
            "run",
            "--agent",
            "ci-shard-run",
            "--pr",
            "test-run",
            "--wait",
            "0",
            "--hold",
            "30",
            "--",
            "/bin/true",
            env=env,
        )
        self.assertIn("FREE", self.run_bounded("land-lock", "status", env=env).stdout)
        # A wedged land subtree must NOT hold the lock forever (the head-of-line
        # starvation bug): --child-deadline hard-kills the child, exits 124, and
        # RELEASES the lock so the FIFO can proceed -- all well within the wall.
        deadline_run = self.run_bounded(
            "land-lock",
            "run",
            "--agent",
            "ci-shard-wedged",
            "--pr",
            "test-wedged",
            "--wait",
            "0",
            "--hold",
            "30",
            "--child-deadline",
            "1",
            "--",
            "/bin/sleep",
            "30",
            expected={124},
            env=env,
        )
        self.assertIn("ABANDON", deadline_run.stdout + deadline_run.stderr)
        self.assertIn("FREE", self.run_bounded("land-lock", "status", env=env).stdout)
        # The old behavior could be restored with --child-deadline 0. Reject it
        # before acquiring so no caller can create an unbounded FIFO holder.
        unbounded = self.run_bounded(
            "land-lock",
            "run",
            "--agent",
            "ci-shard-unbounded",
            "--pr",
            "test-unbounded",
            "--wait",
            "0",
            "--hold",
            "30",
            "--child-deadline",
            "0",
            "--",
            "/bin/true",
            expected={2},
            env=env,
        )
        self.assertIn("must be positive", unbounded.stdout + unbounded.stderr)
        self.assertIn("FREE", self.run_bounded("land-lock", "status", env=env).stdout)

    def test_cleanup_authority_runtime_files_have_exact_ignore_brackets(self) -> None:
        for runtime_path in (
            ".landing-lock.cleanup-required",
            ".landing-lock.cleanup-required.tmp-123",
            ".validate-lock.cleanup-required",
            ".validate-lock.cleanup-required.tmp-123",
        ):
            ignored = subprocess.run(
                ["git", "check-ignore", "--quiet", "--", runtime_path],
                cwd=ROOT,
                check=False,
                timeout=5,
            )
            self.assertEqual(ignored.returncode, 0, runtime_path)
        for durable_near_miss in (
            ".landing-lock.cleanup-required.backup",
            ".validate-lock.cleanup-required.tmp",
        ):
            visible = subprocess.run(
                ["git", "check-ignore", "--quiet", "--", durable_near_miss],
                cwd=ROOT,
                check=False,
                timeout=5,
            )
            self.assertEqual(visible.returncode, 1, durable_near_miss)

    def test_land_lock_quarantines_a_killed_supervisor_and_refuses_overlap(
        self,
    ) -> None:
        lock = self.temp / "killed-supervisor.lock"
        owner = Path(f"{lock}.owner")
        child_pid_file = self.temp / "land-child.pid"
        child = self._script(
            "long-land",
            f"printf '%s\\n' \"$$\" > {child_pid_file!s}; exec sleep 30",
        )
        env = self.env | {"CI_HUB_LANDING_LOCK": str(lock)}
        process = subprocess.Popen(
            [
                str(CI_HUB),
                "land-lock",
                "run",
                "--agent",
                "killed-lander",
                "--pr",
                "test-killed",
                "--wait",
                "0",
                "--hold",
                "300",
                "--child-deadline",
                "60",
                "--",
                str(child),
            ],
            cwd=ROOT,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
            preexec_fn=_limit_cpu,
        )
        child_pid: int | None = None
        try:
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if owner.exists() and child_pid_file.exists():
                    break
                time.sleep(0.05)
            self.assertTrue(owner.exists(), "supervised lock did not record its owner")
            self.assertTrue(child_pid_file.exists(), "supervised child did not start")
            child_pid = int(child_pid_file.read_text().strip())
            owner_pid = int(
                next(
                    line.removeprefix("pid=")
                    for line in owner.read_text().splitlines()
                    if line.startswith("pid=")
                )
            )
            live = self.run_bounded("land-lock", "status", env=env)
            self.assertIn("lock_holder_process=alive", live.stdout)

            os.kill(owner_pid, signal.SIGKILL)
            process.wait(timeout=5)

            quarantined = self.run_bounded("land-lock", "status", env=env)
            self.assertIn("QUARANTINED (cleanup active)", quarantined.stdout)
            self.assertIn("phase=published", quarantined.stdout)
            self.assertIn("lock_holder_process=exited:", quarantined.stdout)
            self.assertNotIn("owner_process=dead:", quarantined.stdout)
            refused = self.run_bounded(
                "land-lock",
                "acquire",
                "--agent",
                "replacement-lander",
                "--pr",
                "replacement",
                "--wait",
                "0",
                "--hold",
                "30",
                expected={3},
                env=env,
            )
            self.assertIn(
                "cleanup quarantine",
                (refused.stdout + refused.stderr).lower(),
            )
            active_reclaim = self.run_bounded(
                "land-lock", "reclaim-dead", expected={3}, env=env
            )
            self.assertIn(
                "cannot reclaim lease",
                (active_reclaim.stdout + active_reclaim.stderr).lower(),
            )

            os.killpg(child_pid, signal.SIGKILL)
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline and Path(f"/proc/{child_pid}").exists():
                time.sleep(0.05)
            self.assertFalse(
                Path(f"/proc/{child_pid}").exists(),
                "recorded payload identity survived exact group kill",
            )
            child_pid = None
            uncensused = self.run_bounded("land-lock", "status", env=env)
            self.assertIn(
                "QUARANTINED (payload domain lacks final census)",
                uncensused.stdout,
            )
            # Leader/group absence is necessary but not sufficient after the
            # supervisor died before its residual census: an escaped descendant
            # may be unrecorded. Same-boot explicit recovery therefore remains
            # refused; a prior-boot authority is the stronger recovery proof.
            absent_reclaim = self.run_bounded(
                "land-lock", "reclaim-dead", expected={3}, env=env
            )
            self.assertIn(
                "cannot reclaim lease",
                (absent_reclaim.stdout + absent_reclaim.stderr).lower(),
            )
        finally:
            if process.poll() is None:
                process.kill()
            if child_pid is not None:
                try:
                    os.killpg(child_pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            process.communicate(timeout=5)

    def test_shared_lander_preserves_recoverable_arm_and_durable_abandonment(
        self,
    ) -> None:
        front_door = (ROOT / "ci-hub/ci-hub.rs").read_text()
        self.assertTrue(
            front_door.startswith("#!/usr/bin/env -S rust-script --force\n")
        )
        script = (ROOT / "ci-hub/landing/land-pr.sh").read_text()
        detached = script.index('nohup setsid "$0"')
        inherit = script.index('"$ROOT/ci-hub/ci-hub" inherit-obligations')
        prepare = script.index('"$ROOT/ci-hub/remediation/land_and_arm.py" prepare')
        acquire = script.index('"$ROOT/ci-hub/ci-hub" land-lock run')
        receipt = script.index(
            'receipt_detail=$(validation_landability "$HEAD" "$current_base"'
        )
        merge = script.index('gh pr merge "$PR"')
        ancestry = script.index("merge-base --is-ancestor", merge)
        inner_marker = script.index("# --- inner: we now hold the land-lock")
        complete_command = '"$ROOT/ci-hub/remediation/land_and_arm.py" complete'
        complete = script.index(complete_command)
        lock_result = script.index("rc=$?", acquire)
        self.assertLess(detached, inherit)
        self.assertLess(inherit, prepare)
        self.assertLess(prepare, acquire)
        self.assertLess(acquire, lock_result)
        self.assertLess(lock_result, complete)
        self.assertLess(complete, inner_marker)
        self.assertNotIn(complete_command, script[inner_marker:])
        self.assertLess(inner_marker, receipt)
        self.assertLess(receipt, merge)
        self.assertLess(acquire, merge)
        self.assertLess(merge, ancestry)
        self.assertIn("MERGED UNDER LOCK:", script[ancestry:])
        self.assertIn("124) comment_abandon", script)
        self.assertIn("publish_landing_comment abandoned", script)
        self.assertIn("CI_HUB_LANDING_LOG_DIR", script)
        self.assertIn("DETACHED LAND: pid=%s log=%s", script)
        self.assertIn("GATE_DEADLINE=1080", script)
        self.assertIn("CHILD_DEADLINE=$((GATE_DEADLINE * 2))", script)
        self.assertIn("child deadline must be greater than gate deadline", script)
        self.assertIn("actions/workflows/merge-gate.yml/runs?head_sha=$HEAD", script)
        self.assertIn("--select-latest-run --head-sha \"$HEAD\"", script)
        self.assertNotIn(
            'gh pr view "$PR" -R "$R" --json statusCheckRollup', script
        )
        self.assertIn("merge-gate diagnostic only", script)
        self.assertIn('check_outcome=$(python3 "$outcome_helper"', script)
        self.assertNotIn("gh workflow run merge-gate.yml", script)
        self.assertNotIn("merge-gate remained NO_RESULT", script)
        self.assertIn("validation_landability() {", script)
        self.assertIn('python3 "$SCRIPT_DIR/rebase_wrapper.py" authorize', script)
        self.assertIn("authority_mode=local-only", script)
        self.assertNotIn("authority_mode=hosted-only", script)
        self.assertNotIn("authority_mode=(--hosted-only)", script)
        self.assertNotIn("producer-changing PR requires --no-rebase", script)
        self.assertNotIn('scripts/validate.rs | grep -Fxq scripts/validate.rs', script)
        self.assertNotIn("producer_exception", script)
        self.assertIn(
            "A scripts/validate.rs change still uses\n"
            "# the same local ledger authority",
            script,
        )
        self.assertNotIn("local-validation-eligibility.sh", script)
        self.assertIn("apply-local-label --pr", script)
        self.assertIn('--comments "$receipt_comments"', script)
        self.assertIn('--current-reverie-base "$current_reverie_base"', script)
        self.assertIn('--reverie-checkout "$ROOT/reverie"', script)
        self.assertIn('rm -f -- "$receipt_comments"', script)
        self.assertIn('--match-head-commit "$HEAD"', script)
        self.assertNotIn(
            'gh pr edit "$PR" -R "$R" --add-label locally-validated', script
        )
        authority = (
            ROOT / "ci-hub/landing/exact-head-validation-authority.sh"
        ).read_text()
        self.assertIn("ci-hub/validation/verify_receipt.sh", authority)
        self.assertIn("validate-status --sha", authority)
        self.assertIn("hosted-status --repo", authority)

        plugin = (ROOT / ".orc/plugins/hermit-dev/index.ts").read_text()
        heartbeat = plugin[plugin.index("speculativeLandRemediationHeartbeat") :]
        wake = heartbeat.index("await orc.sendWakeup(")
        record = heartbeat.index("hermitSpeculativeLandWakeSent", wake)
        self.assertLess(wake, record)
        self.assertIn("sent but not yet acknowledged", plugin)

    def test_lander_ignores_unbacked_label_cache(self) -> None:
        result = subprocess.run(
            [str(ROOT / "ci-hub/landing/test-local-validation-eligibility.sh")],
            cwd=ROOT,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=ELIGIBILITY_BRACKET_WALL_SECONDS,
        )
        self.assertIn("unbacked label rejected 2/2", result.stdout)
        self.assertIn("validated head admitted 2/2", result.stdout)
        # Both legs must be present AND non-inert: the negative leg must refuse
        # every planted case and the positive leg must still admit a genuinely
        # ledger-backed head. Asserting the summary line alone would pass on a
        # harness that silently stopped executing cases.
        self.assertIn("NEGATIVE refusals: 30/30", result.stdout)
        self.assertIn("POSITIVE admissions: 4/4", result.stdout)
        self.assertIn("0 invocations", result.stdout)

    def test_lander_no_rebase_is_opt_in_and_removes_only_the_mutation(self) -> None:
        """`--no-rebase` skips step 2 alone; the default path and every check stay.

        Bracketed on both sides: the flag must fire when asked for, and must be
        inert when not. A flag that silently became the default would delete the
        rebase for other teams, which is the blast radius this guards.
        """
        script = (ROOT / "ci-hub/landing/land-pr.sh").read_text()
        guard = script.index('if [ "$NO_REBASE" -eq 1 ]; then\n  say "no-rebase:')
        union_branch = script.index('elif [ "$UNION" -eq 1 ]; then')
        rebase = script.index("rebase origin/main >/dev/null")
        force_push = script.index("push -q --force-with-lease")
        merge = script.index('gh pr merge "$PR"')
        # The skip guard dominates BOTH rewrite drivers, and the rewrite commands
        # remain reachable only below it (i.e. only on the default path).
        self.assertLess(guard, union_branch)
        self.assertLess(guard, rebase)
        self.assertLess(rebase, force_push)
        self.assertLess(force_push, merge)
        # --union is itself a rewrite driver, so the combination is refused
        # rather than resolved to one of the two.
        self.assertIn("--union and --no-rebase are mutually exclusive", script)
        # Carried across BOTH re-execs; a flag lost at the detach or lock
        # boundary would silently rebase in the process that actually lands.
        self.assertIn('[ "$NO_REBASE" -eq 1 ] && detached_args+=(--no-rebase)', script)
        self.assertIn('[ "$NO_REBASE" -eq 1 ] && args+=(--no-rebase)', script)
        # The rationale must stay re-verifiable, not merely asserted.
        self.assertIn("strict_required_status_checks_policy", script)

        def parse(*extra: str) -> str:
            env = dict(os.environ, CI_HUB_DOCS_PARSE_ONLY="1")
            return subprocess.run(
                [str(ROOT / "ci-hub/landing/land-pr.sh"), "123", "b", *extra],
                cwd=ROOT,
                env=env,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                timeout=WALL_SECONDS,
            )

        positive = parse("--no-rebase")
        self.assertEqual(positive.returncode, 0)
        self.assertIn("no_rebase=1", positive.stdout)
        default = parse()
        self.assertEqual(default.returncode, 0)
        self.assertIn("no_rebase=0", default.stdout)
        both = parse("--union", "--no-rebase")
        self.assertEqual(both.returncode, 2)
        self.assertIn("mutually exclusive", both.stdout)

    def test_lander_exact_head_authority_brackets_both_positive_paths(self) -> None:
        result = subprocess.run(
            [
                str(
                    ROOT
                    / "ci-hub/landing/test-exact-head-validation-authority.sh"
                )
            ],
            cwd=ROOT,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=WALL_SECONDS,
        )
        self.assertIn("local-only positive=1", result.stdout)
        self.assertIn(
            "local-only negatives=3",
            result.stdout,
        )
        self.assertIn("hosted-only positive=1", result.stdout)
        self.assertIn("hosted-only negatives=4", result.stdout)

    def test_gate_rotation_brackets_sanctioned_composition(self) -> None:
        result = subprocess.run(
            [
                sys.executable,
                str(ROOT / "ci-hub/landing/test_gate_definition_rotation.py"),
            ],
            cwd=ROOT,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=RECEIPT_BRACKET_WALL_SECONDS,
        )
        self.assertIn("nine_classes=9/9", result.stdout)
        self.assertIn("mutation_refusals=9/9", result.stdout)
        self.assertIn("public_e2e=1/1", result.stdout)
        self.assertIn("exact_events=10/10", result.stdout)
        self.assertIn("direct_inner_refused=2/2", result.stdout)
        self.assertIn("entry_points=3/3", result.stdout)

    def test_skill_denylist_guards_validate_first_landing_authority(self) -> None:
        denylist = (
            ROOT / "ci-hub/health/skill-contradiction-denylist.txt"
        ).read_text().lower()
        guarded_claims = (
            ("local-or-hosted",),
            ("interchangeable positives",),
            ("required merge-gate",),
            ("red from either authority",),
        )
        skill_bodies = {
            path: path.read_text().lower()
            for path in (ROOT / ".claude/skills").rglob("SKILL.md")
        }
        for needles in guarded_claims:
            with self.subTest(needles=needles):
                self.assertIn("+".join(needles), denylist)
                for path, body in skill_bodies.items():
                    self.assertFalse(
                        all(needle in body for needle in needles),
                        f"{path} resurrects stale landing authority: {needles}",
                    )

    def test_lander_receipt_authorization_has_both_controls_and_cleans_plant(
        self,
    ) -> None:
        verifier = (ROOT / "ci-hub/validation/verify_receipt.sh").read_text()
        self.assertEqual(
            verifier.count("receipt-digest --sha \"$sha\""),
            1,
            "candidate verification must use one Rust digest/qualification authority",
        )
        self.assertIn("--require-qualifying", verifier)
        self.assertNotIn("qualifying_receipt.py", verifier)
        result = subprocess.run(
            [str(ROOT / "ci-hub/validation/test_verify_receipt.sh")],
            cwd=ROOT,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=RECEIPT_BRACKET_WALL_SECONDS,
        )
        self.assertIn("2/2 legitimate exact-head landing receipts accepted", result.stdout)
        self.assertIn("forged", result.stdout)
        self.assertIn("fixture plant deleted cleanly", result.stdout)

    def test_merge_gate_selector_includes_exact_head_dispatch(self) -> None:
        """Plant the run shape PR statusCheckRollup omitted for PR #1219."""
        sha = "a" * 40
        fixture = {
            "workflow_runs": [
                {
                    "head_sha": sha,
                    "event": "pull_request",
                    "status": "completed",
                    "conclusion": "failure",
                    "id": 10,
                    "html_url": "https://example.invalid/runs/10",
                    "created_at": "2026-08-04T02:48:24Z",
                },
                {
                    "head_sha": sha,
                    "event": "workflow_dispatch",
                    "status": "completed",
                    "conclusion": "success",
                    "id": 11,
                    "html_url": "https://example.invalid/runs/11",
                    "created_at": "2026-08-04T02:48:24Z",
                },
                {
                    "head_sha": "b" * 40,
                    "event": "workflow_dispatch",
                    "status": "completed",
                    "conclusion": "failure",
                    "id": 12,
                    "html_url": "https://example.invalid/runs/12",
                    "created_at": "2026-08-04T02:49:00Z",
                },
                {
                    "head_sha": sha,
                    "event": "workflow_run",
                    "status": "completed",
                    "conclusion": "success",
                    "id": 13,
                    "html_url": "https://example.invalid/runs/13",
                    "created_at": "2026-08-04T02:50:00Z",
                },
            ]
        }
        selected = subprocess.run(
            [
                sys.executable,
                str(ROOT / "ci-hub/check_outcome.py"),
                "--select-latest-run",
                "--head-sha",
                sha,
                "--event",
                "pull_request",
                "--event",
                "workflow_dispatch",
            ],
            input=json.dumps(fixture),
            text=True,
            capture_output=True,
            check=True,
            timeout=5,
        )
        result = subprocess.run(
            [
                "jq",
                "-r",
                "-f",
                str(ROOT / "ci-hub/landing/merge-gate-status.jq"),
            ],
            input=selected.stdout,
            text=True,
            capture_output=True,
            check=True,
            timeout=5,
        )
        self.assertEqual(
            result.stdout.strip(),
            "COMPLETED\tSUCCESS\tworkflow_dispatch\t11\t"
            "https://example.invalid/runs/11\t2026-08-04T02:48:24Z",
        )


if __name__ == "__main__":
    unittest.main()
