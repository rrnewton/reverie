#!/usr/bin/env python3
"""Tests for the validation CPU placement (https://github.com/rrnewton/hermit/issues/3265).

The derivation reads fixture sysfs trees, never the host's. The launch tests
prove each launcher carries the property; they do not start a unit.
``RealTransientUnitTest`` does start one, through the host's user manager, and
skips with the reason when this host cannot run it.
"""
from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

VALIDATE = Path(__file__).resolve().parents[1]
CI_HUB = VALIDATE.parent
sys.path.insert(0, str(VALIDATE))
import cpu_placement as cp  # noqa: E402
import run_registry  # noqa: E402
import start_unit as su  # noqa: E402


def _load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


pressure_schedule = _load(
    "pressure_schedule_for_cpu_placement", CI_HUB / "series" / "pressure_schedule.py"
)
integration_tip_validate = _load(
    "integration_tip_validate_for_cpu_placement",
    CI_HUB / "health" / "integration_tip_validate.py",
)

# devbig014's shape, scaled down: 64 online CPUs, uncore PMUs on CPU 0, one
# amd_l3 per 16-CPU complex, and a per-core PMU spanning half the host.
ONLINE_64 = "0-63\n"


class SysfsFixture:
    def __init__(self, root: Path, online: str | None = ONLINE_64) -> None:
        self.root = root
        self.pmu_root = root / "event_source" / "devices"
        self.pmu_root.mkdir(parents=True)
        self.online_path = root / "online"
        if online is not None:
            self.online_path.write_text(online, encoding="ascii")

    def pmu(self, name: str, cpumask: str | None) -> "SysfsFixture":
        directory = self.pmu_root / name
        directory.mkdir()
        # A core PMU (cpu, software, ...) has no cpumask file at all.
        if cpumask is not None:
            (directory / "cpumask").write_text(cpumask, encoding="ascii")
        return self

    def derive(self, environ: dict[str, str] | None = None) -> cp.CpuPlacement:
        return cp.derive(
            environ={} if environ is None else environ,
            pmu_root=self.pmu_root,
            online_path=self.online_path,
        )


def require_the_ceiling_before_a_huge_range(test: unittest.TestCase) -> None:
    """Fail fast, outside any subTest, unless both parsers refuse CPU 8192.

    Called before a test hands a huge range such as ``0-999999999`` to a
    parser: without the ceiling that range would be expanded into a
    billion-element set, so a regression must stop here instead of hanging or
    exhausting memory.
    """
    test.assertIsNone(cp.parse_cpu_list("0-8192"))
    record = {
        "source": "sysfs-none",
        "excluded_cpus": "",
        "allowed_cpus": "",
        "online_cpus": "0-8192",
        "pmus": [],
        "skipped_pmus": [],
        "detail": "ceiling probe",
        "override_env": cp.OVERRIDE_ENV,
    }
    with test.assertRaisesRegex(RuntimeError, "names CPU 8192, above 8191"):
        run_registry._cpu_placement(record)


class CpuListTest(unittest.TestCase):
    def test_parse_and_format_round_trip(self) -> None:
        self.assertEqual(cp.parse_cpu_list("0-3,8\n"), frozenset({0, 1, 2, 3, 8}))
        self.assertEqual(cp.parse_cpu_list(""), frozenset())
        self.assertEqual(cp.format_cpu_list(frozenset({0, 1, 2, 3, 8, 10, 11})), "0-3,8,10-11")
        self.assertEqual(cp.format_cpu_list(frozenset()), "")

    def test_malformed_lists_are_none(self) -> None:
        for text in ("x", "0-", "3-1", "0,,1", "-1", "0 1", "0x1", "1,2,"):
            with self.subTest(text=text):
                self.assertIsNone(cp.parse_cpu_list(text))

    def test_a_cpu_above_the_kernel_ceiling_is_malformed(self) -> None:
        require_the_ceiling_before_a_huge_range(self)
        self.assertEqual(cp.MAX_CPU_NUMBER, 8191)
        self.assertEqual(cp.parse_cpu_list("8191"), frozenset({8191}))
        self.assertEqual(cp.parse_cpu_list("08191"), frozenset({8191}))
        for text in ("8192", "0,8192", "0-999999999", "9" * 5000, "0-" + "9" * 5000):
            with self.subTest(text=text[:30]):
                self.assertIsNone(cp.parse_cpu_list(text))
                self.assertEqual(
                    cp.cpu_number_above_ceiling(text), text.rsplit("-", 1)[-1].rsplit(",", 1)[-1]
                )


class DeriveTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.sysfs = SysfsFixture(Path(self._tmp.name))

    def test_amd_df_cpumask_zero_excludes_exactly_cpu_zero(self) -> None:
        self.sysfs.pmu("amd_df", "0\n").pmu("cpu", None)
        placement = self.sysfs.derive()
        self.assertEqual(placement.source, "sysfs-pmu-cpumask")
        self.assertEqual(placement.excluded, frozenset({0}))
        self.assertEqual(placement.allowed, frozenset(range(1, 64)))
        self.assertEqual(placement.pmus, ("amd_df=0",))
        self.assertEqual(placement.systemd_property(), "AllowedCPUs=1-63")
        self.assertEqual(
            placement.environment(),
            {
                cp.ALLOWED_ENV: "1-63",
                cp.EXCLUDED_ENV: "0",
                cp.SOURCE_ENV: "sysfs-pmu-cpumask",
            },
        )

    def test_multiple_pmus_are_unioned_and_a_per_core_mask_is_skipped(self) -> None:
        (
            self.sysfs.pmu("amd_df", "0\n")
            .pmu("amd_iommu_0", "0\n")
            .pmu("amd_l3", "0,16,32,48\n")
            .pmu("amd_umc_0", "0\n")
            .pmu("power_core", "0-31\n")
            .pmu("software", None)
        )
        placement = self.sysfs.derive()
        self.assertEqual(placement.source, "sysfs-pmu-cpumask")
        self.assertEqual(placement.excluded, frozenset({0, 16, 32, 48}))
        self.assertEqual(
            placement.pmus,
            ("amd_df=0", "amd_iommu_0=0", "amd_l3=0,16,32,48", "amd_umc_0=0"),
        )
        self.assertEqual(len(placement.skipped_pmus), 1)
        self.assertTrue(placement.skipped_pmus[0].startswith("power_core=0-31 "))
        self.assertEqual(
            placement.systemd_property(), "AllowedCPUs=1-15,17-31,33-47,49-63"
        )

    def test_a_mask_at_exactly_a_quarter_still_counts(self) -> None:
        self.sysfs.pmu("quarter", "0-15\n")
        placement = self.sysfs.derive()
        self.assertEqual(placement.excluded, frozenset(range(16)))
        self.assertEqual(placement.skipped_pmus, ())

    def test_offline_cpus_in_a_mask_are_ignored(self) -> None:
        self.sysfs.pmu("amd_df", "0,70\n")
        self.assertEqual(self.sysfs.derive().excluded, frozenset({0}))

    def test_no_cpumask_files_exclude_nothing_and_say_so(self) -> None:
        self.sysfs.pmu("cpu", None).pmu("software", None).pmu("empty_mask", "\n")
        placement = self.sysfs.derive()
        self.assertEqual(placement.source, "sysfs-none")
        self.assertEqual(placement.excluded, frozenset())
        self.assertEqual(placement.allowed, frozenset())
        self.assertIsNone(placement.systemd_property())
        self.assertEqual(placement.systemd_properties(), [])
        self.assertEqual(placement.environment()[cp.ALLOWED_ENV], "")
        self.assertIn("nothing excluded", placement.detail)
        self.assertIsNone(cp.validate_record(placement.record()))

    def test_absent_pmu_directory_excludes_nothing_and_says_so(self) -> None:
        placement = cp.derive(
            environ={},
            pmu_root=Path(self._tmp.name) / "missing",
            online_path=self.sysfs.online_path,
        )
        self.assertEqual(placement.source, "sysfs-none")
        self.assertEqual(placement.excluded, frozenset())
        self.assertIn("absent", placement.detail)

    def test_malformed_cpumask_excludes_nothing_and_names_the_file(self) -> None:
        # One good mask plus one malformed: the good one must NOT survive alone.
        self.sysfs.pmu("amd_df", "0\n").pmu("zz_bad", "0-x\n")
        placement = self.sysfs.derive()
        self.assertEqual(placement.source, "sysfs-malformed")
        self.assertEqual(placement.excluded, frozenset())
        self.assertIsNone(placement.systemd_property())
        self.assertIn("zz_bad/cpumask", placement.detail)
        self.assertIn("'0-x\\n'", placement.detail)
        self.assertIsNone(cp.validate_record(placement.record()))

    def test_malformed_online_list_excludes_nothing(self) -> None:
        self.sysfs.online_path.write_text("garbage\n", encoding="ascii")
        self.sysfs.pmu("amd_df", "0\n")
        placement = self.sysfs.derive()
        self.assertEqual(placement.source, "sysfs-malformed")
        self.assertEqual(placement.excluded, frozenset())
        self.assertIn("online", placement.detail)

    def test_too_broad_a_union_excludes_nothing(self) -> None:
        for index in range(3):
            base = index * 12
            self.sysfs.pmu(f"narrow_{index}", f"{base}-{base + 11}\n")
        placement = self.sysfs.derive()
        self.assertEqual(placement.source, "sysfs-too-broad")
        self.assertEqual(placement.excluded, frozenset())
        self.assertEqual(len(placement.pmus), 3)

    def test_override_none_excludes_nothing(self) -> None:
        self.sysfs.pmu("amd_df", "0\n")
        placement = self.sysfs.derive({cp.OVERRIDE_ENV: "none"})
        self.assertEqual(placement.source, "override-none")
        self.assertEqual(placement.excluded, frozenset())
        self.assertIsNone(placement.systemd_property())

    def test_override_list_excludes_exactly_that_list(self) -> None:
        self.sysfs.pmu("amd_df", "0\n")
        placement = self.sysfs.derive({cp.OVERRIDE_ENV: "5,7-8,99"})
        self.assertEqual(placement.source, "override")
        self.assertEqual(placement.excluded, frozenset({5, 7, 8}))
        self.assertIn("99", placement.detail)

    def test_override_auto_and_empty_derive_from_sysfs(self) -> None:
        self.sysfs.pmu("amd_df", "0\n")
        for value in ("", "auto"):
            with self.subTest(value=value):
                placement = self.sysfs.derive({cp.OVERRIDE_ENV: value})
                self.assertEqual(placement.source, "sysfs-pmu-cpumask")

    def test_bad_override_refuses(self) -> None:
        for value in ("0-x", "0-63", "3-1"):
            with self.subTest(value=value):
                with self.assertRaises(cp.CpuPlacementError):
                    self.sysfs.derive({cp.OVERRIDE_ENV: value})
        self.assertTrue(issubclass(cp.CpuPlacementError, ValueError))

    def test_an_override_above_the_kernel_ceiling_refuses_before_expanding(self) -> None:
        self.sysfs.pmu("amd_df", "0\n")
        # 9000 alone is fast either way: without the ceiling it is merely
        # "not online and ignored", so this fails fast before the huge range.
        with self.assertRaisesRegex(
            cp.CpuPlacementError,
            rf"\A{cp.OVERRIDE_ENV} names CPU 9000, above 8191, the highest CPU number "
            "a Linux kernel can have; refused before expanding the list\\Z",
        ):
            self.sysfs.derive({cp.OVERRIDE_ENV: "5,9000"})
        require_the_ceiling_before_a_huge_range(self)
        for value, shown in (
            ("0-999999999", "999999999"),
            ("1," + "9" * 5000, "9" * 20 + "... (5000 digits)"),
        ):
            with self.subTest(value=value[:30]):
                with self.assertRaises(cp.CpuPlacementError) as caught:
                    self.sysfs.derive({cp.OVERRIDE_ENV: value})
                message = str(caught.exception)
                self.assertIn(f"names CPU {shown}, above 8191", message)
                self.assertIn("refused before expanding the list", message)
        # The ceiling itself is a CPU number a kernel can have: not refused.
        placement = self.sysfs.derive({cp.OVERRIDE_ENV: "5,8191"})
        self.assertEqual(placement.excluded, frozenset({5}))
        self.assertIn("not online and ignored: 8191", placement.detail)

    def test_cli_refuses_an_override_above_the_ceiling_with_exit_two(self) -> None:
        import contextlib
        import io
        from unittest import mock

        require_the_ceiling_before_a_huge_range(self)
        stderr = io.StringIO()
        with mock.patch.dict(os.environ, {cp.OVERRIDE_ENV: "0-999999999"}):
            with contextlib.redirect_stderr(stderr):
                self.assertEqual(cp.main(["--json"]), 2)
        self.assertIn("REFUSED", stderr.getvalue())
        self.assertIn("names CPU 999999999, above 8191", stderr.getvalue())

    def test_cli_refuses_a_bad_override_with_exit_two(self) -> None:
        import os
        from unittest import mock

        with mock.patch.dict(os.environ, {cp.OVERRIDE_ENV: "0-x"}):
            self.assertEqual(cp.main(["--json"]), 2)


class RecordTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.sysfs = SysfsFixture(Path(self._tmp.name))
        self.sysfs.pmu("amd_df", "0\n").pmu("amd_l3", "0,16\n")
        self.placement = self.sysfs.derive()

    def test_run_registry_field_set_matches_the_producer(self) -> None:
        self.assertEqual(run_registry.CPU_PLACEMENT_FIELDS, cp.RECORD_FIELDS)
        self.assertEqual(run_registry.CPU_PLACEMENT_SOURCES, cp.SOURCES)
        self.assertEqual(run_registry.CPU_PLACEMENT_MAX_CPU, cp.MAX_CPU_NUMBER)
        self.assertEqual(set(self.placement.record()), cp.RECORD_FIELDS)
        self.assertEqual(
            set(run_registry.CpuPlacementRecord.__annotations__), cp.RECORD_FIELDS
        )
        self.assertIn("cpu_placement", run_registry.VALIDATION_FIELDS)
        self.assertIn("cpu_placement", run_registry.BENCH_FIELDS)

    def test_integration_tip_override_name_matches_the_producer(self) -> None:
        self.assertEqual(
            integration_tip_validate.CPU_PLACEMENT_OVERRIDE_ENV, cp.OVERRIDE_ENV
        )

    def test_record_is_valid_json_and_self_consistent(self) -> None:
        record = json.loads(json.dumps(self.placement.record()))
        self.assertIsNone(cp.validate_record(record))
        self.assertEqual(record["excluded_cpus"], "0,16")
        self.assertEqual(record["allowed_cpus"], "1-15,17-63")
        self.assertEqual(record["online_cpus"], "0-63")
        run_registry._cpu_placement(record)

    def test_run_registry_rejects_a_damaged_record(self) -> None:
        require_the_ceiling_before_a_huge_range(self)
        good = self.placement.record()
        damaged = [
            {**good, "extra": 1},
            {key: value for key, value in good.items() if key != "detail"},
            {**good, "allowed_cpus": "1-x"},
            {**good, "allowed_cpus": ""},
            {**good, "pmus": "amd_df=0"},
            {**good, "source": ""},
            {**good, "source": "bogus"},
            {**good, "excluded_cpus": "0,16,70"},
            {**good, "allowed_cpus": "1-15"},
            {**good, "online_cpus": "0-999999999"},
            "not an object",
        ]
        for value in damaged:
            with self.subTest(value=value):
                with self.assertRaisesRegex(
                    RuntimeError, "validation-run-handle-cpu_placement"
                ):
                    run_registry._cpu_placement(value)
                self.assertIsNotNone(cp.validate_record(value))


    def test_each_consistency_rule_refuses_with_its_own_message(self) -> None:
        require_the_ceiling_before_a_huge_range(self)
        good = self.placement.record()
        self.assertEqual(
            (good["excluded_cpus"], good["allowed_cpus"], good["online_cpus"]),
            ("0,16", "1-15,17-63", "0-63"),
        )
        cases = (
            (
                {**good, "source": "bogus"},
                r"\Avalidation-run-handle-cpu_placement\.source: 'bogus' is not one of ",
                "source 'bogus' is not one of ",
            ),
            (
                # allowed still equals online - excluded; only the subset rule fails.
                {**good, "excluded_cpus": "0,16,70"},
                r"\Avalidation-run-handle-cpu_placement: excluded_cpus must be a subset "
                r"of online_cpus\Z",
                "excluded_cpus must be a subset of online_cpus",
            ),
            (
                {**good, "allowed_cpus": "1-15"},
                r"\Avalidation-run-handle-cpu_placement: allowed_cpus must equal "
                r"online_cpus minus excluded_cpus\Z",
                "allowed_cpus must equal online_cpus minus excluded_cpus",
            ),
            (
                {**good, "online_cpus": "0-999999999"},
                r"\Avalidation-run-handle-cpu_placement\.online_cpus: names CPU "
                r"999999999, above 8191\Z",
                "online_cpus must be a kernel CPU list string",
            ),
        )
        for value, registry_message, producer_message in cases:
            with self.subTest(value=value):
                with self.assertRaisesRegex(RuntimeError, registry_message):
                    run_registry._cpu_placement(value)
                problem = cp.validate_record(value)
                self.assertIsNotNone(problem)
                assert problem is not None
                self.assertTrue(problem.startswith(producer_message), problem)

    def test_every_derivation_source_writes_a_record_both_readers_accept(self) -> None:
        records: dict[str, dict] = {}
        tmp = Path(self._tmp.name)
        records["sysfs-pmu-cpumask"] = self.placement.record()
        records["sysfs-none"] = SysfsFixture(tmp / "none").derive().record()
        bad = SysfsFixture(tmp / "bad").pmu("zz", "0-x\n")
        records["sysfs-malformed"] = bad.derive().record()
        records["sysfs-malformed (online)"] = SysfsFixture(
            tmp / "online-bad", online="garbage\n"
        ).derive().record()
        broad = SysfsFixture(tmp / "broad")
        for index in range(3):
            broad.pmu(f"narrow_{index}", f"{index * 12}-{index * 12 + 11}\n")
        records["sysfs-too-broad"] = broad.derive().record()
        records["override"] = self.sysfs.derive({cp.OVERRIDE_ENV: "5,7-8,99"}).record()
        records["override-none"] = self.sysfs.derive({cp.OVERRIDE_ENV: "none"}).record()
        self.assertEqual(
            {record["source"] for record in records.values()}, set(cp.SOURCES)
        )
        for name, record in records.items():
            with self.subTest(source=name):
                self.assertIsNone(cp.validate_record(record))
                run_registry._cpu_placement(record)


class LaunchPropertyTest(unittest.TestCase):
    """Each launcher hands the placement to the unit it starts."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)
        sysfs = SysfsFixture(self.root / "sysfs")
        sysfs.pmu("amd_df", "0\n").pmu("amd_l3", "0,16,32,48\n")
        self.placement = sysfs.derive()
        self.environment = {
            "HOME": "/home/test",
            "PATH": "/usr/bin",
            "FLEET_REAL_GH": sys.executable,
        }

    def build(self, repo: str, placement: cp.CpuPlacement | None) -> list[str]:
        return su.build_systemd_command(
            root=self.root,
            checkout=self.root / "checkout",
            target="9" * 40,
            agent="test-agent",
            unit="validate-test.service",
            record=self.root / "run.json",
            log=self.root / "out.log",
            pr=None,
            validate_args=["full"],
            wait=1,
            hold=1,
            child_deadline=3600,
            environment=self.environment,
            repo=repo,
            placement=placement,
        )

    @staticmethod
    def setenvs(command: list[str]) -> dict[str, str]:
        out: dict[str, str] = {}
        for flag, value in zip(command, command[1:]):
            if flag == "--setenv" and "=" in value:
                key, _, val = value.partition("=")
                out[key] = val
        return out

    @staticmethod
    def properties(command: list[str]) -> list[str]:
        values = [value for flag, value in zip(command, command[1:]) if flag == "--property"]
        values += [arg.split("=", 1)[1] for arg in command if arg.startswith("--property=")]
        return values

    def test_validate_run_service_carries_allowed_cpus_and_the_scope_hand_off(self) -> None:
        command = self.build("rrnewton/hermit", self.placement)
        wrapped = command[: command.index("--")] if "--" in command else command
        self.assertIn("AllowedCPUs=1-15,17-31,33-47,49-63", self.properties(wrapped))
        env = self.setenvs(command)
        self.assertEqual(env[cp.ALLOWED_ENV], "1-15,17-31,33-47,49-63")
        self.assertEqual(env[cp.EXCLUDED_ENV], "0,16,32,48")
        self.assertEqual(env[cp.SOURCE_ENV], "sysfs-pmu-cpumask")

    def test_unrestricted_placement_adds_no_property_but_still_hands_off(self) -> None:
        sysfs = SysfsFixture(self.root / "empty-sysfs")
        placement = sysfs.derive()
        command = self.build("rrnewton/hermit", placement)
        self.assertFalse(
            any(value.startswith("AllowedCPUs=") for value in self.properties(command))
        )
        env = self.setenvs(command)
        self.assertEqual(env[cp.ALLOWED_ENV], "")
        self.assertEqual(env[cp.SOURCE_ENV], "sysfs-none")

    def test_no_placement_changes_nothing(self) -> None:
        with_placement = self.build("rrnewton/hermit", self.placement)
        without = self.build("rrnewton/hermit", None)
        added = [arg for arg in with_placement if arg not in without]
        self.assertEqual(
            sorted(added),
            sorted(
                [
                    "AllowedCPUs=1-15,17-31,33-47,49-63",
                    f"{cp.ALLOWED_ENV}=1-15,17-31,33-47,49-63",
                    f"{cp.EXCLUDED_ENV}=0,16,32,48",
                    f"{cp.SOURCE_ENV}=sysfs-pmu-cpumask",
                ]
            ),
        )
        self.assertEqual(len(with_placement) - len(without), 8)

    def test_pressure_test_unit_carries_allowed_cpus_and_the_scope_hand_off(self) -> None:
        command = pressure_schedule.launch_command(
            Path("/parent"),
            "/bin/systemd-run",
            "/bin/with-proxy",
            "a" * 40,
            Path("/parent/run.json"),
            {"PATH": "/bin"},
            placement=self.placement,
        )
        wrapped = command[: command.index("--")]
        self.assertIn("--property=AllowedCPUs=1-15,17-31,33-47,49-63", wrapped)
        self.assertIn(f"--setenv={cp.ALLOWED_ENV}=1-15,17-31,33-47,49-63", wrapped)
        self.assertIn(f"--setenv={cp.EXCLUDED_ENV}=0,16,32,48", wrapped)
        self.assertIn(f"--setenv={cp.SOURCE_ENV}=sysfs-pmu-cpumask", wrapped)

    def test_pressure_test_bad_override_is_a_value_error_before_launch(self) -> None:
        with self.assertRaises(ValueError):
            pressure_schedule.launch_command(
                Path("/parent"),
                "/bin/systemd-run",
                "/bin/with-proxy",
                "a" * 40,
                Path("/parent/run.json"),
                {"PATH": "/bin", cp.OVERRIDE_ENV: "0-x"},
            )

    def test_integration_tip_forwards_only_an_explicit_override(self) -> None:
        forward = integration_tip_validate.cpu_placement_override_setenv
        self.assertEqual(forward({}), [])
        self.assertEqual(
            forward({cp.OVERRIDE_ENV: "none"}), [f"--setenv={cp.OVERRIDE_ENV}=none"]
        )
        self.assertEqual(
            forward({cp.OVERRIDE_ENV: "0,16"}), [f"--setenv={cp.OVERRIDE_ENV}=0,16"]
        )



def _user_manager_cpuset_reason() -> str | None:
    """Why a real user unit cannot prove AllowedCPUs here, or None when it can."""
    if shutil.which("systemd-run") is None:
        return "systemd-run is not installed"
    probe = subprocess.run(
        ["systemd-run", "--user", "--quiet", "--wait", "--pipe", "--collect", "--", "true"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
        timeout=30,
        check=False,
    )
    if probe.returncode != 0:
        return f"no reachable systemd user manager: {probe.stderr.strip()[:200]}"
    uid = os.getuid()
    controllers = Path(
        f"/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/cgroup.controllers"
    )
    try:
        available = controllers.read_text(encoding="ascii").split()
    except OSError as error:
        return f"cannot read {controllers}: {error}"
    if "cpuset" not in available:
        return f"the cpuset controller is not delegated to the user manager ({controllers})"
    return None


class RealTransientUnitTest(unittest.TestCase):
    """A real ``systemd-run --user`` unit given the launcher's own argv words."""

    def setUp(self) -> None:
        reason = _user_manager_cpuset_reason()
        if reason is not None:
            self.skipTest(reason)

    def _unit_cpus_allowed(self, placement: cp.CpuPlacement) -> frozenset[int]:
        completed = subprocess.run(
            [
                "systemd-run",
                "--user",
                "--quiet",
                "--wait",
                "--pipe",
                "--collect",
                *placement.systemd_properties(),
                "--",
                "cat",
                "/proc/self/status",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=60,
            check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        lines = [
            line.split(":", 1)[1].strip()
            for line in completed.stdout.splitlines()
            if line.startswith("Cpus_allowed_list:")
        ]
        self.assertEqual(len(lines), 1, completed.stdout)
        parsed = cp.parse_cpu_list(lines[0])
        self.assertIsNotNone(parsed, lines[0])
        assert parsed is not None
        return parsed

    def test_override_zero_unit_never_sees_cpu_zero(self) -> None:
        placement = cp.derive(environ={cp.OVERRIDE_ENV: "0"})
        if not placement.allowed:
            self.skipTest("this host has only CPU 0 online")
        allowed = self._unit_cpus_allowed(placement)
        self.assertNotIn(0, allowed)
        self.assertEqual(allowed, placement.allowed)

    def test_host_derived_unit_excludes_exactly_the_derived_cpus(self) -> None:
        placement = cp.derive(environ={})
        if not placement.allowed:
            self.skipTest(f"this host derives no exclusion: {placement.summary()}")
        allowed = self._unit_cpus_allowed(placement)
        self.assertEqual(allowed, placement.allowed)
        self.assertFalse(allowed & placement.excluded)

    def test_control_without_the_property_the_unit_can_reach_cpu_zero(self) -> None:
        # The control for the two tests above: the same unit, launched with
        # override=none, still reaches CPU 0, so CPU 0's absence there is the
        # property's doing and not the user manager's own restriction.
        placement = cp.derive(environ={cp.OVERRIDE_ENV: "none"})
        self.assertEqual(placement.systemd_properties(), [])
        if 0 not in placement.online:
            self.skipTest("CPU 0 is not online on this host")
        self.assertIn(0, self._unit_cpus_allowed(placement))


if __name__ == "__main__":
    unittest.main()
