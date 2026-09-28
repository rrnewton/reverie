"""A periodic launch must not start a launcher whose checkout hook cannot clone.

Measured 2026-09-28 on devbig014: ~/.gitconfig rewrote every Hermit submodule
URL to a local mirror, the systemd user manager carried no GIT_ALLOW_PROTOCOL,
and the launcher's `git submodule update --init --recursive` hook refused
"transport 'file' not allowed". The refused create left a wrkslots CREATE
journal that blocked every lane's wrkslots commands. These tests pin the
preflight that refuses such a launch before systemd-run, and prove against real
git that its verdict matches what the hook itself does. They also pin the fix:
the launcher unit gets GIT_ALLOW_PROTOCOL through its own `--setenv`, the
preflight probes that same unit environment, and without the allowance the
preflight still refuses.
"""

from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location(
    "integration_tip_validate_provisioning_target",
    ROOT / "ci-hub" / "health" / "integration_tip_validate.py",
)
driver = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = driver
SPEC.loader.exec_module(driver)

SHA = "1234567890abcdef1234567890abcdef12345678"
NETWORK_URL = "https://example.invalid/sub.git"


def settings(tmp_path: Path, source: Path | None = None):
    source = source or tmp_path / "hermit"
    source.mkdir(exist_ok=True)
    operational_tool = tmp_path / "operational-tool"
    operational_tool.write_text("#!/bin/sh\nexit 0\n")
    operational_tool.chmod(0o755)
    return driver.Settings.defaults(
        tool_root=ROOT,
        state_root=tmp_path,
        source_checkout=source,
        ci_hub=tmp_path / "ci-hub",
        operational_tool=operational_tool,
        tool_parent_sha="f" * 40,
        state_dir=tmp_path / "tick-state",
    )


def load_authorities():
    """Reuse the main suite's fake status and admission authorities."""

    spec = importlib.util.spec_from_file_location(
        "integration_tip_validate_main_suite_for_provisioning",
        Path(__file__).with_name("test_integration_tip_validate.py"),
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module.Authorities


def done(command, returncode=0, stdout="", stderr=""):
    return subprocess.CompletedProcess(list(command), returncode, stdout, stderr)


def git_arguments(command):
    """Strip the `env -i NAME=VALUE ... git` prefix the preflight adds."""

    assert command[:2] == ["env", "-i"], command
    index = command.index("git")
    return {
        name: value
        for name, _, value in (item.partition("=") for item in command[2:index])
    }, command[index + 1:]


class FakeGit:
    """Answer the preflight's questions without touching a real repository."""

    def __init__(
        self,
        *,
        manager="HOME=/home/fixture\n",
        manager_rc=0,
        gitmodules=f"submodule.sub.url {NETWORK_URL}\n",
        gitmodules_rc=0,
        gitmodules_stderr="",
        configured_url=None,
        expanded=None,
        probe_rc=0,
        probe_stderr="",
    ):
        self.manager = manager
        self.manager_rc = manager_rc
        self.gitmodules = gitmodules
        self.gitmodules_rc = gitmodules_rc
        self.gitmodules_stderr = gitmodules_stderr
        self.configured_url = configured_url
        self.expanded = expanded or {}
        self.probe_rc = probe_rc
        self.probe_stderr = probe_stderr
        self.commands: list[list[str]] = []
        self.probes: list[tuple[dict[str, str], list[str]]] = []

    def run(self, command, cwd, timeout):
        command = list(command)
        self.commands.append(command)
        assert timeout > 0
        if command == ["systemctl", "--user", "show-environment"]:
            return done(command, self.manager_rc, self.manager, "" if self.manager_rc == 0 else "no bus")
        if command[0] == "systemd-run":
            return done(command)
        environment, arguments = git_arguments(command)
        verb = arguments[2:]
        if verb[:2] == ["config", "--blob"]:
            assert verb[2] == f"{SHA}:.gitmodules"
            return done(command, self.gitmodules_rc, self.gitmodules, self.gitmodules_stderr)
        if verb[:2] == ["config", "--get"]:
            if self.configured_url is None:
                return done(command, 1)
            return done(command, 0, self.configured_url + "\n")
        if verb[:2] == ["ls-remote", "--get-url"]:
            return done(command, 0, self.expanded.get(verb[2], verb[2]) + "\n")
        if verb[0] == "ls-remote":
            self.probes.append((environment, verb))
            return done(command, self.probe_rc, "", self.probe_stderr)
        raise AssertionError(command)


def test_a_network_submodule_url_needs_no_probe(tmp_path: Path) -> None:
    fake = FakeGit()
    assert driver.launcher_provisioning_preflight(settings(tmp_path), SHA, 4.0, fake.run) == []
    assert fake.probes == [], "a network source is not the local transport this guards"


def test_a_target_without_submodules_passes(tmp_path: Path) -> None:
    fake = FakeGit(gitmodules="", gitmodules_rc=1)
    assert driver.launcher_provisioning_preflight(settings(tmp_path), SHA, 4.0, fake.run) == []


def test_an_unreadable_gitmodules_blob_refuses(tmp_path: Path) -> None:
    fake = FakeGit(
        gitmodules="",
        gitmodules_rc=1,
        gitmodules_stderr=f"error: unable to resolve config blob '{SHA}:.gitmodules'",
    )
    with pytest.raises(driver.CommandNotStarted, match="cannot read the submodule URLs"):
        driver.launcher_provisioning_preflight(settings(tmp_path), SHA, 4.0, fake.run)


def test_a_refused_local_rewrite_refuses_and_names_the_rewrite(tmp_path: Path) -> None:
    # The allowance is absent, as when LAUNCHER_UNIT_GIT_SETENV is emptied.
    fake = FakeGit(
        expanded={NETWORK_URL: "/mirrors/sub"},
        probe_rc=128,
        probe_stderr="fatal: transport 'file' not allowed",
    )
    with pytest.raises(driver.CommandNotStarted) as refused:
        driver.launcher_provisioning_preflight(
            settings(tmp_path), SHA, 4.0, fake.run, unit_setenv={}
        )
    message = str(refused.value)
    assert "transport 'file' not allowed" in message
    assert NETWORK_URL in message and "/mirrors/sub" in message
    assert "CREATE journal" in message and "no launcher started" in message
    assert "--setenv Git environment (none) does not admit the file transport" in message
    assert "restore LAUNCHER_UNIT_GIT_SETENV" in message
    assert "set-environment" not in message, "the remedy must not ask for a manager-wide change"
    assert "remove the url.insteadOf" not in message, "nor for removing a shared rewrite"
    [(environment, verb)] = fake.probes
    assert environment["GIT_PROTOCOL_FROM_USER"] == "0", "a submodule clone's policy, not a user's"
    assert "GIT_ALLOW_PROTOCOL" not in environment
    assert verb == ["ls-remote", "/mirrors/sub", "HEAD"]


def test_a_refusal_despite_the_allowance_does_not_blame_a_missing_allowance(tmp_path: Path) -> None:
    """Real git cannot refuse `file` under this allowlist; if it ever does, say so."""

    fake = FakeGit(
        expanded={NETWORK_URL: "/mirrors/sub"},
        probe_rc=128,
        probe_stderr="fatal: transport 'file' not allowed",
    )
    with pytest.raises(driver.CommandNotStarted) as refused:
        driver.launcher_provisioning_preflight(settings(tmp_path), SHA, 4.0, fake.run)
    message = str(refused.value)
    assert "CREATE journal" in message and "no launcher started" in message
    assert "although the launcher unit's GIT_ALLOW_PROTOCOL=https:file:ssh admits it" in message
    assert "restore LAUNCHER_UNIT_GIT_SETENV" not in message
    assert "does not admit the file transport" not in message
    [(environment, _)] = fake.probes
    assert environment["GIT_ALLOW_PROTOCOL"] == "https:file:ssh"


@pytest.mark.parametrize(
    "source,transport",
    [
        ("git://example.invalid/sub.git", "git"),
        ("http://example.invalid/sub.git", "http"),
        ("ext::sh -c true", "ext"),
        ("ftp://example.invalid/sub.git", "ftp"),
    ],
)
def test_a_network_source_outside_the_unit_allowlist_refuses(
    tmp_path: Path, source: str, transport: str
) -> None:
    """The unit's allowlist refuses these in the hook, so the preflight must too."""

    configured = settings(tmp_path)
    fake = FakeGit(expanded={NETWORK_URL: source})
    with pytest.raises(driver.CommandNotStarted) as refused:
        driver.spawn_validate(
            configured, SHA, "attempt", "validate-ops-tick-fixture",
            tmp_path / "validate.log", tmp_path / "launcher.log", 8.0, run=fake.run,
        )
    message = str(refused.value)
    assert f"over the {transport!r} transport" in message
    assert "GIT_ALLOW_PROTOCOL=https:file:ssh does not admit" in message
    assert source in message and NETWORK_URL in message
    assert "CREATE journal" in message and "no launcher started" in message
    assert "point the submodule at an https URL, or admit that transport" in message
    assert fake.probes == [], "a string check, not a network probe"
    assert [command for command in fake.commands if command[0] == "systemd-run"] == []
    assert not (tmp_path / "launcher.log").exists()


@pytest.mark.parametrize(
    "source",
    [
        "https://example.invalid/sub.git",
        "ssh://git@example.invalid/sub.git",
        "git@example.invalid:sub.git",
        "git+ssh://example.invalid/sub.git",
    ],
)
def test_a_network_source_inside_the_unit_allowlist_passes_unprobed(
    tmp_path: Path, source: str
) -> None:
    fake = FakeGit(expanded={NETWORK_URL: source})
    assert driver.launcher_provisioning_preflight(settings(tmp_path), SHA, 4.0, fake.run) == []
    assert fake.probes == []


def test_without_an_allowlist_a_git_source_keeps_gits_default(tmp_path: Path) -> None:
    """No GIT_ALLOW_PROTOCOL anywhere: git's own default admits git://, so pass."""

    fake = FakeGit(expanded={NETWORK_URL: "git://example.invalid/sub.git"})
    assert driver.launcher_provisioning_preflight(
        settings(tmp_path), SHA, 4.0, fake.run, unit_setenv={}
    ) == []
    assert fake.probes == []


def test_a_manager_allowlist_without_a_unit_value_is_still_applied(tmp_path: Path) -> None:
    fake = FakeGit(
        manager="HOME=/home/fixture\nGIT_ALLOW_PROTOCOL=file\n",
        expanded={NETWORK_URL: "https://example.invalid/sub.git"},
    )
    with pytest.raises(driver.CommandNotStarted, match="GIT_ALLOW_PROTOCOL=file does not admit") as refused:
        driver.launcher_provisioning_preflight(
            settings(tmp_path), SHA, 4.0, fake.run, unit_setenv={}
        )
    # The refused transport is already https, so the remedy must not suggest https.
    message = str(refused.value)
    assert "point the submodule at an https URL" not in message
    assert "admit that transport in LAUNCHER_UNIT_GIT_SETENV" in message


def test_the_probe_sees_the_manager_environment_not_this_process(tmp_path: Path, monkeypatch) -> None:
    # A value distinct from the unit's own, so a leak from this process is visible.
    monkeypatch.setenv("GIT_ALLOW_PROTOCOL", "ext:file")
    monkeypatch.setenv("GIT_CONFIG_PARAMETERS", "'protocol.file.allow'='always'")
    fake = FakeGit(
        manager="HOME=/home/fixture\nGIT_ALLOW_PROTOCOL=file\nLANG=C\n",
        expanded={NETWORK_URL: "/mirrors/sub"},
    )
    assert driver.launcher_provisioning_preflight(
        settings(tmp_path), SHA, 4.0, fake.run, unit_setenv={}
    ) == [{"submodule": "sub", "url": NETWORK_URL, "source": "/mirrors/sub"}]
    [(environment, _)] = fake.probes
    assert environment == {
        "GIT_ALLOW_PROTOCOL": "file",
        "GIT_PROTOCOL_FROM_USER": "0",
        "HOME": "/home/fixture",
        "PATH": os.environ.get("PATH", ""),
    }


def test_the_unit_setenv_wins_over_the_manager_value(tmp_path: Path, monkeypatch) -> None:
    """systemd applies a unit's --setenv over the manager block; so does the probe."""

    monkeypatch.setenv("GIT_ALLOW_PROTOCOL", "ext:file")
    monkeypatch.setenv("GIT_CONFIG_PARAMETERS", "'protocol.file.allow'='always'")
    fake = FakeGit(
        manager="HOME=/home/fixture\nGIT_ALLOW_PROTOCOL=https\nGIT_CONFIG_GLOBAL=/home/fixture/g\n",
        expanded={NETWORK_URL: "/mirrors/sub"},
    )
    driver.launcher_provisioning_preflight(settings(tmp_path), SHA, 4.0, fake.run)
    [(environment, _)] = fake.probes
    assert environment == {
        "GIT_ALLOW_PROTOCOL": "https:file:ssh",
        "GIT_CONFIG_GLOBAL": "/home/fixture/g",
        "GIT_PROTOCOL_FROM_USER": "0",
        "HOME": "/home/fixture",
        "PATH": os.environ.get("PATH", ""),
    }


def test_the_launcher_unit_carries_exactly_the_probed_git_environment(tmp_path: Path) -> None:
    configured = settings(tmp_path)
    fake = FakeGit(expanded={NETWORK_URL: "/mirrors/sub"})
    driver.spawn_validate(
        configured, SHA, "attempt", "validate-ops-tick-fixture",
        tmp_path / "validate.log", tmp_path / "launcher.log", 8.0, run=fake.run,
    )
    [launch] = [command for command in fake.commands if command[0] == "systemd-run"]
    options = launch[: launch.index("--")]
    git_setenv = [
        option for option in options
        if option.startswith("--setenv=GIT_")
    ]
    assert git_setenv == ["--setenv=GIT_ALLOW_PROTOCOL=https:file:ssh"]
    [(environment, _)] = fake.probes
    assert environment["GIT_ALLOW_PROTOCOL"] == "https:file:ssh"
    assert driver.LAUNCHER_UNIT_GIT_SETENV == {"GIT_ALLOW_PROTOCOL": "https:file:ssh"}


def test_a_non_git_unit_variable_is_refused(tmp_path: Path) -> None:
    fake = FakeGit()
    with pytest.raises(driver.CommandNotStarted, match="not a Git environment variable"):
        driver.launcher_provisioning_preflight(
            settings(tmp_path), SHA, 4.0, fake.run, unit_setenv={"LANG": "C"}
        )


def test_the_repository_config_url_wins_over_gitmodules(tmp_path: Path) -> None:
    fake = FakeGit(configured_url="https://example.invalid/other.git", expanded={"https://example.invalid/other.git": "/mirrors/other"})
    driver.launcher_provisioning_preflight(settings(tmp_path), SHA, 4.0, fake.run)
    [(_, verb)] = fake.probes
    assert verb == ["ls-remote", "/mirrors/other", "HEAD"]


@pytest.mark.parametrize(
    "manager",
    ["", "HOME=$'/home/fix\\nture'\n"],
    ids=["manager-unreadable", "undecodable-value"],
)
def test_an_unreproducible_manager_environment_refuses(tmp_path: Path, manager: str) -> None:
    fake = FakeGit(manager=manager, manager_rc=1 if not manager else 0)
    with pytest.raises(driver.CommandNotStarted, match="user manager"):
        driver.launcher_provisioning_preflight(settings(tmp_path), SHA, 4.0, fake.run)
    assert all(command[0] == "systemctl" for command in fake.commands)


def test_a_quoted_manager_value_is_decoded(tmp_path: Path) -> None:
    fake = FakeGit(manager="HOME='/home/with space'\n", expanded={NETWORK_URL: "/mirrors/sub"})
    driver.launcher_provisioning_preflight(settings(tmp_path), SHA, 4.0, fake.run)
    [(environment, _)] = fake.probes
    assert environment["HOME"] == "/home/with space"


@pytest.mark.parametrize(
    "url,local",
    [
        ("/abs/path", True),
        ("file:///abs/path", True),
        ("relative/dir", True),
        ("https://github.com/rrnewton/reverie.git", False),
        ("ssh://git@github.com/rrnewton/reverie.git", False),
        ("git@github.com:rrnewton/reverie.git", False),
    ],
)
def test_transport_classification_matches_git(url: str, local: bool) -> None:
    assert driver.uses_file_transport(url) is local


@pytest.mark.skipif(shutil.which("git") is None, reason="git is required")
@pytest.mark.parametrize(
    "url",
    [
        "git://127.0.0.1:9/x",
        "http://127.0.0.1:9/x",
        "https://127.0.0.1:9/x",
        "ext::true",
        "git+ssh://127.0.0.1:9/x",
        "ssh+git://127.0.0.1:9/x",
        "ssh://127.0.0.1:9/x",
        "git@127.0.0.1:x",
        "ftp://127.0.0.1:9/x",
        "HTTPS://127.0.0.1:9/x",
        "1x://127.0.0.1:9/x",
        "/nonexistent/local/path",
        "file:///nonexistent/local/path",
    ],
)
def test_transport_names_match_what_real_git_refuses(tmp_path: Path, url: str) -> None:
    """An allowlist that admits nothing makes git name the transport before connecting."""

    completed = subprocess.run(
        ["git", "ls-remote", url, "HEAD"],
        env={
            "PATH": os.environ.get("PATH", ""),
            "HOME": str(tmp_path),
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_ALLOW_PROTOCOL": "none-admitted",
            "GIT_PROTOCOL_FROM_USER": "0",
            "GIT_TERMINAL_PROMPT": "0",
        },
        cwd=tmp_path,
        capture_output=True,
        text=True,
        timeout=20,
        check=False,
    )
    assert completed.returncode != 0
    assert f"transport '{driver.transport_of(url)}' not allowed" in completed.stderr, completed.stderr


def test_a_refused_preflight_starts_no_launcher_and_writes_no_log(tmp_path: Path) -> None:
    configured = settings(tmp_path)
    fake = FakeGit(
        expanded={NETWORK_URL: "/mirrors/sub"},
        probe_rc=128,
        probe_stderr="fatal: transport 'file' not allowed",
    )
    launcher_log = tmp_path / "logs" / "launcher.log"
    with pytest.raises(driver.CommandNotStarted):
        driver.spawn_validate(
            configured, SHA, "attempt", "validate-ops-tick-fixture",
            tmp_path / "validate.log", launcher_log, 8.0, run=fake.run,
        )
    assert not any(command[0] == "systemd-run" for command in fake.commands)
    assert not launcher_log.exists()


def test_a_passing_preflight_still_starts_one_bounded_launcher(tmp_path: Path) -> None:
    configured = settings(tmp_path)
    fake = FakeGit()
    timeouts: list[tuple[str, float]] = []

    def run(command, cwd, timeout):
        timeouts.append((command[0], timeout))
        return fake.run(command, cwd, timeout)

    result = driver.spawn_validate(
        configured, SHA, "attempt", "validate-ops-tick-fixture",
        tmp_path / "validate.log", tmp_path / "launcher.log", 8.0, run=run,
    )
    assert result == {"unit": "validate-ops-tick-fixture-launcher.service", "accepted": True}
    assert [name for name, _ in timeouts].count("systemd-run") == 1
    assert timeouts[-1][0] == "systemd-run", "the preflight runs before the launch"
    assert all(timeout <= 4.0 for name, timeout in timeouts if name != "systemd-run")
    assert 4.0 <= timeouts[-1][1] <= 8.0


def test_a_run_tick_refusal_is_recorded_as_not_started(tmp_path: Path, capsys) -> None:
    """The decision reports the refusal loudly and a later decision retries."""

    configured = settings(tmp_path)
    fake = FakeGit(
        expanded={NETWORK_URL: "/mirrors/sub"},
        probe_rc=128,
        probe_stderr="fatal: transport 'file' not allowed",
    )

    authorities = load_authorities()(target=SHA)

    def spawn(*arguments):
        return driver.spawn_validate(*arguments, run=fake.run)

    rc = driver.run_tick(configured, fetch=authorities.fetch, run=authorities.run, spawn=spawn)
    output = capsys.readouterr().out
    assert rc == 1, output
    latest = json.loads((configured.state_dir / "latest.json").read_text())
    assert latest["state"] == "launch-not-started"
    assert "transport 'file' not allowed" in latest["detail"]
    assert "state=launch-not-started" in output
    assert not any(command[0] == "systemd-run" for command in fake.commands)


# Real git: the preflight's verdict must equal what the checkout hook does.

def real_git(*arguments, cwd: Path, env: dict[str, str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(["git", *arguments], cwd=cwd, env=env, text=True, capture_output=True)


@pytest.fixture
def mirrored_superproject(tmp_path: Path):
    if shutil.which("git") is None:
        pytest.skip("git is not installed")
    home = tmp_path / "home"
    home.mkdir()
    base_env = {
        "HOME": str(home),
        "PATH": os.environ.get("PATH", ""),
        "GIT_AUTHOR_NAME": "fixture",
        "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
        "GIT_COMMITTER_NAME": "fixture",
        "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
        "GIT_CONFIG_NOSYSTEM": "1",
    }
    source = tmp_path / "subsource"
    source.mkdir()
    for arguments in (("init", "-q", "-b", "main"), ("commit", "-q", "--allow-empty", "-m", "sub")):
        assert real_git(*arguments, cwd=source, env=base_env).returncode == 0
    mirror = tmp_path / "mirror" / "sub.git"
    assert real_git("clone", "-q", "--bare", str(source), str(mirror), cwd=tmp_path, env=base_env).returncode == 0
    (home / ".gitconfig").write_text(f'[url "{mirror}"]\n\tinsteadOf = {NETWORK_URL}\n')
    superproject = tmp_path / "hermit"
    superproject.mkdir()
    assert real_git("init", "-q", "-b", "main", cwd=superproject, env=base_env).returncode == 0
    added = real_git(
        "-c", "protocol.file.allow=always", "submodule", "add", "-q", NETWORK_URL, "sub",
        cwd=superproject, env=base_env,
    )
    assert added.returncode == 0, added.stderr
    assert real_git("commit", "-q", "-m", "super", cwd=superproject, env=base_env).returncode == 0
    target = real_git("rev-parse", "HEAD", cwd=superproject, env=base_env).stdout.strip()
    return tmp_path, home, mirror, superproject, target, base_env


def hook_succeeds(tmp_path: Path, superproject: Path, target: str, env: dict[str, str], name: str) -> bool:
    """Run the wrkslots post-provision hook in a fresh worktree of the target."""

    worktree = tmp_path / f"worktree-{name}"
    added = real_git("worktree", "add", "-q", "--detach", str(worktree), target, cwd=superproject, env=env)
    assert added.returncode == 0, added.stderr
    hook = real_git("submodule", "update", "--init", "--recursive", cwd=worktree, env=env)
    return hook.returncode == 0


PRODUCTION_UNIT = {"GIT_ALLOW_PROTOCOL": "https:file:ssh"}


@pytest.mark.parametrize(
    "manager_extra,gitconfig_extra,unit_setenv,expect_ok",
    [
        ({}, "", {}, False),
        ({"GIT_ALLOW_PROTOCOL": "file"}, "", {}, True),
        ({}, '[protocol "file"]\n\tallow = always\n', {}, True),
        ({}, "", PRODUCTION_UNIT, True),
        ({"GIT_ALLOW_PROTOCOL": "https"}, "", {}, False),
        ({"GIT_ALLOW_PROTOCOL": "https"}, "", PRODUCTION_UNIT, True),
    ],
    ids=[
        "file-transport-denied-without-unit-allowance",
        "manager-allows-file",
        "config-allows-file",
        "unit-allowance-admits-file",
        "manager-excludes-file",
        "unit-allowance-overrides-manager",
    ],
)
def test_the_preflight_verdict_matches_the_real_checkout_hook(
    mirrored_superproject, manager_extra, gitconfig_extra, unit_setenv, expect_ok
) -> None:
    tmp_path, home, mirror, superproject, target, base_env = mirrored_superproject
    if gitconfig_extra:
        with (home / ".gitconfig").open("a") as config:
            config.write(gitconfig_extra)
    manager = {"HOME": str(home), **manager_extra}

    def run(command, cwd, timeout):
        if list(command) == ["systemctl", "--user", "show-environment"]:
            return done(command, 0, "".join(f"{k}={v}\n" for k, v in manager.items()))
        return driver.run_bounded(command, cwd, timeout)

    configured = settings(tmp_path, superproject)
    # The launcher unit's environment: the manager block, then its own --setenv.
    hook_env = {**base_env, **manager_extra, **unit_setenv}
    assert hook_succeeds(tmp_path, superproject, target, hook_env, "hook") is expect_ok
    if expect_ok:
        assert driver.launcher_provisioning_preflight(
            configured, target, 20.0, run, unit_setenv=unit_setenv
        ) == [{"submodule": "sub", "url": NETWORK_URL, "source": str(mirror)}]
    else:
        with pytest.raises(driver.CommandNotStarted, match="transport 'file' not allowed"):
            driver.launcher_provisioning_preflight(
                configured, target, 20.0, run, unit_setenv=unit_setenv
            )


@pytest.mark.parametrize("allowance", ["production", "removed"])
def test_the_launch_starts_only_with_the_unit_allowance(
    mirrored_superproject, monkeypatch, allowance
) -> None:
    """End to end over real git: the manager admits no file transport, as measured.

    With the production allowance the launch starts one launcher that carries
    it, and the real hook in that unit's environment succeeds. With the
    allowance removed the same launch refuses before systemd-run.
    """

    tmp_path, home, mirror, superproject, target, base_env = mirrored_superproject
    if allowance == "removed":
        monkeypatch.setattr(driver, "LAUNCHER_UNIT_GIT_SETENV", {})
    commands: list[list[str]] = []

    def run(command, cwd, timeout):
        commands.append(list(command))
        if list(command) == ["systemctl", "--user", "show-environment"]:
            return done(command, 0, f"HOME={home}\nLANG=C\n")
        if command[0] == "systemd-run":
            return done(command)
        return driver.run_bounded(command, cwd, timeout)

    configured = settings(tmp_path, superproject)
    arguments = (
        configured, target, "attempt", "validate-ops-tick-fixture",
        tmp_path / "validate.log", tmp_path / "launcher.log", 40.0,
    )

    def launches() -> list[list[str]]:
        return [command for command in commands if command[0] == "systemd-run"]

    if allowance == "removed":
        with pytest.raises(driver.CommandNotStarted, match="transport 'file' not allowed"):
            driver.spawn_validate(*arguments, run=run)
        assert launches() == []
        assert not (tmp_path / "launcher.log").exists()
        return
    assert driver.spawn_validate(*arguments, run=run)["accepted"] is True
    [launch] = launches()
    unit_setenv = dict(
        option.removeprefix("--setenv=").split("=", 1)
        for option in launch[: launch.index("--")]
        if option.startswith("--setenv=GIT_")
    )
    assert unit_setenv == PRODUCTION_UNIT
    assert hook_succeeds(tmp_path, superproject, target, {**base_env, **unit_setenv}, "unit")
    assert not hook_succeeds(tmp_path, superproject, target, base_env, "manager-only"), (
        "the fixture must reproduce the measured refusal, or this test proves nothing"
    )


def test_without_a_rewrite_the_real_preflight_never_probes_the_network(mirrored_superproject) -> None:
    tmp_path, home, _mirror, superproject, target, _base_env = mirrored_superproject
    (home / ".gitconfig").write_text("")
    commands: list[list[str]] = []

    def run(command, cwd, timeout):
        commands.append(list(command))
        if list(command) == ["systemctl", "--user", "show-environment"]:
            return done(command, 0, f"HOME={home}\n")
        return driver.run_bounded(command, cwd, timeout)

    assert driver.launcher_provisioning_preflight(settings(tmp_path, superproject), target, 20.0, run) == []
    assert not any("HEAD" in command for command in commands)
