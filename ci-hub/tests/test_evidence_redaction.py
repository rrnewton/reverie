#!/usr/bin/env python3
"""Per-cell evidence must lose host identity and keep everything else.

Two failure directions matter equally and they pull opposite ways. Under-redact
and a public page discloses the developer's username and workspace. OVER-redact
and the syscall traces the page exists to show are corrupted -- a guest opening
``/tmp/test`` is the test's subject, not a leak. Every test here pins one
direction or the other, and each has a control: the mutation that would break it
is named in the docstring and verified to fail.
"""

from __future__ import annotations

import hashlib
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "ci-hub" / "compatibility-website"))
sys.path.insert(0, str(ROOT / "ci-hub" / "series"))

import evidence_redaction as redaction  # noqa: E402


WORKSPACE = "/home/newton/work/dev-hermit"


class TokenAgreementTests(unittest.TestCase):
    """One token and one boundary rule across the repository, not three."""

    def test_the_token_matches_the_one_already_in_use(self) -> None:
        """Control: change WORKSPACE_TOKEN here and this fails.

        `series.py` and `ledger/validate_rows.py` already canonicalise a
        workspace root to this exact token. A second spelling would publish
        evidence a reader cannot correlate with a ledger row.
        """

        import series

        self.assertEqual(redaction.WORKSPACE_TOKEN, series.WORKSPACE_TOKEN)
        validate_rows = (ROOT / "ci-hub/ledger/validate_rows.py").read_text()
        self.assertIn(
            f'WORKSPACE_TOKEN = "{redaction.WORKSPACE_TOKEN}"', validate_rows
        )

    def test_the_delimiter_set_matches_the_one_already_curated(self) -> None:
        """Control: drop or add one delimiter here and this fails.

        The set is Unicode White_Space -- deliberately NOT Python's broader
        `str.isspace` -- plus the punctuation that ends a path token. It was
        curated once in series.py; this module must not drift from it.
        """

        import series

        self.assertEqual(
            redaction._EVIDENCE_PATH_DELIMITERS,
            series._EVIDENCE_PATH_DELIMITERS,
        )

    def test_the_byte_rule_agrees_with_the_string_rule_it_mirrors(self) -> None:
        """The bytes implementation must decide boundaries like series.py's.

        ⚠️ AGAINST `_replace_evidence_root_token`, NOT `_replace_at_path_boundary`.
        series.py has BOTH, and the second is the one a reader reaches for. It
        accepts a match only when what follows is end-of-string, `/` or a
        closing quote, because it rewrites structured recorded fields where the
        path IS the field. On free-form log text it under-redacts: the first
        case below, a path followed by a tab, is left intact by it. Evidence is
        free-form, so the evidence-named rule is the correct source of truth,
        and cross-checking the wrong one produced a divergence that was mine to
        fix in the test rather than in the module.

        Control: change either boundary condition in
        `_replace_evidence_root_token` and one of these cases diverges.
        """

        import series

        cases = [
            f"run 1: {WORKSPACE}/ignored/x.log",
            f"{WORKSPACE}",
            f"{WORKSPACE}-sibling/x",
            f"prefix{WORKSPACE}/x",
            f"({WORKSPACE}/a) and [{WORKSPACE}/b]",
            f"cwd={WORKSPACE}",
            f"{WORKSPACE}\t{WORKSPACE}/deep",
            "nothing to rewrite at all",
        ]
        for text in cases:
            with self.subTest(text=text):
                expected = series._replace_evidence_root_token(
                    text, WORKSPACE, redaction.WORKSPACE_TOKEN
                )
                actual = redaction._replace_evidence_root_token(
                    text.encode(),
                    WORKSPACE.encode(),
                    redaction.WORKSPACE_TOKEN.encode(),
                ).decode()
                self.assertEqual(actual, expected)


class HostIdentityIsRemovedTests(unittest.TestCase):
    def test_the_workspace_prefix_is_rewritten(self) -> None:
        raw = f"::   run 1: {WORKSPACE}/ignored/compat-envelope/x/run1_log_SwZtS\n".encode()

        out = redaction.redact_evidence_bytes(raw, workspace_root=WORKSPACE)

        self.assertNotIn(b"/home/newton", out)
        self.assertIn(b"{{WORKSPACE_ROOT}}/ignored/compat-envelope", out)
        # No HOME token here: the workspace rule already consumed the whole
        # prefix, so there is no bare home left for the second rule to find.
        self.assertNotIn(b"{{HOME_ROOT}}", out)

    def test_the_bare_home_above_the_workspace_is_rewritten(self) -> None:
        """The username leaks from the parent directory too.

        Control: delete the home-rewrite block and this fails while the test
        above still passes -- which is why it is a separate case.
        """

        raw = b"failed to stat /home/newton for the runner\n"

        out = redaction.redact_evidence_bytes(raw, workspace_root=WORKSPACE)

        self.assertNotIn(b"/home/newton", out)
        self.assertIn(b"{{HOME_ROOT}}", out)

    def test_the_home_is_not_the_workspaces_parent(self) -> None:
        """⚠️ `/home/u/work` is the parent; `/home/u` is the leak.

        Control: derive the home with `root.rsplit("/", 1)[0]` and this fails
        while every other test here still passes -- which is exactly how the
        bug got written in the first place.
        """

        raw = b"stat /home/newton failed; /home/newton/work is fine\n"

        out = redaction.redact_evidence_bytes(raw, workspace_root=WORKSPACE)

        self.assertNotIn(b"/home/newton", out)

    def test_a_root_under_no_recognised_home_rewrites_only_the_workspace(self) -> None:
        """A `/srv/build/tree` root discloses no username; nothing to add."""

        raw = b"/srv/build/tree/x and /srv/build elsewhere\n"

        out = redaction.redact_evidence_bytes(raw, workspace_root="/srv/build/tree")

        self.assertIn(b"{{WORKSPACE_ROOT}}/x", out)
        self.assertIn(b"/srv/build elsewhere", out)
        self.assertNotIn(b"{{HOME_ROOT}}", out)

    def test_an_unrelated_home_directory_is_left_alone(self) -> None:
        """Only the workspace's own parent is rewritten, never every home.

        Control: rewrite `/home/<anything>` instead of the exact parent and
        this fails.
        """

        raw = b"guest opened /home/otheruser/data\n"

        out = redaction.redact_evidence_bytes(raw, workspace_root=WORKSPACE)

        self.assertEqual(out, raw)


class EvidenceIsNotCorruptedTests(unittest.TestCase):
    """The over-redaction direction, measured from real evidence bytes."""

    def test_guest_tmp_paths_survive_untouched(self) -> None:
        """⚠️ These are the test's SUBJECT, not a leak.

        All nine distinct /tmp paths in a 300-file sample of retained cells are
        guest-visible paths belonging to the programs under test. An earlier
        pass named them a leak class; none discloses a user, home or host.
        Rewriting them would corrupt the syscall trace the page publishes.

        Control: add a /tmp rule to the redactor and this fails.
        """

        raw = (
            b'inbound syscall: openat(-100, "/tmp/test", O_CLOEXEC)\n'
            b"/tmp/hermit-copy-file-range-source -> /tmp/hermit-copy-file-range-destination\n"
            b"/tmp/hermit_file_io_roundtrip.bin /tmp/hermit-proc-fd-link-aliases\n"
        )

        out = redaction.redact_evidence_bytes(raw, workspace_root=WORKSPACE)

        self.assertEqual(out, raw)

    def test_a_guest_reading_etc_passwd_is_not_treated_as_a_credential(self) -> None:
        """The six credential-word hits in 264 MB were all this line.

        Control: add a keyword-based redaction rule and this fails.
        """

        raw = b'inbound syscall: openat(-100, 0x1330e38 -> "/etc/passwd", O_CLOEXEC)\n'

        out = redaction.redact_evidence_bytes(raw, workspace_root=WORKSPACE)

        self.assertEqual(out, raw)

    def test_a_sibling_directory_sharing_the_prefix_is_not_rewritten(self) -> None:
        """`/home/newton/work/dev-hermit-scratch` is a DIFFERENT directory.

        Control: replace the boundary rule with a plain `bytes.replace` and
        this fails.
        """

        raw = b"/home/newton/work/dev-hermit-scratch/file\n"

        out = redaction.redact_evidence_bytes(raw, workspace_root=WORKSPACE)

        self.assertIn(b"dev-hermit-scratch", out)
        self.assertNotIn(b"{{WORKSPACE_ROOT}}-scratch", out)

    def test_invalid_utf8_passes_through(self) -> None:
        """An INFO log is guest output and need not be valid UTF-8.

        Control: decode the input instead of working in bytes and this raises.
        """

        raw = b"\xff\xfe binary \x00 noise " + WORKSPACE.encode() + b"/x \x80\n"

        out = redaction.redact_evidence_bytes(raw, workspace_root=WORKSPACE)

        self.assertIn(b"\xff\xfe binary \x00 noise ", out)
        self.assertIn(b"{{WORKSPACE_ROOT}}/x", out)


class PurityTests(unittest.TestCase):
    def test_the_transform_is_idempotent(self) -> None:
        """Required for the deterministic-rebuild property."""

        raw = f"{WORKSPACE}/a and /home/newton/b\n".encode()

        once = redaction.redact_evidence_bytes(raw, workspace_root=WORKSPACE)
        twice = redaction.redact_evidence_bytes(once, workspace_root=WORKSPACE)

        self.assertEqual(once, twice)

    def test_the_same_bytes_give_the_same_digest(self) -> None:
        raw = f"{WORKSPACE}/a\n".encode()

        first = redaction.redact_and_bind(raw, workspace_root=WORKSPACE)
        second = redaction.redact_and_bind(raw, workspace_root=WORKSPACE)

        self.assertEqual(first.sha256, second.sha256)
        self.assertEqual(first.byte_length, second.byte_length)


class BindingOrderTests(unittest.TestCase):
    def test_the_digest_covers_the_published_bytes_not_the_input(self) -> None:
        """⚠️ THE ORDERING TRAP, PINNED.

        Digesting the raw bytes and publishing the redacted ones makes every
        published file fail its own verification, and nothing notices until a
        verifier runs. Control: digest `data` instead of `published` in
        `redact_and_bind` and this fails.
        """

        raw = f"{WORKSPACE}/a\n".encode()

        bound = redaction.redact_and_bind(raw, workspace_root=WORKSPACE)

        self.assertEqual(bound.sha256, hashlib.sha256(bound.data).hexdigest())
        self.assertNotEqual(bound.sha256, hashlib.sha256(raw).hexdigest())
        self.assertEqual(bound.byte_length, len(bound.data))
        self.assertTrue(bound.redacted)

    def test_untouched_evidence_reports_that_it_was_not_redacted(self) -> None:
        raw = b"nothing host-specific here\n"

        bound = redaction.redact_and_bind(raw, workspace_root=WORKSPACE)

        self.assertFalse(bound.redacted)
        self.assertEqual(bound.data, raw)


class RefusalTests(unittest.TestCase):
    """A root that would make the rewrite unsafe is refused, not applied."""

    def test_refuses_a_root_that_would_swallow_the_filesystem(self) -> None:
        for root in ("/", "//", "/home", "/tmp"):
            with self.subTest(root=root):
                with self.assertRaises(redaction.RedactionRefused):
                    redaction.redact_evidence_bytes(b"x", workspace_root=root)

    def test_refuses_an_empty_or_relative_root(self) -> None:
        for root in ("", "work/dev-hermit", "./dev-hermit"):
            with self.subTest(root=root):
                with self.assertRaises(redaction.RedactionRefused):
                    redaction.redact_evidence_bytes(b"x", workspace_root=root)

    def test_refuses_an_already_redacted_root(self) -> None:
        """Passing the token back in would produce nested tokens."""

        with self.assertRaises(redaction.RedactionRefused):
            redaction.redact_evidence_bytes(
                b"x", workspace_root=f"/home/{redaction.WORKSPACE_TOKEN}/w"
            )


if __name__ == "__main__":
    unittest.main()
