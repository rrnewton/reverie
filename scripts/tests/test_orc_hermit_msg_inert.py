#!/usr/bin/env python3
"""End-to-end relay tests against an INERT fake Orc pane.

These drive the real `scripts/orc-hermit-msg.py` through a real tmux server, a
real pane, and a real terminal application that really consumes the keystrokes —
but the application is `fake_orc_pane.py`, which can reach nobody. The unit
tests in `test_orc_hermit_msg.py` stub `run_tmux` and so cannot catch a mistake
in the tmux invocations themselves; these can, without the cost that discovered
the need for them (a self-test message delivered to the owner from the live
coordinator pane).

SAFETY INVARIANTS, asserted rather than assumed:

* every tmux server used here lives on a private socket inside a temp dir, so
  the orc runtime socket cannot be reached even by accident;
* `--socket` is always passed explicitly, which after this change disables the
  "find some other socket" fallback that caused that delivery;
* `test_the_default_socket_is_never_touched` proves the default path is not
  consulted at all.
"""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
RELAY = HERE.parent / "orc-hermit-msg.py"
FAKE_PANE = HERE / "fake_orc_pane.py"
CAPTURE = HERE / "data/captured-orc-composer.txt"
#: The pane must be at least as wide as the captured frame or tmux wraps it, and
#: the wrapped remainder reads as body text -- which makes an empty composer look
#: occupied. Derived rather than hardcoded so a re-capture from a wider terminal
#: cannot silently start wrapping. Measured: the 2026-09-05 capture is 214
#: columns and the old hardcoded 200 wrapped every frame line.
PANE_WIDTH = max(200, max(len(line) for line in CAPTURE.read_text(
    encoding="utf-8").splitlines()) + 2)


def _load_module():
    spec = importlib.util.spec_from_file_location("orc_hermit_msg_inert", RELAY)
    module = importlib.util.module_from_spec(spec)
    sys.modules["orc_hermit_msg_inert"] = module
    spec.loader.exec_module(module)
    return module


ohm = _load_module()


@unittest.skipIf(shutil.which("tmux") is None, "tmux is not installed")
class InertPaneTestCase(unittest.TestCase):
    """One private tmux server per test, torn down unconditionally."""

    DB = "faketest"
    #: Which composer rendering the fixture draws. Both are exercised -- see the
    #: box-layout subclasses at the bottom of this file.
    LAYOUT = "ascii"

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="orc-msg-inert-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.socket = self.tmp / "tmux.sock"
        self.record = self.tmp / "delivered.jsonl"
        self.log = self.tmp / "relay.log"

        # tmux reports `pane_current_command` from the exec'd file's basename,
        # so running the interpreter through a symlink named `orc` is what makes
        # this pane discoverable by a relay that (correctly) only talks to a
        # pane running `orc`.
        self.orc_shim = self.tmp / "orc"
        self.orc_shim.symlink_to(sys.executable)

    def start_pane(self, *extra: str) -> None:
        command = " ".join(
            [
                str(self.orc_shim),
                str(FAKE_PANE),
                "--layout",
                self.LAYOUT,
                "--db",
                self.DB,
                "--record",
                str(self.record),
                *extra,
            ]
        )
        subprocess.run(
            [
                "tmux", "-S", str(self.socket), "new-session", "-d",
                "-s", f"orc-{self.DB}", "-n", "orc",
                "-x", str(PANE_WIDTH), "-y", "50", command,
            ],
            check=True,
            capture_output=True,
        )
        self.addCleanup(
            subprocess.run,
            ["tmux", "-S", str(self.socket), "kill-server"],
            capture_output=True,
        )
        self.wait_for_pane()

    def wait_for_pane(self) -> None:
        for _ in range(100):
            result = subprocess.run(
                ["tmux", "-S", str(self.socket), "capture-pane", "-p", "-t", "%0"],
                capture_output=True,
                text=True,
            )
            if ohm.parse_composer(result.stdout) is not None:
                return
            time.sleep(0.1)
        self.fail("fake Orc pane never rendered a composer")

    def relay(self, *args: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [
                sys.executable, str(RELAY),
                "--socket", str(self.socket),
                "--orc-db", self.DB,
                # There is no orc CLI here; discovery falls back to scanning the
                # socket, which is the path the live fleet uses under load.
                "--orc-command", str(self.tmp / "no-such-orc"),
                "--log-file", str(self.log),
                *args,
            ],
            capture_output=True,
            text=True,
        )

    def relay_with_tg_db(self, *args: str) -> subprocess.CompletedProcess:
        environment = os.environ.copy()
        environment["TG_DB_PATH"] = str(self.tmp / f"{self.DB}.db")
        return subprocess.run(
            [
                sys.executable, str(RELAY),
                "--socket", str(self.socket),
                "--orc-command", str(self.tmp / "no-such-orc"),
                "--log-file", str(self.log),
                *args,
            ],
            capture_output=True,
            text=True,
            env=environment,
        )

    def delivered(self) -> list[str]:
        if not self.record.exists():
            return []
        return [
            json.loads(line)["message"]
            for line in self.record.read_text(encoding="utf-8").splitlines()
            if line.strip()
        ]

    def log_records(self) -> list[dict]:
        if not self.log.exists():
            return []
        return [
            json.loads(line)
            for line in self.log.read_text(encoding="utf-8").splitlines()
            if line.strip()
        ]


class TestInertDeliveryPositiveBracket(InertPaneTestCase):
    def test_tg_db_successor_identity_delivers_to_the_live_coordinator(self):
        self.start_pane()
        result = self.relay_with_tg_db("derived successor delivery")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.delivered(), ["derived successor delivery"])
        record = self.log_records()[0]
        self.assertEqual(record["database"], self.DB)
        self.assertEqual(record["database_source"], "TG_DB_PATH")

    def test_a_message_is_delivered_once_and_reported_sent(self):
        self.start_pane()
        result = self.relay("hello inert coordinator")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.delivered(), ["hello inert coordinator"])
        records = self.log_records()
        self.assertEqual([record["status"] for record in records], ["sent"])
        self.assertTrue(records[0]["echo"])

    def test_a_multiline_message_survives_the_wire(self):
        self.start_pane()
        result = self.relay("first line\nsecond line")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.delivered(), ["first line\nsecond line"])

    def test_a_message_beginning_with_a_dash_is_not_read_as_a_flag(self):
        # `send-keys -l` misparses these; the buffer-paste path must not.
        self.start_pane()
        result = self.relay("--not-a-flag but a message")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.delivered(), ["--not-a-flag but a message"])

    def test_a_submission_queued_mid_turn_is_still_a_delivery(self):
        self.start_pane("--state", "streaming", "--queue-submissions")
        result = self.relay("queued while streaming")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.delivered(), ["queued while streaming"])
        self.assertEqual(self.log_records()[0]["pending"], 1)


class TestInertDeliveryNegativeBracket(InertPaneTestCase):
    def test_a_pane_that_consumes_nothing_fails_and_delivers_nothing(self):
        # Every tmux call still succeeds here; only the application ignores the
        # input. This is the exact shape that used to exit 0 and log "sent".
        self.start_pane("--ignore-input")
        result = self.relay("this must not be reported as sent")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.delivered(), [])
        self.assertEqual(
            [record["status"] for record in self.log_records()], ["failed"]
        )

    def test_a_draft_orc_already_consumed_is_a_delivery_not_a_failure(self):
        """The twin of the case above, and the one that used to double-send.

        Both panes present an EMPTY composer after typing. They differ only in
        whether the message is findable in the transcript. Reading the empty box
        alone as non-delivery is what made the 2026-08-07T03:00Z hourly wake
        report `relay-rc=1` and release its hour: the caller then treats the
        hour as undelivered and a later run sends the whole status a second
        time. An empty composer is ambiguous; the transcript disambiguates it.
        """
        message = "this was taken before the relay pressed Enter"
        seed = self.tmp / "seed.txt"
        seed.write_text(message, encoding="utf-8")
        self.start_pane("--ignore-input", "--seed-transcript", str(seed))

        result = self.relay(message)
        self.assertEqual(result.returncode, 0, result.stderr)
        # Enter was NOT pressed, so the fake pane recorded no submission -- the
        # relay must not "deliver" something Orc already holds.
        self.assertEqual(self.delivered(), [])
        records = self.log_records()
        self.assertEqual([record["status"] for record in records], ["sent"])
        self.assertTrue(records[0]["echo"])
        # NOTE ON WHAT THIS DOES AND DOES NOT COVER. Here the consumed message
        # is still on the visible screen, so the PRE-EXISTING "probe visible"
        # branch already returns success -- this test pins that behaviour so it
        # cannot regress, but it does NOT exercise the scrollback branch added
        # for the scrolled-off case. That branch is isolated in
        # test_orc_hermit_msg.py::VerifyInjectedConsumptionTests, because
        # driving a real pane into "consumed AND scrolled out of view" through
        # tmux history is timing-dependent and would make this suite flaky.
        self.assertIn(records[0]["ack"], ("composer-drained+echo", "consumed-before-submit+echo"))

    def test_the_two_empty_composer_outcomes_are_not_conflated(self):
        """Same empty box, opposite verdicts, decided only by the transcript."""
        message = "identical text, different pane state"
        seed = self.tmp / "seed2.txt"
        seed.write_text(message, encoding="utf-8")

        # Consumed: transcript carries it -> success, no submission.
        self.start_pane("--ignore-input", "--seed-transcript", str(seed))
        consumed = self.relay(message)
        self.assertEqual(consumed.returncode, 0, consumed.stderr)
        self.assertEqual(self.delivered(), [])

        # Not consumed: nothing anywhere -> still a hard failure. The new path
        # must not have turned a genuine non-delivery into a false success.
        self.tearDown()
        self.setUp()
        self.start_pane("--ignore-input")
        lost = self.relay(message)
        self.assertNotEqual(lost.returncode, 0)
        self.assertEqual(self.delivered(), [])
        self.assertEqual([r["status"] for r in self.log_records()], ["failed"])

    def test_an_occupied_composer_is_refused_without_delivering(self):
        self.start_pane()
        subprocess.run(
            ["tmux", "-S", str(self.socket), "send-keys", "-t", "%0",
             "somebody elses draft"],
            check=True, capture_output=True,
        )
        time.sleep(0.5)
        result = self.relay("must not be concatenated")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsent draft", result.stderr)
        self.assertEqual(self.delivered(), [])

    def test_a_dry_run_checks_readiness_and_delivers_nothing(self):
        """A ready composer is not a deliverable coordinator, so report both.

        This asserted only "dry-run OK". Measured 2026-09-05 against the live
        coordinator: the composer was ready and Orc was not answering its own
        control socket, so a real send would have refused while the dry-run said
        OK. These runs pass --orc-command at a path that does not exist, so the
        readiness state here is `not-probed` -- which must appear, because a
        check that could not run must not read as a pass.
        """
        self.start_pane()
        result = self.relay("--dry-run")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("composer ready", result.stdout)
        self.assertIn("coordinator not-probed", result.stdout)
        self.assertIn("nothing was typed or sent", result.stdout)
        self.assertEqual(self.delivered(), [])
        self.assertEqual(
            [record["status"] for record in self.log_records()], ["dry-run"]
        )

    def test_no_orc_pane_on_the_socket_fails_without_delivering(self):
        subprocess.run(
            ["tmux", "-S", str(self.socket), "new-session", "-d",
             "-s", f"orc-{self.DB}", "-n", "sh", "sh"],
            check=True, capture_output=True,
        )
        self.addCleanup(
            subprocess.run,
            ["tmux", "-S", str(self.socket), "kill-server"],
            capture_output=True,
        )
        result = self.relay("nowhere to go")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.delivered(), [])


class TestExplicitSocketNeverRetargets(InertPaneTestCase):
    """The regression that delivered a self-test message to the owner.

    An explicit --socket that does not exist used to be replaced by the
    most-recently-modified socket under the orc runtime dir — the live
    coordinator — and the message was delivered there.
    """

    def test_a_missing_explicit_socket_is_refused_not_substituted(self):
        missing = self.tmp / "definitely-absent.sock"
        message = "must never be redirected"
        result = self.relay_to(missing, message)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Refusing to look for another socket", result.stderr)
        self.assertIn("recoverable message:", result.stderr)
        records = self.log_records()
        self.assertEqual([record["status"] for record in records], ["failed"])
        recovery = Path(records[0]["recoverable_message_file"])
        self.assertEqual(recovery.read_text(encoding="utf-8"), message)

    def test_a_missing_explicit_socket_does_not_reach_a_live_pane(self):
        # A real inert pane exists on self.socket. Aiming at a different,
        # absent path must NOT fall through to it.
        self.start_pane()
        missing = self.tmp / "definitely-absent.sock"
        result = self.relay_to(missing, "must never be redirected")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.delivered(), [])

    def test_the_default_socket_is_never_touched(self):
        # Proves these tests cannot reach the orc runtime socket at all.
        self.start_pane()
        self.relay("harmless")
        self.assertFalse(str(ohm.DEFAULT_SOCKET).startswith(str(self.tmp)))
        for record in self.log_records():
            self.assertNotIn("orc-tmux", json.dumps(record))

    def test_resolve_socket_still_falls_back_for_the_default(self):
        # The fallback exists so the hourly tick survives an orc restart into a
        # renamed socket; narrowing it must not delete it.
        missing = self.tmp / "absent.sock"
        with self.assertRaises(ohm.OrcMessageError):
            ohm.resolve_socket(missing, allow_fallback=False)
        self.assertEqual(ohm.resolve_socket(self.socket_placeholder()), self.socket_placeholder())

    def socket_placeholder(self) -> Path:
        path = self.tmp / "present.sock"
        path.touch()
        return path

    def relay_to(self, socket: Path, message: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [
                sys.executable, str(RELAY),
                "--socket", str(socket),
                "--orc-db", self.DB,
                "--orc-command", str(self.tmp / "no-such-orc"),
                "--log-file", str(self.log),
                message,
            ],
            capture_output=True,
            text=True,
        )


# ⚠️ EVERY CASE ABOVE, RE-RUN AGAINST THE RENDERING THE SHIPPED BUILD DRAWS. The
# fixture drew only the hyphen-rule layout while the coordinator drew box-drawing
# characters, so the relay's detection was dead in production against a fully
# green suite. Subclassing rather than adding cases is deliberate: a case added
# above is automatically run against both, so the two renderings cannot end up
# covered unequally.
class BoxLayoutDeliveryPositiveBracket(TestInertDeliveryPositiveBracket):
    LAYOUT = "box"


class BoxLayoutDeliveryNegativeBracket(TestInertDeliveryNegativeBracket):
    LAYOUT = "box"


class BoxLayoutExplicitSocketNeverRetargets(TestExplicitSocketNeverRetargets):
    LAYOUT = "box"


if __name__ == "__main__":
    unittest.main(verbosity=2)
