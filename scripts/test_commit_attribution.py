#!/usr/bin/env python3
"""Both-direction tests for machine-bearing commit attribution."""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts" / "check_commit_attribution.py"
DISPATCHER = ROOT / ".githooks" / "dev-hermit-hook-dispatch"
COMMIT_HOOK = ROOT / ".githooks" / "commit-msg"
INSTALLER = ROOT / "scripts" / "setup-hooks.sh"
TRAILER_030 = "[impl agent, gpt-5.6-sol] [hermit-902, devbig030]"
TRAILER_014 = "[coordinator, claude-opus-5] [hermit-coord, devbig014]"
DISCLOSURE_030 = "[hermit-902, fixture-agent, gpt-5.6-sol, devbig030, role=impl]"
DISCLOSURE_014 = (
    "[hermit-coord, fixture-agent, claude-opus-5, devbig014, role=coordinator]"
)
QUERY_030 = (
    r"^\[(impl agent|adversarial-reviewer agent|coordinator|Human), [^][]+\] "
    r"\[[^][]+, devbig030\]$"
)


def run(*args: str, cwd: Path | None = None, env: dict[str, str] | None = None):
    merged = dict(os.environ, **(env or {}))
    return subprocess.run(
        args, cwd=cwd, env=merged, text=True, capture_output=True, check=False
    )


def tagged(subject: str, attribution: str = DISCLOSURE_030) -> str:
    return f"{subject}\n\n{attribution}\n"


class MessageValidationTests(unittest.TestCase):
    def check(self, message: str, *extra: str):
        return subprocess.run(
            (sys.executable, str(CHECKER), "--stdin", *extra),
            input=message,
            text=True,
            capture_output=True,
            check=False,
            cwd=ROOT,
            env=dict(os.environ, HERMIT_COMMIT_MACHINE="devbig030"),
        )

    def test_positive_exact_machine_is_accepted(self) -> None:
        result = self.check(tagged("change"), "--expected-machine", "auto")
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("machines=devbig030", result.stdout)

    def test_positive_other_machine_is_valid_when_preserving_provenance(self) -> None:
        result = self.check(tagged("change", DISCLOSURE_014))
        self.assertEqual(0, result.returncode, result.stderr)

    def test_negative_wrong_writer_machine_is_refused(self) -> None:
        result = self.check(
            tagged("change", DISCLOSURE_014), "--expected-machine", "auto"
        )
        self.assertEqual(1, result.returncode)
        self.assertIn("claims machine devbig014", result.stderr)

    def test_negative_missing_machine_field_is_refused(self) -> None:
        result = self.check("change\n\n[hermit-902, fixture-agent, model, role=impl]\n")
        self.assertEqual(1, result.returncode)
        self.assertIn("is malformed", result.stderr)

    def test_negative_machine_embedded_in_team_slug_is_refused(self) -> None:
        result = self.check(
            "change\n\n[hermit-902-devbig030, fixture-agent, model, role=impl]\n"
        )
        self.assertEqual(1, result.returncode)

    def test_negative_canonical_attribution_not_first_in_body_is_refused(self) -> None:
        result = self.check(
            f"change\n\nPlain Language Summary\n\n{DISCLOSURE_030}\n\n"
            f"{TRAILER_030}\n"
        )
        self.assertEqual(1, result.returncode)
        self.assertIn("not the first commit-body line", result.stderr)

    def test_negative_canonical_attribution_in_subject_is_refused(self) -> None:
        result = self.check(f"{DISCLOSURE_030}\n\nbody\n")
        self.assertEqual(1, result.returncode)
        self.assertIn("not the first commit-body line", result.stderr)

    def test_negative_unknown_role_is_refused(self) -> None:
        result = self.check(
            "change\n\n[hermit-902, fixture-agent, model, devbig030, role=owner]\n"
        )
        self.assertEqual(1, result.returncode)
        self.assertIn("role must be one of", result.stderr)

    def test_negative_wrong_writer_agent_is_refused(self) -> None:
        result = self.check(tagged("change"), "--expected-agent", "different-agent")
        self.assertEqual(1, result.returncode)
        self.assertIn("fixture-agent", result.stderr)
        self.assertIn("different-agent", result.stderr)

    def test_positive_prose_after_first_body_disclosure_is_accepted(self) -> None:
        result = self.check(
            tagged("change") + "\nPlain Language Summary\n\nThe change.\n"
        )
        self.assertEqual(0, result.returncode, result.stderr)

    def test_positive_legacy_final_line_remains_explicitly_accepted(self) -> None:
        result = self.check(
            f"change\n\nbody\n\n{TRAILER_030}\n", "--expected-machine", "auto"
        )
        self.assertEqual(0, result.returncode, result.stderr)

    def test_positive_legacy_form_keeps_its_original_position_compatibility(
        self,
    ) -> None:
        result = self.check(TRAILER_030, "--expected-machine", "auto")
        self.assertEqual(0, result.returncode, result.stderr)

    def test_positive_consistent_hybrid_is_accepted(self) -> None:
        result = self.check(
            f"change\n\n{DISCLOSURE_030}\n\nbody\n\n{TRAILER_030}\n",
            "--expected-machine",
            "auto",
            "--expected-agent",
            "fixture-agent",
        )
        self.assertEqual(0, result.returncode, result.stderr)

    def test_positive_historical_hybrid_team_difference_is_accepted(self) -> None:
        result = self.check(
            f"change\n\n{DISCLOSURE_030}\n\nbody\n\n"
            "[impl agent, gpt-5.6-sol] [hermit-902-dagrun, devbig030]\n"
        )
        self.assertEqual(0, result.returncode, result.stderr)

    def test_negative_conflicting_hybrid_is_refused(self) -> None:
        result = self.check(
            f"change\n\n{DISCLOSURE_030}\n\nbody\n\n"
            "[impl agent, gpt-5.6-sol] [hermit-902, devbig014]\n"
        )
        self.assertEqual(1, result.returncode)
        self.assertIn("conflicts with the canonical", result.stderr)

    def test_negative_exact_duplicate_canonical_disclosure_is_refused(self) -> None:
        result = self.check(f"change\n\n{DISCLOSURE_030}\n\nbody\n\n{DISCLOSURE_030}\n")
        self.assertEqual(1, result.returncode)
        self.assertIn("must appear exactly once", result.stderr)

    def test_negative_conflicting_later_canonical_disclosure_is_refused(self) -> None:
        result = self.check(
            f"change\n\n{DISCLOSURE_030}\n\nbody\n\n"
            "[other-team, other-agent, unresolved, devbig014, role=reviewer]\n"
        )
        self.assertEqual(1, result.returncode)
        self.assertIn("must appear exactly once", result.stderr)

    def test_negative_malformed_later_canonical_candidate_is_refused(self) -> None:
        result = self.check(
            f"change\n\n{DISCLOSURE_030}\n\nbody\n\n"
            "[other-team, other-agent, unresolved, role=reviewer]\n"
        )
        self.assertEqual(1, result.returncode)
        self.assertIn("must appear exactly once", result.stderr)

    def test_positive_trailing_editor_comments_do_not_hide_the_final_line(self) -> None:
        result = self.check(tagged("change") + "\n# Changes to be committed:\n")
        self.assertEqual(0, result.returncode, result.stderr)


class RepositoryRangeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.repo = Path(self.temp.name)
        run("git", "init", "-q", "-b", "main", cwd=self.repo)
        run("git", "config", "user.name", "test", cwd=self.repo)
        run("git", "config", "user.email", "test@example.invalid", cwd=self.repo)

    def tearDown(self) -> None:
        self.temp.cleanup()

    def commit(self, subject: str, trailer: str | None) -> str:
        path = self.repo / f"f-{subject}"
        path.write_text(subject)
        run("git", "add", "--", path.name, cwd=self.repo)
        args = ["git", "-c", "core.hooksPath=/dev/null", "commit", "-q", "-m", subject]
        if trailer is not None:
            args.extend(("-m", trailer))
        result = run(*args, cwd=self.repo)
        self.assertEqual(0, result.returncode, result.stderr)
        return run("git", "rev-parse", "HEAD", cwd=self.repo).stdout.strip()

    def test_range_refuses_one_missing_commit(self) -> None:
        base = self.commit("base", TRAILER_014)
        self.commit("good", TRAILER_030)
        self.commit("bad", None)
        result = run(
            sys.executable,
            str(CHECKER),
            "--repo",
            str(self.repo),
            "--range",
            f"{base}..HEAD",
        )
        self.assertEqual(1, result.returncode)
        self.assertIn(
            "bad", run("git", "log", "-1", "--format=%s", cwd=self.repo).stdout
        )

    def test_range_does_not_retroactively_reject_a_historical_author(self) -> None:
        base = self.commit("base", TRAILER_014)
        self.commit("collaborator", TRAILER_030)
        result = run(
            sys.executable,
            str(CHECKER),
            "--repo",
            str(self.repo),
            "--range",
            f"{base}..HEAD",
        )
        self.assertEqual(0, result.returncode, result.stderr)

    def test_git_query_separates_devbig030_from_devbig014(self) -> None:
        sha_014 = self.commit("other-team", TRAILER_014)
        sha_030 = self.commit("our-team", TRAILER_030)
        result = run(
            "git",
            "log",
            "--all",
            "--extended-regexp",
            f"--grep={QUERY_030}",
            "--format=%H",
            cwd=self.repo,
        )
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertEqual([sha_030], result.stdout.splitlines())
        self.assertNotIn(sha_014, result.stdout)


class HookAndInstallerTests(unittest.TestCase):
    def hooked_repo(self, repo: Path, email: str, name: str = "Ryan Newton") -> None:
        run("git", "init", "-q", "-b", "main", cwd=repo)
        run("git", "config", "user.name", name, cwd=repo)
        run("git", "config", "user.email", email, cwd=repo)
        hooks = repo / ".githooks"
        scripts = repo / "scripts"
        hooks.mkdir()
        scripts.mkdir()
        shutil.copy2(COMMIT_HOOK, hooks / "commit-msg")
        shutil.copy2(CHECKER, scripts / CHECKER.name)
        (hooks / "commit-msg").chmod(0o755)
        (scripts / CHECKER.name).chmod(0o755)
        run("git", "config", "core.hooksPath", ".githooks", cwd=repo)

    def real_commit(self, repo: Path, *extra: str):
        path = repo / "tracked"
        path.write_text("content\n")
        run("git", "add", "--", path.name, cwd=repo)
        return run(
            "git",
            "commit",
            "-q",
            *extra,
            "-m",
            tagged("identity bracket"),
            cwd=repo,
            env={
                "HERMIT_COMMIT_MACHINE": "devbig030",
                "DG_AGENT_NAME": "fixture-agent",
            },
        )

    def test_real_commit_accepts_owner_approved_oss_email(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.hooked_repo(repo, "rrnewton@gmail.com")
            result = self.real_commit(repo)
            self.assertEqual(0, result.returncode, result.stderr)
            author = run("git", "show", "-s", "--format=%ae", cwd=repo)
            self.assertEqual("rrnewton@gmail.com", author.stdout.strip())

    def test_real_commit_accepts_reserved_fixture_identity(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.hooked_repo(repo, "test@example.com", "Test")
            result = self.real_commit(repo)
            self.assertEqual(0, result.returncode, result.stderr)
            author = run("git", "show", "-s", "--format=%an <%ae>", cwd=repo)
            self.assertEqual("Test <test@example.com>", author.stdout.strip())

    def test_real_commit_refuses_a_retyped_canonical_agent(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.hooked_repo(repo, "test@example.com", "Test")
            path = repo / "tracked"
            path.write_text("content\n")
            run("git", "add", "--", path.name, cwd=repo)
            result = run(
                "git",
                "commit",
                "-q",
                "-m",
                tagged("identity bracket"),
                cwd=repo,
                env={
                    "HERMIT_COMMIT_MACHINE": "devbig030",
                    "DG_AGENT_NAME": "actual-agent",
                },
            )
            self.assertEqual(1, result.returncode)
            self.assertIn("fixture-agent", result.stderr)
            self.assertIn("actual-agent", result.stderr)

    def test_real_commit_refuses_owner_name_with_reserved_fixture_email(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.hooked_repo(repo, "test@example.com")
            result = self.real_commit(repo)
            self.assertEqual(1, result.returncode)
            self.assertIn("reserved example addresses are fixture-only", result.stderr)

    def test_real_commit_refuses_fabricated_repo_email(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.hooked_repo(repo, "rrnewton@meta.com")
            result = self.real_commit(repo)
            self.assertEqual(1, result.returncode)
            self.assertIn("<rrnewton@meta.com>", result.stderr)
            self.assertIn("owner-approved", result.stderr)

    def test_real_commit_refuses_fabricated_explicit_author(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.hooked_repo(repo, "rrnewton@gmail.com")
            result = self.real_commit(repo, "--author=Ryan Newton <rrnewton@meta.com>")
            self.assertEqual(1, result.returncode)
            self.assertIn("<rrnewton@meta.com>", result.stderr)

    def test_dispatcher_refuses_a_wrong_machine_before_repo_hooks(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.hooked_repo(repo, "test@example.com", "Test")
            hook = repo / ".githooks" / "commit-msg"
            message = repo / "message"
            message.write_text(tagged("change", DISCLOSURE_014))
            result = run(
                str(hook),
                str(message),
                cwd=repo,
                env={
                    "HERMIT_COMMIT_MACHINE": "devbig030",
                    "DG_AGENT_NAME": "fixture-agent",
                },
            )
            self.assertEqual(1, result.returncode)
            self.assertIn("claims machine devbig014", result.stderr)

    def test_installer_covers_parent_and_submodule_common_dirs(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "scripts").mkdir()
            (root / ".githooks").mkdir()
            shutil.copy2(CHECKER, root / "scripts" / CHECKER.name)
            shutil.copy2(INSTALLER, root / "scripts" / INSTALLER.name)
            shutil.copy2(DISPATCHER, root / ".githooks" / DISPATCHER.name)
            for path in (
                root / "scripts" / CHECKER.name,
                root / "scripts" / INSTALLER.name,
                root / ".githooks" / DISPATCHER.name,
            ):
                path.chmod(0o755)
            run("git", "init", "-q", "-b", "main", cwd=root)
            child = root / "child"
            run("git", "init", "-q", "-b", "main", str(child), cwd=root)
            run(
                "git",
                "config",
                "-f",
                ".gitmodules",
                "submodule.child.path",
                "child",
                cwd=root,
            )
            for repo in (root, child):
                (repo / ".githooks").mkdir(exist_ok=True)
                local_hook = repo / ".githooks" / "pre-commit"
                local_hook.write_text("#!/bin/sh\necho delegated\n")
                local_hook.chmod(0o755)

            installed = run(
                str(root / "scripts" / INSTALLER.name),
                cwd=root,
                env={"HERMIT_COMMIT_MACHINE": "devbig030"},
            )
            self.assertEqual(0, installed.returncode, installed.stderr)
            self.assertIn("repositories=2", installed.stdout)
            for repo in (root, child):
                configured = run(
                    "git", "config", "--get", "core.hooksPath", cwd=repo
                ).stdout.strip()
                self.assertTrue(configured.endswith("/dev-hermit-hooks"), configured)
                delegated = run(str(Path(configured) / "pre-commit"), cwd=repo)
                self.assertEqual("delegated", delegated.stdout.strip())
                checked = run(
                    str(root / "scripts" / INSTALLER.name),
                    "--check",
                    cwd=root,
                    env={"HERMIT_COMMIT_MACHINE": "devbig030"},
                )
                self.assertEqual(0, checked.returncode, checked.stderr)


sys.path.insert(0, str(ROOT / "scripts"))
import check_commit_attribution as cca  # noqa: E402


class AgentIdentityDisjointness(unittest.TestCase):
    """Both-direction tests for identity carried by legacy Agent trailers.

    Measured 2026-08-11 across 32 PRs and 252 main commits: 8 of 32 PRs carried
    more than one team slug, the model field held 10 spellings for at most 3
    real families, and 24 commits put a non-model (``automation``,
    ``primary-checkout``) in the model slot.  Neither field separates one agent
    under two slugs from two genuinely distinct agents, which is exactly the
    discrimination author-disjointness needs.
    """

    def test_one_agent_twice_does_not_satisfy_two_reviewers(self):
        # NEGATIVE bracket: the bypass this exists to close.
        self.assertFalse(cca.reviewers_are_disjoint(["codex-1", "codex-1"]))

    def test_one_agent_under_two_cases_is_still_one_agent(self):
        # The cheap half: casing must not manufacture a second reviewer.
        self.assertFalse(cca.reviewers_are_disjoint(["claude", "CLAUDE"]))
        self.assertEqual(
            cca.canonical_identity("CLAUDE"), cca.canonical_identity("claude")
        )

    def test_two_distinct_agents_are_still_distinct(self):
        # POSITIVE bracket: over-tightening must not collapse real reviewers.
        self.assertTrue(cca.reviewers_are_disjoint(["codex-1", "claude-1"]))

    def test_trailing_ordinals_are_not_normalised_away(self):
        # codex-1635 and codex-2 are two real agents seen on main tonight; a
        # normaliser that strips ordinals would merge them and defeat the gate
        # in the opposite direction.
        self.assertTrue(cca.reviewers_are_disjoint(["codex-1635", "codex-2"]))
        self.assertNotEqual(
            cca.canonical_identity("codex-1635"), cca.canonical_identity("codex-2")
        )

    def test_unknown_identity_fails_closed(self):
        # A missing Agent trailer must never count as an anonymous extra
        # reviewer.
        self.assertFalse(cca.reviewers_are_disjoint(["codex-1", None]))
        self.assertFalse(cca.reviewers_are_disjoint(["codex-1", "  "]))

    def test_agent_trailer_is_parsed_from_a_message(self):
        message = f"subject\n\nbody\n\nAgent: codex-1\nTask: t\n{TRAILER_014}\n"
        self.assertEqual("codex-1", cca.agent_from_message(message))

    def test_absent_agent_trailer_is_accepted_for_incremental_adoption(self):
        message = f"subject\n\nbody\n{TRAILER_014}\n"
        self.assertIsNone(cca.agent_from_message(message))
        # Must not raise: absence is not a false claim.
        cca.validate_message(message, "devbig014", "codex-1")

    def test_a_retyped_agent_trailer_is_rejected(self):
        # The spoof: claiming an identity the writer's environment contradicts.
        message = f"subject\n\nAgent: someone-else\n{TRAILER_014}\n"
        with self.assertRaises(cca.AttributionError) as caught:
            cca.validate_message(message, "devbig014", "codex-1")
        self.assertIn("someone-else", str(caught.exception))

    def test_agent_trailer_matching_the_writer_is_accepted(self):
        message = f"subject\n\nAgent: codex-1\n{TRAILER_014}\n"
        self.assertEqual(
            "devbig014", cca.validate_message(message, "devbig014", "codex-1")
        )

    def test_detect_agent_reads_the_environment_not_the_message(self):
        previous = os.environ.get("DG_AGENT_NAME")
        try:
            os.environ["DG_AGENT_NAME"] = "codex-7"
            self.assertEqual("codex-7", cca.detect_agent())
            del os.environ["DG_AGENT_NAME"]
            self.assertIsNone(cca.detect_agent())
        finally:
            if previous is None:
                os.environ.pop("DG_AGENT_NAME", None)
            else:
                os.environ["DG_AGENT_NAME"] = previous


class ModelFamilyIsDerivedNotTyped(unittest.TestCase):
    """The model field was the one identity field nothing derived.

    ``machine`` comes from the hostname and the canonical agent field (or
    legacy ``Agent:`` trailer) from ``DG_AGENT_NAME``; both are refused when
    retyped.  ``model`` was any bracket-free text -- and it is the only
    machine-readable discriminator between the Claude-family and Codex-family
    approvals ``post-facto-review`` requires.
    """

    def check(self, model: str, *extra: str, env: dict[str, str] | None = None):
        message = (
            f"subject\n\n[hermit2, fixture-agent, {model}, " "devbig014, role=impl]\n"
        )
        return subprocess.run(
            (sys.executable, str(CHECKER), "--stdin", *extra),
            input=message,
            text=True,
            capture_output=True,
            check=False,
            cwd=ROOT,
            env=dict(os.environ, HERMIT_COMMIT_MACHINE="devbig014", **(env or {})),
        )

    # ⚠️ THE CELL THAT MUST NOT BE DELETED, AND THE ONE THAT WOULD HAVE CAUGHT
    # THE DEFECT I SHIPPED WHILE WRITING THIS. My first version loaded
    # ``agent_identity`` without registering it in ``sys.modules``, so its first
    # ``@dataclass`` raised, the deliberately-total ``except`` swallowed it, and
    # ``detect_model_family()`` returned None -- "no family could be
    # established".  Every mislabelled trailer PASSED and the output was
    # indistinguishable from having no violations.  A check that cannot report
    # its own unavailability is an assumed guard, which is worse than a missing
    # one, because nobody looks again.
    def test_the_derivation_resolves_on_this_host(self) -> None:
        family = checker_module().detect_model_family()
        self.assertIsNotNone(
            family,
            "the launcher family could not be derived from /proc, so the family "
            "check is INERT here and every mislabelled trailer would pass",
        )
        self.assertIn(family, ("claude", "codex"))

    def test_a_codex_model_from_a_claude_launcher_is_refused_by_name(self) -> None:
        result = self.check("gpt-5.6-sol", "--expected-family", "claude")
        self.assertEqual(1, result.returncode, result.stdout)
        for token in ("gpt-5.6-sol", "codex family", "claude client"):
            self.assertIn(token, result.stderr)

    def test_a_claude_model_from_a_codex_launcher_is_refused_by_name(self) -> None:
        result = self.check("claude-opus-5", "--expected-family", "codex")
        self.assertEqual(1, result.returncode, result.stdout)
        for token in ("claude-opus-5", "claude family", "codex client"):
            self.assertIn(token, result.stderr)

    def test_a_matching_family_is_accepted_both_ways(self) -> None:
        for model, family in (("claude-opus-5", "claude"), ("gpt-5.6-sol", "codex")):
            with self.subTest(model=model):
                result = self.check(model, "--expected-family", family)
                self.assertEqual(0, result.returncode, result.stderr)

    # ⚠️ 583 OF THE LAST 3000 COMMITS CARRY ONE OF THESE, so an unrecognised
    # model must be an ABSENCE of a claim rather than a violation.  Refusing
    # them would stop routine work to enforce a label.
    def test_a_model_naming_no_family_is_accepted(self) -> None:
        for model in ("automation", "unresolved", "primary-checkout"):
            for family in ("claude", "codex"):
                with self.subTest(model=model, family=family):
                    result = self.check(model, "--expected-family", family)
                    self.assertEqual(0, result.returncode, result.stderr)

    # ⚠️ NAMING BOTH IS UNRESOLVED, NOT A PICK.  Forcing one would refuse an
    # honest commit, and a gate that refuses honest commits gets switched off.
    def test_a_model_naming_both_families_is_accepted(self) -> None:
        result = self.check("claude-via-gpt-bridge", "--expected-family", "claude")
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIsNone(
            checker_module().declared_model_family("claude-via-gpt-bridge")
        )

    # ⚠️ WITHOUT THE FLAG NOTHING IS CHECKED, which is what keeps this file's
    # verdict independent of who runs it.
    # ⚠️ A RECORDED COMMIT IS NEVER FAMILY-CHECKED, and this cell is what keeps
    # it that way. `expected_family` is derived from THIS process's ancestry,
    # which is the writer only while a commit is being created. Applied to a
    # range it would compare a claude reviewer's launcher against a codex
    # agent's commit from last week and refuse it -- the same category error as
    # checking a hostname against somebody else's machine.
    def test_a_recorded_commit_is_never_family_checked(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            repo = Path(name)
            run("git", "init", "-q", "-b", "main", cwd=repo)
            run("git", "config", "user.name", "test", cwd=repo)
            run("git", "config", "user.email", "test@example.com", cwd=repo)
            (repo / "f").write_text("x")
            run("git", "add", "f", cwd=repo)
            run(
                "git",
                "commit",
                "-q",
                "--no-verify",
                "-m",
                f"subject\n\n{TRAILER_030}",
                cwd=repo,
            )
            result = subprocess.run(
                (
                    sys.executable,
                    str(CHECKER),
                    "--commit",
                    "HEAD",
                    "--repo",
                    str(repo),
                    "--expected-family",
                    "claude",
                ),
                text=True,
                capture_output=True,
                check=False,
                cwd=ROOT,
                env=dict(os.environ, HERMIT_COMMIT_MACHINE="devbig030"),
            )
        self.assertEqual(0, result.returncode, result.stderr)

    def test_no_flag_means_no_family_check(self) -> None:
        result = self.check("gpt-5.6-sol")
        self.assertEqual(0, result.returncode, result.stderr)

    # ⚠️ AN EMPTY OVERRIDE DERIVES; IT DOES NOT DISABLE.  An environment
    # variable that switches an identity check off is a bypass wearing the shape
    # of a test hook.
    def test_an_empty_override_does_not_disable_the_check(self) -> None:
        family = checker_module().detect_model_family()
        self.assertIn(family, ("claude", "codex"))
        wrong_model = "gpt-5.6-sol" if family == "claude" else "claude-opus-5"
        claimed_family = "codex" if family == "claude" else "claude"
        result = self.check(
            wrong_model,
            "--expected-family",
            "auto",
            env={"HERMIT_COMMIT_MODEL_FAMILY": ""},
        )
        self.assertEqual(1, result.returncode, result.stdout)
        self.assertIn(f"{claimed_family} family", result.stderr)
        self.assertIn(f"{family} client", result.stderr)


def checker_module():
    import importlib.machinery
    import importlib.util

    loader = importlib.machinery.SourceFileLoader("cca_under_test", str(CHECKER))
    spec = importlib.util.spec_from_loader("cca_under_test", loader)
    module = importlib.util.module_from_spec(spec)
    sys.modules["cca_under_test"] = module
    loader.exec_module(module)
    return module


if __name__ == "__main__":
    unittest.main()
