#!/usr/bin/env python3
"""The structured publication record, and the check that reads it.

The point of these cases is not that the happy path works. It is that the three
false-positive classes the prose regex had to be taught, by enumeration, one
corpus at a time, are closed HERE BY CONSTRUCTION -- and that the check still
refuses when nothing was published, which is the only property that makes it a
gate rather than a formality.
"""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path

CLOSURE = Path(__file__).resolve().parent.parent


def _load(name: str):
    spec = importlib.util.spec_from_file_location(name, CLOSURE / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    # Registered before execution because `@dataclass` resolves annotations
    # through sys.modules; without this the DrainTask definition fails to build.
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


pr_mod = _load("publication_record")
dp = _load("drain_publication")

URL = "https://github.com/rrnewton/hermit/pull/2370#issuecomment-5409832352"
RECORD = pr_mod.render_publication("rrnewton/hermit", 2370, URL, "hermit-005")


def task(task_id="drain_hermit_pr_2370", title="drain hermit#2370", notes=(), desc="",
         status="closed", tags='["drain"]'):
    return {
        "id": task_id,
        "title": title,
        "desc": desc,
        "status": status,
        "tags": tags,
        "modified": "2026-08-25T10:00:00",
        "notes": [{"content": n, "created": "2026-08-25T09:00:00"} for n in notes],
    }


class Grammar(unittest.TestCase):
    def test_a_record_roundtrips(self):
        published, _ = pr_mod.parse_records([RECORD])
        self.assertEqual(len(published), 1)
        self.assertEqual(published[0].comment_id, 5409832352)
        self.assertEqual(published[0].actor, "hermit-005")

    def test_a_record_quoted_inside_prose_is_not_a_record(self):
        """THE WHOLE POINT. A marker matched anywhere in text can be written by
        describing it; this grammar cannot."""
        quoted = (
            "Here is what the poster writes:\n\n"
            + RECORD
            + "\n\n...and that is how the coupling works."
        )
        published, _ = pr_mod.parse_records([quoted])
        self.assertEqual(published, [], "a quoted example counted as a publication")

    def test_fields_that_disagree_with_the_permalink_are_rejected(self):
        """A record is built from the URL the post returned. One whose loose
        fields contradict that URL is a claim about a publication, not one."""
        tampered = RECORD.replace("pr: 2370", "pr: 9999")
        published, _ = pr_mod.parse_records([tampered])
        self.assertEqual(published, [])

    def test_a_record_cannot_be_built_from_a_non_permalink(self):
        with self.assertRaises(ValueError):
            pr_mod.render_publication(
                "rrnewton/hermit", 2370, "https://github.com/rrnewton/hermit/pull/2370",
                "hermit-005",
            )

    def test_an_exemption_must_state_a_reason(self):
        with self.assertRaises(ValueError):
            pr_mod.render_exemption("rrnewton/hermit", 2370, "hermit-005", "   ")


class FalsePositiveClasses(unittest.TestCase):
    """The three classes agent(hermit-013) hit, carried forward deliberately."""

    def test_the_foreign_identifier_cannot_become_the_subject(self):
        """A pull-request number arriving from ANOTHER agent's notes.

        The regex path reads title and id for the subject and can be fooled when
        a task's own identity is ambiguous. The record path takes the subject
        from what was actually posted, so neighbouring prose is inert.
        """
        noisy = task(
            task_id="drain_publication_sweep",
            title="drain sweep",
            notes=[
                "coordination: agent(hermit-012) is on hermit#2223, "
                "agent(hermit-014) has hermit#2311",
                RECORD,
            ],
        )
        scoped, excluded = dp.collect([noisy])
        self.assertEqual(excluded, 0, "a task with a record should never be excluded")
        self.assertEqual(len(scoped), 1)
        self.assertEqual(scoped[0].number, 2370, "subject was taken from foreign prose")
        self.assertEqual(scoped[0].link_source, dp.SOURCE_RECORD)
        self.assertEqual(scoped[0].links, [URL])

    def test_the_rollup_is_still_excluded_not_guessed(self):
        """144 of 226 drain-tagged tasks name many artifacts. Records disagreeing
        about the subject means a rollup, and this rule still has nothing to say
        about one -- it must fall through, not pick a record arbitrarily."""
        other = pr_mod.render_publication(
            "rrnewton/hermit", 2311,
            "https://github.com/rrnewton/hermit/pull/2311#issuecomment-1111111111",
            "hermit-005",
        )
        rollup = task(task_id="drain_rollup", title="drain the queue",
                      notes=[RECORD, other])
        scoped, excluded = dp.collect([rollup])
        self.assertEqual(scoped, [])
        self.assertEqual(excluded, 1)

    def test_a_bare_comment_id_is_still_a_valid_citation(self):
        """The regex path stays reachable and unchanged for tasks with no record,
        so nothing that was accepted before is rejected now."""
        legacy = task(notes=[f"posted the verdict: {URL}"])
        scoped, _ = dp.collect([legacy])
        self.assertEqual(len(scoped), 1)
        self.assertEqual(scoped[0].links, [URL])
        self.assertEqual(scoped[0].link_source, dp.SOURCE_REGEX)


class Exemption(unittest.TestCase):
    def test_a_structured_exemption_carries_its_author(self):
        body = pr_mod.render_exemption(
            "rrnewton/hermit", 2370, "hermit-005", "the landing published itself"
        )
        t = task(notes=[RECORD, body])
        scoped, _ = dp.collect([t])
        self.assertEqual(scoped[0].exemption_source, dp.SOURCE_RECORD)
        self.assertEqual(scoped[0].exemption_actor, "hermit-005")
        self.assertIn("landing published itself", scoped[0].exemption)

    def test_a_quoted_exemption_does_not_open_the_hatch(self):
        body = pr_mod.render_exemption("rrnewton/hermit", 2370, "hermit-005", "duplicate")
        t = task(notes=[RECORD, "for example you could write:\n" + body + "\nbut we did not"])
        scoped, _ = dp.collect([t])
        self.assertEqual(scoped[0].exemption, "", "a quoted exemption was honoured")

    def test_the_dispatch_template_does_not_open_the_hatch(self):
        """Found live on 2026-08-25, not hypothesised.

        ORC's drain dispatch quotes the rule into the task DESCRIPTION, and
        `EXEMPTION.search` over description+notes returned that quote. Two tasks
        were affected; drain_hermit_pr_2255 had no genuine exemption at all and
        would have closed exempt with nobody having decided anything.
        """
        dispatch = (
            "CLOSURE CONDITION: this is a drain task, so it must not close without "
            "a link to the comment published on hermit#2370, or a written\n"
            "NO-PUBLICATION: <reason>. The gate will name this task otherwise."
        )
        t = task(desc=dispatch, notes=["did the work"])
        scoped, _ = dp.collect([t])
        self.assertEqual(
            scoped[0].exemption, "",
            "the instruction explaining how to claim an exemption WAS one",
        )

    def test_a_real_exemption_below_the_template_is_still_found(self):
        """The guard must not shadow the real decision it sits above -- otherwise
        it trades a false accept for a false reject."""
        dispatch = "or a written\nNO-PUBLICATION: <reason>. The gate will name it."
        t = task(desc=dispatch, notes=["NO-PUBLICATION: the landing published itself"])
        scoped, _ = dp.collect([t])
        self.assertEqual(scoped[0].exemption, "the landing published itself")

    def test_the_prose_marker_still_works(self):
        t = task(notes=["NO-PUBLICATION: duplicate of hermit#2300"])
        scoped, _ = dp.collect([t])
        self.assertEqual(scoped[0].exemption_source, dp.SOURCE_REGEX)
        self.assertIn("duplicate", scoped[0].exemption)
        self.assertEqual(scoped[0].exemption_actor, "", "prose cannot name an author")


class ItCanFail(unittest.TestCase):
    """⚠️ DEMONSTRATE IT CAN FAIL.

    A check that cannot be shown refusing is not evidence of anything. These
    close a drain task with no publication record and no prose link, and require
    the check to name it.
    """

    def test_a_closed_drain_task_with_no_record_is_unsatisfied(self):
        t = task(notes=["did the review, looked fine"])
        scoped, _ = dp.collect([t])
        self.assertEqual(len(scoped), 1)
        subject = scoped[0]
        subject.merged = False  # the PR did not land, so nothing published itself
        self.assertTrue(subject.terminal)
        self.assertFalse(
            dp.satisfied(subject),
            "a drain task closed with no published verdict was accepted",
        )

    def test_the_census_names_the_offending_task(self):
        t = task(task_id="drain_hermit_pr_2370", notes=["did the review, looked fine"])
        scoped, excluded = dp.collect([t])
        scoped[0].merged = False
        census = dp.render_census(scoped, excluded, 1, pre=True)
        self.assertIn("unmerged_UNPUBLISHED=1", census)
        self.assertIn(
            "drain_hermit_pr_2370", census,
            "the check refused but did not say WHICH task, which is unactionable",
        )

    def test_a_record_satisfies_the_same_task(self):
        """The control: identical task, one record added, now accepted. Without
        this the refusal above could be a check that always refuses."""
        t = task(task_id="drain_hermit_pr_2370", notes=["did the review", RECORD])
        scoped, _ = dp.collect([t])
        scoped[0].merged = False
        scoped[0].confirmed = [URL]  # GitHub corroborates the permalink
        self.assertTrue(dp.satisfied(scoped[0]))


class CoverageReporting(unittest.TestCase):
    def test_the_census_reports_which_source_established_each_verdict(self):
        """The number that decides when the prose path can be deleted."""
        by_record = task(task_id="drain_a", title="drain hermit#2370", notes=[RECORD])
        by_prose = task(task_id="drain_b", title="drain hermit#2311",
                        notes=["https://github.com/rrnewton/hermit/pull/2311"
                               "#issuecomment-1111111111"])
        scoped, excluded = dp.collect([by_record, by_prose])
        census = dp.render_census(scoped, excluded, 2, pre=False)
        self.assertIn("evidence_from_structured_record=1/2", census)
        self.assertIn("evidence_from_prose_regex=1", census)


if __name__ == "__main__":
    unittest.main(verbosity=2)
