#!/usr/bin/env python3
"""Network-free tests for the env the validate systemd unit inherits.

WHY THIS EXISTS. The unit gets a DELIBERATELY MINIMAL environment (systemd-run
does not inherit the caller's), so anything the build needs must be passed
explicitly. libunwind is not installed system-wide on this host class, and when
its three variables are missing the workspace build dies INSIDE the unit --
which surfaces as a failed DAG lane, i.e. it reads as a product red when it is
an environment fault. Worse, cache-warming cannot paper over it: unwind-sys's
build.rs emits `cargo:rerun-if-env-changed=PKG_CONFIG_PATH`, so cargo re-runs it
precisely when the variable is absent.

The three variables are NOT interchangeable, and that is the specific trap this
guards -- propagating only two still fails, just later and with a different
message:
  PKG_CONFIG_PATH  build time; without it build.rs panics on pkg-config.
  LIBRARY_PATH     LINK time; without it the link dies `rust-lld: error: unable
                   to find library -lunwind` even though build.rs succeeded.
  LD_LIBRARY_PATH  RUN time only. The loader is not the linker; this one alone
                   never fixes a link.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import start_unit as su  # noqa: E402


TARGET = "9" * 40


def setenvs(command: list[str]) -> dict[str, str]:
    """Extract the KEY=VALUE pairs systemd-run is told to set."""
    out: dict[str, str] = {}
    for flag, value in zip(command, command[1:]):
        if flag == "--setenv" and "=" in value:
            key, _, val = value.partition("=")
            out[key] = val
    return out


def build(root: Path, environment: dict[str, str]) -> list[str]:
    return su.build_systemd_command(
        root=root,
        checkout=root / "hermit",
        target=TARGET,
        agent="test-agent",
        unit="validate-test.service",
        log=root / "out.log",
        pr=None,
        validate_args=["full"],
        wait=1,
        hold=1,
        child_deadline=1,
        environment=environment,
    )


class StartUnitEnvTest(unittest.TestCase):
    def setUp(self) -> None:
        import tempfile

        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)
        self.lu = self.root / "ignored/lu-parity/usr/lib64"
        self.base_env = {"HOME": "/home/test", "PATH": "/usr/bin"}

    def plant_libunwind(self, *, shared_ptrace: bool = True) -> None:
        """Plant the in-repo libunwind tree.

        `shared_ptrace` controls whether it carries `libunwind-ptrace.so.0`.
        The real `ignored/lu-parity` does carry it, which is why the link picks
        the SHARED ptrace library and the loader then needs a directory that has
        it -- the coupling the runtime probe exists to serve.
        """
        (self.lu / "pkgconfig").mkdir(parents=True, exist_ok=True)
        (self.lu / "pkgconfig/libunwind-ptrace.pc").write_text("Name: libunwind-ptrace\n")
        (self.lu / "libunwind.so.8").write_text("")
        if shared_ptrace:
            (self.lu / "libunwind-ptrace.so.0").write_text("")

    def set_runtime_candidates(self, *candidates: Path | str) -> None:
        """Pin RUNTIME_CANDIDATES for the test.

        Without this the suite reads REAL absolute host paths (an fbsource
        checkout), so its result depends on the machine it runs on rather than on
        the code under test. Two tests here previously asserted the in-repo path
        while the probe returned the host's fbsource path, and they failed on any
        box that had fbsource -- a host-dependence bug in the test, not the code.
        """
        from unittest import mock

        patcher = mock.patch.object(su, "RUNTIME_CANDIDATES", tuple(str(c) for c in candidates))
        patcher.start()
        self.addCleanup(patcher.stop)

    # -- POSITIVE: the qualifying case fires, and fires COMPLETELY ------------

    def test_all_three_libunwind_vars_are_propagated(self) -> None:
        self.plant_libunwind()
        self.set_runtime_candidates()  # no external candidate: the repo tree serves
        env = setenvs(build(self.root, dict(self.base_env)))
        # LIBRARY_PATH is the one that was missing: build.rs would pass and the
        # LINK would then fail. Assert all three, not just the two obvious ones.
        for var in ("PKG_CONFIG_PATH", "LIBRARY_PATH", "LD_LIBRARY_PATH"):
            self.assertIn(var, env, f"{var} must reach the unit")
        self.assertEqual(env["PKG_CONFIG_PATH"], str(self.lu / "pkgconfig"))
        self.assertEqual(env["LIBRARY_PATH"], str(self.lu))
        self.assertEqual(env["LD_LIBRARY_PATH"], str(self.lu))

    def test_existing_caller_values_are_preserved_not_clobbered(self) -> None:
        self.plant_libunwind()
        self.set_runtime_candidates()
        env = setenvs(
            build(
                self.root,
                {**self.base_env, "LIBRARY_PATH": "/opt/x", "LD_LIBRARY_PATH": "/opt/y"},
            )
        )
        # Repo path first (it must win), caller's entries retained after it.
        self.assertEqual(env["LIBRARY_PATH"], f"{self.lu}:/opt/x")
        self.assertEqual(env["LD_LIBRARY_PATH"], f"{self.lu}:/opt/y")

    def test_repo_path_is_not_duplicated_when_already_present(self) -> None:
        self.plant_libunwind()
        env = setenvs(build(self.root, {**self.base_env, "LIBRARY_PATH": str(self.lu)}))
        self.assertEqual(env["LIBRARY_PATH"], str(self.lu))

    # -- NEGATIVE: absent libunwind must not fabricate anything --------------

    def test_absent_libunwind_sets_none_of_the_three(self) -> None:
        # No .pc planted. The unit must run exactly as before and let the build
        # fail loudly -- silently substituting a path would be worse than the bug.
        env = setenvs(build(self.root, dict(self.base_env)))
        for var in ("PKG_CONFIG_PATH", "LIBRARY_PATH", "LD_LIBRARY_PATH"):
            self.assertNotIn(var, env, f"{var} must not be invented when libunwind is absent")

    # -- the RUNTIME probe must pick a dir that can actually satisfy the loader -
    #
    # LIBRARY_PATH pointing at a tree with a SHARED libunwind-ptrace.so is what
    # makes the link depend on libunwind-ptrace.so.0 (measured: with neither var
    # the link resolves to the system STATIC .a and there is no runtime dep at
    # all). So the runtime directory must be chosen on that exact object. A tree
    # with libunwind.so.8 but no ptrace variant CANNOT satisfy it.

    def make_tree(self, name: str, *files: str) -> Path:
        d = self.root / "cand" / name
        d.mkdir(parents=True, exist_ok=True)
        for f in files:
            (d / f).write_text("")
        return d

    def test_runtime_probe_prefers_a_candidate_carrying_the_ptrace_object(self) -> None:
        # Unchanged behaviour: an external candidate that HAS the ptrace object
        # still wins over the repo tree, so this host's current selection stands.
        good = self.make_tree("good", "libunwind-ptrace.so.0", "libunwind.so.8")
        self.plant_libunwind()
        self.set_runtime_candidates(good)
        self.assertEqual(su._libunwind_runtime_dir(self.root, self.lu), good)

    def test_candidate_without_ptrace_object_never_beats_the_repo_tree(self) -> None:
        # THE REGRESSION THIS GUARDS. ~/.local/hermit-deps/lu/usr/lib64 ships only
        # a static libunwind-ptrace.a. It used to win on a libunwind.so.8
        # tiebreak, over a repo tree that DOES have libunwind-ptrace.so.0,
        # producing exactly the `libunwind-ptrace.so.0: cannot open shared object
        # file` failure the probe exists to prevent.
        static_only = self.make_tree("static_only", "libunwind.so.8", "libunwind-ptrace.a")
        self.plant_libunwind(shared_ptrace=True)
        self.set_runtime_candidates(static_only)
        self.assertEqual(
            su._libunwind_runtime_dir(self.root, self.lu),
            self.lu,
            "a candidate lacking libunwind-ptrace.so.0 must not be preferred over one that has it",
        )

    def test_repo_tree_is_used_when_no_candidate_exists_at_all(self) -> None:
        self.plant_libunwind(shared_ptrace=True)
        self.set_runtime_candidates("/nonexistent/one", "/nonexistent/two")
        self.assertEqual(su._libunwind_runtime_dir(self.root, self.lu), self.lu)

    def test_falls_back_to_base_library_only_when_no_ptrace_object_anywhere(self) -> None:
        # If nothing has the ptrace object the link will have used the static
        # variant, so a tree with the base library is still the best guess.
        base_only = self.make_tree("base_only", "libunwind.so.8")
        self.plant_libunwind(shared_ptrace=False)
        self.set_runtime_candidates(base_only)
        self.assertEqual(su._libunwind_runtime_dir(self.root, self.lu), base_only)

    # -- the pre-existing contract must survive ------------------------------

    def test_home_and_path_still_propagate(self) -> None:
        self.plant_libunwind()
        env = setenvs(build(self.root, dict(self.base_env)))
        self.assertEqual(env["HOME"], "/home/test")
        self.assertEqual(env["PATH"], "/usr/bin")
        self.assertEqual(env["CI_HUB_VALIDATE_PRODUCER"], "systemd-user-v1")

    def test_missing_home_or_path_is_refused(self) -> None:
        for missing in ("HOME", "PATH"):
            env = dict(self.base_env)
            del env[missing]
            with self.assertRaises(ValueError):
                build(self.root, env)

    # -- parent-owned validation authorities remain reachable -----------------

    def test_parent_root_is_propagated_for_shared_authorities(self) -> None:
        """DEV_HERMIT_PARENT reaches consumers that still need the workspace root.

        The ledger producer deliberately does not consult this variable: it uses
        one fixed adapter path even when DEV_HERMIT_PARENT is absent. Other
        parent-owned operations (receipt finalization, durable logs and ci-hub
        helpers) still need the workspace root, so the admitted unit propagates
        it explicitly.

        Asserting the exact path, not just presence: pointing at the wrong root
        would make those other authorities unreachable.
        """
        self.plant_libunwind()
        env = setenvs(build(self.root, dict(self.base_env)))
        self.assertIn(
            "DEV_HERMIT_PARENT", env, "the unit cannot reach parent-owned authorities"
        )
        self.assertEqual(env["DEV_HERMIT_PARENT"], str(self.root))

    def test_parent_ledger_is_set_even_without_libunwind(self) -> None:
        # The libunwind block is conditional; the ledger anchor must not be
        # accidentally nested inside it, or receipts vanish on hosts that have
        # libunwind installed system-wide.
        env = setenvs(build(self.root, dict(self.base_env)))
        self.assertEqual(env["DEV_HERMIT_PARENT"], str(self.root))

    def test_make_validate_entrypoint_is_literal_and_admitted(self) -> None:
        command = su.build_systemd_command(
            root=self.root,
            checkout=self.root / "hermit",
            target=TARGET,
            agent="owner-checkout-watch",
            unit="validate-owner.service",
            log=self.root / "owner.log",
            pr=None,
            validate_args=[],
            wait=1,
            hold=1,
            child_deadline=1,
            environment=dict(self.base_env),
            make_validate=True,
        )
        joined = " ".join(command)
        self.assertIn("validate-lock run", joined)
        self.assertIn("with-proxy make validate", joined)
        self.assertNotIn("./scripts/validate.rs", joined)

    def test_owner_symlink_exception_keeps_literal_make_and_marks_dirty_run(self) -> None:
        command = su.build_systemd_command(
            root=self.root,
            checkout=self.root / "hermit",
            target=TARGET,
            agent="owner-checkout-watch",
            unit="validate-owner.service",
            log=self.root / "owner.log",
            pr=None,
            validate_args=[],
            wait=1,
            hold=1,
            child_deadline=1,
            environment=dict(self.base_env),
            make_validate=True,
            allow_owner_validate_symlink=True,
        )
        joined = " ".join(command)
        self.assertIn("VALIDATE_RUN_ON_DIRTY_TREE=1", joined)
        self.assertIn("with-proxy make validate", joined)


if __name__ == "__main__":
    unittest.main()
