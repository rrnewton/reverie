#!/usr/bin/env python3
"""Read Hermit's guest-argument export and prepare NUL-delimited argv files.

Accept the original TSV and the JSON Lines export. Parse and validate the whole
input before writing any argument file. The collector only measures verify;
an explicit empty vector still gets a file, preventing reference fallback.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys


BACKENDS = {"ptrace", "dbt", "kvm", "sabre", "liteinst"}
FIELDS = {"test_id", "mode", "backend", "args"}


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON field: {key}")
        result[key] = value
    return result


def parse_export(text: str) -> list[dict]:
    if not text:
        return []
    lines = text.split("\n")
    if lines[-1] == "":
        lines.pop()
    json_lines = lines[0].lstrip().startswith("{")
    records = []
    seen = set()
    for number, line in enumerate(lines, 1):
        try:
            if json_lines:
                record = json.loads(line, object_pairs_hook=unique_object)
                if not isinstance(record, dict) or set(record) != FIELDS:
                    raise ValueError("expected exactly test_id, mode, backend and args")
            else:
                fields = line.split("\t")
                if len(fields) < 4:
                    raise ValueError("expected TSV test_id, mode, backend and arguments")
                record = dict(zip(("test_id", "mode", "backend"), fields[:3]))
                record["args"] = fields[3:]

            test_id, mode, backend = (record[name] for name in ("test_id", "mode", "backend"))
            if not isinstance(test_id, str) or not re.fullmatch(
                r"[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)+", test_id
            ) or any(part in {".", ".."} for part in test_id.split("/")):
                raise ValueError("invalid test_id")
            if not isinstance(mode, str) or not re.fullmatch(r"[a-z][a-z0-9_-]*", mode):
                raise ValueError("invalid mode")
            allowed = {"native"} if mode == "naked" else BACKENDS
            if not isinstance(backend, str) or backend not in allowed:
                raise ValueError("invalid backend for mode")
            args = record["args"]
            if not isinstance(args, list) or any(not isinstance(arg, str) for arg in args):
                raise ValueError("args must be an array of strings")
            for arg in args:
                if "\0" in arg:
                    raise ValueError("Linux arguments cannot contain NUL")
                arg.encode("utf-8", errors="strict")
            identity = (test_id, mode, backend)
            if identity in seen:
                raise ValueError("duplicate test/mode/backend record")
            seen.add(identity)
            records.append(record)
        except (ValueError, UnicodeError) as error:
            raise ValueError(f"guest-argument export line {number}: {error}") from error
    return records


def prepare_arguments(text: str, directory: Path) -> int:
    files = {}
    count = 0
    for record in parse_export(text):
        if record["mode"] != "verify":
            continue
        key = record["test_id"].replace("/", "_")
        names = [f"{key}.{record['backend']}"]
        if record["backend"] == "ptrace":
            names.append(f"{key}.__reference")
        data = b"".join(arg.encode("utf-8") + b"\0" for arg in record["args"])
        for name in names:
            if name in files:
                raise ValueError(f"test IDs collide in argument filename: {name}")
            files[name] = data
        count += 1
    for name, data in files.items():
        with (directory / name).open("xb") as output:
            output.write(data)
    return count


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("export", type=Path)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    try:
        # read_bytes avoids universal-newline conversion inside legacy argv.
        text = args.export.read_bytes().decode("utf-8", errors="strict")
        count = prepare_arguments(text, args.directory)
    except (OSError, ValueError, UnicodeError) as error:
        print(f"guest_args: {error}", file=sys.stderr)
        return 2
    print(count)
    return 0


if __name__ == "__main__":
    sys.exit(main())
