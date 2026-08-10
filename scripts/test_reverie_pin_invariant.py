#!/usr/bin/env python3
"""Tests for the recorded-gitlink reverie pin invariant.

Every case builds a throwaway parent + product repos, so nothing here touches
the real workspace, the network, or any shared state. The networked currency
legs (C, D) use local `file://`-style remotes, which exercise the same
`git ls-remote origin refs/heads/main` path without leaving the machine.

Bracketed both ways throughout: each violation case has a matching holds case,
because a checker that fails everything would pass a violations-only suite --
and this one genuinely reports 3 failing legs against the live workspace, so
"it printed FAIL" is not by itself evidence that it works.
"""

from __future__ import annotations

from io import StringIO
from pathlib import Path
import os
import subprocess
import tempfile
import unittest
import unittest.mock

from scripts import check_reverie_pin_invariant as inv


REVERIE_URL = "https://github.com/rrnewton/reverie.git"


def git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ("git", "-C", str(repo), *args), text=True, capture_output=True, check=True
    ).stdout.strip()


def manifest(rev: str) -> str:
    return (
        '[package]\nname = "hermit-test"\nversion = "0.1.0"\n\n'
        "[dependencies]\n"
        f'reverie = {{ git = "{REVERIE_URL}", rev = "{rev}" }}\n'
    )


class PinInvariantFixture(unittest.TestCase):
    """A miniature parent: hermit + reverie submodules, each with a remote."""

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        base = Path(self.temp.name)
        self.root = base / "parent"
        self.root.mkdir()
        self.remotes: dict[str, Path] = {}
        for product in ("hermit", "reverie"):
            remote = base / f"{product}.git"
            subprocess.run(("git", "init", "--bare", "--initial-branch=main", str(remote)),
                           check=True, capture_output=True)
            self.remotes[product] = remote
            repo = self.root / product
            subprocess.run(("git", "init", "--initial-branch=main", str(repo)),
                           check=True, capture_output=True)
            git(repo, "config", "user.email", "t@e.c")
            git(repo, "config", "user.name", "T")
            (repo / "README").write_text("x\n")
            git(repo, "add", "README")
            git(repo, "commit", "-m", "initial")
            git(repo, "remote", "add", "origin", str(remote))
            git(repo, "push", "origin", "main")

        self.reverie_head = git(self.root / "reverie", "rev-parse", "HEAD")
        # hermit's manifest pins whatever reverie's tip is -> the invariant holds
        (self.root / "hermit" / "Cargo.toml").write_text(manifest(self.reverie_head))
        git(self.root / "hermit", "add", "Cargo.toml")
        git(self.root / "hermit", "commit", "-m", "pin reverie")
        git(self.root / "hermit", "push", "origin", "main")

        subprocess.run(("git", "init", "--initial-branch=main", str(self.root)),
                       check=True, capture_output=True)
        git(self.root, "config", "user.email", "t@e.c")
        git(self.root, "config", "user.name", "T")
        git(self.root, "add", "hermit", "reverie")
        git(self.root, "commit", "-m", "record gitlinks")

    def tearDown(self) -> None:
        self.temp.cleanup()

    def run_check(self, offline: bool = False) -> tuple[list[str], str]:
        out = StringIO()
        violations = inv.check(self.root, offline=offline, out=out)
        return violations, out.getvalue()

    def advance(self, product: str) -> str:
        repo = self.root / product
        (repo / "README").write_text("next\n")
        git(repo, "add", "README")
        git(repo, "commit", "-m", "next")
        git(repo, "push", "origin", "main")
        return git(repo, "rev-parse", "HEAD")


class InvariantHoldsTests(PinInvariantFixture):
    """POSITIVE CONTROLS -- without these, a broken checker looks correct."""

    def test_all_legs_hold_on_a_coherent_parent(self) -> None:
        violations, text = self.run_check()
        self.assertEqual(violations, [], text)
        self.assertIn("holds on every leg", inv_summary(self.root))

    def test_offline_skips_only_the_networked_legs(self) -> None:
        violations, text = self.run_check(offline=True)
        self.assertEqual(violations, [])
        self.assertIn("[skip]", text)
        self.assertIn("A internal", text)
        self.assertIn("B coherence", text)


class InvariantViolationTests(PinInvariantFixture):
    """NEGATIVE CONTROLS -- one per leg, each planted independently."""

    def test_leg_a_two_distinct_revs_is_detected(self) -> None:
        second = self.root / "hermit" / "detcore" / "Cargo.toml"
        second.parent.mkdir(parents=True, exist_ok=True)
        second.write_text(manifest("a" * 40))
        git(self.root / "hermit", "add", "detcore/Cargo.toml")
        git(self.root / "hermit", "commit", "-m", "second rev")
        git(self.root, "add", "hermit")
        violations, text = self.run_check(offline=True)
        self.assertIn("A internal", violations)
        self.assertIn("2 distinct revs", text)

    def test_leg_b_manifest_disagreeing_with_the_reverie_gitlink_is_detected(self) -> None:
        # Advance reverie and record the new gitlink WITHOUT repinning the manifest.
        new = self.advance("reverie")
        git(self.root, "add", "reverie")
        violations, text = self.run_check(offline=True)
        self.assertIn("B coherence", violations)
        self.assertIn(new[:12], text)

    def test_leg_c_reverie_gitlink_behind_its_main_is_detected(self) -> None:
        self.advance("reverie")            # remote moves; gitlink stays put
        violations, _ = self.run_check()
        self.assertIn("C currency", violations)

    def test_leg_d_hermit_gitlink_behind_its_main_is_detected(self) -> None:
        self.advance("hermit")
        violations, _ = self.run_check()
        self.assertIn("D currency", violations)


class RecordedPinVsCheckoutTests(PinInvariantFixture):
    """The distinction the whole file exists for.

    A colleague who clones and runs `git submodule update --init` receives the
    RECORDED gitlink. An invariant read off this box's working tree would pass
    or fail on state nobody else can see -- which is exactly how the existing
    advisory check reports a different reverie rev (working tree) than the one
    actually recorded.
    """

    def test_a_dirty_working_tree_cannot_change_the_verdict(self) -> None:
        before, _ = self.run_check(offline=True)
        self.assertEqual(before, [])
        # Rewrite the manifest in the working tree only -- never committed.
        (self.root / "hermit" / "Cargo.toml").write_text(manifest("b" * 40))
        after, text = self.run_check(offline=True)
        self.assertEqual(after, [], f"a working-tree edit changed the verdict: {text}")
        self.assertNotIn("b" * 12, text)

    def test_a_committed_but_unrecorded_bump_cannot_change_the_verdict(self) -> None:
        # Commit inside the submodule but do NOT stage the parent gitlink.
        (self.root / "hermit" / "Cargo.toml").write_text(manifest("c" * 40))
        git(self.root / "hermit", "add", "Cargo.toml")
        git(self.root / "hermit", "commit", "-m", "unrecorded repin")
        violations, text = self.run_check(offline=True)
        self.assertEqual(violations, [], f"an unrecorded submodule commit leaked in: {text}")

    def test_a_staged_gitlink_bump_IS_seen(self) -> None:
        # The pre-commit consumer must see what the commit will record.
        self.advance("reverie")
        git(self.root, "add", "reverie")
        violations, _ = self.run_check(offline=True)
        self.assertIn("B coherence", violations)


class ExitCodeTests(PinInvariantFixture):
    def test_report_mode_exits_zero_and_strict_exits_one(self) -> None:
        self.advance("reverie")
        git(self.root, "add", "reverie")
        report = inv.main(["--root", str(self.root), "--offline"], out=StringIO())
        strict = inv.main(["--root", str(self.root), "--offline", "--strict"], out=StringIO())
        self.assertEqual(report, 0, "report mode must not fail a caller")
        self.assertEqual(strict, 1, "--strict must fail on a violation")

    def test_strict_exits_zero_when_the_invariant_holds(self) -> None:
        # Without this, --strict returning 1 proves nothing.
        self.assertEqual(inv.main(["--root", str(self.root), "--offline", "--strict"],
                                  out=StringIO()), 0)


class ProxyDisableFlagTests(unittest.TestCase):
    """PIN_INVARIANT_DISABLE_PROXY is VALUE-checked, not presence-checked.

    Bracketed four ways because three of them were broken: the presence form
    `os.environ.get(name)` is truthy for "0", so `DISABLE_PROXY=0` -- the
    natural way to write "do not disable" -- silently switched the proxy OFF.
    The inverse case matters just as much: a seam that never engages is as
    broken as one that always does, so "1" must really suppress the proxy.
    """

    def _command(self, value: str | None) -> list[str]:
        """The argv the networked git path would actually build."""
        captured: list[list[str]] = []

        def fake_run(command, **kwargs):
            captured.append(list(command))
            return subprocess.CompletedProcess(command, 0, "", "")

        env = dict(os.environ)
        env.pop("PIN_INVARIANT_DISABLE_PROXY", None)
        if value is not None:
            env["PIN_INVARIANT_DISABLE_PROXY"] = value
        with unittest.mock.patch.dict(os.environ, env, clear=True), \
             unittest.mock.patch.object(inv.shutil, "which", return_value="/usr/bin/with-proxy"), \
             unittest.mock.patch.object(inv.subprocess, "run", fake_run):
            inv.git(Path("/tmp"), "ls-remote", network=True)
        return captured[0]

    def test_unset_keeps_the_proxy(self) -> None:
        self.assertIn("/usr/bin/with-proxy", self._command(None))

    def test_zero_keeps_the_proxy(self) -> None:
        # THE REGRESSION. "0" is a non-empty string and therefore truthy, so
        # the old presence check disabled the proxy here.
        self.assertIn("/usr/bin/with-proxy", self._command("0"))

    def test_empty_keeps_the_proxy(self) -> None:
        self.assertIn("/usr/bin/with-proxy", self._command(""))

    def test_arbitrary_value_keeps_the_proxy(self) -> None:
        # Only the exact "1" disables; we do not guess at "true".
        self.assertIn("/usr/bin/with-proxy", self._command("true"))

    def test_one_actually_disables_the_proxy(self) -> None:
        # INVERSE CONTROL: the seam must still work when correctly set.
        self.assertNotIn("/usr/bin/with-proxy", self._command("1"))

    def test_predicate_directly(self) -> None:
        for value, want in ((None, False), ("", False), ("0", False), ("true", False), ("1", True)):
            env = dict(os.environ)
            env.pop("PIN_INVARIANT_DISABLE_PROXY", None)
            if value is not None:
                env["PIN_INVARIANT_DISABLE_PROXY"] = value
            with unittest.mock.patch.dict(os.environ, env, clear=True):
                self.assertEqual(inv.proxy_disabled(), want, f"value={value!r}")



def inv_summary(root: Path) -> str:
    out = StringIO()
    inv.main(["--root", str(root), "--offline"], out=out)
    return out.getvalue()


if __name__ == "__main__":
    unittest.main()
