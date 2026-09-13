#!/usr/bin/env python3
"""Exercise the export parser and the collector's actual argument consumer."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

import guest_args


HERE = Path(__file__).resolve().parent


def record(test_id="fixture/guest", backend="kvm", args=None, mode="verify"):
    return {"test_id": test_id, "mode": mode, "backend": backend, "args": [] if args is None else args}


def json_lines(*records):
    return "".join(json.dumps(value, ensure_ascii=True) + "\n" for value in records)


class ExportParser(unittest.TestCase):
    def test_json_preserves_every_argument_byte_and_explicit_empty_vector(self):
        args = ["", "tab\tinside", "line\ninside\n", "carriage\rreturn", "é日本", "'\\$()"]
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            count = guest_args.prepare_arguments(json_lines(
                record(backend="ptrace", args=args), record(),
                record("fixture/native", "native", ["naked"], "naked"),
            ), directory)
            expected = b"".join(arg.encode() + b"\0" for arg in args)
            self.assertEqual(count, 2)
            self.assertEqual((directory / "fixture_guest.ptrace").read_bytes(), expected)
            self.assertEqual((directory / "fixture_guest.__reference").read_bytes(), expected)
            self.assertEqual((directory / "fixture_guest.kvm").read_bytes(), b"")
            self.assertEqual(len(list(directory.iterdir())), 3)

    def test_legacy_tsv_preserves_empty_arguments_and_carriage_return(self):
        text = "fixture/guest\tverify\tptrace\tfirst\t\tlast\r\t\n"
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            self.assertEqual(guest_args.prepare_arguments(text, directory), 1)
            self.assertEqual((directory / "fixture_guest.ptrace").read_bytes(), b"first\0\0last\r\0\0")

    def test_empty_export_has_no_declarations(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            self.assertEqual(guest_args.prepare_arguments("", directory), 0)
            self.assertEqual(list(directory.iterdir()), [])

    def test_bad_record_after_valid_one_writes_no_arguments(self):
        good = json_lines(record(args=["valid"]))
        bad_records = [
            {**record(), "extra": False},
            {key: value for key, value in record().items() if key != "args"},
            {**record(), "args": "not an array"},
            record(args=[2]), record(args=[None]), record(args=["bad\0arg"]),
            record(args=["\ud800"]), record(backend="ptrcae"),
            record(backend="native"), record(backend="kvm", mode="naked"),
            record(test_id="../escape"), record(test_id="fixture/../escape"),
            record(test_id="/absolute/guest"), record(test_id="fixture/line\nname"),
            {**record(), "mode": None}, {**record(), "test_id": []},
            {**record(), "backend": []}, [], None,
        ]
        bad_lines = [json.dumps(value) for value in bad_records]
        bad_lines += [
            '{"test_id":"fixture/other","mode":"verify","backend":"kvm","args":[],"args":["duplicate"]}',
            '{"broken":', "", "fixture/other\tverify\tkvm\tlegacy",
            json.dumps(record(args=["duplicate record"])),
        ]
        for bad in bad_lines:
            with self.subTest(bad=bad), tempfile.TemporaryDirectory() as tmp:
                directory = Path(tmp)
                with self.assertRaises(ValueError):
                    guest_args.prepare_arguments(good + bad + "\n", directory)
                self.assertEqual(list(directory.iterdir()), [])

    def test_legacy_malformed_and_mixed_formats_refuse(self):
        for text in [
            "fixture/guest\tverify\tkvm\n", "\n", "just text\n",
            "fixture/guest\tverify\tkvm\targ\n" + json_lines(record("fixture/other")),
        ]:
            with self.subTest(text=text), self.assertRaises(ValueError):
                guest_args.parse_export(text)

    def test_distinct_ids_cannot_overwrite_the_same_argument_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            with self.assertRaisesRegex(ValueError, "collide"):
                guest_args.prepare_arguments(json_lines(
                    record("fixture/a_b", args=["one"]),
                    record("fixture_a/b", args=["two"]),
                ), directory)
            self.assertEqual(list(directory.iterdir()), [])


class CollectorConsumer(unittest.TestCase):
    def invoke(self, exported: str, ids: list[str], *, export_status=0, collector=None):
        """Run the shipped import block and sweep function with measurement stubbed.

        The actual exporter boundary and xargs/mapfile/argv construction run.
        No Hermit process or corpus measurement is started.
        """
        source = collector if collector is not None else (HERE / "collect-fullcorpus.sh").read_text()
        start = source.index('\nGARGS_DIR="$BUILD/guest-args"')
        end = source.index("\nexport GARGS_DIR\n", start) + len("\nexport GARGS_DIR\n")
        sweep_start = source.index("\nsweep_backend() {")
        sweep_end = source.index("\nexport ROWS\n", sweep_start)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            build = root / "build"
            hroot = root / "hermit"
            (hroot / "scripts").mkdir(parents=True)
            exporter = hroot / "scripts/manifest-to-commands.rs"
            exporter.write_text('#!/usr/bin/env bash\ncat "$EXPORT_FIXTURE"\nexit "$EXPORT_STATUS"\n')
            exporter.chmod(0o755)
            export_file = root / "export"
            export_file.write_bytes(exported.encode())
            for test_id in ids:
                cell = build / test_id.replace("/", "_")
                cell.mkdir(parents=True)
                (cell / "guest").write_text("unused measurement stub\n")
                (cell / "guest").chmod(0o755)
            corpus = root / "corpus"
            corpus.write_text("".join(f"{test_id}|unused|||portable|green\n" for test_id in ids))
            nonc = root / "nonc"
            nonc.write_text("")
            measured = root / "measured"
            env = os.environ.copy()
            env.update(here=str(HERE), HROOT=str(hroot), BUILD=str(build),
                       EXPORT_FIXTURE=str(export_file), EXPORT_STATUS=str(export_status),
                       CORPUS_C=str(corpus), CORPUS_NONC=str(nonc),
                       ROWS=str(root / "rows"), MEASURED=str(measured))
            script = 'set -uo pipefail\n' + source[start:end] + "\n" + source[sweep_start:sweep_end]
            script += '\nmeasure() { printf "%s\\0" "$@" >> "$MEASURED"; }\nexport -f measure\nsweep_backend kvm 1 0\n'
            result = subprocess.run(["bash", "-c", script], env=env, capture_output=True)
            measured_bytes = measured.read_bytes() if measured.exists() else b""
            files = {p.name: p.read_bytes() for p in (build / "guest-args").iterdir()}
            return result, measured_bytes, files

    def test_actual_json_consumer_preserves_vector_and_reference_rules(self):
        special = ["", "tab\there", "line\nhere\n", "日本", "$(unexecuted)`text`\\"]
        result, measured, files = self.invoke(json_lines(
            record("fixture/empty", "ptrace", ["reference"]), record("fixture/empty"),
            record("fixture/fallback", "ptrace", ["reference"]),
            record("fixture/special", args=special),
        ), ["fixture/empty", "fixture/fallback", "fixture/special"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(b"4 declared (verify mode)", result.stdout)
        args = measured.split(b"\0")[:-1]
        self.assertEqual(args[0], b"kvm")
        self.assertEqual(args[3], b"fixture/empty")
        self.assertEqual(args[5], b"kvm", "empty declaration must not inherit ptrace arguments")
        self.assertEqual(args[8], b"fixture/fallback")
        self.assertEqual(args[10], b"reference")
        self.assertEqual(args[11], b"kvm")
        self.assertEqual(args[14], b"fixture/special")
        self.assertEqual(args[16:], [arg.encode() for arg in special])
        self.assertEqual(files["fixture_empty.kvm"], b"")

    def test_actual_legacy_consumer_preserves_scenario(self):
        result, measured, _ = self.invoke(
            "fixture/guest\tverify\tkvm\tvalue with spaces\tsecond\n", ["fixture/guest"]
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(b"1 declared (verify mode)", result.stdout)
        self.assertEqual(measured.split(b"\0")[5:-1], [b"value with spaces", b"second"])

    def test_actual_malformed_export_refuses_before_measurement(self):
        result, measured, files = self.invoke(
            json_lines(record(args=["good"])) + '{"invalid":\n', ["fixture/guest"]
        )
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(b"REFUSED", result.stderr)
        self.assertEqual(measured, b"")
        self.assertEqual(set(files), {"declared.txt", "err"})

    def test_unavailable_old_exporter_remains_visibly_unavailable(self):
        result, measured, _ = self.invoke("discarded partial output", ["fixture/guest"], export_status=1)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(b"UNAVAILABLE", result.stderr)
        self.assertNotIn(b"declared (verify mode)", result.stdout)
        self.assertEqual(len(measured.split(b"\0")[:-1]), 5)


if __name__ == "__main__":
    unittest.main()
