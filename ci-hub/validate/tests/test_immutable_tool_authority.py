from __future__ import annotations

import fcntl
import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import immutable_tool_authority as authority_module  # noqa: E402


SHA = "a" * 40
HERMIT_SHA = "b" * 40
AGENT_UTILS_SHA = "c" * 40


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


class AuthorityFixture:
    def __init__(
        self,
        root: Path,
        state_root: Path,
        target_root: Path,
        *,
        bootstrap_sha256: str,
        sealed: bool = True,
        update: dict[str, object] | None = None,
        remove: set[str] | None = None,
    ) -> None:
        self.root_fd = os.open(
            root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
        )
        self.state_fd = os.open(
            state_root,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
        )
        self.target_fd = os.open(
            target_root,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
        )
        self.authority_fd = os.memfd_create(
            "dev-hermit-tool-authority",
            os.MFD_CLOEXEC | os.MFD_ALLOW_SEALING,
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
            "parent_sha": SHA,
            "hermit_sha": HERMIT_SHA,
            "agent_utils_sha": AGENT_UTILS_SHA,
            "bootstrap_sha256": bootstrap_sha256,
        }
        self.record.update(update or {})
        for name in remove or set():
            self.record.pop(name, None)
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
        self.target_root = target_root
        self.authority_path = Path(
            f"/proc/{os.getpid()}/fd/{self.authority_fd}"
        )

    def environment(self) -> dict[str, str]:
        return {
            authority_module.TOOL_AUTHORITY_ENV: str(self.authority_path),
            authority_module.TOOL_CONTENT_SHA256_ENV: str(
                self.record.get("content_sha256", "")
            ),
            authority_module.TOOL_PARENT_SHA_ENV: str(self.record.get("parent_sha", "")),
            authority_module.TOOL_HERMIT_SHA_ENV: str(self.record.get("hermit_sha", "")),
            authority_module.TOOL_AGENT_UTILS_SHA_ENV: str(
                self.record.get("agent_utils_sha", "")
            ),
            authority_module.TOOL_BOOTSTRAP_SHA256_ENV: str(
                self.record.get("bootstrap_sha256", "")
            ),
        }

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


class ExecutingAuthorityFixture(AuthorityFixture):
    """Real distinct Git code/state roots for installed-caller regressions."""

    def __init__(self, *, sealed: bool = True) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="executing-tool-authority-")
        self.base = Path(self.temporary.name).resolve()
        self.code = self.base / "executable"
        state = self.base / "state"
        heads = []
        for path, marker in ((self.code, "code"), (state, "mutable state")):
            path.mkdir()
            (path / "marker").write_text(marker)
            for arguments in (
                ["init", "-b", "main"],
                ["config", "user.name", "Authority Fixture"],
                ["config", "user.email", "fixture@example.com"],
                ["add", "marker"],
                ["commit", "-m", marker],
            ):
                subprocess.run(
                    ["git", "-C", str(path), *arguments],
                    check=True, capture_output=True, text=True,
                )
            heads.append(subprocess.run(
                ["git", "-C", str(path), "rev-parse", "HEAD^{commit}"],
                check=True, capture_output=True, text=True,
            ).stdout.strip())
        self.code_head, self.state_head = heads
        target = self.base / "cache/trees" / self.code_head
        target.mkdir(parents=True)
        make_tree_read_only(self.code)
        make_tree_read_only(target)
        super().__init__(
            self.code, state, target, bootstrap_sha256="d" * 64,
            sealed=sealed, update={"parent_sha": self.code_head},
        )

    def environment(self) -> dict[str, str]:
        return dict(
            super().environment(),
            HOME=str(self.base / "home"),
            DEV_HERMIT_TOOL_ROOT=str(self.tool_root),
            DEV_HERMIT_PARENT=str(self.state_root),
        )

    def __enter__(self) -> ExecutingAuthorityFixture:
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()
        for directory, _children, _files in os.walk(self.base):
            Path(directory).chmod(0o755)
        self.temporary.cleanup()


class ImmutableToolAuthorityTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="immutable-tool-authority-")
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name).resolve()
        self.sequence = 0
        self.addCleanup(self.make_writable)

    def make_writable(self) -> None:
        # Fixtures deliberately freeze directories; restore only this test's
        # private tree so TemporaryDirectory can remove it after closing FDs.
        for directory, _children, _files in os.walk(self.base):
            Path(directory).chmod(0o755)

    def fixture(self, **kwargs: object) -> AuthorityFixture:
        self.sequence += 1
        base = self.base / str(self.sequence)
        tool = base / "executable"
        (tool / "ci-hub").mkdir(parents=True)
        (tool / "ci-hub" / "payload").write_bytes(b"pinned executable bytes\n")
        (tool / "marker").symlink_to("ci-hub/payload")
        target = base / "cache" / "trees" / SHA
        target.mkdir(parents=True)
        state = base / "state"
        state.mkdir()
        make_tree_read_only(tool)
        make_tree_read_only(target)
        fixture = AuthorityFixture(
            tool, state, target, bootstrap_sha256="d" * 64, **kwargs
        )
        self.addCleanup(fixture.close)
        return fixture

    @staticmethod
    def read(fixture: AuthorityFixture) -> authority_module.ImmutableToolAuthority:
        return authority_module.read_immutable_tool_authority(
            fixture.tool_root,
            fixture.state_root,
            fixture.environment(),
        )

    def test_qualifying_authority_loads_in_isolated_standard_library_process(self) -> None:
        fixture = self.fixture()
        script = """
import importlib.util
import json
import sys
from pathlib import Path

spec = importlib.util.spec_from_file_location("pinned_authority", sys.argv[1])
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
environment = json.loads(sys.argv[4])
observed = module.read_immutable_tool_authority(
    Path(sys.argv[2]), Path(sys.argv[3]), environment
)
print(observed.parent_sha, observed.hermit_sha, observed.agent_utils_sha)
environment[module.TOOL_BOOTSTRAP_SHA256_ENV] = "e" * 64
try:
    module.read_immutable_tool_authority(
        Path(sys.argv[2]), Path(sys.argv[3]), environment
    )
except ValueError as error:
    print(str(error))
else:
    raise AssertionError("changed bootstrap binding was accepted")
"""
        result = subprocess.run(
            [
                sys.executable,
                "-I",
                "-S",
                "-c",
                script,
                authority_module.__file__,
                str(fixture.tool_root),
                str(fixture.record["state_root"]),
                json.dumps(fixture.environment()),
            ],
            cwd=self.base,
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(
            [
                f"{SHA} {HERMIT_SHA} {AGENT_UTILS_SHA}",
                "DEV_HERMIT_TOOL_BOOTSTRAP_SHA256 does not match immutable tool authority",
            ],
            result.stdout.splitlines(),
        )
        observed = self.read(fixture)
        self.assertIs(type(observed), authority_module.ImmutableToolAuthority)
        self.assertEqual(fixture.record["content_sha256"], observed.content_sha256)
        self.assertEqual(fixture.record["state_ino"], observed.state_ino)
        self.assertEqual(fixture.record["root_ino"], observed.root_ino)
        self.assertEqual(fixture.record["target_ino"], observed.target_ino)

    def test_protocol_and_typed_fields_still_refuse_mutations(self) -> None:
        cases = (
            ({"unknown": "field"}, "fields are incomplete or unknown"),
            ({"schema": "dev-hermit-tool-authority-v2"}, "unsupported schema"),
            ({"holder_pid": True}, "nonnegative integer holder_pid"),
            ({"root_fd": -1}, "nonnegative integer root_fd"),
            ({"state_ino": "1"}, "nonnegative integer state_ino"),
            ({"bootstrap_sha256": None}, "no string bootstrap_sha256"),
            ({"parent_sha": "A" * 40}, "invalid parent SHA"),
            ({"hermit_sha": "b" * 39}, "invalid Hermit SHA"),
            ({"agent_utils_sha": "c" * 41}, "invalid agent-utils SHA"),
            ({"bootstrap_sha256": "d" * 63}, "invalid bootstrap digest"),
            ({"content_sha256": "e" * 65}, "invalid content digest"),
            ({"root_dev": 0}, "root descriptor identity"),
            ({"target_dev": 0}, "target descriptor identity"),
            ({"state_dev": 0}, "state-root descriptor identity"),
            ({"extra": "x" * 4096}, "exceeds 4096 bytes"),
        )
        for update, refusal in cases:
            with self.subTest(update=update):
                fixture = self.fixture(update=update)
                with self.assertRaisesRegex(ValueError, refusal):
                    self.read(fixture)
        for field in authority_module.TOOL_AUTHORITY_FIELDS:
            with self.subTest(missing=field):
                fixture = self.fixture(remove={field})
                with self.assertRaisesRegex(ValueError, "fields are incomplete"):
                    self.read(fixture)

    def test_each_required_seal_remains_required(self) -> None:
        required = (
            fcntl.F_SEAL_SEAL,
            fcntl.F_SEAL_SHRINK,
            fcntl.F_SEAL_GROW,
            fcntl.F_SEAL_WRITE,
        )
        for missing in required:
            with self.subTest(missing_seal=missing):
                fixture = self.fixture(sealed=False)
                fcntl.fcntl(
                    fixture.authority_fd,
                    fcntl.F_ADD_SEALS,
                    sum(seal for seal in required if seal != missing),
                )
                with self.assertRaisesRegex(ValueError, "completely sealed"):
                    self.read(fixture)

    def test_each_environment_binding_remains_required_and_exact(self) -> None:
        fixture = self.fixture()
        names = (
            authority_module.TOOL_CONTENT_SHA256_ENV,
            authority_module.TOOL_PARENT_SHA_ENV,
            authority_module.TOOL_HERMIT_SHA_ENV,
            authority_module.TOOL_AGENT_UTILS_SHA_ENV,
            authority_module.TOOL_BOOTSTRAP_SHA256_ENV,
        )
        for name in names:
            for missing in (False, True):
                with self.subTest(name=name, missing=missing):
                    environment = fixture.environment()
                    if missing:
                        del environment[name]
                    else:
                        environment[name] = "changed"
                    with self.assertRaisesRegex(ValueError, name):
                        authority_module.read_immutable_tool_authority(
                            fixture.tool_root,
                            Path(str(fixture.record["state_root"])),
                            environment,
                        )

    def test_content_bytes_symlink_and_modes_remain_bound(self) -> None:
        for change in ("bytes", "symlink", "writable-file", "writable-root", "hardlink"):
            with self.subTest(change=change):
                fixture = self.fixture()
                tool = fixture.tool_root.resolve()
                payload = tool / "ci-hub" / "payload"
                if change == "bytes":
                    payload.chmod(0o644)
                    payload.write_bytes(b"mutated executable bytes\n")
                    payload.chmod(0o444)
                    refusal = "content digest is"
                elif change == "symlink":
                    tool.chmod(0o755)
                    (tool / "marker").unlink()
                    (tool / "marker").symlink_to("ci-hub/different")
                    tool.chmod(0o555)
                    refusal = "content digest is"
                elif change == "writable-file":
                    payload.chmod(0o644)
                    refusal = "writable or multiply linked"
                elif change == "writable-root":
                    tool.chmod(0o755)
                    refusal = "root descriptor identity"
                else:
                    os.link(payload, self.base / f"alias-{self.sequence}")
                    refusal = "writable or multiply linked"
                with self.assertRaisesRegex(ValueError, refusal):
                    self.read(fixture)

    def test_unbound_path_and_replaced_state_remain_refused(self) -> None:
        fixture = self.fixture()
        state = Path(str(fixture.record["state_root"]))
        foreign = self.base / "foreign-state"
        foreign.mkdir()
        with self.assertRaisesRegex(ValueError, "does not match immutable tool authority state root"):
            authority_module.read_immutable_tool_authority(
                fixture.tool_root, foreign, fixture.environment()
            )
        with self.assertRaisesRegex(ValueError, "must be an exact /proc"):
            authority_module.read_immutable_tool_authority(
                fixture.tool_root.resolve(), state, fixture.environment()
            )
        state.rename(state.with_name("retained-state"))
        state.mkdir()
        with self.assertRaisesRegex(ValueError, "pathname no longer names"):
            self.read(fixture)


if __name__ == "__main__":
    unittest.main()
