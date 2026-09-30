"""The scorecard history's part layout, with real temporary Git repositories.

scorecard/cells.json passed GitHub's 100 MiB file refusal on 2026-09-30, so
the ledger stores it in parts (scorecard_cells). These tests pin the layout's
exact shape, every refusal a reader makes, and the render copy's replacement
view, including that the view never leaves the repository that holds it.
"""
from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "scorecard_cells_tested", Path(__file__).resolve().parents[1] / "scorecard_cells.py")
assert SPEC is not None and SPEC.loader is not None
cells = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(cells)

# No user or system Git configuration: the fleet's lives there.
ENVIRONMENT = {**{key: value for key, value in os.environ.items() if not key.startswith("GIT_")},
               "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1"}


def git(repo: Path, *args: str, environment: dict | None = None) -> str:
    return subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, text=True,
                          env=environment or ENVIRONMENT).stdout.strip()


def document(size: int) -> bytes:
    """SIZE deterministic bytes in which no two parts are equal."""
    return hashlib.shake_256(str(size).encode()).digest(size)


class SplitTests(unittest.TestCase):
    def test_parts_are_full_then_one_final_part_and_reassemble_exactly(self):
        for size, parts in ((1, 1), (15, 1), (16, 1), (17, 2), (48, 3), (49, 4)):
            with self.subTest(size=size):
                data = document(size)
                files = cells.split(data, part_max_bytes=16)
                self.assertEqual([cells.part_path(index) for index in range(parts)] + [cells.MANIFEST_PATH],
                                 list(files))
                self.assertEqual(data, b"".join(files[cells.part_path(index)] for index in range(parts)))
                self.assertTrue(all(len(files[cells.part_path(index)]) == 16 for index in range(parts - 1)))
                manifest = json.loads(files[cells.MANIFEST_PATH])
                self.assertEqual({"schema": cells.MANIFEST_SCHEMA, "path": cells.LOGICAL_PATH,
                                  "bytes": size, "sha256": hashlib.sha256(data).hexdigest(),
                                  "part_max_bytes": 16,
                                  "parts": [{"path": cells.part_path(index),
                                             "bytes": len(files[cells.part_path(index)]),
                                             "sha256": hashlib.sha256(files[cells.part_path(index)]).hexdigest()}
                                            for index in range(parts)]}, manifest)

    def test_the_default_part_size_is_the_ceiling_and_every_file_fits_under_it(self):
        # The refused commit 317771bb carried 107,322,430 bytes; 108,090,921 is
        # the size the ledger held when this layout was introduced.
        data = document(108_090_921)
        files = cells.split(data)
        self.assertEqual(3, len(files) - 1)
        self.assertEqual(48 * 1024 * 1024, cells.FILE_CEILING_BYTES)
        self.assertLess(2 * cells.FILE_CEILING_BYTES, 100 * 1024 * 1024)
        self.assertTrue(all(len(encoded) <= cells.FILE_CEILING_BYTES for encoded in files.values()))
        self.assertEqual(data, cells._assemble(set(files), files.__getitem__))

    def test_split_refuses_an_empty_document_and_an_out_of_range_part_size(self):
        with self.assertRaisesRegex(cells.CellsRefused, "empty scorecard history"):
            cells.split(b"")
        for size in (0, -1, cells.FILE_CEILING_BYTES + 1, True, 16.0, "16"):
            with self.subTest(size=size), self.assertRaisesRegex(cells.CellsRefused, "part size"):
                cells.split(b"data", part_max_bytes=size)
        with self.assertRaisesRegex(cells.CellsRefused, "at most 10000 are allowed"):
            cells.split(b"x" * (cells.MAX_PARTS + 1), part_max_bytes=1)


class ManifestTests(unittest.TestCase):
    def setUp(self):
        self.data = document(40)
        self.files = cells.split(self.data, part_max_bytes=16)
        self.manifest = json.loads(self.files[cells.MANIFEST_PATH])

    def assemble(self, manifest=None, **replaced):
        files = {**self.files, **replaced}
        if manifest is not None:
            files[cells.MANIFEST_PATH] = json.dumps(manifest).encode()
        return cells._assemble(set(files), files.__getitem__)

    def test_the_exact_manifest_reassembles(self):
        self.assertEqual(self.data, self.assemble())

    def test_every_malformed_manifest_is_refused(self):
        part = self.manifest["parts"][0]
        cases = {
            "extra field": {**self.manifest, "note": "x"},
            "missing field": {key: value for key, value in self.manifest.items() if key != "sha256"},
            "other schema": {**self.manifest, "schema": "hermit-test-ledger-scorecard-cells/v2"},
            "other path": {**self.manifest, "path": "scorecard/other.json"},
            "boolean size": {**self.manifest, "bytes": True},
            "zero size": {**self.manifest, "bytes": 0},
            "part size above the ceiling": {**self.manifest, "part_max_bytes": cells.FILE_CEILING_BYTES + 1},
            "uppercase digest": {**self.manifest, "sha256": self.manifest["sha256"].upper()},
            "a part too few": {**self.manifest, "parts": self.manifest["parts"][:-1]},
            "parts out of order": {**self.manifest, "parts": [self.manifest["parts"][1],
                                                              self.manifest["parts"][0],
                                                              self.manifest["parts"][2]]},
            "short middle part": {**self.manifest, "bytes": 40, "part_max_bytes": 16,
                                  "parts": [part, {**self.manifest["parts"][1], "bytes": 15},
                                            self.manifest["parts"][2]]},
            "extra part field": {**self.manifest, "parts": [{**part, "mode": 420},
                                                            *self.manifest["parts"][1:]]},
        }
        for name, manifest in cases.items():
            with self.subTest(name), self.assertRaises(cells.CellsRefused):
                self.assemble(manifest)
        with self.assertRaisesRegex(cells.CellsRefused, "not JSON"):
            self.assemble(**{cells.MANIFEST_PATH: b"{"})
        with self.assertRaisesRegex(cells.CellsRefused, "larger than"):
            self.assemble(**{cells.MANIFEST_PATH: b" " * (cells.MANIFEST_MAX_BYTES + 1)})

    def test_a_part_or_the_whole_that_differs_from_the_manifest_is_refused(self):
        with self.assertRaisesRegex(cells.CellsRefused, "part-0001 differs from its manifest entry"):
            self.assemble(**{cells.part_path(1): b"y" * 16})
        with self.assertRaisesRegex(cells.CellsRefused, "part-0002 differs from its manifest entry"):
            self.assemble(**{cells.part_path(2): self.files[cells.part_path(2)] + b"z"})
        forged = {**self.manifest, "sha256": hashlib.sha256(b"other").hexdigest()}
        with self.assertRaisesRegex(cells.CellsRefused, "reassembled scorecard history differs"):
            self.assemble(forged)

    def test_both_layouts_an_unlisted_part_or_a_missing_part_or_manifest_are_refused(self):
        listed = set(self.files)
        read = {**self.files, cells.LOGICAL_PATH: self.data}.__getitem__
        with self.assertRaisesRegex(cells.CellsRefused, "both scorecard/cells.json and scorecard/cells/"):
            cells._assemble(listed | {cells.LOGICAL_PATH}, read)
        with self.assertRaisesRegex(cells.CellsRefused, "unexpected scorecard/cells/part-0003"):
            cells._assemble(listed | {cells.part_path(3)}, read)
        with self.assertRaisesRegex(cells.CellsRefused, "missing scorecard/cells/part-0001"):
            cells._assemble(listed - {cells.part_path(1)}, read)
        with self.assertRaisesRegex(cells.CellsRefused, "has no manifest.json"):
            cells._assemble(listed - {cells.MANIFEST_PATH}, read)
        self.assertIsNone(cells._assemble(set(), read))
        self.assertEqual(self.data, cells._assemble({cells.LOGICAL_PATH}, read))


class TreeTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="scorecard-cells-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def listing(self):
        return sorted(str(path.relative_to(self.root)) for path in self.root.rglob("*") if path.is_file())

    def test_replace_tree_moves_the_single_file_to_parts_then_drops_stale_parts(self):
        (self.root / "scorecard").mkdir()
        (self.root / cells.LOGICAL_PATH).write_bytes(document(10))
        (self.root / "scorecard/SCORECARD.md").write_text("kept\n")
        self.assertEqual(document(10), cells.read_tree(self.root))
        large = document(40)
        cells.replace_tree(self.root, cells.split(large, part_max_bytes=16))
        self.assertEqual(["scorecard/SCORECARD.md", cells.MANIFEST_PATH, cells.part_path(0),
                          cells.part_path(1), cells.part_path(2)], self.listing())
        self.assertEqual(large, cells.read_tree(self.root))
        small = document(5)
        cells.replace_tree(self.root, cells.split(small, part_max_bytes=16))
        self.assertEqual(["scorecard/SCORECARD.md", cells.MANIFEST_PATH, cells.part_path(0)], self.listing())
        self.assertEqual(small, cells.read_tree(self.root))

    def test_replace_tree_refuses_before_writing_anything(self):
        (self.root / "scorecard").mkdir()
        (self.root / cells.LOGICAL_PATH).write_bytes(b"original")
        files = cells.split(document(40), part_max_bytes=16)
        refused = {
            "a part set missing a part": {key: value for key, value in files.items()
                                         if key != cells.part_path(1)},
            "a part that differs from the manifest": {**files, cells.part_path(0): b"x" * 16},
            "the single file itself": {cells.LOGICAL_PATH: b"x"},
            "a manifest alone": {cells.MANIFEST_PATH: files[cells.MANIFEST_PATH]},
            "nothing": {},
        }
        for name, candidate in refused.items():
            with self.subTest(name), self.assertRaises(cells.CellsRefused):
                cells.replace_tree(self.root, candidate)
            self.assertEqual([cells.LOGICAL_PATH], self.listing())
            self.assertEqual(b"original", (self.root / cells.LOGICAL_PATH).read_bytes())

    def test_symbolic_links_and_non_files_are_refused(self):
        outside = self.root / "outside"
        outside.mkdir()
        cells.replace_tree(outside, cells.split(document(20), part_max_bytes=16))
        tree = self.root / "tree"
        tree.mkdir()
        (tree / "scorecard").symlink_to(outside / "scorecard")
        for action in (lambda: cells.read_tree(tree),
                       lambda: cells.replace_tree(tree, cells.split(b"new", part_max_bytes=16))):
            with self.assertRaisesRegex(cells.CellsRefused, "scorecard is a symbolic link"):
                action()
        (tree / "scorecard").unlink()
        (tree / "scorecard").mkdir()
        (tree / "scorecard/cells").symlink_to(outside / "scorecard/cells")
        with self.assertRaisesRegex(cells.CellsRefused, "scorecard/cells is a symbolic link"):
            cells.read_tree(tree)
        (tree / "scorecard/cells").unlink()
        (tree / "scorecard/cells").mkdir()
        for name in ("manifest.json", "part-0000", "part-0001"):
            (tree / "scorecard/cells" / name).symlink_to(outside / "scorecard/cells" / name)
        with self.assertRaisesRegex(cells.CellsRefused, "cannot open"):
            cells.read_tree(tree)
        with self.assertRaisesRegex(cells.CellsRefused, "is not a regular file"):
            cells.replace_tree(tree, cells.split(b"new", part_max_bytes=16))
        self.assertTrue((outside / cells.part_path(1)).is_file())
        (tree / "scorecard/cells").rename(tree / "moved")
        (tree / "scorecard/cells").write_text("a file")
        with self.assertRaisesRegex(cells.CellsRefused, "scorecard/cells is not a directory"):
            cells.read_tree(tree)


class CommitTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="scorecard-cells-git-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.repo = self.root / "ledger"
        self.repo.mkdir()
        git(self.repo, "init", "-q", "-b", "main")
        git(self.repo, "config", "user.name", "Fixture")
        git(self.repo, "config", "user.email", "fixture@example.invalid")
        (self.repo / "series").mkdir()
        (self.repo / "series/rows.jsonl").write_text('{"row":1}\n')
        (self.repo / "scorecard").mkdir()
        (self.repo / "scorecard/SCORECARD.md").write_text("published table\n")
        self.data = document(40)

    def commit(self, message: str = "fixture") -> str:
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "--allow-empty", "-m", message)
        return git(self.repo, "rev-parse", "HEAD")

    def split_commit(self) -> str:
        cells.replace_tree(self.repo, cells.split(self.data, part_max_bytes=16))
        return self.commit("parts")

    def test_read_commit_reads_either_layout_and_none_when_absent(self):
        empty = self.commit("no history")
        self.assertIsNone(cells.read_commit(self.repo, empty, environment=ENVIRONMENT))
        (self.repo / cells.LOGICAL_PATH).write_bytes(self.data)
        single = self.commit("single file")
        stored = self.split_commit()
        self.assertEqual(self.data, cells.read_commit(self.repo, single, environment=ENVIRONMENT))
        self.assertEqual(self.data, cells.read_commit(self.repo, stored, environment=ENVIRONMENT))
        self.assertEqual("", git(self.repo, "ls-tree", stored, "--", cells.LOGICAL_PATH))

    def test_read_commit_refuses_a_link_or_a_submodule_where_a_part_belongs(self):
        stored = self.split_commit()
        blob = git(self.repo, "rev-parse", f"{stored}:{cells.part_path(1)}")
        for mode, kind, name in (("120000", "blob", blob), ("160000", "commit", stored)):
            with self.subTest(mode=mode):
                git(self.repo, "update-index", "--cacheinfo", f"{mode},{name},{cells.part_path(1)}")
                tree = git(self.repo, "write-tree")
                forged = git(self.repo, "commit-tree", tree, "-p", stored, "-m", mode)
                with self.assertRaisesRegex(cells.CellsRefused, f"not a regular file \\(mode {mode}, {kind}\\)"):
                    cells.read_commit(self.repo, forged, environment=ENVIRONMENT)
                git(self.repo, "reset", "-q", stored)

    def test_render_view_shows_the_single_file_under_the_same_commit_name(self):
        stored = self.split_commit()
        copy = self.root / "copy"
        git(self.root, "clone", "-q", "--no-checkout", str(self.repo), str(copy))
        view = cells.install_render_view(copy, stored, ENVIRONMENT)
        self.assertRegex(view, "^[0-9a-f]{40}$")
        self.assertEqual({stored: view}, cells.replacements(copy, ENVIRONMENT))
        git(copy, "checkout", "-q", "--detach", stored)
        self.assertEqual(stored, git(copy, "rev-parse", "HEAD^{commit}"))
        self.assertEqual("", git(copy, "status", "--porcelain", "--untracked-files=all"))
        self.assertEqual(self.data, (copy / cells.LOGICAL_PATH).read_bytes())
        self.assertFalse((copy / cells.PARTS_DIRECTORY).exists())
        shown = subprocess.run(["git", "-C", str(copy), "show", f"HEAD:{cells.LOGICAL_PATH}"],
                               check=True, capture_output=True, env=ENVIRONMENT).stdout
        self.assertEqual(self.data, shown)
        # The view differs from the stored commit only in that one entry.
        stored_tree = git(copy, "--no-replace-objects", "ls-tree", "-r", stored).splitlines()
        viewed_tree = git(copy, "ls-tree", "-r", stored).splitlines()
        self.assertEqual([line for line in stored_tree if "\tscorecard/cells/" not in line],
                         [line for line in viewed_tree if not line.endswith(f"\t{cells.LOGICAL_PATH}")])
        self.assertEqual(
            git(copy, "--no-replace-objects", "cat-file", "commit", stored).split("\n", 1)[1],
            git(copy, "cat-file", "commit", view).split("\n", 1)[1])
        # Readers that ignore replacements still see, and read_commit still reads, the parts.
        self.assertEqual(self.data, cells.read_commit(copy, stored, environment=ENVIRONMENT))
        hidden = {**ENVIRONMENT, "GIT_NO_REPLACE_OBJECTS": "1"}
        self.assertIn(cells.MANIFEST_PATH, git(copy, "ls-tree", "-r", "--name-only", stored,
                                               environment=hidden).splitlines())

    def test_render_view_is_none_for_the_single_file_or_no_history(self):
        empty = self.commit("no history")
        (self.repo / cells.LOGICAL_PATH).write_bytes(self.data)
        single = self.commit("single file")
        for commit in (empty, single):
            with self.subTest(commit=commit):
                self.assertIsNone(cells.install_render_view(self.repo, commit, ENVIRONMENT))
        self.assertEqual({}, cells.replacements(self.repo, ENVIRONMENT))

    def test_render_view_refuses_a_malformed_part_set_and_installs_nothing(self):
        cells.replace_tree(self.repo, cells.split(self.data, part_max_bytes=16))
        (self.repo / cells.part_path(2)).write_bytes(b"forged")
        forged = self.commit("forged part")
        with self.assertRaisesRegex(cells.CellsRefused, "part-0002 differs from its manifest entry"):
            cells.install_render_view(self.repo, forged, ENVIRONMENT)
        self.assertEqual({}, cells.replacements(self.repo, ENVIRONMENT))

    def test_pushing_from_a_repository_holding_the_view_sends_only_the_stored_objects(self):
        stored = self.split_commit()
        remote = self.root / "remote.git"
        subprocess.run(["git", "init", "-q", "--bare", str(remote)], check=True, env=ENVIRONMENT)
        view = cells.install_render_view(self.repo, stored, ENVIRONMENT)
        self.assertIsNotNone(view)
        # The publisher's own refspec form: one explicit commit to main.
        git(self.repo, "push", "-q", str(remote), f"{stored}:refs/heads/main")
        self.assertEqual(f"{stored} refs/heads/main",
                         git(remote, "for-each-ref", "--format=%(objectname) %(refname)"))
        self.assertEqual("", git(remote, "for-each-ref", "refs/replace/"))
        missing = subprocess.run(["git", "-C", str(remote), "cat-file", "-e", view],
                                 capture_output=True, env=ENVIRONMENT)
        self.assertNotEqual(0, missing.returncode)
        self.assertEqual(self.data, cells.read_commit(remote, "main", environment=ENVIRONMENT))
        self.assertEqual("", git(remote, "ls-tree", "main", "--", cells.LOGICAL_PATH))


if __name__ == "__main__":
    unittest.main()
