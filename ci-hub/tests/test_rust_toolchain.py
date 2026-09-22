from __future__ import annotations

import importlib.util
import subprocess
import sys
from pathlib import Path

import pytest


LIB = Path(__file__).resolve().parents[1] / "lib"
sys.path.insert(0, str(LIB))
SPEC = importlib.util.spec_from_file_location("tested_rust_toolchain", LIB / "rust_toolchain.py")
assert SPEC is not None and SPEC.loader is not None
resolver = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(resolver)
sys.path.pop(0)


def configure(root: Path, channel: str = "nightly-2099-01-01") -> None:
    (root / "rust-toolchain.toml").write_text(f'[toolchain]\nchannel = "{channel}"\n')


def test_declared_channel_overrides_ambient_and_directory_selection(tmp_path, monkeypatch):
    configure(tmp_path)
    environment = {"PATH": "/fixture", "RUSTUP_TOOLCHAIN": "stable", "RUSTUP_AUTO_INSTALL": "1"}
    before = dict(environment)
    selected = "nightly-2099-01-01-x86_64-unknown-linux-gnu"

    def probe(command, *, cwd, env, text, timeout):
        assert command == ["rustup", "show", "active-toolchain"]
        assert cwd == tmp_path and text is False and timeout == 20
        assert env["RUSTUP_AUTO_INSTALL"] == "0" and env["PATH"] == "/fixture"
        # Model rustup's precedence: without explicit selection this directory
        # has an unrelated stable override, even after ambient removal.
        token = selected if env.get("RUSTUP_TOOLCHAIN") == "nightly-2099-01-01" else "stable-x86_64-unknown-linux-gnu"
        return subprocess.CompletedProcess(command, 0, f"{token} (fixture selection)\n".encode(), b"")

    monkeypatch.setattr(resolver, "capture", probe)
    assert resolver.selected_toolchain(tmp_path, environment=environment) == selected
    assert environment == before


@pytest.mark.parametrize("encoded", [
    b"", b"[toolchain\n", b'[toolchain]\nchannel = ""\n',
    b'[toolchain]\nchannel = "nightly stable"\n', b'[toolchain]\nchannel = 7\n',
    b'[toolchain]\nchannel = "-nightly"\n', b'[toolchain]\npath = "/elsewhere"\n',
    b'[toolchain]\nchannel = "nightly"\npath = "/elsewhere"\n', b"\xff", b"x" * 16385,
])
def test_invalid_configuration_refuses_before_native_probe(tmp_path, monkeypatch, encoded):
    (tmp_path / "rust-toolchain.toml").write_bytes(encoded)
    monkeypatch.setattr(resolver, "capture", lambda *_a, **_k: pytest.fail("probe must not run"))
    with pytest.raises(resolver.ToolchainRefused):
        resolver.selected_toolchain(tmp_path)


def test_missing_configuration_refuses(tmp_path):
    with pytest.raises(resolver.ToolchainRefused, match="cannot read"):
        resolver.selected_toolchain(tmp_path)


@pytest.mark.parametrize("result", [
    subprocess.CompletedProcess([], 1, b"", b"toolchain unavailable"),
    subprocess.CompletedProcess([], 0, b"", b""),
    subprocess.CompletedProcess([], 0, b"nightly-2099-01-01\nstable\n", b""),
    subprocess.CompletedProcess([], 0, b"\xff", b""),
    subprocess.CompletedProcess([], 0, b"stable-x86_64-unknown-linux-gnu\n", b""),
    subprocess.CompletedProcess([], 0, b"nightly-2099-02-01-x86_64-unknown-linux-gnu\n", b""),
    subprocess.CompletedProcess([], 0, b"x" * 4097, b""),
    subprocess.CompletedProcess([], 0, b"nightly-2099-01-01\n", b"x" * 4097),
])
def test_unavailable_or_unrelated_native_selection_refuses(tmp_path, monkeypatch, result):
    configure(tmp_path)
    monkeypatch.setattr(resolver, "capture", lambda *_a, **_k: result)
    with pytest.raises(resolver.ToolchainRefused):
        resolver.selected_toolchain(tmp_path)


@pytest.mark.parametrize("error", [FileNotFoundError("rustup"), subprocess.TimeoutExpired("rustup", 20)])
def test_native_failure_is_a_scoped_refusal(tmp_path, monkeypatch, error):
    configure(tmp_path)
    def fail(*_a, **_k):
        raise error
    monkeypatch.setattr(resolver, "capture", fail)
    with pytest.raises(resolver.ToolchainRefused, match="cannot resolve"):
        resolver.selected_toolchain(tmp_path)


def test_unqualified_channel_cannot_resolve_to_a_different_date(tmp_path, monkeypatch):
    configure(tmp_path, "nightly")
    monkeypatch.setattr(resolver, "capture", lambda *_a, **_k: subprocess.CompletedProcess(
        [], 0, b"nightly-2099-01-01-x86_64-unknown-linux-gnu\n", b""))
    with pytest.raises(resolver.ToolchainRefused, match="did not select"):
        resolver.selected_toolchain(tmp_path)
