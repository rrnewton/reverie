from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("check-codex-setup.py")
README_TEXT = """# Codex skill entrypoints

Stock Codex discovers repository skills here. Each tracked entry is a
whole-package symlink to the canonical package in `.claude/skills/<name>/`, so
Claude, Codex, and `.llms` consumers read the same `SKILL.md` and bundled
resources. Do not replace package links with generated pointer files or with a
link to `SKILL.md` alone.

`pr-landing-planner` is the deliberate external-package exception. The checker
accepts only the fixed `.claude/skills/pr-landing-planner` link to
`agent-utils/skills/pr-landing-planner` and rejects a duplicate `.agents`
entry. Codex uses the registered agent-utils package named by `AGENTS.md`;
absence here is intentional, not an instruction to skip the mandatory planner.

Run `scripts/check-codex-setup.py` after an intentional skill change. The
checker is read-only and rejects wrong, dangling, escaping, root-level, and
file-only links.
"""


class CheckCodexSetupTest(unittest.TestCase):
    def fixture(self) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        package = root / ".claude/skills/example"
        (package / "references").mkdir(parents=True)
        (root / ".agents/skills").mkdir(parents=True)
        (root / ".llms").mkdir()
        (root / ".codex").mkdir()
        (root / "AGENTS.md").write_text("root policy\n")
        (root / "CLAUDE.md").symlink_to("AGENTS.md")
        (root / ".codex/config.toml").write_text(
            "project_doc_max_bytes = 98304\n"
        )
        (package / "SKILL.md").write_text(
            "---\n"
            "name: example\n"
            'description: "Use for an example task."\n'
            "---\n\n"
            "Do the example task. Read [details](references/details.md).\n"
        )
        (package / "references/details.md").write_text("Package resource.\n")
        (root / ".agents/skills/example").symlink_to(
            "../../.claude/skills/example", target_is_directory=True
        )
        (root / ".agents/skills/README.md").write_text(README_TEXT)
        (root / ".llms/skills").symlink_to(
            "../.claude/skills", target_is_directory=True
        )
        self.git(root, "init", "--quiet")
        self.git(root, "add", "AGENTS.md")
        return root

    def run_check(self, root: Path, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["python3", str(SCRIPT), "--root", str(root), *args],
            text=True,
            capture_output=True,
            check=False,
        )

    def git(self, root: Path, *args: str) -> str:
        result = subprocess.run(
            ["git", "-C", str(root), *args],
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.strip()

    def test_one_package_and_resources_resolve_through_all_clients(self) -> None:
        root = self.fixture()
        result = self.run_check(root)
        self.assertEqual(result.returncode, 0, result.stderr)
        relative = Path("example/references/details.md")
        contents = {
            (root / ".claude/skills" / relative).read_text(),
            (root / ".agents/skills" / relative).read_text(),
            (root / ".llms/skills" / relative).read_text(),
        }
        self.assertEqual(contents, {"Package resource.\n"})

    def test_write_mode_was_removed(self) -> None:
        result = self.run_check(self.fixture(), "--write")
        self.assertEqual(result.returncode, 2)
        self.assertIn("unrecognized arguments: --write", result.stderr)

    def test_rejects_default_sized_instruction_budget(self) -> None:
        root = self.fixture()
        (root / ".codex/config.toml").write_text(
            "project_doc_max_bytes = 32768\n"
        )
        result = self.run_check(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("below the 98304-byte coordinator minimum", result.stderr)

    def test_rejects_broken_canonical_skill_link(self) -> None:
        root = self.fixture()
        source = root / ".claude/skills/example/SKILL.md"
        source.write_text(source.read_text() + "\n[missing](references/missing.md)\n")
        result = self.run_check(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("broken local link 'references/missing.md'", result.stderr)

    def test_cross_submodule_link_uses_indexed_gitlink_not_live_head(self) -> None:
        root = self.fixture()
        child = root / "child"
        child.mkdir()
        self.git(child, "init", "--quiet")
        self.git(child, "config", "user.name", "Skill Test")
        self.git(child, "config", "user.email", "skill-test@example.invalid")
        (child / "README.md").write_text("old tree\n")
        self.git(child, "add", "README.md")
        self.git(child, "commit", "--quiet", "-m", "old tree")
        old_commit = self.git(child, "rev-parse", "HEAD")

        package = child / "only-new"
        package.mkdir()
        (package / "SKILL.md").write_text("new tree only\n")
        (child / "skill-alias").symlink_to("only-new", target_is_directory=True)
        self.git(child, "add", "only-new/SKILL.md", "skill-alias")
        self.git(child, "commit", "--quiet", "-m", "new tree")
        new_commit = self.git(child, "rev-parse", "HEAD")

        self.git(root, "init", "--quiet")
        self.git(
            root,
            "update-index",
            "--add",
            "--cacheinfo",
            f"160000,{old_commit},child",
        )
        source = root / ".claude/skills/example/SKILL.md"
        source.write_text(
            source.read_text()
            + "\n[Pinned child skill](../../../child/skill-alias/SKILL.md)\n"
        )

        stale_pin = self.run_check(root)
        self.assertEqual(stale_pin.returncode, 1)
        self.assertIn(
            f"indexed gitlink child@{old_commit} does not contain "
            "skill-alias/SKILL.md",
            stale_pin.stderr,
        )

        self.git(
            root,
            "update-index",
            "--add",
            "--cacheinfo",
            f"160000,{new_commit},child",
        )
        current_pin = self.run_check(root)
        self.assertEqual(current_pin.returncode, 0, current_pin.stderr)

        bridge = root / ".claude/skills/example/references/child-skill"
        bridge.symlink_to("../../../../child/skill-alias", target_is_directory=True)
        self.git(root, "add", ".claude/skills/example/references/child-skill")
        indexed_bridge = self.git(
            root,
            "ls-files",
            "--stage",
            "--",
            ".claude/skills/example/references/child-skill",
        )
        self.assertTrue(indexed_bridge.startswith("120000 "), indexed_bridge)
        source.write_text(
            source.read_text().replace(
                "../../../child/skill-alias/SKILL.md",
                "references/child-skill/SKILL.md",
            )
        )
        self.git(
            root,
            "update-index",
            "--add",
            "--cacheinfo",
            f"160000,{old_commit},child",
        )

        stale_pin_through_parent_symlink = self.run_check(root)
        self.assertEqual(stale_pin_through_parent_symlink.returncode, 1)
        self.assertIn(
            f"indexed gitlink child@{old_commit} does not contain "
            "skill-alias/SKILL.md",
            stale_pin_through_parent_symlink.stderr,
        )

        self.git(
            root,
            "update-index",
            "--add",
            "--cacheinfo",
            f"160000,{new_commit},child",
        )
        current_pin_through_parent_symlink = self.run_check(root)
        self.assertEqual(
            current_pin_through_parent_symlink.returncode,
            0,
            current_pin_through_parent_symlink.stderr,
        )

        bridge_object = indexed_bridge.split()[1]
        ordinary_blob = subprocess.run(
            ["git", "-C", str(root), "hash-object", "-w", "--stdin"],
            input="indexed ordinary file\n",
            text=True,
            capture_output=True,
            check=True,
        ).stdout.strip()
        self.git(
            root,
            "update-index",
            "--add",
            "--cacheinfo",
            f"100644,{ordinary_blob},.claude/skills/example/references/child-skill",
        )
        mismatched_mode = self.run_check(root)
        self.assertEqual(mismatched_mode.returncode, 1)
        self.assertIn(
            "symlink component .claude/skills/example/references/child-skill "
            "is not mode 120000 in the parent index",
            mismatched_mode.stderr,
        )

        self.git(
            root,
            "update-index",
            "--add",
            "--cacheinfo",
            f"120000,{bridge_object},.claude/skills/example/references/child-skill",
        )
        bridge.unlink()
        bridge.symlink_to("../../../../child/only-new", target_is_directory=True)
        mismatched_target = self.run_check(root)
        self.assertEqual(mismatched_target.returncode, 1)
        self.assertIn(
            "live symlink .claude/skills/example/references/child-skill "
            "differs from parent index",
            mismatched_target.stderr,
        )

        bridge.unlink()
        bridge.symlink_to("../../../../child/skill-alias", target_is_directory=True)
        actual_child = root / "actual-child"
        child.rename(actual_child)
        child.symlink_to("actual-child", target_is_directory=True)
        mismatched_gitlink_mode = self.run_check(root)
        self.assertEqual(mismatched_gitlink_mode.returncode, 1)
        self.assertIn(
            "symlink component child is not mode 120000 in the parent index",
            mismatched_gitlink_mode.stderr,
        )

    def test_rejects_duplicate_frontmatter_fields(self) -> None:
        root = self.fixture()
        source = root / ".claude/skills/example/SKILL.md"
        source.write_text(
            "---\n"
            "name: example\n"
            'description: "First."\n'
            'description: "Second."\n'
            "---\n\nBody.\n"
        )
        result = self.run_check(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("exactly name then description", result.stderr)

    def test_rejects_empty_instruction_body(self) -> None:
        root = self.fixture()
        source = root / ".claude/skills/example/SKILL.md"
        source.write_text(
            "---\nname: example\ndescription: \"Useful.\"\n---\n"
        )
        result = self.run_check(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("metadata has no skill instructions", result.stderr)

    def test_counts_every_policy_in_deepest_nested_chain(self) -> None:
        root = self.fixture()
        (root / "AGENTS.md").write_text("r" * 40_000)
        (root / "product/nested").mkdir(parents=True)
        (root / "product/AGENTS.md").write_text("p" * 40_000)
        (root / "product/nested/AGENTS.md").write_text("n" * 20_000)
        self.git(
            root,
            "add",
            "AGENTS.md",
            "product/AGENTS.md",
            "product/nested/AGENTS.md",
        )
        result = self.run_check(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "largest canonical instruction chain is 100004 bytes "
            "at scope parent/product/nested",
            result.stderr,
        )

    def test_excludes_untracked_runtime_runner_checkout(self) -> None:
        root = self.fixture()
        residue = root / "ci-runner/state-gate/_work/hermit/hermit/trusted"
        residue.mkdir(parents=True)
        (residue.parent / "AGENTS.md").write_text("x" * 70_000)
        (residue / "AGENTS.md").write_text("y" * 70_000)
        result = self.run_check(root)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("instruction_chain_bytes_canonical=12", result.stdout)
        self.assertIn("instruction_chain_max_scope=parent", result.stdout)
        self.assertNotIn("ci-runner", result.stdout)

    def test_reports_unmaterialized_submodule_as_partial_scope(self) -> None:
        root = self.fixture()
        source = root / "source-hermit"
        source.mkdir()
        self.git(source, "init", "--quiet")
        self.git(source, "config", "user.name", "Instruction Test")
        self.git(source, "config", "user.email", "instruction@example.invalid")
        (source / "AGENTS.md").write_text("child policy\n")
        self.git(source, "add", "AGENTS.md")
        self.git(source, "commit", "--quiet", "-m", "child policy")
        child_commit = self.git(source, "rev-parse", "HEAD")
        (root / ".gitmodules").write_text(
            '[submodule "hermit"]\n\tpath = hermit\n\turl = https://example.invalid/hermit\n'
        )
        self.git(root, "add", ".gitmodules")
        self.git(
            root,
            "update-index",
            "--add",
            "--cacheinfo",
            f"160000,{child_commit},hermit",
        )
        result = self.run_check(root)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("instruction_chain_materialization=partial", result.stdout)
        self.assertIn("instruction_chain_roots=parent", result.stdout)
        self.assertIn("instruction_chain_unmaterialized=hermit", result.stdout)
        self.assertIn("instruction_chain_bytes_partial=12", result.stdout)
        self.assertNotIn("instruction_chain_bytes_canonical=", result.stdout)

    def test_includes_materialized_canonical_submodule(self) -> None:
        root = self.fixture()
        (root / "AGENTS.md").write_text("r" * 40_000)
        self.git(root, "add", "AGENTS.md")
        child = root / "hermit"
        child.mkdir()
        self.git(child, "init", "--quiet")
        self.git(child, "config", "user.name", "Instruction Test")
        self.git(child, "config", "user.email", "instruction@example.invalid")
        (child / "AGENTS.md").write_text("h" * 20_000)
        self.git(child, "add", "AGENTS.md")
        self.git(child, "commit", "--quiet", "-m", "child policy")
        child_commit = self.git(child, "rev-parse", "HEAD")
        (root / ".gitmodules").write_text(
            '[submodule "hermit"]\n\tpath = hermit\n\turl = https://example.invalid/hermit\n'
        )
        self.git(root, "add", ".gitmodules")
        self.git(
            root,
            "update-index",
            "--add",
            "--cacheinfo",
            f"160000,{child_commit},hermit",
        )
        result = self.run_check(root)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("instruction_chain_materialization=complete", result.stdout)
        self.assertIn("instruction_chain_roots=parent,hermit", result.stdout)
        self.assertIn("instruction_chain_unmaterialized=none", result.stdout)
        self.assertIn("instruction_chain_bytes_canonical=60002", result.stdout)
        self.assertIn("instruction_chain_max_scope=hermit", result.stdout)

    def test_rejects_wrong_package_link(self) -> None:
        root = self.fixture()
        (root / ".agents/skills/example").unlink()
        (root / ".agents/skills/example").symlink_to(
            "../../.claude/skills/not-example", target_is_directory=True
        )
        result = self.run_check(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("expected '../../.claude/skills/example'", result.stderr)

    def test_rejects_file_only_link(self) -> None:
        root = self.fixture()
        (root / ".agents/skills/example").unlink()
        (root / ".agents/skills/example").symlink_to(
            "../../.claude/skills/example/SKILL.md"
        )
        result = self.run_check(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("expected '../../.claude/skills/example'", result.stderr)

    def test_rejects_symlinked_codex_root(self) -> None:
        root = self.fixture()
        example = root / ".agents/skills/example"
        readme = root / ".agents/skills/README.md"
        example.unlink()
        readme.unlink()
        (root / ".agents/skills").rmdir()
        (root / ".agents/skills").symlink_to(
            "../.claude/skills", target_is_directory=True
        )
        result = self.run_check(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("Codex skill root must be a real directory", result.stderr)

    def test_rejects_escaping_discovery_ancestor(self) -> None:
        root = self.fixture()
        external = root.parent / f"{root.name}-outside-agents"
        external.mkdir()
        self.addCleanup(external.rmdir)
        example = root / ".agents/skills/example"
        readme = root / ".agents/skills/README.md"
        example.unlink()
        readme.unlink()
        (root / ".agents/skills").rmdir()
        (root / ".agents").rmdir()
        (root / ".agents").symlink_to(external, target_is_directory=True)
        result = self.run_check(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("Codex discovery ancestor must be a real directory", result.stderr)

    def test_rejects_dangling_claude_policy_link(self) -> None:
        root = self.fixture()
        (root / "CLAUDE.md").unlink()
        (root / "CLAUDE.md").symlink_to("missing-policy.md")
        result = self.run_check(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("expected 'AGENTS.md'", result.stderr)

    def test_rejects_canonical_resource_link_outside_repository(self) -> None:
        root = self.fixture()
        external = root.parent / f"{root.name}-outside.md"
        external.write_text("outside\n")
        self.addCleanup(external.unlink)
        resource = root / ".claude/skills/example/references/outside.md"
        resource.symlink_to(external)
        result = self.run_check(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("canonical package link escapes repository", result.stderr)

    def test_rejects_flat_canonical_skill(self) -> None:
        root = self.fixture()
        (root / ".claude/skills/flat.md").write_text(
            "---\nname: flat\ndescription: flat\n---\n"
        )
        result = self.run_check(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("flat canonical skill is unsupported", result.stderr)

    def test_rejects_wrong_legacy_planner_target(self) -> None:
        root = self.fixture()
        (root / ".claude/skills/pr-landing-planner").symlink_to("../../wrong-target")
        result = self.run_check(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("expected '../../agent-utils/skills/pr-landing-planner'", result.stderr)

    def test_accepts_fixed_planner_source_and_rejects_codex_duplicate(self) -> None:
        root = self.fixture()
        (root / ".claude/skills/pr-landing-planner").symlink_to(
            "../../agent-utils/skills/pr-landing-planner"
        )
        result = self.run_check(root)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((root / ".agents/skills/pr-landing-planner").exists())

        (root / ".agents/skills/pr-landing-planner").symlink_to(
            "../../agent-utils/skills/pr-landing-planner"
        )
        result = self.run_check(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("planner is an external agent-utils package", result.stderr)

    def test_rejects_unowned_codex_entry(self) -> None:
        root = self.fixture()
        (root / ".agents/skills/runtime-state.json").write_text("{}\n")
        result = self.run_check(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("unowned Codex skill entry", result.stderr)

    def test_rejects_prose_wiki_link_but_allows_literal_code(self) -> None:
        root = self.fixture()
        source = root / ".claude/skills/example/SKILL.md"
        source.write_text(
            source.read_text()
            + "\nCargo supports `[[bin]]` syntax. See [[local-memory-only]].\n"
        )
        result = self.run_check(root)
        self.assertEqual(result.returncode, 1)
        self.assertIn("unsupported wiki link '[[local-memory-only]]'", result.stderr)
        self.assertNotIn("unsupported wiki link '[[bin]]'", result.stderr)


if __name__ == "__main__":
    unittest.main()
