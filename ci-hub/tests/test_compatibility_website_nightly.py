from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import tarfile
import subprocess
import sys
from types import SimpleNamespace

import pytest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "website_nightly_test", ROOT / "ci-hub/compatibility-website/nightly_publication.py"
)
assert SPEC and SPEC.loader
nightly = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(nightly)

BUILDER_SPEC = importlib.util.spec_from_file_location(
    "website_nightly_real_builder", ROOT / "ci-hub/compatibility-website/build_and_serve.py"
)
assert BUILDER_SPEC and BUILDER_SPEC.loader
real_builder = importlib.util.module_from_spec(BUILDER_SPEC)
sys.modules[BUILDER_SPEC.name] = real_builder
BUILDER_SPEC.loader.exec_module(real_builder)


def test_archive_is_reproducible_and_contains_exact_modes_and_bytes(tmp_path: Path) -> None:
    tree = tmp_path / "tree"
    tree.mkdir()
    (tree / "assets").mkdir()
    (tree / "build.json").write_bytes(b'{"source":"immutable"}\n')
    (tree / "assets/site.css").write_bytes(b"body { color: black; }\n")
    for path in tree.rglob("*"):
        path.chmod(0o555 if path.is_dir() else 0o444)
    tree.chmod(0o555)
    first, second = tmp_path / "first.gz", tmp_path / "second.gz"
    assert nightly.make_archive(tree, first) == nightly.make_archive(tree, second)
    assert first.read_bytes() == second.read_bytes()
    with tarfile.open(first) as archive:
        assert archive.getnames() == [".", "./assets", "./assets/site.css", "./build.json"]
        for member in archive:
            assert member.mode == (0o555 if member.isdir() else 0o444)
            assert member.uid == member.gid == member.mtime == 0
        assert archive.extractfile("./build.json").read() == (tree / "build.json").read_bytes()


def test_archive_refuses_a_symlink_instead_of_exporting_it(tmp_path: Path) -> None:
    tree = tmp_path / "tree"
    tree.mkdir()
    (tree / "outside").symlink_to("/etc/passwd")
    with pytest.raises(RuntimeError, match="non-regular"):
        nightly.make_archive(tree, tmp_path / "archive.gz")


@pytest.mark.parametrize("change", ["digest", "size", "state", "duplicate"])
def test_release_receipt_refuses_changed_or_ambiguous_asset(change: str) -> None:
    asset = {"id": 7, "name": "site.gz", "digest": "sha256:" + "a" * 64, "size": 42, "state": "uploaded"}
    release = {"assets": [asset]}
    assert nightly.verify_asset(release, "site.gz", "a" * 64, 42) == 7
    if change == "duplicate":
        release["assets"].append(dict(asset))
    else:
        asset[change] = "wrong"
    with pytest.raises(RuntimeError, match="differs"):
        nightly.verify_asset(release, "site.gz", "a" * 64, 42)


def test_nightly_uses_existing_timer_handoff_and_bounded_service(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    calls = []
    (tmp_path / "target").mkdir()
    monkeypatch.setenv("CARGO_TARGET_DIR", str(tmp_path / "unrelated-target"))
    monkeypatch.setattr(nightly, "floor", lambda _: None)
    monkeypatch.setattr(nightly, "command", lambda argv, **_: calls.append(argv) or b"inactive\n")
    for key in ("DEV_HERMIT_OPERATIONAL_TOOL", "DEV_HERMIT_TOOL_PARENT_SHA", "DEV_HERMIT_TOOL_CACHE"):
        monkeypatch.delenv(key, raising=False)
    assert nightly.launch(SimpleNamespace(REPOSITORY=ROOT), tmp_path) == 0
    launch = calls[-1]
    assert launch[:4] == ["systemd-run", "--user", "--quiet", "--collect"]
    assert "--property=RuntimeMaxSec=3600" in launch
    assert "--property=MemoryMax=8G" in launch
    assert "--property=CPUQuota=200%" in launch
    assert f"--setenv=CARGO_TARGET_DIR={tmp_path / 'target'}" in launch
    assert not (tmp_path / "unrelated-target").exists()
    assert launch[-4:] == ["nightly", "run", "--state-root", str(tmp_path)]
    assert not any("timer" in value for value in launch)


def test_nightly_missing_state_target_refuses_before_service_launch(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    calls = []
    monkeypatch.setattr(nightly, "floor", lambda _: None)
    monkeypatch.setattr(nightly, "command", lambda argv, **_: calls.append(argv) or b"inactive\n")
    with pytest.raises(RuntimeError, match="existing state Cargo target is unavailable"):
        nightly.launch(SimpleNamespace(REPOSITORY=ROOT), tmp_path)
    assert len(calls) == 1 and calls[0][0] == "systemctl"
    assert not (tmp_path / "target").exists()


def test_failed_build_records_failure_without_creating_release(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(nightly, "floor", lambda _: (_ for _ in ()).throw(RuntimeError("floor refused")))
    monkeypatch.setattr(nightly, "gh", lambda *_: pytest.fail("must not publish after refusal"))
    builder = SimpleNamespace(REPOSITORY=ROOT, _git_oid=lambda *_: "b" * 40)
    with pytest.raises(RuntimeError, match="floor refused"):
        nightly.run(builder, tmp_path)
    state = json.loads((tmp_path / "ignored/ci-hub/compatibility-website/nightly/state.json").read_bytes())
    assert state["status"] == "failed"
    assert state["error"] == "floor refused"
    assert "verified_at" not in state


@pytest.mark.parametrize("status", [200, 404, 403, 0])
def test_exact_release_lookup_only_treats_http_404_as_absence(status: int, monkeypatch: pytest.MonkeyPatch) -> None:
    response = (f"HTTP/2.0 {status}\r\ncontent-type: application/json\r\n\r\n".encode()
                + b'{"id":17}') if status else b""
    calls = []
    monkeypatch.setattr(nightly.subprocess, "run", lambda argv, **kwargs:
                        calls.append(argv) or subprocess.CompletedProcess(argv, 0 if status == 200 else 1, response, b"request failed"))
    if status in (200, 404):
        assert nightly.release_by_tag("old-immutable-tag") == ({"id": 17} if status == 200 else None)
    else:
        with pytest.raises(RuntimeError, match="lookup failed"):
            nightly.release_by_tag("old-immutable-tag")
    assert calls[0][-1].endswith("/releases/tags/old-immutable-tag")


def test_nightly_build_uses_explicit_state_not_immutable_tool_cache(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    tool, state = tmp_path / "tool", tmp_path / "state"
    state.mkdir()
    calls = []
    exporter = SimpleNamespace(_load_authorities=lambda: None,
                               series_store=SimpleNamespace(ledger_root=lambda root: root / "hermit_test_ledger"))
    def build(output, **kwargs):
        calls.append((output, kwargs))
        raise RuntimeError("stop after real routing boundary")
    builder = SimpleNamespace(REPOSITORY=tool, _git_oid=lambda *_: "b" * 40,
                              _modules=lambda: SimpleNamespace(export_inputs=exporter), build=build,
                              _output_location=real_builder._output_location)
    monkeypatch.setattr(nightly.tempfile, "gettempdir", lambda: str(tmp_path))
    monkeypatch.setattr(nightly, "floor", lambda _: None)
    monkeypatch.setattr(nightly, "command", lambda *_args, **_kwargs: b"")
    monkeypatch.setattr(nightly, "gh", lambda *_: pytest.fail("routing refusal must not publish"))
    with pytest.raises(RuntimeError, match="routing boundary"):
        nightly.run(builder, state)
    expected_output = tmp_path / f"hermit-compatibility-website-{os.geteuid()}" / "builds"
    assert calls[0][0] == expected_output
    assert calls[0][1] == {"series_ref": "b" * 40, "published_only": True, "state_root": state,
                           "validation_history_commit": "b" * 40}
    receipt = json.loads((state / "ignored/ci-hub/compatibility-website/nightly/state.json").read_bytes())
    assert receipt["output_root"] == str(expected_output)


@pytest.mark.parametrize("contradiction", [None, "parent_commit", "reader_commit", "series_commit", "published_only"])
def test_nightly_binds_captured_history_before_any_publication(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, contradiction: str | None,
) -> None:
    tool, state, ledger = (tmp_path / name for name in ("tool", "state", "ledger"))
    state.mkdir()
    reader, history, cells = "a" * 40, "b" * 40, "c" * 40
    refs = {(tool, "HEAD"): reader, (state, "refs/remotes/origin/main"): history,
            (ledger, "refs/remotes/origin/main"): cells}
    calls, closed = [], []
    exporter = SimpleNamespace(_load_authorities=lambda: None,
                               series_store=SimpleNamespace(ledger_root=lambda _: ledger))

    def build(_output, **kwargs):
        assert calls == [
            ["with-proxy", "git", "-C", str(path), "fetch", "--no-tags", "--no-recurse-submodules",
             "origin", "main:refs/remotes/origin/main"] for path in (ledger, state)]
        assert kwargs == {"series_ref": cells, "published_only": True, "state_root": state,
                          "validation_history_commit": history}
        # A later ref advancement cannot silently replace either captured input.
        refs[(state, "refs/remotes/origin/main")] = "d" * 40
        refs[(ledger, "refs/remotes/origin/main")] = "e" * 40
        provenance = {"series_commit": cells, "published_only": True,
                      "parent_commit": history, "reader_commit": reader}
        if contradiction:
            provenance[contradiction] = False if contradiction == "published_only" else "f" * 40
        return SimpleNamespace(recheck=lambda: None, reused=False,
                               manifest={"freshness_sha256": "0" * 64,
                                         "provenance": {"canonical_input_provenance": provenance}},
                               close=lambda: closed.append(True))

    builder = SimpleNamespace(REPOSITORY=tool, _git_oid=lambda path, ref: refs[(path, ref)],
                              _modules=lambda: SimpleNamespace(export_inputs=exporter), build=build,
                              _output_location=real_builder._output_location)
    monkeypatch.setattr(nightly.tempfile, "gettempdir", lambda: str(tmp_path))
    monkeypatch.setattr(nightly, "floor", lambda _: None)
    monkeypatch.setattr(nightly, "command", lambda argv, **_: calls.append(argv) or b"")
    monkeypatch.setattr(nightly, "gh", lambda *_: pytest.fail("must not publish in this control"))
    def reached_registry(*_args):
        assert contradiction is None, "contradictory provenance reached publication"
        raise RuntimeError("captured inputs reached publication boundary")
    monkeypatch.setattr(nightly, "published_registry", reached_registry)
    reason = ("captured inputs reached publication boundary" if contradiction is None else
              "requested published ledger commit" if contradiction in {"series_commit", "published_only"} else
              "captured validation history and reader commits")
    with pytest.raises(RuntimeError, match=reason):
        nightly.run(builder, state)
    receipt = json.loads((state / "ignored/ci-hub/compatibility-website/nightly/state.json").read_bytes())
    assert receipt["validation_history_commit"] == history
    assert receipt["ledger_commit"] == cells
    assert receipt["builder_commit"] == reader
    assert receipt["status"] == "failed"
    assert "verified_at" not in receipt
    assert closed == [True]


def test_nightly_artifacts_stay_outside_git_managed_home_and_reuse_directory(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    real_builder._modules()
    home, temporary = tmp_path / "home", tmp_path / "temporary"
    tool = home / "work/tool"
    tool.mkdir(parents=True)
    (home / ".git").mkdir()
    temporary.mkdir()
    monkeypatch.setattr(real_builder, "REPOSITORY", tool)
    monkeypatch.setattr(nightly.tempfile, "gettempdir", lambda: str(temporary))
    output = nightly.build_root(real_builder)
    assert not real_builder._overlaps(output, home)
    assert stat.S_IMODE(output.parent.stat().st_mode) == 0o700
    inode = output.parent.stat().st_ino
    assert nightly.build_root(real_builder) == output
    assert output.parent.stat().st_ino == inode
    assert real_builder._output_location(output)[0] == output


@pytest.mark.parametrize("bad_location", ["source", "writable", "symlink"])
def test_nightly_invalid_artifact_location_refuses_before_fetch_or_build(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, bad_location: str,
) -> None:
    real_builder._modules()
    home, temporary = tmp_path / "home", tmp_path / "temporary"
    tool = home / "work/tool"
    tool.mkdir(parents=True)
    (home / ".git").mkdir()
    temporary.mkdir()
    monkeypatch.setattr(real_builder, "REPOSITORY", tool)
    if bad_location == "source":
        temporary = home
    parent = temporary / f"hermit-compatibility-website-{os.geteuid()}"
    if bad_location == "writable":
        parent.mkdir(mode=0o777)
        parent.chmod(0o777)
    elif bad_location == "symlink":
        parent.symlink_to(home, target_is_directory=True)
    monkeypatch.setattr(nightly.tempfile, "gettempdir", lambda: str(temporary))
    monkeypatch.setattr(nightly, "floor", lambda _: None)
    monkeypatch.setattr(nightly, "command", lambda *_args, **_kwargs: pytest.fail("must refuse before fetch"))
    builder = SimpleNamespace(REPOSITORY=tool, _git_oid=lambda *_: "b" * 40,
                              _output_location=real_builder._output_location,
                              _modules=lambda: pytest.fail("must refuse before exporter"),
                              build=lambda *_args, **_kwargs: pytest.fail("must refuse before build"))
    with pytest.raises(real_builder.WebsiteRefused):
        nightly.run(builder, home / "state")
    receipt = json.loads((home / "state/ignored/ci-hub/compatibility-website/nightly/state.json").read_bytes())
    assert receipt["status"] == "failed"
    assert "verified_at" not in receipt
    if bad_location == "writable":
        assert stat.S_IMODE(parent.stat().st_mode) == 0o777
