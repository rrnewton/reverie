#!/usr/bin/env python3
"""Consumer-owned coverage of the REAL published immutable-tool verifier.

WHY THIS EXISTS BESIDE `test_immutable_tool_authority.py` RATHER THAN INSIDE IT.
The Agent Utils contract tests deliberately drive a STUB, so they cannot detect a
producer dataclass, field, protocol, interpreter or signature change: a stub that
still satisfies the contract certifies nothing about the bytes production loads.
That gap is consumer-side knowledge, and it stays HERE rather than in Agent
Utils, which must remain generic and name no client.

WHAT THE NEIGHBOURING PRODUCER TEST ALREADY COVERS, so this file does not repeat
it: each required seal, each environment binding being required and exact,
content bytes/symlink/modes, state-root mismatch, an unbound path, a replaced
state root, and typed-field mutation refusal.

WHAT IT DOES NOT, AND THIS FILE DOES. Measured 2026-09-16 against
`test_immutable_tool_authority.py` at parent `eea4bb0e`:

  1. INTERPRETER. Its isolated-process case runs `sys.executable` -- whichever
     interpreter happens to run the suite. Production loads this verifier under
     the root-owned `/usr/bin/python3.9`. A producer edit using syntax or a
     standard-library name that 3.9 does not have is invisible to a test run on
     a newer interpreter, and invisible to a stub by construction.
  2. VERIFIER BYTES. It never digests `immutable_tool_authority.py` itself. Its
     fixtures pin `content_sha256` and `bootstrap_sha256`, which are the digests
     of the TOOL, never of the verifier. A producer change is therefore
     undetectable from the consumer side.
  3. THE THREE LOADED GIT HEADS. Its fixture uses synthetic shas -- `"a" * 40`,
     `"b" * 40`, `"c" * 40`. The authority binds three REAL heads, and the
     target root is required to be `trees/<parent-sha>`, so the parent head is
     load-bearing rather than decorative. Synthetic values exercise the format
     check and nothing about the loaded chain.
  4. THE LOADED TOOL'S INODE. Mode and link checks are covered; an inode that
     does not match the loaded descriptor is not asserted as its own refusal.

NOT COVERED HERE, AND NOT CLAIMED. Two further legs the task names require the
wrkslots bridge, which is not published: the Agent Utils checkpoint carrying it
is not on the Agent Utils remote, and no file on that remote or on the revision
Hermit's gitlink names knows `DEV_HERMIT_TOOL_AUTHORITY` or the
`dev-hermit-tool-authority-v1` schema. Those legs are the bridge round trip and
the installed full-tree timing check. They are absent rather than stubbed,
because a stub here would recreate exactly the defect this file exists to close.

EVERY CASE BELOW WAS DEMONSTRATED FAILING against a real producer change before
being accepted. A case that cannot fail certifies the gap instead of closing it.
"""

from __future__ import annotations

import fcntl
import hashlib
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
VALIDATE_ROOT = HERE.parent
CHECKOUT_ROOT = VALIDATE_ROOT.parent.parent
sys.path.insert(0, str(VALIDATE_ROOT))
import immutable_tool_authority as authority_module  # noqa: E402

VERIFIER_PATH = VALIDATE_ROOT / "immutable_tool_authority.py"

# ⚠️ AN EXACT PIN, AND IT IS MEANT TO FAIL ON A DELIBERATE PRODUCER EDIT.
# The Agent Utils stub cannot notice that these bytes changed. Nothing else in
# the tree digests the verifier itself, so without this literal a dataclass,
# field, protocol or signature change reaches production unremarked. Whoever
# edits the verifier updates this line in the same commit, which is the whole
# point of pinning it rather than computing it.
PUBLISHED_VERIFIER_SHA256 = (
    "cd9dbcb99d0cc8d8e9dfebeeaaa0546a6d0eae525477e858277cb7c1a6c28bdd"
)

# The interpreter production actually loads the verifier under. Not
# `sys.executable`: that is whatever runs this suite, and the difference is the
# entire point of case 1.
PRODUCTION_INTERPRETER = Path("/usr/bin/python3.9")

# One isolated child that imports the published verifier BY PATH and loads an
# authority held by this test process. `-I -S` keeps the user site directory and
# this process's environment out of it, so the child exercises the standard
# library the interpreter actually ships.
CHILD_PROGRAM = """
import importlib.util, json, sys
spec = importlib.util.spec_from_file_location("published_verifier", sys.argv[1])
module = importlib.util.module_from_spec(spec)
# Register before executing. `dataclasses._is_type` resolves a field annotation
# through sys.modules[cls.__module__]; an unregistered module makes that None and
# the frozen dataclass fails to build. That is the LOADER's defect, not the
# verifier's -- reporting it as a producer failure would have been wrong.
sys.modules[spec.name] = module
spec.loader.exec_module(module)
from pathlib import Path
authority = module.read_immutable_tool_authority(
    Path(sys.argv[2]), Path(sys.argv[3]), json.loads(sys.argv[4])
)
print(
    json.dumps(
        {
            "parent_sha": authority.parent_sha,
            "hermit_sha": authority.hermit_sha,
            "agent_utils_sha": authority.agent_utils_sha,
            "content_sha256": authority.content_sha256,
        },
        sort_keys=True,
    )
)
"""


def git_text(arguments: list[str], *, cwd: Path) -> "str | None":
    """One committed Git fact, or ``None`` when this checkout cannot supply it."""
    try:
        result = subprocess.run(
            ["git", *arguments], cwd=cwd, capture_output=True, text=True, timeout=120
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if result.returncode != 0:
        return None
    return result.stdout.strip()


def resolve_loaded_heads() -> "tuple[dict[str, str] | None, str]":
    """The three heads the authority binds, read from this actual checkout.

    Returns ``(heads, reason)``. ``heads`` is ``None`` when this checkout cannot
    present the chain -- a linked worktree has an EMPTY `hermit` directory with
    no `.git`, so the nested gitlink is unreadable there.

    ⚠️ THIS RETURNS RATHER THAN SKIPPING, AND THAT IS THE WHOLE POINT. An earlier
    version raised SkipTest from setUp. Measured 2026-09-16 in a linked worktree
    at the landed tip: ALL EIGHT CASES SKIPPED and the suite reported `OK`. A
    file written to close a "cannot fail for the reason it names" gap had become
    exactly that. Only the chain leg needs these heads; every other leg runs
    without them, and `test_the_chain_leg_is_absent_only_for_a_known_reason`
    refuses a lookup that broke for any other cause.
    """
    parent_sha = git_text(["rev-parse", "HEAD"], cwd=CHECKOUT_ROOT)
    if parent_sha is None:
        return None, "this checkout has no readable HEAD"
    hermit_entry = git_text(["ls-tree", "HEAD", "hermit"], cwd=CHECKOUT_ROOT)
    if hermit_entry is None or len(hermit_entry.split()) < 3:
        return None, "this checkout records no hermit gitlink"
    hermit_sha = hermit_entry.split()[2]
    if not (CHECKOUT_ROOT / "hermit" / ".git").exists():
        return None, "the hermit submodule is not initialized in this checkout"
    agent_utils_entry = git_text(
        ["ls-tree", hermit_sha, "agent-utils"], cwd=CHECKOUT_ROOT / "hermit"
    )
    if agent_utils_entry is None or len(agent_utils_entry.split()) < 3:
        return None, "the hermit checkout cannot resolve its agent-utils gitlink"
    agent_utils_sha = agent_utils_entry.split()[2]
    for name, value in (
        ("parent", parent_sha),
        ("hermit", hermit_sha),
        ("agent-utils", agent_utils_sha),
    ):
        if len(value) != 40:
            return None, f"the {name} head is not a full sha: {value!r}"
    return (
        {
            "parent_sha": parent_sha,
            "hermit_sha": hermit_sha,
            "agent_utils_sha": agent_utils_sha,
        },
        "",
    )


LOADED_HEADS, HEADS_UNAVAILABLE_REASON = resolve_loaded_heads()


def fixture_heads() -> dict[str, str]:
    """Heads for the legs that do not assert the chain, so they always run.

    The parent value must be a full 40-hex because the verifier requires the
    cached target to be canonical ``trees/<parent-sha>``; it does not have to be
    the loaded head for the interpreter, byte-pin and inode legs.
    """
    if LOADED_HEADS is not None:
        return LOADED_HEADS
    return {
        "parent_sha": "0" * 40,
        "hermit_sha": "1" * 40,
        "agent_utils_sha": "2" * 40,
    }


def make_tree_read_only(path: Path) -> None:
    metadata = path.lstat()
    if stat.S_ISLNK(metadata.st_mode):
        return
    if path.is_dir():
        for child in path.iterdir():
            make_tree_read_only(child)
        path.chmod(0o555)
    elif path.is_file():
        path.chmod(0o555 if metadata.st_mode & 0o111 else 0o444)


class PublishedAuthorityFixture:
    """A qualifying sealed authority over REAL loaded heads.

    The descriptors live in this process, exactly as a holder's do in
    production, and the verifier is handed `/proc/<holder>/fd/<n>` capability
    paths. A separate process can therefore authenticate them, which is what
    case 1 does under the production interpreter.
    """

    def __init__(
        self,
        base: Path,
        heads: dict[str, str],
        *,
        sealed: bool = True,
        update: dict[str, object] | None = None,
    ) -> None:
        # The verifier requires the cached target to be canonical
        # `trees/<parent-sha>`, which is why the real parent head is
        # load-bearing here rather than a decorative field.
        target_root = base / "trees" / heads["parent_sha"]
        target_root.mkdir(parents=True)
        state_root = base / "state"
        state_root.mkdir()
        tool_root = base / "tool"
        tool_root.mkdir()
        (tool_root / "published-entrypoint").write_text("#!/bin/sh\nexit 0\n")
        make_tree_read_only(tool_root)
        make_tree_read_only(target_root)

        self.root_fd = os.open(
            tool_root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
        )
        self.state_fd = os.open(
            state_root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
        )
        self.target_fd = os.open(
            target_root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
        )
        self.authority_fd = os.memfd_create(
            "dev-hermit-tool-authority", os.MFD_CLOEXEC | os.MFD_ALLOW_SEALING
        )
        root_metadata = os.fstat(self.root_fd)
        state_metadata = os.fstat(self.state_fd)
        target_metadata = os.fstat(self.target_fd)
        content_sha256 = authority_module.immutable_tool_content_sha256(
            Path(f"/proc/{os.getpid()}/fd/{self.root_fd}")
        )
        self.record: dict[str, object] = {
            "schema": authority_module.TOOL_AUTHORITY_SCHEMA,
            "holder_pid": os.getpid(),
            "authority_fd": self.authority_fd,
            "target_fd": self.target_fd,
            "target_root": str(target_root.resolve()),
            "target_dev": target_metadata.st_dev,
            "target_ino": target_metadata.st_ino,
            "root_fd": self.root_fd,
            "root_dev": root_metadata.st_dev,
            "root_ino": root_metadata.st_ino,
            "state_fd": self.state_fd,
            "state_root": str(state_root.resolve()),
            "state_dev": state_metadata.st_dev,
            "state_ino": state_metadata.st_ino,
            "content_sha256": content_sha256,
            "parent_sha": heads["parent_sha"],
            "hermit_sha": heads["hermit_sha"],
            "agent_utils_sha": heads["agent_utils_sha"],
            "bootstrap_sha256": hashlib.sha256(b"published-bootstrap").hexdigest(),
        }
        self.record.update(update or {})
        os.write(
            self.authority_fd,
            json.dumps(self.record, sort_keys=True, separators=(",", ":")).encode()
            + b"\n",
        )
        os.fchmod(self.authority_fd, 0o400)
        if sealed:
            fcntl.fcntl(
                self.authority_fd,
                fcntl.F_ADD_SEALS,
                fcntl.F_SEAL_SEAL
                | fcntl.F_SEAL_SHRINK
                | fcntl.F_SEAL_GROW
                | fcntl.F_SEAL_WRITE,
            )
        self.tool_root = Path(f"/proc/{os.getpid()}/fd/{self.root_fd}")
        self.state_root = state_root
        self.authority_path = Path(f"/proc/{os.getpid()}/fd/{self.authority_fd}")

    def environment(self, **override: str) -> dict[str, str]:
        environment = {
            authority_module.TOOL_AUTHORITY_ENV: str(self.authority_path),
            authority_module.TOOL_CONTENT_SHA256_ENV: str(self.record["content_sha256"]),
            authority_module.TOOL_PARENT_SHA_ENV: str(self.record["parent_sha"]),
            authority_module.TOOL_HERMIT_SHA_ENV: str(self.record["hermit_sha"]),
            authority_module.TOOL_AGENT_UTILS_SHA_ENV: str(
                self.record["agent_utils_sha"]
            ),
            authority_module.TOOL_BOOTSTRAP_SHA256_ENV: str(
                self.record["bootstrap_sha256"]
            ),
        }
        environment.update(override)
        return environment

    def close(self) -> None:
        for descriptor in (
            self.authority_fd,
            self.state_fd,
            self.root_fd,
            self.target_fd,
        ):
            try:
                os.close(descriptor)
            except OSError:
                pass


class PublishedToolAuthorityTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="published-tool-authority-")
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.addCleanup(self.restore_writability)
        self.heads = fixture_heads()

    def restore_writability(self) -> None:
        for path in sorted(self.base.rglob("*"), reverse=True):
            try:
                if path.is_dir() and not path.is_symlink():
                    path.chmod(0o755)
                elif path.is_file():
                    path.chmod(0o644)
            except OSError:
                pass

    def fixture(self, **kwargs: object) -> PublishedAuthorityFixture:
        root = Path(tempfile.mkdtemp(dir=self.base))
        instance = PublishedAuthorityFixture(root, self.heads, **kwargs)  # type: ignore[arg-type]
        self.addCleanup(instance.close)
        return instance

    # -- 1. the production interpreter -------------------------------------

    def test_production_interpreter_is_present_and_root_owned(self) -> None:
        """Fail loudly rather than skip: a missing interpreter measures nothing."""
        self.assertTrue(
            PRODUCTION_INTERPRETER.exists(),
            f"{PRODUCTION_INTERPRETER} is absent; production loads the verifier under it",
        )
        metadata = PRODUCTION_INTERPRETER.stat()
        self.assertEqual(metadata.st_uid, 0, "the production interpreter must be root-owned")

    def test_published_verifier_loads_under_the_production_interpreter(self) -> None:
        """The real bytes, the real interpreter, a real held authority.

        `sys.executable` would pass here on a newer interpreter while production
        refused, which is precisely the case the stub cannot reach.
        """
        fixture = self.fixture()
        result = subprocess.run(
            [
                str(PRODUCTION_INTERPRETER),
                "-I",
                "-S",
                "-c",
                CHILD_PROGRAM,
                str(VERIFIER_PATH),
                str(fixture.tool_root),
                str(fixture.state_root),
                json.dumps(fixture.environment()),
            ],
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(
            result.returncode,
            0,
            f"the published verifier did not load under {PRODUCTION_INTERPRETER}: "
            f"{result.stderr.strip()[:800]}",
        )
        loaded = json.loads(result.stdout)
        self.assertEqual(loaded["parent_sha"], self.heads["parent_sha"])
        self.assertEqual(loaded["hermit_sha"], self.heads["hermit_sha"])
        self.assertEqual(loaded["agent_utils_sha"], self.heads["agent_utils_sha"])
        self.assertEqual(loaded["content_sha256"], fixture.record["content_sha256"])

    # -- 2. the verifier's own bytes ---------------------------------------

    def test_published_verifier_bytes_are_pinned(self) -> None:
        """A producer edit must be visible from the consumer side."""
        observed = hashlib.sha256(VERIFIER_PATH.read_bytes()).hexdigest()
        self.assertEqual(
            observed,
            PUBLISHED_VERIFIER_SHA256,
            "the published verifier's bytes changed. If that change is intended, "
            "update PUBLISHED_VERIFIER_SHA256 in the same commit and say in the "
            "message which field, signature or protocol moved.",
        )

    # -- 3. the three loaded Git heads -------------------------------------

    def test_three_loaded_git_heads_round_trip_through_the_authority(self) -> None:
        if LOADED_HEADS is None:
            self.skipTest(HEADS_UNAVAILABLE_REASON)
        fixture = self.fixture()
        authority = authority_module.read_immutable_tool_authority(
            fixture.tool_root, fixture.state_root, fixture.environment()
        )
        self.assertEqual(authority.parent_sha, LOADED_HEADS["parent_sha"])
        self.assertEqual(authority.hermit_sha, LOADED_HEADS["hermit_sha"])
        self.assertEqual(authority.agent_utils_sha, LOADED_HEADS["agent_utils_sha"])
        # The parent head is load-bearing, not decorative: the cached target must
        # be canonical trees/<parent-sha>.
        self.assertEqual(authority.target_root.name, LOADED_HEADS["parent_sha"])
        self.assertEqual(authority.target_root.parent.name, "trees")

    def test_each_loaded_head_must_match_its_environment_binding(self) -> None:
        for variable in (
            authority_module.TOOL_PARENT_SHA_ENV,
            authority_module.TOOL_HERMIT_SHA_ENV,
            authority_module.TOOL_AGENT_UTILS_SHA_ENV,
        ):
            with self.subTest(binding=variable):
                fixture = self.fixture()
                environment = fixture.environment(**{variable: "d" * 40})
                with self.assertRaisesRegex(
                    ValueError, "does not match immutable tool authority"
                ):
                    authority_module.read_immutable_tool_authority(
                        fixture.tool_root, fixture.state_root, environment
                    )

    # -- 4. the loaded tool's inode ----------------------------------------

    def test_wrong_loaded_tool_inode_is_refused(self) -> None:
        # An inode that is not the loaded descriptor's. Everything else in the
        # record stays exact, so only the identity check can refuse it.
        fixture = self.fixture(update={"root_ino": 1})
        with self.assertRaisesRegex(
            ValueError, "immutable tool root descriptor identity does not match"
        ):
            authority_module.read_immutable_tool_authority(
                fixture.tool_root, fixture.state_root, fixture.environment()
            )

    # -- the qualifying case and one seal control --------------------------

    def test_qualifying_authority_loads_in_this_process(self) -> None:
        fixture = self.fixture()
        authority = authority_module.read_immutable_tool_authority(
            fixture.tool_root, fixture.state_root, fixture.environment()
        )
        self.assertEqual(authority.holder_pid, os.getpid())
        self.assertEqual(authority.content_sha256, fixture.record["content_sha256"])

    def test_unsealed_authority_is_refused(self) -> None:
        fixture = self.fixture(sealed=False)
        with self.assertRaises(ValueError):
            authority_module.read_immutable_tool_authority(
                fixture.tool_root, fixture.state_root, fixture.environment()
            )


class ChainAvailabilityGuard(unittest.TestCase):
    """The chain leg may be absent, but only for a reason this file recognises.

    Without this, a lookup that broke for ANY new cause would silently downgrade
    the chain leg to a skip and the suite would still report OK -- which is how
    all eight cases skipped in a linked worktree before it existed.
    """

    KNOWN_ABSENCES = frozenset(
        {"the hermit submodule is not initialized in this checkout"}
    )

    def test_the_chain_leg_is_absent_only_for_a_known_reason(self) -> None:
        if LOADED_HEADS is not None:
            return
        self.assertIn(
            HEADS_UNAVAILABLE_REASON,
            self.KNOWN_ABSENCES,
            "the loaded-head lookup failed for an unrecognised reason, so the "
            "chain leg was downgraded to a skip rather than measured: "
            f"{HEADS_UNAVAILABLE_REASON!r}",
        )


if __name__ == "__main__":
    unittest.main()
