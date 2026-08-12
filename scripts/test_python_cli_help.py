#!/usr/bin/env python3
"""Keep parent and subcommand help separate for every argparse command."""

from __future__ import annotations

import ast
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SEARCH_ROOTS = (ROOT / "scripts", ROOT / "ci-hub", ROOT / "agent-utils")


def _call_name(call: ast.Call) -> str:
    if isinstance(call.func, ast.Attribute):
        return call.func.attr
    if isinstance(call.func, ast.Name):
        return call.func.id
    return ""


def _keyword(call: ast.Call, name: str) -> ast.keyword | None:
    return next((keyword for keyword in call.keywords if keyword.arg == name), None)


def _has_visible_keyword(call: ast.Call, name: str) -> bool:
    keyword = _keyword(call, name)
    if keyword is None:
        return False
    value = keyword.value
    if isinstance(value, ast.Constant) and isinstance(value.value, str):
        return bool(value.value.strip())
    return ast.unparse(value) != "argparse.SUPPRESS"


class PythonCliHelpTest(unittest.TestCase):
    def test_every_subcommand_has_parent_and_own_help(self) -> None:
        parser_files = 0
        subcommand_definitions = 0
        missing: list[str] = []

        for search_root in SEARCH_ROOTS:
            for path in search_root.rglob("*.py"):
                if ".git" in path.parts:
                    continue
                tree = ast.parse(path.read_text(), filename=str(path))
                calls = [
                    node for node in ast.walk(tree) if isinstance(node, ast.Call)
                ]
                if not any(_call_name(call) == "add_subparsers" for call in calls):
                    continue
                parser_files += 1
                for call in calls:
                    if _call_name(call) != "add_parser":
                        continue
                    subcommand_definitions += 1
                    absent = [
                        name
                        for name in ("help", "description")
                        if not _has_visible_keyword(call, name)
                    ]
                    if absent:
                        missing.append(
                            f"{path.relative_to(ROOT)}:{call.lineno}: "
                            f"missing {', '.join(absent)}"
                        )

        self.assertGreaterEqual(parser_files, 27)
        self.assertGreaterEqual(subcommand_definitions, 125)
        self.assertEqual(missing, [])


if __name__ == "__main__":
    unittest.main()
