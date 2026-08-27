#!/usr/bin/env python3
"""Regression tests for the publication-exempt command."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import io
import os
import subprocess
import tempfile
import textwrap
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock


CI_HUB = Path(__file__).resolve().parents[2]
MODULE_PATH = CI_HUB / "bin" / "publication-exempt"
LOADER = importlib.machinery.SourceFileLoader("publication_exempt", str(MODULE_PATH))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
assert SPEC is not None
publication_exempt = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(publication_exempt)


class RecordingRunner:
    def __init__(self, returncode: int = 0) -> None:
        self.returncode = returncode
        self.command: list[str] = []
        self.body = b""
        self.note_path: Path | None = None
        self.env: dict[str, str] = {}

    def __call__(self, command, *, cwd=None, env=None, **_kwargs):
        self.command = list(command)
        self.note_path = Path(command[3])
        self.body = self.note_path.read_bytes()
        self.env = dict(env or {})
        return subprocess.CompletedProcess(
            command,
            self.returncode,
            "tg-note-verified: STORED AND VERIFIED byte-for-byte\n"
            if self.returncode == 0
            else "",
            "deliberate verification failure" if self.returncode != 0 else "",
        )


class PublicationExemptCommandTest(unittest.TestCase):
    def test_reason_file_reaches_verified_writer_byte_for_byte(self) -> None:
        reason = r"nothing owed: $(touch /tmp/nope) `uname` $? \\ \n \t C:\tmp\file"
        with tempfile.TemporaryDirectory() as tmp:
            reason_file = Path(tmp) / "reason.txt"
            reason_file.write_text(reason, encoding="utf-8")
            runner = RecordingRunner()
            stdout = io.StringIO()
            with redirect_stdout(stdout):
                rc = publication_exempt.main(
                    [
                        "drain-test",
                        "--repo",
                        "rrnewton/hermit",
                        "--pr",
                        "2738",
                        "--reason-file",
                        str(reason_file),
                        "--who",
                        "hermit-test",
                    ],
                    run=runner,
                    actor_resolver=lambda claimed: claimed,
                )

        expected = publication_exempt.publication_record.render_exemption(
            "rrnewton/hermit", 2738, "hermit-test", reason
        ).encode()
        self.assertEqual(rc, 0)
        self.assertEqual(runner.body, expected)
        self.assertTrue(runner.command[0].endswith("/tg-note-verified"))
        self.assertEqual(runner.command[1:3], ["drain-test", "--file"])
        self.assertEqual(runner.env.get("TG_NOTE_PRIVATE_TEMP"), "1")
        self.assertIsNotNone(runner.note_path)
        self.assertFalse(runner.note_path.exists(), "private note file was not removed")
        self.assertIn("STORED AND VERIFIED byte-for-byte", stdout.getvalue())

    def test_verified_writer_failure_is_reported_and_not_called_success(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            reason_file = Path(tmp) / "reason.txt"
            reason_file.write_text("nothing was owed", encoding="utf-8")
            runner = RecordingRunner(returncode=2)
            stdout = io.StringIO()
            stderr = io.StringIO()
            with redirect_stdout(stdout), redirect_stderr(stderr):
                rc = publication_exempt.main(
                    [
                        "drain-test",
                        "--repo",
                        "rrnewton/hermit",
                        "--pr",
                        "2747",
                        "--reason-file",
                        str(reason_file),
                    ],
                    run=runner,
                    actor_resolver=lambda _claimed: "hermit-test",
                )

        self.assertEqual(rc, 2)
        self.assertNotIn("recorded exemption", stdout.getvalue())
        self.assertIn("tg-note-verified failed (rc=2)", stderr.getvalue())
        self.assertFalse(runner.note_path.exists(), "failed note file was not removed")

    def test_inline_reason_argument_is_rejected(self) -> None:
        with redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as raised:
                publication_exempt.main(
                    [
                        "drain-test",
                        "--repo",
                        "rrnewton/hermit",
                        "--pr",
                        "2738",
                        "--reason",
                        "unsafe inline text",
                    ],
                    actor_resolver=lambda _claimed: "hermit-test",
                )
        self.assertEqual(raised.exception.code, 2)

    def test_real_verified_writer_reads_back_awkward_reason_byte_for_byte(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            sentinel = root / "must-not-exist"
            reason = rf"nothing owed: $(touch {sentinel}) `uname` $? \\ \n \t C:\tmp\file"
            reason_file = root / "reason.txt"
            store = root / "stored-note"
            fake_bin = root / "bin"
            fake_bin.mkdir()
            reason_file.write_text(reason, encoding="utf-8")
            fake_tg = fake_bin / "tg"
            fake_tg.write_text(
                textwrap.dedent(
                    """\
                    #!/usr/bin/env python3
                    import os
                    import sys
                    from pathlib import Path

                    store = Path(os.environ["FAKE_TG_STORE"])

                    def decode(value: str) -> str:
                        out = []
                        i = 0
                        while i < len(value):
                            if value[i] == "\\\\" and i + 1 < len(value):
                                escaped = value[i + 1]
                                if escaped == "\\\\":
                                    out.append("\\\\")
                                    i += 2
                                    continue
                                if escaped == "n":
                                    out.append("\\n")
                                    i += 2
                                    continue
                                if escaped == "t":
                                    out.append("\\t")
                                    i += 2
                                    continue
                            out.append(value[i])
                            i += 1
                        return "".join(out)

                    if sys.argv[1] == "note":
                        if os.environ.get("TG_NOTE_VERIFIED") != "1":
                            print("inline note refused", file=sys.stderr)
                            raise SystemExit(2)
                        store.write_text(decode(sys.argv[3]), encoding="utf-8")
                        raise SystemExit(0)
                    if sys.argv[1] == "sql":
                        if not store.exists():
                            print("content\\n-------\\n\\n(0 rows)")
                            raise SystemExit(0)
                        content = store.read_text(encoding="utf-8")
                        sys.stdout.write("content\\n-------\\n")
                        sys.stdout.write(content)
                        sys.stdout.write("\\n\\n(1 row)\\n")
                        raise SystemExit(0)
                    raise SystemExit(2)
                    """
                ),
                encoding="utf-8",
            )
            fake_tg.chmod(0o755)
            env = {
                "PATH": f"{fake_bin}:{os.environ['PATH']}",
                "FAKE_TG_STORE": str(store),
            }
            stdout = io.StringIO()
            with mock.patch.dict(os.environ, env), redirect_stdout(stdout):
                rc = publication_exempt.main(
                    [
                        "drain-test",
                        "--repo",
                        "rrnewton/hermit",
                        "--pr",
                        "2738",
                        "--reason-file",
                        str(reason_file),
                    ],
                    actor_resolver=lambda _claimed: "hermit-test",
                )

            expected = publication_exempt.publication_record.render_exemption(
                "rrnewton/hermit", 2738, "hermit-test", reason
            ).encode()
            self.assertEqual(rc, 0)
            self.assertEqual(store.read_bytes(), expected)
            self.assertFalse(sentinel.exists(), "shell metacharacters were executed")
            self.assertIn("STORED AND VERIFIED byte-for-byte", stdout.getvalue())


if __name__ == "__main__":
    unittest.main()
