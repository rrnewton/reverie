"""Nightly orchestration for the existing builder and Docs publisher.

This is a module of build_and_serve.py, not another scheduler. The maintained
health tick launches one bounded user service; all build/output checks remain
in the normal website builder and Hermit's release publisher.
"""

from __future__ import annotations

import datetime as dt
import fcntl
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile
import time


UNIT = "hermit-compatibility-website"
REPOSITORY = "rrnewton/hermit"
PUBLIC = "https://rrnewton.github.io/hermit/compatibility/latest/"
FLOOR_BYTES = 400 * 1024**3


def now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat()


def canonical(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode() + b"\n"


def command(argv: list[str], *, cwd: Path | None = None, timeout: int = 120) -> bytes:
    result = subprocess.run(argv, cwd=cwd, check=True, stdout=subprocess.PIPE,
                            stderr=None, timeout=timeout)
    return result.stdout


def gh(*args: str) -> bytes:
    return command(["with-proxy", "gh", *args])


def api(path: str) -> object:
    return json.loads(gh("api", path))


def release_by_tag(tag: str) -> dict | None:
    result = subprocess.run(
        ["with-proxy", "gh", "api", "--include", f"repos/{REPOSITORY}/releases/tags/{tag}"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=120,
    )
    header, separator, body = result.stdout.replace(b"\r\n", b"\n").partition(b"\n\n")
    status = header.splitlines()[0].split()[1:2] if header else []
    if separator and status == [b"404"] and result.returncode != 0:
        return None
    if result.returncode:
        raise RuntimeError("release lookup failed: " + result.stderr.decode(errors="replace")[:1024])
    if not separator or status != [b"200"]:
        raise RuntimeError("release lookup returned an unexpected HTTP response")
    return json.loads(body)


def floor(root: Path) -> None:
    facts = os.statvfs(root)
    if facts.f_bavail * facts.f_frsize < FLOOR_BYTES:
        raise RuntimeError("website publication refused below the owner's 400 GiB free-space floor")


def write_state(root: Path, state: dict) -> None:
    temporary = root / f".state-{os.getpid()}.json"
    temporary.write_bytes(canonical(state))
    os.replace(temporary, root / "state.json")


def build_root(builder: object) -> Path:
    # Generated site trees must be outside every source checkout, including a
    # Git-managed home directory. Keep one reusable per-user artifact directory;
    # durable publication receipts and source evidence remain in STATE.
    parent = Path(tempfile.gettempdir()) / f"hermit-compatibility-website-{os.geteuid()}"
    parent.mkdir(mode=0o700, exist_ok=True)
    destination, _metadata = builder._output_location(parent / "builds")
    return destination


def launch(builder: object, state_root: Path) -> int:
    root = state_root / "ignored/ci-hub/compatibility-website/nightly"
    root.mkdir(parents=True, exist_ok=True)
    with (root / "launch.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        status = command(["systemctl", "--user", "show", UNIT + ".service",
                          "--property=ActiveState", "--value"]).decode().strip()
        if status in {"active", "activating", "deactivating"}:
            print("state=running\nsummary=nightly compatibility build is already active")
            return 0
        floor(state_root)
        cargo_target = (state_root / "target").resolve()
        if not cargo_target.is_dir():
            raise RuntimeError(f"existing state Cargo target is unavailable: {cargo_target}")
        prior = json.loads((root / "state.json").read_bytes()) if (root / "state.json").exists() else {}
        environment = os.environ
        operational = environment.get("DEV_HERMIT_OPERATIONAL_TOOL")
        sha = environment.get("DEV_HERMIT_TOOL_PARENT_SHA")
        cache = environment.get("DEV_HERMIT_TOOL_CACHE")
        if operational or sha or cache:
            if not operational or not Path(operational).is_file() or not cache or not re.fullmatch(r"[0-9a-f]{40}", sha or ""):
                raise RuntimeError("incomplete immutable operational-tool handoff")
            worker = [operational, "--run-current-tool", "--state-root", str(state_root),
                      "--cache-root", cache, "--tool-parent-sha", sha,
                      "--tool-relative", "ci-hub/compatibility-website/build_and_serve.py", "--"]
        else:
            worker = [sys.executable, str(builder.REPOSITORY / "ci-hub/compatibility-website/build_and_serve.py")]
        command(["systemd-run", "--user", "--quiet", "--collect", f"--unit={UNIT}",
                 "--property=RuntimeMaxSec=3600", "--property=MemoryMax=8G",
                 "--property=MemorySwapMax=0", "--property=CPUQuota=200%", "--property=TasksMax=512", "--property=LimitCORE=0",
                 f"--property=StandardOutput=append:{root / 'run.log'}",
                 f"--property=StandardError=append:{root / 'run.log'}",
                 f"--working-directory={state_root}",
                 f"--setenv=PATH={environment.get('PATH', '')}",
                 f"--setenv=DEV_HERMIT_PARENT={state_root}",
                 f"--setenv=CARGO_TARGET_DIR={cargo_target}",
                 "--", "with-proxy", *worker, "nightly", "run", "--state-root", str(state_root)])
        print("state=launched\nsummary=bounded nightly compatibility rebuild launched")
        if prior.get("status") == "failed":
            print(f"previous_failure={prior.get('error', 'see retained run.log')}")
            return 1
        return 0


def published_registry(helper: Path, checked_in: Path, root: Path) -> dict:
    baseline = json.loads(checked_in.read_bytes())
    commit = api(f"repos/{REPOSITORY}/git/ref/heads/gh-pages")["object"]["sha"]
    entries = api(f"repos/{REPOSITORY}/contents/compatibility?ref={commit}")
    names = {entry["name"] for entry in entries}
    if "releases.json" in names:
        payload = gh("api", "-H", "Accept: application/vnd.github.raw+json",
                     f"repos/{REPOSITORY}/contents/compatibility/releases.json?ref={commit}")
    else:
        if names != {"latest", *(pin["identity"] for pin in baseline["releases"])}:
            raise RuntimeError("existing public trees are not accounted for by the initial registry")
        payload = checked_in.read_bytes()
    previous = root / "previous-registry.json"
    previous.write_bytes(payload)
    command([sys.executable, str(helper), "validate-update", str(previous),
             str(checked_in), str(checked_in.parent.parent), str(previous)])
    return json.loads(payload)


def make_archive(tree: Path, destination: Path) -> tuple[str, int]:
    with destination.open("xb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
        with tarfile.open(mode="w", fileobj=compressed, format=tarfile.PAX_FORMAT) as archive:
            for path in [tree, *sorted(tree.rglob("*"))]:
                if path.is_symlink() or not (path.is_dir() or path.is_file()):
                    raise RuntimeError(f"website contains a non-regular archive member: {path}")
                name = "." if path == tree else "./" + path.relative_to(tree).as_posix()
                member = archive.gettarinfo(str(path), arcname=name)
                member.uid = member.gid = 0
                member.uname = member.gname = ""
                member.mtime = 0
                if path.is_file():
                    with path.open("rb") as stream:
                        archive.addfile(member, stream)
                else:
                    archive.addfile(member)
    digest = hashlib.sha256()
    with destination.open("rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest(), destination.stat().st_size


def verify_asset(release: dict, name: str, digest: str, size: int) -> int:
    assets = [asset for asset in release["assets"] if asset["name"] == name]
    if len(assets) != 1 or assets[0].get("digest") != "sha256:" + digest or assets[0].get("size") != size or assets[0].get("state") != "uploaded":
        raise RuntimeError(f"published release asset differs from the built bytes: {name}")
    return assets[0]["id"]


def _run(builder: object, state_root: Path) -> int:
    root = state_root / "ignored/ci-hub/compatibility-website/nightly"
    root.mkdir(parents=True, exist_ok=True)
    state = {"status": "running", "started_at": now(), "builder_commit": builder._git_oid(builder.REPOSITORY, "HEAD"),
             "cargo_target_dir": os.environ.get("CARGO_TARGET_DIR")}
    write_state(root, state)
    website = None
    try:
        floor(state_root)
        output_root = build_root(builder)
        state["output_root"] = str(output_root)
        write_state(root, state)
        exporter = builder._modules().export_inputs
        exporter._load_authorities()
        ledger = exporter.series_store.ledger_root(state_root)
        command(["with-proxy", "git", "-C", str(ledger), "fetch", "--no-tags", "origin", "main:refs/remotes/origin/main"])
        ledger_commit = builder._git_oid(ledger, "refs/remotes/origin/main")
        state["ledger_commit"] = ledger_commit
        # This captures only published bytes at one immutable commit; it never
        # changes a date label over an old source pin or mixes in mutable spools.
        website = builder.build(output_root, series_ref=ledger_commit, published_only=True, state_root=state_root)
        website.recheck()
        state.update(build_finished_at=now(), identity=website.manifest["freshness_sha256"], reused=website.reused)
        provenance = website.manifest["provenance"]["canonical_input_provenance"]
        if provenance.get("series_commit") != ledger_commit or provenance.get("published_only") is not True:
            raise RuntimeError("built website does not bind the requested published ledger commit")
        helper = builder.REPOSITORY / "hermit/.github/scripts/publish-compatibility-site.py"
        checked_in = builder.REPOSITORY / "hermit/.github/compatibility-site-releases.json"
        registry = published_registry(helper, checked_in, root)
        pin = json.loads(command([sys.executable, str(helper), "describe", str(website.path)]))
        tag = "compatibility-website-" + pin["identity"]
        attempt_root = root / ("attempt-" + str(time.time_ns()))
        attempt_root.mkdir()
        archive = attempt_root / (tag + ".tar.gz")
        sha, size = make_archive(website.path, archive)
        pin.update(tag=tag, asset=tag + ".tar.gz", release_title="Compatibility website " + pin["identity"],
                   archive_sha256=sha, archive_bytes=size,
                   archive_member_count=pin["file_count"] + len(pin["directories"]) + 1)
        by_identity = {item["identity"]: item for item in registry["releases"]}
        if pin["identity"] in by_identity and by_identity[pin["identity"]] != pin:
            raise RuntimeError("an existing immutable release identity has different bytes")
        by_identity[pin["identity"]] = pin
        registry.update(latest_identity=pin["identity"], releases=[by_identity[key] for key in sorted(by_identity)])
        descriptor_bytes = canonical(registry)
        descriptor_sha = hashlib.sha256(descriptor_bytes).hexdigest()
        descriptor = attempt_root / ("registry-" + descriptor_sha + ".json")
        descriptor.write_bytes(descriptor_bytes)
        command([sys.executable, str(helper), "validate-update", str(descriptor), str(checked_in),
                 str(builder.REPOSITORY / "hermit"), str(root / "previous-registry.json")])
        floor(state_root)
        release = release_by_tag(tag)
        if release is None:
            gh("release", "create", tag, "--repo", REPOSITORY, "--target", builder._git_oid(builder.REPOSITORY / "hermit", "HEAD"),
               "--title", pin["release_title"], "--notes", "Immutable compatibility website built from ledger " + ledger_commit,
               str(archive), str(descriptor))
        release = api(f"repos/{REPOSITORY}/releases/tags/{tag}")
        verify_asset(release, pin["asset"], sha, size)
        if not any(asset["name"] == descriptor.name for asset in release["assets"]):
            gh("release", "upload", tag, str(descriptor), "--repo", REPOSITORY)
            release = api(f"repos/{REPOSITORY}/releases/tags/{tag}")
        asset_id = verify_asset(release, descriptor.name, descriptor_sha, descriptor.stat().st_size)
        gh("workflow", "run", "docs.yml", "--repo", REPOSITORY, "--ref", "main",
           "-f", f"compatibility_registry_asset={asset_id}", "-f", f"compatibility_registry_sha256={descriptor_sha}")
        state.update(status="publishing", release=release["html_url"], registry_asset=asset_id)
        write_state(root, state)
        # Poll only this public immutable identity, with a finite whole-service
        # bound. A queued or failed workflow is never reported as delivered.
        deadline = time.monotonic() + 1800
        expected = (website.path / "build.json").read_bytes()
        while time.monotonic() < deadline:
            try:
                matched = True
                for path in ("build.json", "index.html", "data/site-summary.json.gz"):
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        matched = False
                        break
                    observed = command(["with-proxy", "curl", "--fail", "--silent", "--show-error", "--max-time", "30", PUBLIC + path], timeout=min(35, remaining))
                    if observed != (expected if path == "build.json" else (website.path / path).read_bytes()):
                        matched = False
                        break
            except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
                matched = False
                state["last_readback_error"] = str(error)
                write_state(root, state)
            if matched:
                state.update(status="published", public_url=PUBLIC, verified_at=now())
                write_state(root, state)
                print(json.dumps(state, sort_keys=True))
                return 0
            time.sleep(min(20, max(0, deadline - time.monotonic())))
        raise RuntimeError("Docs did not publish the exact built identity within 30 minutes")
    except BaseException as error:
        state.update(status="failed", finished_at=now(), error=str(error))
        write_state(root, state)
        raise
    finally:
        if website is not None:
            website.close()


def run(builder: object, state_root: Path) -> int:
    root = state_root / "ignored/ci-hub/compatibility-website/nightly"
    root.mkdir(parents=True, exist_ok=True)
    with (root / "run.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return _run(builder, state_root)
