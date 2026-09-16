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

The bridge tests below additionally load the committed nested Agent Utils
package in a separate process and execute its real authenticated verifier
loader. One local three-repository fixture serves all negative cases. Its
qualifying revisions and source bytes come from this checkout's actual pins;
deliberately committed verifier variants are negative fixtures only.

The installed-wrapper/full-tree timing observation remains separate evidence.
These tests neither dispatch the installed wrapper nor measure its deployment
cost, and they do not change the bridge's production 15-second timeout.

EVERY CASE BELOW WAS DEMONSTRATED FAILING against a real producer change before
being accepted. A case that cannot fail certifies the gap instead of closing it.
"""

from __future__ import annotations

import fcntl
import contextlib
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
        tool_root: Path | None = None,
        state_root: Path | None = None,
        target_root: Path | None = None,
        bootstrap_sha256: str | None = None,
    ) -> None:
        # The verifier requires the cached target to be canonical
        # `trees/<parent-sha>`, which is why the real parent head is
        # load-bearing here rather than a decorative field.
        if target_root is None:
            target_root = base / "trees" / heads["parent_sha"]
            target_root.mkdir(parents=True)
            make_tree_read_only(target_root)
        if state_root is None:
            state_root = base / "state"
            state_root.mkdir()
        if tool_root is None:
            tool_root = base / "tool"
            tool_root.mkdir()
            (tool_root / "published-entrypoint").write_text("#!/bin/sh\nexit 0\n")
            make_tree_read_only(tool_root)

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
            "bootstrap_sha256": bootstrap_sha256
            or hashlib.sha256(b"published-bootstrap").hexdigest(),
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
            authority_module.TOOL_CONTENT_SHA256_ENV: str(
                self.record["content_sha256"]
            ),
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
        self.assertEqual(
            metadata.st_uid, 0, "the production interpreter must be root-owned"
        )

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


# This child imports the actual nested package, not a copied function or the
# generic contract stub. The observer delegates every command unchanged and
# records the interpreter/bounds selected by production, including on refusal.
BRIDGE_PROGRAM = r"""
import hashlib, json, sys
from pathlib import Path

tool = Path(sys.argv[1])
state = Path(sys.argv[2])
sys.path.insert(0, str(tool / "hermit/agent-utils/py"))
from wrkslots import cli

loaded = Path(cli.__file__).resolve(strict=True)
assert loaded == tool / "hermit/agent-utils/py/wrkslots/cli.py"
config = cli.Config(
    root=state, config_path=state / ".wrkslots.yml",
    worktrees=state / "worktrees", control=state / "worktrees",
    machine="published-authority-fixture", default_remote="origin",
    default_landed_ref="refs/remotes/origin/main", heartbeat_ttl_seconds=60,
    liveness_command=state / "unused-liveness.py",
)
commands = []
real_command = cli._run_bounded_read_only_command
def observed_command(command, **kwargs):
    if "-c" in command and command[command.index("-c") + 1] == cli._FROZEN_TOOL_AUTHORITY_BOOTSTRAP:
        commands.append({
            "interpreter": command[0],
            "flags": list(command[1:3]),
            "timeout_seconds": kwargs["timeout_seconds"],
            "stdout_limit": kwargs["stdout_limit"],
            "stderr_limit": kwargs["stderr_limit"],
            "snapshot_sha256": hashlib.sha256(Path(command[-1]).read_bytes()).hexdigest(),
        })
    return real_command(command, **kwargs)
cli._run_bounded_read_only_command = observed_command
report = {"loaded_module": str(loaded), "loaded_sha256": hashlib.sha256(loaded.read_bytes()).hexdigest()}
try:
    authority, head, blobs = cli._authenticated_frozen_parser_blobs(config)
except cli.Refusal as error:
    report.update(state="refused", detail=str(error))
    code = 3
else:
    report.update(state="accepted", authority=str(authority), head=head,
                  blobs={name: hashlib.sha256(value).hexdigest() for name, value in blobs.items()})
    code = 0
report["verifier_commands"] = commands
print(json.dumps(report, sort_keys=True))
raise SystemExit(code)
"""


class PublishedWrkslotsBridgeTests(unittest.TestCase):
    """Real loaded Git chain and producer, with no verifier/Git result mocks."""

    @staticmethod
    def git(path: Path, *arguments: str) -> str:
        result = subprocess.run(
            [
                "/usr/bin/git",
                "--no-replace-objects",
                "-c",
                "core.hooksPath=/dev/null",
                *arguments,
            ],
            cwd=path,
            capture_output=True,
            text=True,
            timeout=30,
            env=dict(
                os.environ, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null"
            ),
        )
        if result.returncode:
            raise AssertionError(f"fixture git {arguments!r}: {result.stderr}")
        return result.stdout.strip()

    @classmethod
    def setUpClass(cls) -> None:
        # Do not convert missing prerequisites into an all-green skipped bridge.
        if LOADED_HEADS is None:
            raise AssertionError(
                f"real bridge requires initialized pins: {HEADS_UNAVAILABLE_REASON}"
            )
        cls.temporary = tempfile.TemporaryDirectory(prefix="published-wrkslots-bridge-")
        cls.addClassCleanup(cls.temporary.cleanup)
        cls.base = Path(cls.temporary.name).resolve()
        cls.addClassCleanup(cls.restore_fixture_modes)
        cls.tool = cls.base / "tool"
        cls.state = cls.base / "state"
        cls.other_state = cls.base / "other-state"
        # --shared borrows immutable Git objects, not working files. All index,
        # ref and working-file mutations below stay in the disposable clones.
        for relative, pin in (
            ("", "parent_sha"),
            ("hermit", "hermit_sha"),
            ("hermit/agent-utils", "agent_utils_sha"),
        ):
            source = CHECKOUT_ROOT / relative
            destination = cls.tool / relative
            if cls.git(source, "rev-parse", "HEAD") != LOADED_HEADS[pin]:
                raise AssertionError(
                    f"materialized {relative or 'parent'} HEAD differs from its recorded pin"
                )
            cls.git(
                cls.base,
                "clone",
                "--quiet",
                "--shared",
                "--no-recurse-submodules",
                str(source),
                str(destination),
            )
            cls.git(destination, "checkout", "--quiet", "--detach", LOADED_HEADS[pin])
        for state in (cls.state, cls.other_state):
            cls.git(
                cls.base,
                "clone",
                "--quiet",
                "--shared",
                "--no-checkout",
                str(CHECKOUT_ROOT),
                str(state),
            )
        cls.module = cls.tool / "ci-hub/validate/immutable_tool_authority.py"
        if cls.module.read_bytes() != VERIFIER_PATH.read_bytes():
            raise AssertionError(
                "working verifier differs from the committed fixture source"
            )
        cls.agent_sha256 = hashlib.sha256(
            (cls.tool / "hermit/agent-utils/py/wrkslots/cli.py").read_bytes()
        ).hexdigest()
        cls.bootstrap_sha256 = hashlib.sha256(
            (cls.tool / "scripts/hermit-health-tick.sh").read_bytes()
        ).hexdigest()
        cls.other_tool = cls.base / "other-tool"
        cls.other_tool.mkdir()
        (cls.other_tool / "payload").write_bytes(
            b"authority for a different loaded directory\n"
        )
        make_tree_read_only(cls.other_tool)
        # The producer excludes the three Git administration directories from
        # its digest. Keep those fixture-only directories writable for explicit
        # negative HEAD variants; freeze every executable-tree byte and mode.
        for directory, children, files in os.walk(cls.tool):
            children[:] = [name for name in children if name != ".git"]
            for name in files:
                path = Path(directory) / name
                if not path.is_symlink():
                    path.chmod(0o555 if path.stat().st_mode & 0o111 else 0o444)
            Path(directory).chmod(0o555)

    @classmethod
    def restore_fixture_modes(cls) -> None:
        for directory, _children, files in os.walk(cls.base):
            Path(directory).chmod(0o755)
            for name in files:
                path = Path(directory) / name
                if not path.is_symlink():
                    path.chmod(0o644)

    def setUp(self) -> None:
        self.assertEqual(
            self.git(self.tool, "rev-parse", "HEAD"), LOADED_HEADS["parent_sha"]
        )
        self.assertEqual(self.module.read_bytes(), VERIFIER_PATH.read_bytes())

    @contextlib.contextmanager
    def authority(self, *, update=None, sealed=True, other_tool=False):
        heads = dict(LOADED_HEADS, parent_sha=self.git(self.tool, "rev-parse", "HEAD"))
        record_heads = dict(
            heads,
            **{key: value for key, value in (update or {}).items() if key in heads},
        )
        case = Path(tempfile.mkdtemp(dir=self.base))
        fixture = PublishedAuthorityFixture(
            case,
            record_heads,
            sealed=sealed,
            update=update,
            tool_root=self.other_tool if other_tool else self.tool,
            state_root=self.state,
            bootstrap_sha256=self.bootstrap_sha256,
        )
        try:
            yield fixture
        finally:
            fixture.close()

    @contextlib.contextmanager
    def changed_bytes(self, path: Path, data: bytes):
        before = path.read_bytes()
        mode = stat.S_IMODE(path.stat().st_mode)
        path.chmod(0o600)
        path.write_bytes(data)
        path.chmod(mode)
        try:
            yield
        finally:
            path.chmod(0o600)
            path.write_bytes(before)
            path.chmod(mode)

    @contextlib.contextmanager
    def committed_verifier_variant(self, addition: bytes):
        """An actual incompatible producer commit, only in the owned fixture."""
        original = self.git(self.tool, "rev-parse", "HEAD")
        with self.changed_bytes(self.module, self.module.read_bytes() + addition):
            self.git(self.tool, "add", "ci-hub/validate/immutable_tool_authority.py")
            self.git(
                self.tool,
                "-c",
                "user.name=Authority fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "-m",
                "Deliberately incompatible verifier fixture",
            )
            variant = self.git(self.tool, "rev-parse", "HEAD")
            try:
                yield
            finally:
                self.git(self.tool, "read-tree", original)
                self.git(self.tool, "update-ref", "HEAD", original, variant)

    def run_bridge(self, fixture, *, refusal=None, state=None, verifier_called=True):
        result = subprocess.run(
            [
                sys.executable,
                "-I",
                "-B",
                "-c",
                BRIDGE_PROGRAM,
                str(self.tool),
                str(state or self.state),
            ],
            env=dict(os.environ, **fixture.environment()),
            cwd=self.base,
            capture_output=True,
            text=True,
            timeout=45,
        )
        self.assertEqual(
            result.returncode, 3 if refusal else 0, result.stdout + result.stderr
        )
        self.assertEqual(result.stderr, "")
        report = json.loads(result.stdout)
        print(
            "BRIDGE_CASE "
            + json.dumps(
                {"test": self.id(), "returncode": result.returncode, **report},
                sort_keys=True,
            ),
            flush=True,
        )
        self.assertEqual(
            report["loaded_module"],
            str(self.tool / "hermit/agent-utils/py/wrkslots/cli.py"),
        )
        self.assertEqual(report["loaded_sha256"], self.agent_sha256)
        self.assertEqual(len(report["verifier_commands"]), int(verifier_called))
        if verifier_called:
            self.assertEqual(
                report["verifier_commands"][0],
                {
                    "interpreter": str(PRODUCTION_INTERPRETER),
                    "flags": ["-I", "-B"],
                    "timeout_seconds": 15,
                    "stdout_limit": 16 * 1024,
                    "stderr_limit": 16 * 1024,
                    "snapshot_sha256": hashlib.sha256(
                        self.module.read_bytes()
                    ).hexdigest(),
                },
            )
        if refusal:
            self.assertEqual(report["state"], "refused")
            self.assertIn(refusal, report["detail"])
        else:
            self.assertEqual(report["state"], "accepted")
            self.assertEqual(report["authority"], str(self.tool))
            self.assertEqual(report["head"], LOADED_HEADS["parent_sha"])
            self.assertEqual(
                report["blobs"],
                {
                    f"ci-hub/validate/{name}.py": hashlib.sha256(
                        (self.tool / f"ci-hub/validate/{name}.py").read_bytes()
                    ).hexdigest()
                    for name in (
                        "run_registry",
                        "service_result",
                        "final_validate_status",
                    )
                },
            )

    def test_separate_state_clone_passes_the_real_loaded_bridge(self) -> None:
        self.assertNotEqual(
            self.git(
                self.tool, "rev-parse", "--path-format=absolute", "--git-common-dir"
            ),
            self.git(
                self.state, "rev-parse", "--path-format=absolute", "--git-common-dir"
            ),
        )
        with self.authority() as fixture:
            self.run_bridge(fixture)

    def test_materialized_heads_must_match_both_enclosing_gitlinks(self) -> None:
        for relative, detail in (
            ("hermit/agent-utils", "loaded agent-utils HEAD differs"),
            ("hermit", "enclosing checkout HEAD differs"),
        ):
            with self.subTest(repository=relative):
                repository = self.tool / relative
                original = self.git(repository, "rev-parse", "HEAD")
                previous = self.git(
                    repository,
                    "-c",
                    "user.name=Authority fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit-tree",
                    "HEAD^{tree}",
                    "-p",
                    original,
                    "-m",
                    "Fixture HEAD disagreement with enclosing Gitlink",
                )
                with self.authority() as fixture:
                    self.git(repository, "update-ref", "HEAD", previous, original)
                    try:
                        self.run_bridge(fixture, refusal=detail, verifier_called=False)
                    finally:
                        self.git(repository, "update-ref", "HEAD", original, previous)

    def test_unsealed_descriptor_refuses_through_the_bridge(self) -> None:
        with self.authority(sealed=False) as fixture:
            self.run_bridge(fixture, refusal="not bounded, anonymous and sealed")

    def test_changed_signed_content_refuses_through_the_bridge(self) -> None:
        path = self.tool / "ci-hub/validate/final_validate_status.py"
        with self.authority() as fixture, self.changed_bytes(
            path, path.read_bytes() + b"\n# changed after signing\n"
        ):
            self.run_bridge(fixture, refusal="immutable tool content digest is")

    def test_each_sealed_pin_must_match_the_actual_loaded_git_chain(self) -> None:
        for pin in ("parent_sha", "hermit_sha", "agent_utils_sha"):
            with self.subTest(pin=pin), self.authority(
                update={pin: "e" * 40}
            ) as fixture:
                self.run_bridge(
                    fixture,
                    refusal="sealed tool pins differ from the loaded enclosing Git chain",
                )

    def test_valid_authority_for_another_loaded_inode_refuses(self) -> None:
        with self.authority(other_tool=True) as fixture:
            self.run_bridge(
                fixture,
                refusal="sealed tool root_ino differs from the loaded authority/state root",
            )

    def test_valid_authority_for_another_state_clone_refuses(self) -> None:
        with self.authority() as fixture:
            self.run_bridge(
                fixture,
                state=self.other_state,
                refusal="does not match immutable tool authority state root",
            )

    def test_changed_verifier_bytes_refuse_before_execution(self) -> None:
        with self.authority() as fixture, self.changed_bytes(
            self.module, self.module.read_bytes() + b"\n# uncommitted verifier change\n"
        ):
            self.run_bridge(
                fixture,
                refusal="tool authority module differs from authenticated HEAD",
                verifier_called=False,
            )

    def test_incompatible_protocol_refuses_through_the_bridge(self) -> None:
        for update, detail in (
            ({"schema": "dev-hermit-tool-authority-v2"}, "unsupported schema"),
            ({"unknown_field": 1}, "fields are incomplete or unknown"),
        ):
            with self.subTest(update=update), self.authority(update=update) as fixture:
                self.run_bridge(fixture, refusal=detail)

    def test_real_python39_rejects_incompatible_committed_verifiers(self) -> None:
        cases = (
            (b"\nmatch 1:\n    case 1:\n        pass\n", "SyntaxError"),
            (
                b"\ndef read_immutable_tool_authority(tool_root, state_root):\n    raise AssertionError('must fail at the call signature')\n",
                "TypeError",
            ),
        )
        for addition, detail in cases:
            with self.subTest(detail=detail), self.committed_verifier_variant(
                addition
            ), self.authority() as fixture:
                self.run_bridge(fixture, refusal=detail)


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
