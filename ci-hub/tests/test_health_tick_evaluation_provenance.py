#!/usr/bin/env python3
"""Regression tests for visible health-tick evaluation provenance."""

from __future__ import annotations

import json
import hashlib
import datetime as dt
import os
import shutil
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parents[2]
HEALTH_TICK = ROOT / "ci-hub" / "bin" / "health-tick"
OPERATIONAL_HEALTH = ROOT / "ci-hub" / "health" / "operational_health.py"
TICK_CONFIG = ROOT / "ci-hub" / "health" / "tick-hub.yaml"
ORC_PLUGIN = ROOT / ".orc" / "plugins" / "hermit-dev" / "index.ts"
SYSTEMD_SERVICE = ROOT / "scripts" / "systemd" / "hermit-health-tick.service"
OBSERVED_AT = "2026-08-20T19:57:31Z"


class HealthTickEvaluationProvenanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="health-tick-provenance-")
        self.addCleanup(self.temporary.cleanup)
        self.repo = Path(self.temporary.name)
        self.records = Path(f"{self.repo}-records")
        self.records.mkdir()
        self.addCleanup(shutil.rmtree, self.records)
        (self.repo / "ci-hub" / "bin").mkdir(parents=True)
        shutil.copy2(HEALTH_TICK, self.repo / "ci-hub" / "bin" / "health-tick")

        agent_tool = self.repo / "ci-hub" / "bin" / "agent-tool"
        agent_tool.write_text(
            textwrap.dedent(
                """\
                #!/usr/bin/env bash
                set -euo pipefail
                if [[ -n ${FAKE_AGENT_TOOL_RECORD:-} ]]; then
                    printf 'parent=%s\\ntool=%s\\nops_state=%s\\npwd=%s\\nargs=%s\\n' \\
                        "$DEV_HERMIT_PARENT" "${DEV_HERMIT_TOOL_ROOT:-<unset>}" \\
                        "$DEV_HERMIT_OPS_STATE" "$PWD" "$*" \\
                        > "$FAKE_AGENT_TOOL_RECORD"
                fi
                printf 'fake-agent stderr\\n' >&2
                if [[ ${FAKE_TICK_MODE:-warnings} == real-gate ]]; then
                    config=
                    shift 2
                    while (( $# )); do
                        if [[ $1 == --config ]]; then
                            config=$2
                            break
                        fi
                        shift
                    done
                    [[ -n $config ]]
                    command=$(python3 -c 'import sys, yaml; print(yaml.safe_load(open(sys.argv[1]))["checks"][0]["gate"]["cmd"])' "$config")
                    exec bash -c "$command"
                fi
                case ${FAKE_TICK_MODE:-warnings} in
                    warnings)
                        printf 'HEALTH: evaluated\\n'
                        printf 'ACTION: investigate component=pin title="Reverie pin is stale"\\n'
                        printf 'NOTE: pin lag is within the note band\\n'
                        printf 'NOTE: emitted 1 instruction(s) this tick\\n'
                        ;;
                    quiet)
                        printf 'HEALTH: quiet evaluation\\n'
                        printf 'NOTE: emitted 0 instruction(s) this tick\\n'
                        ;;
                    note-only)
                        printf 'NOTE: ops-state enabled label=fixture tick_frequency_min=60 flags=0\\n'
                        ;;
                    clean)
                        printf 'CLEAN: fixture_gate ran and found nothing to report\\n'
                        ;;
                    clean-complete)
                        printf 'CLEAN: fixture_gate ran and found nothing to report\\n'
                        printf 'NOTE: emitted 0 instruction(s) this tick\\n'
                        ;;
                    *) exit 64 ;;
                esac
                exit "${FAKE_TICK_EXIT:-0}"
                """
            )
        )
        agent_tool.chmod(0o755)

        health = self.repo / "ci-hub" / "health"
        health.mkdir()
        operational_health = health / "operational_health.py"
        operational_health.write_text(
            textwrap.dedent(
                """\
                import os
                import pathlib
                import sys

                if len(sys.argv) != 4 or sys.argv[1:3] != ["cache-agents", "--snapshot-file"]:
                    raise SystemExit(64)
                record = os.environ.get("FAKE_CACHE_RECORD")
                snapshot = os.environ.get("HERMIT_AGENT_SNAPSHOT_JSON", "missing")
                if record:
                    pathlib.Path(record).write_text(snapshot)
                status = int(os.environ.get("FAKE_CACHE_EXIT", "0"))
                if status == 0:
                    target = pathlib.Path(sys.argv[3])
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_text(snapshot)
                raise SystemExit(status)
                """
            )
        )

        gate_probe = health / "gate_probe.py"
        gate_probe.write_text(
            textwrap.dedent(
                """\
                import argparse
                from pathlib import Path

                parser = argparse.ArgumentParser()
                parser.add_argument("--state-marker", type=Path, required=True)
                parser.add_argument("--agent-snapshot", type=Path, required=True)
                args = parser.parse_args()
                args.state_marker.write_text("tool-root gate ran\\n")
                print(
                    f"HEALTH: real-gate cwd={Path.cwd()} "
                    f"snapshot={args.agent_snapshot.read_text()}"
                )
                """
            )
        )
        (health / "tick-hub.yaml").write_text(
            textwrap.dedent(
                """\
                checks:
                  - name: fixture_gate
                    gate:
                      cmd: python3 ci-hub/health/gate_probe.py --state-marker "$DEV_HERMIT_PARENT/gate-state.txt" --agent-snapshot "$DEV_HERMIT_PARENT/ignored/ci-hub/agent-snapshot.json"
                """
            )
        )

        self.fake_bin = self.repo / "fake-bin"
        self.fake_bin.mkdir()
        fake_date = self.fake_bin / "date"
        fake_date.write_text(
            textwrap.dedent(
                f"""\
                #!/usr/bin/env bash
                set -euo pipefail
                [[ $* == '-u +%Y-%m-%dT%H:%M:%SZ' ]]
                printf '{OBSERVED_AT}\\n'
                """
            )
        )
        fake_date.chmod(0o755)

        tool_cost = self.repo / "ci-hub" / "bin" / "tool-cost"
        tool_cost.write_text(
            textwrap.dedent(
                """\
                #!/usr/bin/env bash
                set -euo pipefail
                printf '%s\\n' "$*" > "$FAKE_TOOL_COST_RECORD"
                while [[ $1 != -- ]]; do shift; done
                shift
                exec "$@"
                """
            )
        )
        tool_cost.chmod(0o755)

        self._git("init", "-q")
        self._git("add", "ci-hub")
        self._git(
            "-c",
            "user.name=Health Tick Test",
            "-c",
            "user.email=health-tick-test@example.invalid",
            "commit",
            "-qm",
            "fixture",
        )
        self.input_sha = self._git("rev-parse", "HEAD").stdout.strip()

    def _git(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["git", *args],
            cwd=self.repo,
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def _run(
        self,
        *,
        mode: str,
        exit_status: int = 0,
        snapshot: str | None = None,
        cache_exit: int = 0,
        state_root: Path | None = None,
        use_tool_cost: bool = False,
        explicit_tool_root: bool = False,
    ) -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        environment.pop("HERMIT_AGENT_SNAPSHOT_JSON", None)
        environment.pop("DEV_HERMIT_PARENT", None)
        environment.pop("DEV_HERMIT_TOOL_ROOT", None)
        cache_record = self.records / "cache-record.json"
        agent_tool_record = self.records / "agent-tool-record"
        tool_cost_record = self.records / "tool-cost-record"
        environment.update(
            {
                "CI_HUB_TOOL_COST_ACTIVE": "1",
                "FAKE_TICK_MODE": mode,
                "FAKE_TICK_EXIT": str(exit_status),
                "FAKE_CACHE_EXIT": str(cache_exit),
                "FAKE_CACHE_RECORD": str(cache_record),
                "FAKE_AGENT_TOOL_RECORD": str(agent_tool_record),
                "FAKE_TOOL_COST_RECORD": str(tool_cost_record),
                "PATH": f"{self.fake_bin}:{environment['PATH']}",
            }
        )
        if snapshot is not None:
            environment["HERMIT_AGENT_SNAPSHOT_JSON"] = snapshot
        if state_root is not None:
            environment["DEV_HERMIT_PARENT"] = str(state_root)
        if explicit_tool_root:
            environment["DEV_HERMIT_TOOL_ROOT"] = str(self.repo)
        if use_tool_cost:
            environment.pop("CI_HUB_TOOL_COST_ACTIVE", None)
        return subprocess.run(
            [str(self.repo / "ci-hub" / "bin" / "health-tick")],
            cwd=self.repo,
            check=False,
            text=True,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def test_action_and_note_are_visibly_dated_without_suppression(self) -> None:
        result = self._run(mode="warnings", exit_status=23)
        provenance = f"observed_at={OBSERVED_AT} input_sha={self.input_sha}"

        self.assertEqual(23, result.returncode)
        self.assertEqual("fake-agent stderr\n", result.stderr)
        self.assertEqual(
            [
                "HEALTH: evaluated",
                "ACTION: investigate component=pin "
                f'title="Reverie pin is stale {provenance}"',
                f"NOTE: pin lag is within the note band {provenance}",
                f"NOTE: emitted 1 instruction(s) this tick {provenance}",
            ],
            result.stdout.splitlines(),
        )
        self.assertEqual(1, result.stdout.count("ACTION:"))
        self.assertEqual(2, result.stdout.count("NOTE:"))

    def test_quiet_evaluation_remains_quiet(self) -> None:
        result = self._run(mode="quiet")

        self.assertEqual(0, result.returncode)
        self.assertEqual("fake-agent stderr\n", result.stderr)
        self.assertEqual(
            "HEALTH: quiet evaluation\n"
            f"NOTE: emitted 0 instruction(s) this tick "
            f"observed_at={OBSERVED_AT} input_sha={self.input_sha}\n",
            result.stdout,
        )
        self.assertNotIn("ACTION:", result.stdout)

    def test_failed_producer_names_its_exit_and_every_unobserved_check(self) -> None:
        (self.repo / "ci-hub" / "health" / "tick-hub.yaml").write_text(
            "checks:\n  - name: first\n  - name: second\n"
        )
        result = self._run(mode="does-not-run", exit_status=64)

        self.assertEqual(64, result.returncode)
        self.assertIn(
            "ERROR: tick exited 64 before completion; complete evaluation of "
            "2 configured check(s) was not established",
            result.stdout,
        )
        action = next(line for line in result.stdout.splitlines() if line.startswith("ACTION:"))
        self.assertIn(
            "detail=\"producer exit 64; complete evaluation of 2 configured "
            "check(s) was not established",
            action,
        )
        self.assertIn("the tick exited 64 before completing all gates", action)
        self.assertNotIn("producer exit ;", action)
        self.assertEqual(1, result.stdout.count("ACTION:"))

    def test_failed_producer_never_reports_zero_when_config_has_no_checks(self) -> None:
        (self.repo / "ci-hub" / "health" / "tick-hub.yaml").write_text("checks:\n")
        result = self._run(mode="does-not-run", exit_status=64)

        self.assertEqual(64, result.returncode)
        self.assertIn(
            "complete evaluation of unknown configured check(s) was not established",
            result.stdout,
        )
        self.assertNotIn("0 configured check(s)", result.stdout)

    def test_ops_state_note_does_not_hide_a_producer_that_never_reached_gates(
        self,
    ) -> None:
        (self.repo / "ci-hub" / "health" / "tick-hub.yaml").write_text(
            "checks:\n  - name: first\n  - name: second\n"
        )
        result = self._run(mode="note-only", exit_status=64)

        self.assertEqual(64, result.returncode)
        self.assertIn("NOTE: ops-state enabled", result.stdout)
        self.assertIn(
            "ERROR: tick exited 64 before completion; complete evaluation of "
            "2 configured check(s) was not established",
            result.stdout,
        )
        self.assertIn("outcome=NO-SIGNAL gate=ALL", result.stdout)

    def test_partial_clean_reminder_does_not_hide_incomplete_evaluation(self) -> None:
        result = self._run(mode="clean", exit_status=64)

        self.assertEqual(64, result.returncode)
        self.assertIn(
            "CLEAN: fixture_gate ran and found nothing to report", result.stdout
        )
        self.assertIn("gate=ALL", result.stdout)
        self.assertIn("before completing all gates", result.stdout)
        self.assertIn("partial verdicts above remain valid", result.stdout)

    def test_completion_marker_prevents_false_incomplete_alarm(self) -> None:
        result = self._run(mode="clean-complete", exit_status=64)

        self.assertEqual(64, result.returncode)
        self.assertIn(
            "CLEAN: fixture_gate ran and found nothing to report", result.stdout
        )
        self.assertIn("NOTE: emitted 0 instruction(s) this tick", result.stdout)
        self.assertNotIn("gate=ALL", result.stdout)
        self.assertNotIn("before completing all gates", result.stdout)

    def test_scheduled_snapshot_is_passed_to_cache_agents(self) -> None:
        snapshot = '[{"name":"worker","tmux_pane_id":"%7"}]'
        result = self._run(mode="quiet", snapshot=snapshot)

        self.assertEqual(0, result.returncode)
        self.assertEqual(snapshot, (self.records / "cache-record.json").read_text())

    def test_a_rejected_snapshot_does_not_hide_every_other_gate(self) -> None:
        result = self._run(mode="quiet", snapshot="[]", cache_exit=1)

        self.assertEqual(1, result.returncode)
        self.assertIn("HEALTH: quiet evaluation\n", result.stdout)
        self.assertIn("NOTE: emitted 0 instruction(s) this tick", result.stdout)
        self.assertIn("could not cache the ORC agent snapshot", result.stderr)

    def test_tool_cost_path_runs_real_relative_gate_only_from_tool_root(self) -> None:
        state_root = Path(f"{self.repo}-state-root")
        self.addCleanup(shutil.rmtree, state_root)
        poison = state_root / "ci-hub" / "health" / "gate_probe.py"
        poison.parent.mkdir(parents=True)
        poison.write_text(
            "from pathlib import Path\n"
            f"Path({str(state_root / 'poison-ran')!r}).write_text('poison\\n')\n"
            "raise SystemExit(97)\n",
            encoding="utf-8",
        )
        subprocess.run(["git", "init", "-q"], cwd=state_root, check=True)
        subprocess.run(["git", "add", "."], cwd=state_root, check=True)
        subprocess.run(
            [
                "git",
                "-c",
                "user.name=Health Tick Test",
                "-c",
                "user.email=health-tick-test@example.invalid",
                "commit",
                "-qm",
                "poison state fixture",
            ],
            cwd=state_root,
            check=True,
        )
        snapshot = (
            '[{"name":"worker","status":"working",'
            '"current_task":"fixture","tmux_pane_id":"%7"}]'
        )
        tool_status_before = self._git("status", "--porcelain=v1").stdout

        result = self._run(
            mode="real-gate",
            state_root=state_root,
            snapshot=snapshot,
            use_tool_cost=True,
            explicit_tool_root=True,
        )

        self.assertEqual(0, result.returncode, result.stderr)
        recorded = (self.records / "agent-tool-record").read_text(encoding="utf-8")
        self.assertIn(f"parent={state_root}\n", recorded)
        self.assertIn(f"tool={self.repo}\n", recorded)
        self.assertIn(f"ops_state={state_root}/.ops-state.yaml\n", recorded)
        self.assertIn(f"pwd={self.repo}\n", recorded)
        self.assertIn(f"--config {self.repo}/ci-hub/health/tick-hub.yaml", recorded)
        self.assertIn(f"--state {state_root}/.ops-state.yaml", recorded)
        self.assertIn("--report-pending", recorded)
        self.assertIn(f"HEALTH: real-gate cwd={self.repo}", result.stdout)
        self.assertIn(f"snapshot={snapshot}", result.stdout)
        self.assertEqual(
            (state_root / "ignored" / "ci-hub" / "agent-snapshot.json").read_text(),
            snapshot,
        )
        self.assertEqual(
            (state_root / "gate-state.txt").read_text(), "tool-root gate ran\n"
        )
        self.assertFalse((state_root / "poison-ran").exists())
        self.assertFalse(
            (self.repo / "ignored" / "ci-hub" / "agent-snapshot.json").exists()
        )
        self.assertIn(
            f"-- {self.repo}/ci-hub/bin/health-tick",
            (self.records / "tool-cost-record").read_text(),
        )
        self.assertEqual(tool_status_before, self._git("status", "--porcelain=v1").stdout)

    def test_cache_agents_cli_writes_only_the_explicit_state_destination(self) -> None:
        destination = self.records / "state" / "ignored" / "ci-hub" / "agent-snapshot.json"
        default_destination = ROOT / "ignored" / "ci-hub" / "agent-snapshot.json"
        default_before = (
            (True, default_destination.read_bytes())
            if default_destination.exists()
            else (False, b"")
        )
        status_before = subprocess.run(
            ["git", "status", "--porcelain=v1"],
            cwd=ROOT,
            check=True,
            text=True,
            stdout=subprocess.PIPE,
        ).stdout
        snapshot = (
            '[{"name":"worker","status":"working",'
            '"current_task":"fixture","tmux_pane_id":"%7"}]'
        )
        environment = os.environ.copy()
        environment["HERMIT_AGENT_SNAPSHOT_JSON"] = snapshot
        environment["PYTHONDONTWRITEBYTECODE"] = "1"

        result = subprocess.run(
            [
                "python3",
                str(OPERATIONAL_HEALTH),
                "cache-agents",
                "--snapshot-file",
                str(destination),
            ],
            cwd=self.records,
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=environment,
        )

        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("state=ok", result.stdout)
        self.assertEqual(json.loads(destination.read_text())["agents"], json.loads(snapshot))
        default_after = (
            (True, default_destination.read_bytes())
            if default_destination.exists()
            else (False, b"")
        )
        self.assertEqual(default_before, default_after)
        status_after = subprocess.run(
            ["git", "status", "--porcelain=v1"],
            cwd=ROOT,
            check=True,
            text=True,
            stdout=subprocess.PIPE,
        ).stdout
        self.assertEqual(status_before, status_after)

    def test_production_entrypoint_and_config_keep_state_paths_explicit(self) -> None:
        plugin = ORC_PLUGIN.read_text(encoding="utf-8")
        self.assertNotIn("./scripts/hermit-operational-tick", plugin)
        self.assertNotIn("hermitOperationalTick", plugin)
        self.assertNotIn(
            'HERMIT_AGENT_SNAPSHOT_JSON="$1" ./ci-hub/bin/health-tick', plugin
        )
        self.assertIn('operationalHealthOwner: "hermit-health-tick.timer"', plugin)
        service = SYSTEMD_SERVICE.read_text(encoding="utf-8")
        self.assertIn(
            "ExecStart=%h/.local/libexec/dev-hermit/operational-tool "
            "--run-current-tool --state-root %h/work/dev-hermit "
            "--tool-relative scripts/hermit-operational-tick",
            service,
        )
        self.assertNotIn("ExecStart=%h/work/dev-hermit/ci-hub/bin/health-tick", service)
        health_tick = HEALTH_TICK.read_text(encoding="utf-8")
        self.assertIn(
            'export DEV_HERMIT_OPS_STATE="$STATE_ROOT/.ops-state.yaml"',
            health_tick,
        )

        config = yaml.safe_load(TICK_CONFIG.read_text(encoding="utf-8"))
        commands = {
            reminder["name"]: reminder["gate"]["cmd"]
            for reminder in config["reminders"]
            if "gate" in reminder
        }
        expected = {
            "deleted_tool_absent": '--root "$DEV_HERMIT_PARENT"',
            "wrkslots_audit": (
                '--result "$DEV_HERMIT_PARENT/ignored/ci-hub/'
                'wrkslots-audit/latest.json"'
            ),
            "ledger_spool_backlog": '--root "$DEV_HERMIT_PARENT"',
            "series_spool_backlog": '--root "$DEV_HERMIT_PARENT"',
            "series_quarantine": '--root "$DEV_HERMIT_PARENT"',
            "validate_wall_regression": (
                '--ledger "$DEV_HERMIT_PARENT/ledger" '
                '--repo "$DEV_HERMIT_PARENT/hermit" '
                '--reverie-repo "$DEV_HERMIT_PARENT/reverie"'
            ),
            "validate_wall_series_readiness": (
                '--ledger "$DEV_HERMIT_PARENT/ledger" '
                '--repo "$DEV_HERMIT_PARENT/hermit" '
                '--reverie-repo "$DEV_HERMIT_PARENT/reverie"'
            ),
            "pressure_test": '--state-root "$DEV_HERMIT_PARENT"',
            "ledger_referenced_objects": (
                '--ledger "$DEV_HERMIT_PARENT/ledger" '
                '--root "$DEV_HERMIT_PARENT"'
            ),
            "owner_tooling_directives": (
                '--ledger "$DEV_HERMIT_TOOL_ROOT/ci-hub/directives/ledger.json" '
                '--output "$DEV_HERMIT_PARENT/ignored/ci-hub/directives/latest.json" '
                '--state-root "$DEV_HERMIT_PARENT"'
            ),
            "slot_agent_parity": '--root "$DEV_HERMIT_PARENT"',
            "unpushed_parent_commits": '--root "$DEV_HERMIT_PARENT"',
            "primary_checkout_snapshot": '--state-root "$DEV_HERMIT_PARENT"',
            "pull_request_health": (
                '--census-cache-dir '
                '"$DEV_HERMIT_PARENT/ignored/ci-hub/pr-census-cache"'
            ),
            "ci_lane_saturation": (
                '--store "$DEV_HERMIT_PARENT/ignored/ci-hub/lane-health.jsonl"'
            ),
            "agent_container_lifecycle": (
                '--agents-json '
                '"$DEV_HERMIT_PARENT/ignored/ci-hub/agent-snapshot.json"'
            ),
        }
        for name, fragment in expected.items():
            self.assertIn(fragment, commands[name], name)
        self.assertEqual(
            commands["ledger_publish_sweep"],
            "./ci-hub/ledger/launch_publish_backlog.sh",
        )

        publisher = (ROOT / "ci-hub" / "ledger" / "launch_publish_backlog.sh").read_text()
        self.assertIn('STATE_ROOT=${DEV_HERMIT_PARENT:-$TOOL_ROOT}', publisher)
        self.assertIn('PUBLISH_WORKING_DIRECTORY=$TOOL_ROOT', publisher)
        self.assertIn('--working-directory="$PUBLISH_WORKING_DIRECTORY"', publisher)
        self.assertIn('--setenv="DEV_HERMIT_PARENT=$STATE_ROOT"', publisher)
        self.assertIn('--setenv="DEV_HERMIT_TOOL_ROOT=$TOOL_ROOT"', publisher)
        self.assertIn(
            'PUBLISH_COMMAND=(python3 "$TOOL_ROOT/ci-hub/ledger/publish_backlog.py")',
            publisher,
        )
        self.assertIn(
            '"$OPERATIONAL_TOOL"\n    --run-current-tool', publisher
        )
        self.assertIn(
            '--tool-parent-sha "$DEV_HERMIT_TOOL_PARENT_SHA"', publisher
        )
        self.assertIn(
            '--tool-relative ci-hub/ledger/publish_backlog.py', publisher
        )

    def test_real_production_stateful_commands_leave_tool_root_unchanged(self) -> None:
        state_root = self.records / "production-state-root"
        state_root.mkdir()
        fake_bin = self.records / "production-bin"
        fake_bin.mkdir()
        systemd_argv = self.records / "systemd-argv"

        for name, body in {
            "systemctl": "#!/bin/sh\nexit 3\n",
            "systemd-run": (
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$SYSTEMD_RUN_ARGV_SINK\"\n"
            ),
            "with-proxy": "#!/bin/sh\nexec \"$@\"\n",
            "gh": "#!/bin/sh\nexit 1\n",
        }.items():
            executable = fake_bin / name
            executable.write_text(body)
            executable.chmod(0o755)
        operational_tool = fake_bin / "operational-tool"
        operational_tool.write_text("#!/bin/sh\nexit 0\n")
        operational_tool.chmod(0o755)
        tool_cache = self.records / "operational-tool-cache"
        tool_cache.mkdir()

        hermit_remote = self.records / "hermit-origin.git"
        subprocess.run(
            ["git", "init", "-q", "--bare", str(hermit_remote)], check=True
        )
        for name in ("hermit", "reverie"):
            checkout = state_root / name
            subprocess.run(["git", "init", "-q", str(checkout)], check=True)
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(checkout),
                    "-c",
                    "user.name=Health Tick Test",
                    "-c",
                    "user.email=health-tick-test@example.invalid",
                    "commit",
                    "-qm",
                    "fixture",
                    "--allow-empty",
                ],
                check=True,
            )
        subprocess.run(
            ["git", "-C", str(state_root / "hermit"), "remote", "add", "origin", str(hermit_remote)],
            check=True,
        )
        subprocess.run(
            ["git", "-C", str(state_root / "hermit"), "push", "-qu", "origin", "HEAD:main"],
            check=True,
        )
        hermit_head = subprocess.run(
            ["git", "-C", str(state_root / "hermit"), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        (state_root / "ledger").write_text(
            json.dumps({"commit": hermit_head, "repo": "hermit", "result": "pass"})
            + "\n"
        )

        def snapshot(path: Path):
            if not path.exists():
                return None
            if path.is_file():
                return ("file", hashlib.sha256(path.read_bytes()).hexdigest())
            entries = []
            for child in sorted(path.rglob("*")):
                relative = child.relative_to(path).as_posix()
                if child.is_symlink():
                    entries.append((relative, "symlink", os.readlink(child)))
                elif child.is_file():
                    entries.append(
                        (relative, "file", hashlib.sha256(child.read_bytes()).hexdigest())
                    )
                else:
                    entries.append((relative, "directory"))
            return tuple(entries)

        tool_mutable_paths = (
            ROOT / "ignored" / "ci-hub" / "pressure-test-state.json",
            ROOT / "ignored" / "ci-hub" / "pressure-test.log",
            ROOT / "ignored" / "ci-hub" / "ledger-publisher-clone",
            ROOT / "ignored" / "ci-hub" / "directives" / "latest.json",
        )
        tool_paths_before = {path: snapshot(path) for path in tool_mutable_paths}
        tool_status_before = subprocess.run(
            ["git", "status", "--porcelain=v1", "--untracked-files=all"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        config = yaml.safe_load(TICK_CONFIG.read_text(encoding="utf-8"))
        commands = {
            reminder["name"]: reminder["gate"]["cmd"]
            for reminder in config["reminders"]
            if "gate" in reminder
        }
        environment = os.environ | {
            "DEV_HERMIT_PARENT": str(state_root),
            "DEV_HERMIT_TOOL_ROOT": str(ROOT),
            "DIRECTIVE_TASK_DBS": "",
            "LEDGER_PUBLISH_GH_BIN": str(fake_bin / "gh"),
            "PATH": f"{fake_bin}:{os.environ.get('PATH', '')}",
            "PYTHONDONTWRITEBYTECODE": "1",
            "SYSTEMD_RUN_ARGV_SINK": str(systemd_argv),
        }

        pressure_environment = environment | {
            "DEV_HERMIT_OPERATIONAL_TOOL": str(operational_tool),
            "DEV_HERMIT_TOOL_PARENT_SHA": "b" * 40,
            "DEV_HERMIT_TOOL_CACHE": str(tool_cache),
        }
        pressure = subprocess.run(
            ["bash", "-c", commands["pressure_test"]],
            cwd=ROOT,
            env=pressure_environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(0, pressure.returncode, pressure.stdout + pressure.stderr)
        pressure_argv = systemd_argv.read_text()
        self.assertIn(str(operational_tool), pressure_argv)
        self.assertIn("--run-current-tool", pressure_argv)
        self.assertIn("--tool-parent-sha", pressure_argv)
        self.assertIn("b" * 40, pressure_argv)
        self.assertIn("--cache-root", pressure_argv)
        self.assertIn(str(tool_cache), pressure_argv)
        self.assertIn("--tool-relative", pressure_argv)
        self.assertIn("ci-hub/series/pressure_schedule.py", pressure_argv)
        self.assertIn(f"--working-directory={state_root}", pressure_argv)
        self.assertNotIn(
            str(ROOT / "ci-hub" / "series" / "pressure_schedule.py"),
            pressure_argv,
        )
        self.assertIn(f"DEV_HERMIT_PARENT={state_root}", pressure_argv)
        self.assertIn(f"DEV_HERMIT_TOOL_ROOT={ROOT}", pressure_argv)
        self.assertTrue(
            (state_root / "ignored" / "validate" / "runs" / "hermit-pressure-test.json").is_file()
        )

        publisher = subprocess.run(
            ["bash", "-c", commands["ledger_publish_sweep"]],
            cwd=ROOT,
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(0, publisher.returncode, publisher.stdout + publisher.stderr)
        publisher_argv = systemd_argv.read_text()
        self.assertIn(str(ROOT / "ci-hub" / "ledger" / "publish_backlog.py"), publisher_argv)
        self.assertIn(f"DEV_HERMIT_PARENT={state_root}", publisher_argv)
        self.assertIn(f"DEV_HERMIT_TOOL_ROOT={ROOT}", publisher_argv)
        publish_dry_run = subprocess.run(
            [sys.executable, str(ROOT / "ci-hub" / "ledger" / "publish_backlog.py"), "--dry-run"],
            cwd=ROOT,
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(0, publish_dry_run.returncode, publish_dry_run.stderr)
        self.assertEqual(0, json.loads(publish_dry_run.stdout)["parents_with_backlog"])

        ref_pins = subprocess.run(
            ["bash", "-c", commands["ledger_referenced_objects"]],
            cwd=ROOT,
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(0, ref_pins.returncode, ref_pins.stdout + ref_pins.stderr)
        self.assertIn("hermit=source:1 ledger:1 present:1 pinned:1", ref_pins.stdout)
        pinned = subprocess.run(
            [
                "git",
                "-C",
                str(state_root / "hermit"),
                "rev-parse",
                f"refs/ledger-referenced/{hermit_head}",
            ],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        self.assertEqual(hermit_head, pinned)

        directives = subprocess.run(
            ["bash", "-c", commands["owner_tooling_directives"]],
            cwd=ROOT,
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(0, directives.returncode)
        self.assertTrue(
            (state_root / "ignored" / "ci-hub" / "directives" / "latest.json").is_file()
        )

        self.assertEqual(
            tool_paths_before,
            {path: snapshot(path) for path in tool_mutable_paths},
        )
        self.assertEqual(
            tool_status_before,
            subprocess.run(
                ["git", "status", "--porcelain=v1", "--untracked-files=all"],
                cwd=ROOT,
                check=True,
                capture_output=True,
                text=True,
            ).stdout,
        )

    def test_remaining_production_gates_read_only_the_state_root(self) -> None:
        state_root = self.records / "remaining-state-root"
        state_root.mkdir()
        subprocess.run(["git", "init", "-q"], cwd=state_root, check=True)
        (state_root / "state-anchor.txt").write_text("state\n", encoding="utf-8")
        subprocess.run(["git", "add", "state-anchor.txt"], cwd=state_root, check=True)
        subprocess.run(
            [
                "git",
                "-c",
                "user.name=Health Tick Test",
                "-c",
                "user.email=health-tick-test@example.invalid",
                "commit",
                "-qm",
                "state fixture",
            ],
            cwd=state_root,
            check=True,
        )
        deleted = state_root / "ci-hub" / "bin" / "gh-pr-merge-verified"
        deleted.parent.mkdir(parents=True)
        deleted.write_text("#!/bin/sh\nexit 99\n", encoding="utf-8")
        deleted.chmod(0o755)
        for relative in (
            "ignored/ci-hub/validate-ledger-spool/stale.jsonl",
            "ignored/ci-hub/series-spool/stale.jsonl",
            "ignored/ci-hub/series-quarantine/stale.jsonl",
        ):
            entry = state_root / relative
            entry.parent.mkdir(parents=True, exist_ok=True)
            entry.write_text("{}\n", encoding="utf-8")
            os.utime(entry, (1, 1))
        (state_root / "ledger").write_text("", encoding="utf-8")
        (state_root / "hermit").mkdir()
        (state_root / "reverie").mkdir()
        (state_root / ".ops-state.yaml").write_text(
            "flags:\n  github_actions_enabled: false\n", encoding="utf-8"
        )

        provenance = {
            "parent_sha": "a" * 40,
            "hermit_sha": "b" * 40,
            "agent_utils_sha": "c" * 40,
        }
        audit_result = state_root / "ignored/ci-hub/wrkslots-audit/latest.json"
        audit_result.parent.mkdir(parents=True)
        observed_epoch = time.time()
        started_epoch = observed_epoch - 1
        audit_result.write_text(
            json.dumps(
                {
                    "schema": "wrkslots-audit-result/v1",
                    "started_at": dt.datetime.fromtimestamp(
                        started_epoch, dt.timezone.utc
                    ).isoformat().replace("+00:00", "Z"),
                    "started_at_epoch_seconds": started_epoch,
                    "observed_at": dt.datetime.fromtimestamp(
                        observed_epoch, dt.timezone.utc
                    ).isoformat().replace("+00:00", "Z"),
                    "observed_at_epoch_seconds": observed_epoch,
                    "producer": provenance,
                    "limits": {
                        "cpu_budget_seconds": 240,
                        "wall_backstop_seconds": 1800,
                    },
                    "verdict": "OK",
                    "summary": "no worktree slot currently needs coordinator attention",
                    "exit_code": 0,
                    "audit": {
                        "schema": 2,
                        "state": "ok",
                        "summary": "no worktree slot currently needs coordinator attention",
                        "worktree_count": 0,
                        "running_agent_count": 0,
                        "attention_count": 0,
                        "attention_slots": [],
                        "unknown_count": 0,
                        "unknown_slots": [],
                    },
                    "cost": {
                        "schema": "wrkslots-audit-cost/v1",
                        "wall_seconds": 1,
                        "cpu_seconds": 1,
                        "wall_source": "outer monotonic observer",
                        "cpu_source": "cgroup-v2 cpu.stat usage_usec",
                        "cpu_budget_seconds": 240,
                        "wall_backstop_seconds": 1800,
                        "cgroup_usage_usec": 1_000_000,
                        "scope": "hermit-box-run-123-456.scope",
                        "reaped": False,
                        "termination": "completed",
                        "exit_code": 0,
                    },
                    "observer_cost": {
                        "schema_version": 1,
                        "tool": "ci-hub/wrkslots-audit",
                        "estimate": {
                            "kind": "derived",
                            "wall_seconds": 190,
                            "cpu_seconds": 179,
                            "basis": "derived from one completed contention audit",
                        },
                        "actual": {
                            "wall_seconds": 1,
                            "cpu_seconds": 1,
                            "cpu_user_seconds": 1,
                            "cpu_system_seconds": 0,
                            "exit": "0",
                        },
                    },
                    "stderr_sha256": "0" * 64,
                }
            ),
            encoding="utf-8",
        )

        def snapshot(path: Path):
            if not path.exists():
                return None
            if path.is_file():
                return ("file", hashlib.sha256(path.read_bytes()).hexdigest())
            entries = []
            for child in sorted(path.rglob("*")):
                relative = child.relative_to(path)
                if relative.parts[:1] == (".git",):
                    continue
                if child.is_symlink():
                    entries.append((relative.as_posix(), "symlink", os.readlink(child)))
                elif child.is_file():
                    entries.append(
                        (
                            relative.as_posix(),
                            "file",
                            hashlib.sha256(child.read_bytes()).hexdigest(),
                        )
                    )
                else:
                    entries.append((relative.as_posix(), "directory"))
            return tuple(entries)

        config = yaml.safe_load(TICK_CONFIG.read_text(encoding="utf-8"))
        commands = {
            reminder["name"]: reminder["gate"]["cmd"]
            for reminder in config["reminders"]
            if "gate" in reminder
        }
        environment = os.environ | {
            "DEV_HERMIT_PARENT": str(state_root),
            "DEV_HERMIT_TOOL_ROOT": str(ROOT),
            "DEV_HERMIT_TOOL_PARENT_SHA": provenance["parent_sha"],
            "DEV_HERMIT_TOOL_HERMIT_SHA": provenance["hermit_sha"],
            "DEV_HERMIT_TOOL_AGENT_UTILS_SHA": provenance["agent_utils_sha"],
            "PYTHONDONTWRITEBYTECODE": "1",
        }
        environment.pop("DEV_HERMIT_OPS_STATE", None)
        state_before = snapshot(state_root)
        tool_state_paths = (
            ROOT / ".ops-state.yaml",
            ROOT / "ignored" / "ci-hub" / "validate-ledger-spool",
            ROOT / "ignored" / "ci-hub" / "series-spool",
            ROOT / "ignored" / "ci-hub" / "series-quarantine",
            ROOT / "worktrees" / "ACTIVE",
        )
        tool_state_before = {path: snapshot(path) for path in tool_state_paths}
        tool_status_before = subprocess.run(
            ["git", "status", "--porcelain=v1", "--untracked-files=all"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        ).stdout

        deleted_result = subprocess.run(
            ["bash", "-c", commands["deleted_tool_absent"]],
            cwd=ROOT,
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(1, deleted_result.returncode, deleted_result.stdout + deleted_result.stderr)
        self.assertIn(str(state_root), deleted_result.stderr)
        self.assertIn("gh-pr-merge-verified", deleted_result.stderr)

        for name, label in (
            ("ledger_spool_backlog", "validation ledger"),
            ("series_spool_backlog", "per-test series"),
            ("series_quarantine", "series quarantine"),
        ):
            result = subprocess.run(
                ["bash", "-c", commands[name]],
                cwd=ROOT,
                env=environment,
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(1, result.returncode, result.stdout + result.stderr)
            self.assertIn(f"{label} spool publication stalled", result.stdout)
            self.assertIn(str(state_root), result.stdout)

        for name, expected_rc, expected_state in (
            ("validate_wall_regression", 0, "state=holding"),
            ("validate_wall_series_readiness", 1, "state=not-ready"),
        ):
            result = subprocess.run(
                ["bash", "-c", commands[name]],
                cwd=ROOT,
                env=environment,
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(expected_rc, result.returncode, result.stdout + result.stderr)
            self.assertIn(expected_state, result.stdout)
            self.assertIn("n=0", result.stdout)

        wrkslots_result = subprocess.run(
            ["bash", "-c", commands["wrkslots_audit"]],
            cwd=ROOT,
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(0, wrkslots_result.returncode, wrkslots_result.stderr)
        self.assertIn("state=ok", wrkslots_result.stdout)
        self.assertIn("producer_parent_sha=" + provenance["parent_sha"], wrkslots_result.stdout)

        ops_probe = subprocess.run(
            [
                sys.executable,
                "-c",
                (
                    "import sys; "
                    f"sys.path.insert(0, {str(ROOT / 'ci-hub' / 'lib')!r}); "
                    "import ops_state; "
                    "print(ops_state.path_from_environment()); "
                    "print(ops_state.github_actions_policy()[0])"
                ),
            ],
            cwd=ROOT,
            env=environment,
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(0, ops_probe.returncode, ops_probe.stderr)
        self.assertEqual(
            [str(state_root / ".ops-state.yaml"), "False"],
            ops_probe.stdout.splitlines(),
        )

        self.assertEqual(state_before, snapshot(state_root))
        self.assertEqual(
            tool_state_before,
            {path: snapshot(path) for path in tool_state_paths},
        )
        self.assertEqual(
            tool_status_before,
            subprocess.run(
                ["git", "status", "--porcelain=v1", "--untracked-files=all"],
                cwd=ROOT,
                check=True,
                capture_output=True,
                text=True,
            ).stdout,
        )


if __name__ == "__main__":
    unittest.main()
