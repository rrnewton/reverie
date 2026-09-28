#!/usr/bin/env python3
"""The hourly prompt must invoke the delivery-integrity flags that exist.

Enforcement that nobody is told to run is optional in practice. `status-log.rs`
refuses a status whose text does not hash to the delivered message, and
`hourly-status-relay.rs` only closes an hour on a dereferenceable GChat record --
but a coordinator follows `hourly_status_prompt.md`, so if the prompt's command
templates omit those flags the guarantees are never exercised.

Worse than omission actually happened here. Before this suite, step 4 instructed
"Never change `state` ... including a more descriptive one like
`gchat_delivered`", which was correct under the old single-stage model and became
exactly backwards once `gchat_delivered` was made the state that closes an hour.
A coordinator following the prompt verbatim would leave every hour at
`wake_accepted`, get it re-woken to the attempt bound, and land on
`gchat-ack-missing`. Prose drifts out of step with code silently; this suite is
the binding that makes it fail loudly.

Everything here is inert: it reads two markdown blocks and two source files, and
runs no command, no network call, and no send.
"""

from __future__ import annotations

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PROMPT = ROOT / "hourly_status_prompt.md"
STATUS_LOG = ROOT / "scripts/status-log.rs"
RELAY = ROOT / "scripts/hourly-status-relay.rs"

FENCE = re.compile(r"^```[a-z]*\n(.*?)^```", re.MULTILINE | re.DOTALL)
LONG_FLAG = re.compile(r"(--[a-z0-9][a-z0-9-]+)")


def command_blocks(text: str) -> list[str]:
    """Fenced blocks that actually invoke one of the two scripts."""
    return [
        block
        for block in FENCE.findall(text)
        if "status-log.rs" in block or "hourly-status-relay.rs" in block
    ]


def block_invoking(blocks: list[str], script: str, *, must_contain: str = "") -> str:
    for block in blocks:
        if script in block and must_contain in block:
            return block
    raise AssertionError(f"no documented block invokes {script} with {must_contain!r}")


def flags_declared_by(script: Path) -> set[str]:
    """Long flags the script actually matches on, from its own argv parser."""
    return set(re.findall(r'"(--[a-z0-9][a-z0-9-]+)"', script.read_text()))


class HourlyPromptFlagTests(unittest.TestCase):
    def setUp(self) -> None:
        self.text = PROMPT.read_text()
        self.blocks = command_blocks(self.text)
        self.assertTrue(self.blocks, "the prompt documents no runnable command at all")

    # ---- POSITIVE: the documented templates carry the enforcing flags ----

    def test_status_log_template_binds_the_log_to_the_send(self) -> None:
        block = block_invoking(self.blocks, "status-log.rs")
        for flag in ("--expect-sha256", "--expect-bytes"):
            self.assertIn(
                flag,
                block,
                f"{flag} exists and refuses a mismatch, but the prompt never invokes it, "
                "so the binding is optional in practice",
            )
        # The covered hour is not the write hour; a row that does not say so is
        # the ambiguity schema 4 was added to remove.
        self.assertIn("--covers-hour", block)
        self.assertIn("--recovery-for", block, "a late write must be able to declare itself")
        # Denominator authority must survive this edit.
        self.assertIn("--repos", block, "the counts' denominator must stay in the template")
        self.assertIn("--open-prs", block)

    def test_the_total_is_adopted_by_auto_not_counted_by_the_caller(self) -> None:
        """The canonical block must not document the racy path.

        Counting in the caller and re-checking in the writer is a TOCTOU: a real
        status was refused at 104 against an observed 105, and the population
        moved 104 -> 105 -> 106 over the following minutes. `auto` dereferences
        once and adopts, so there is no second reading to disagree with. If this
        block ever regresses to a placeholder count, the prompt is teaching the
        race again.
        """
        block = block_invoking(self.blocks, "status-log.rs")
        self.assertIn("--open-prs auto", block, "the canonical block must adopt, not assert")
        self.assertNotIn(
            "--open-prs <N>",
            block,
            "a placeholder count reinstates the caller-measures/writer-remeasures race",
        )
        # The counts that genuinely ARE the caller's to supply must survive.
        for flag in ("--genuine-reds", "--fleet-count"):
            self.assertIn(flag, block, f"{flag} has no authority to dereference and must stay")

    def test_the_monitored_rollup_is_kept_distinct_from_the_total(self) -> None:
        """Both denominators must stay named and separate in the prose.

        `ci-hub pr-status` reports a monitored ready/non-draft ROLLUP of a
        handful, not the hundred-odd total. An entry once recorded 7 under the
        total-open basis while the true total was 102, so the prompt has to keep
        saying which is which.
        """
        # The prompt is hard-wrapped markdown, so a multi-word phrase can be
        # split across a line break. Collapse whitespace before asserting on
        # prose -- my first version of this test failed on the live file for
        # exactly that reason, with "TOTAL open INCLUDING\nDRAFTS".
        flat = re.sub(r"\s+", " ", self.text.lower())
        self.assertIn("--ready-prs", self.text, "the subset needs its own named field")
        self.assertTrue(
            "ready/non-draft" in flat or "ready-non-draft" in flat,
            "the subset's basis must stay named",
        )
        self.assertTrue(
            "rollup" in flat,
            "the prompt must name pr-status's monitored rollup so it is not read as the total",
        )
        self.assertTrue(
            "total open including drafts" in flat,
            "the total's basis must stay spelled out",
        )

    def test_ack_template_is_documented_and_complete(self) -> None:
        block = block_invoking(self.blocks, "hourly-status-relay.rs", must_contain="--ack-hour")
        for flag in (
            "--ack-hour",
            "--ack-message-name",
            "--ack-space",
            "--ack-thread",
            "--ack-text-file",
        ):
            self.assertIn(flag, block, f"{flag} is required for an acknowledgement")

    # ---- the guard that would have caught the real drift ----

    def test_every_documented_flag_exists_in_the_script_it_is_passed_to(self) -> None:
        known = {
            "status-log.rs": flags_declared_by(STATUS_LOG),
            "hourly-status-relay.rs": flags_declared_by(RELAY),
        }
        for block in self.blocks:
            script = "status-log.rs" if "status-log.rs" in block else "hourly-status-relay.rs"
            for flag in LONG_FLAG.findall(block):
                self.assertIn(
                    flag,
                    known[script],
                    f"the prompt tells the coordinator to pass {flag} to {script}, "
                    "which does not accept it",
                )

    def test_prompt_does_not_carry_the_superseded_state_advice(self) -> None:
        # The specific reversal: `gchat_delivered` used to be forbidden and is
        # now the state that closes an hour.
        # assertIn/assertNotIn on an 8 KB document pastes the whole thing into
        # the failure output, which buries the finding. Assert on booleans and
        # say what is wrong instead.
        lowered = self.text.lower()
        self.assertFalse(
            "never change `state`" in lowered,
            "the prompt still says never to change `state`; that predates the two-stage "
            "split and now prevents an hour from ever being closed",
        )
        self.assertTrue(
            "gchat_delivered" in lowered,
            "the prompt never names gchat_delivered, the state that actually closes an hour",
        )
        self.assertTrue(
            "wake_accepted" in lowered,
            "the prompt never names wake_accepted, so stage one is not identified as NOT a delivery",
        )
        self.assertFalse(
            "os.replace(tmp, p)" in self.text,
            "the prompt still hands the coordinator a raw claim rewrite; the driver owns "
            "the claim schema and --ack-hour rewrites it atomically",
        )

    def test_chat_delivery_stays_with_the_orc_coordinator(self) -> None:
        flat = re.sub(r"\s+", " ", self.text.lower())
        self.assertIn(
            "do not delegate google chat delivery to a worker",
            flat,
            "the hourly prompt must not let its owner-facing send step be delegated",
        )
        self.assertIn(
            "coordinator alone synthesizes and sends",
            flat,
            "worker evidence must return to the coordinator before owner-facing delivery",
        )
        # 2026-09-28 owner policy: TaskGraph is ORC's to-do list, not the record.
        # Worker evidence travels through GitHub or a tracked report.
        self.assertIn("github issue or pull-request comments", flat)
        self.assertIn("taskgraph is orc's own to-do list", flat)
        self.assertNotIn("verified taskgraph notes", flat)
        self.assertIn("tracked `ai_docs` reports", flat)
        self.assertIn("`orc.sendgchat`", flat)

    # ---- NEGATIVE: prove the checks above are not inert ----

    def test_the_checks_fail_on_the_pre_fix_templates(self) -> None:
        """Mutate each template back to its old shape; each check must object."""
        # 1. status-log template without the digest/coverage flags -- the shape
        #    the prompt actually had before this change.
        old_status_block = (
            "```\n./scripts/status-log.rs \\\n"
            "  --status-file <file> \\\n"
            "  --repos <owner/name> \\\n"
            "  --open-prs <N> --genuine-reds <N> --fleet-count <N>\n```\n"
        )
        blocks = command_blocks(old_status_block)
        self.assertEqual(len(blocks), 1)
        for flag in ("--expect-sha256", "--expect-bytes", "--covers-hour"):
            self.assertNotIn(flag, blocks[0], "fixture must reproduce the pre-fix template")

        # 2. no ack block at all -- the pre-fix prompt documented none.
        with self.assertRaises(AssertionError):
            block_invoking(blocks, "hourly-status-relay.rs", must_contain="--ack-hour")

        # 3. an invented flag must be caught rather than waved through.
        known = flags_declared_by(STATUS_LOG)
        self.assertNotIn("--expect-digest", known)
        self.assertIn("--expect-sha256", known, "positive control: the real flag IS declared")

        # 4. the superseded advice must be detectable as such.
        superseded = "**Never change `state`.** It is the dedupe authority"
        self.assertIn("never change `state`", superseded.lower())
        self.assertFalse(
            superseded in self.text,
            "the live prompt still carries the superseded `state` advice",
        )


if __name__ == "__main__":
    unittest.main()
