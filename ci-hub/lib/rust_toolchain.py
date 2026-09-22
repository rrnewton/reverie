"""Resolve a checkout's declared Rust toolchain without changing host settings."""

from __future__ import annotations

import os
import re
import subprocess
import tomllib
from collections.abc import Mapping
from pathlib import Path

from bounded_capture import capture


class ToolchainRefused(Exception):
    """The checkout's configured compiler cannot be selected faithfully."""


def selected_toolchain(
    checkout: Path, *, environment: Mapping[str, str] | None = None
) -> str:
    """Return rustup's installed canonical name for rust-toolchain.toml's channel.

    An explicit environment selection outranks both directory overrides and the
    host default. It is scoped to this probe; callers must carry the result into
    their actual compiler or rust-script invocation, whose cwd may be a cache.
    """
    checkout = Path(checkout).resolve()
    configuration = checkout / "rust-toolchain.toml"
    try:
        with configuration.open("rb") as source:
            encoded = source.read(16_385)
        if len(encoded) > 16_384:
            raise ToolchainRefused("rust-toolchain.toml exceeds 16384 bytes")
        settings = tomllib.loads(encoded.decode("utf-8"))
        toolchain = settings.get("toolchain")
        channel = toolchain.get("channel") if isinstance(toolchain, dict) else None
        if (
            not isinstance(channel, str)
            or len(channel) > 256
            or re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", channel) is None
            or "path" in toolchain
        ):
            raise ToolchainRefused("rust-toolchain.toml must declare one safe channel")
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        raise ToolchainRefused(f"cannot read {configuration}: {error}") from error

    probe_environment = dict(os.environ if environment is None else environment)
    probe_environment["RUSTUP_TOOLCHAIN"] = channel
    probe_environment["RUSTUP_AUTO_INSTALL"] = "0"
    try:
        result = capture(
            ["rustup", "show", "active-toolchain"], cwd=checkout,
            env=probe_environment, text=False, timeout=20,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ToolchainRefused(f"cannot resolve declared Rust toolchain {channel}: {error}") from error
    if len(result.stdout) > 4096 or len(result.stderr) > 4096:
        raise ToolchainRefused("rustup toolchain response exceeds 4096 bytes")
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).decode(errors="replace").strip()
        raise ToolchainRefused(f"declared Rust toolchain {channel} is unavailable: {detail[:2000]}")
    try:
        output = result.stdout.decode("utf-8").strip()
    except UnicodeError as error:
        raise ToolchainRefused("rustup returned an unreadable toolchain name") from error
    selected = output.split(" ", 1)[0]
    # rustup appends the host triple to a distribution channel, but a different
    # release/date is not a permissible resolution of the declared channel.
    host = r"(?:[A-Za-z_][A-Za-z0-9_]*-){2,}[A-Za-z_][A-Za-z0-9_]*"
    if (
        "\n" in output or "\r" in output
        or re.fullmatch(re.escape(channel) + rf"(?:-{host})?", selected) is None
    ):
        raise ToolchainRefused(f"rustup did not select declared Rust toolchain {channel}")
    return selected
