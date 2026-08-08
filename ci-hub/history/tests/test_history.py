from __future__ import annotations

import csv
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HISTORY = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HISTORY))

import ingest
import query


class IngestUnitTest(unittest.TestCase):
    def test_run_to_row_splits_queue_and_run(self):
        run = {
            "id": 42, "run_attempt": 1, "workflow_id": 7, "name": "CI",
            "event": "push", "head_branch": "main", "head_sha": "a" * 40,
            "status": "completed", "conclusion": "success",
            "created_at": "2026-08-03T00:00:00Z",
            "run_started_at": "2026-08-03T00:05:00Z",   # 300s queued
            "updated_at": "2026-08-03T00:12:00Z",       # 420s running
            "pull_requests": [{"number": 1151}],
        }
        row = ingest.run_to_row("rrnewton/hermit", run)
        self.assertEqual(row["queue_s"], "300")
        self.assertEqual(row["run_s"], "420")
        self.assertEqual(row["pull_requests"], "1151")
        self.assertEqual(row["head_sha"], "a" * 40)

    def test_run_s_blank_until_terminal(self):
        run = {"id": 1, "run_attempt": 1, "status": "in_progress",
               "created_at": "2026-08-03T00:00:00Z",
               "run_started_at": "2026-08-03T00:05:00Z",
               "updated_at": "2026-08-03T00:09:00Z"}
        row = ingest.run_to_row("r/x", run)
        self.assertEqual(row["queue_s"], "300")
        self.assertEqual(row["run_s"], "")   # not completed -> no run duration

    def test_upsert_idempotent_and_newest_wins(self):
        rows: dict = {}
        base = {"repo": "r/x", "run_id": "1", "run_attempt": "1"}
        early = {**base, "status": "in_progress", "conclusion": "",
                 "updated_at": "2026-08-03T00:09:00Z"}
        late = {**base, "status": "completed", "conclusion": "success",
                "updated_at": "2026-08-03T00:12:00Z"}
        ingest.upsert(rows, early)
        ingest.upsert(rows, early)          # duplicate observation
        self.assertEqual(len(rows), 1)      # no dup row
        ingest.upsert(rows, late)           # promotion
        self.assertEqual(rows[("r/x", "1", "1")]["conclusion"], "success")
        ingest.upsert(rows, early)          # stale re-observation must not regress
        self.assertEqual(rows[("r/x", "1", "1")]["status"], "completed")


class TempParentTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.parent = Path(self.tmp.name)
        (self.parent / "ignored" / "ci-hub").mkdir(parents=True)
        self._prev_env = os.environ.get("DEV_HERMIT_PARENT")
        self._prev_producer_registry = os.environ.get(
            "PRODUCER_DEFINITION_REGISTRY"
        )
        self._prev_producer_repo = os.environ.get("PRODUCER_DEFINITION_REPO")
        os.environ["DEV_HERMIT_PARENT"] = str(self.parent)
        self.producer_repo = self.parent / "producer-repo"
        (self.producer_repo / ".github" / "workflows").mkdir(parents=True)
        (self.producer_repo / "validate.sh").write_text("validate fixture\n")
        (self.producer_repo / ".github" / "workflows" / "ci-portable.yml").write_text(
            "portable fixture\n"
        )
        subprocess.run(
            ["git", "init", "-q", str(self.producer_repo)], check=True
        )
        subprocess.run(
            [
                "git",
                "-C",
                str(self.producer_repo),
                "-c",
                "user.name=history-test",
                "-c",
                "user.email=history-test@example.invalid",
                "add",
                "validate.sh",
                ".github/workflows/ci-portable.yml",
            ],
            check=True,
        )
        subprocess.run(
            [
                "git",
                "-C",
                str(self.producer_repo),
                "-c",
                "user.name=history-test",
                "-c",
                "user.email=history-test@example.invalid",
                "commit",
                "-qm",
                "producer fixture",
            ],
            check=True,
        )
        self.producer_sha = subprocess.run(
            ["git", "-C", str(self.producer_repo), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        definition = {
            path: subprocess.run(
                [
                    "git",
                    "-C",
                    str(self.producer_repo),
                    "rev-parse",
                    f"HEAD:{path}",
                ],
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip()
            for path in ("validate.sh", ".github/workflows/ci-portable.yml")
        }
        self.producer_registry = self.parent / "producer-definition.json"
        self.producer_registry.write_text(
            json.dumps(
                {
                    "registered_at": self.producer_sha,
                    "registered_coverage_status": "legacy-selected-paths",
                    "registered_valid_commits": [self.producer_sha],
                    "registered": definition,
                }
            )
        )
        os.environ["PRODUCER_DEFINITION_REGISTRY"] = str(self.producer_registry)
        os.environ["PRODUCER_DEFINITION_REPO"] = str(self.producer_repo)

    def tearDown(self):
        if self._prev_env is None:
            os.environ.pop("DEV_HERMIT_PARENT", None)
        else:
            os.environ["DEV_HERMIT_PARENT"] = self._prev_env
        if self._prev_producer_registry is None:
            os.environ.pop("PRODUCER_DEFINITION_REGISTRY", None)
        else:
            os.environ["PRODUCER_DEFINITION_REGISTRY"] = self._prev_producer_registry
        if self._prev_producer_repo is None:
            os.environ.pop("PRODUCER_DEFINITION_REPO", None)
        else:
            os.environ["PRODUCER_DEFINITION_REPO"] = self._prev_producer_repo
        self.tmp.cleanup()

    def _write_gha(self, rows):
        path = self.parent / "ignored" / "ci-hub" / "gha-runs.csv"
        with open(path, "w", newline="") as fh:
            w = csv.DictWriter(fh, fieldnames=ingest.GHA_COLUMNS,
                               extrasaction="ignore")
            w.writeheader()
            for r in rows:
                w.writerow({c: r.get(c, "") for c in ingest.GHA_COLUMNS})

    def _write_step_profiles(self, node_rows):
        prof = (self.parent / "checkout" / ".safe-ci-dag-runner" / "profiles")
        prof.mkdir(parents=True)
        path = prof / "step_profiles_machine_class.csv"
        cols = ["timestamp", "git_sha", "step", "elapsed_s", "user_s", "sys_s"]
        # widen the header to any extra columns the row carries (kill flags,
        # cgroup counters, ...) so a test can exercise them without dropping data.
        for r in node_rows:
            for k in r:
                if k not in cols:
                    cols.append(k)
        with open(path, "w", newline="") as fh:
            w = csv.DictWriter(fh, fieldnames=cols)
            w.writeheader()
            for r in node_rows:
                w.writerow(r)

    def test_node_cpu_budgets_thin_and_suggested(self):
        # build.x has 6 samples -> not thin, suggested = round(max_cpu * 1.5);
        # build.y has 2 samples -> thin, suggested None.
        rows = []
        cpus = [(10, 2), (12, 3), (11, 2), (13, 4), (20, 5), (9, 1)]  # max cpu=25
        for i, (u, s) in enumerate(cpus):
            rows.append({"timestamp": f"2026-08-03T0{i}:00:00Z", "git_sha": "a" * 40,
                         "step": "build.x", "elapsed_s": 30 + i,
                         "user_s": u, "sys_s": s})
        rows.append({"timestamp": "2026-08-03T00:00:00Z", "git_sha": "b" * 40,
                     "step": "build.y", "elapsed_s": 5, "user_s": 1, "sys_s": 1})
        rows.append({"timestamp": "2026-08-03T01:00:00Z", "git_sha": "b" * 40,
                     "step": "build.y", "elapsed_s": 6, "user_s": 2, "sys_s": 1})
        self._write_step_profiles(rows)

        out = {r["node"]: r for r in
               query.node_cpu_budgets(str(self.parent), None, None, 5)}
        self.assertEqual(out["build.x"]["n_samples"], 6)
        self.assertFalse(out["build.x"]["thin"])
        self.assertEqual(out["build.x"]["max_cpu_s"], 25.0)      # 20+5
        self.assertEqual(out["build.x"]["suggested_cpu_timeout"], 38)  # round(25*1.5)
        self.assertTrue(out["build.y"]["thin"])
        self.assertIsNone(out["build.y"]["suggested_cpu_timeout"])

    def test_node_cpu_budgets_skips_rows_missing_cpu(self):
        rows = [{"timestamp": "2026-08-03T00:00:00Z", "git_sha": "a" * 40,
                 "step": "test.a", "elapsed_s": 1, "user_s": "", "sys_s": ""}]
        self._write_step_profiles(rows)
        out = {r["node"]: r for r in
               query.node_cpu_budgets(str(self.parent), None, None, 5)}
        self.assertEqual(out["test.a"]["n_samples"], 0)   # no cpu sample
        self.assertEqual(out["test.a"]["max_wall_s"], 1.0)
        self.assertTrue(out["test.a"]["thin"])

    def test_node_cpu_budgets_excludes_kill_samples(self):
        # THE load/defect-immunity property. A livelocked node hits the wall gate
        # burning ~one core, so its killed samples record cpu ~= wall ~= the gate.
        # Fed raw into round(max_cpu*1.5) those cap artifacts derive a budget so
        # generous the livelock they came from could never trip it (the measured
        # detcore_misc case: 912s from two ~600s kills, when legit runs are ~16s).
        # The budget must come from the VALID samples only.
        rows = []
        # 6 legitimate runs: max legit cpu = 16 (14+2).
        legit = [(10, 2), (12, 3), (11, 2), (13, 1), (14, 2), (9, 1)]
        for i, (u, s) in enumerate(legit):
            rows.append({"timestamp": f"2026-08-03T0{i}:00:00Z", "git_sha": "a" * 40,
                         "step": "test.detcore_misc", "elapsed_s": 30 + i,
                         "user_s": u, "sys_s": s, "ok": "True",
                         "timed_out": "False", "cpu_timed_out": "False",
                         "oom_kills": 0})
        # Two livelock kills at the 600s wall gate — cpu ~= wall, ratio ~1.0.
        rows.append({"timestamp": "2026-08-03T09:00:00Z", "git_sha": "a" * 40,
                     "step": "test.detcore_misc", "elapsed_s": 600.0,
                     "user_s": 590.0, "sys_s": 10.0, "ok": "False",
                     "timed_out": "True", "cpu_timed_out": "False", "oom_kills": 0})
        # A cpu-timeout kill AND an oom kill — both excluded regardless of ratio.
        rows.append({"timestamp": "2026-08-03T10:00:00Z", "git_sha": "a" * 40,
                     "step": "test.detcore_misc", "elapsed_s": 300.0,
                     "user_s": 800.0, "sys_s": 20.0, "ok": "False",
                     "timed_out": "False", "cpu_timed_out": "True", "oom_kills": 0})
        rows.append({"timestamp": "2026-08-03T11:00:00Z", "git_sha": "a" * 40,
                     "step": "test.detcore_misc", "elapsed_s": 45.0,
                     "user_s": 40.0, "sys_s": 5.0, "ok": "False",
                     "timed_out": "False", "cpu_timed_out": "False", "oom_kills": 1})
        self._write_step_profiles(rows)

        out = {r["node"]: r for r in
               query.node_cpu_budgets(str(self.parent), None, None, 5)}
        n = out["test.detcore_misc"]
        # POSITIVE: budget derived from valid samples only (max legit cpu = 16).
        self.assertEqual(n["n_samples"], 6)
        self.assertEqual(n["n_rows"], 9)
        self.assertEqual(n["n_excluded_kill"], 3)
        self.assertEqual(n["max_cpu_s"], 16.0)
        self.assertEqual(n["suggested_cpu_timeout"], 24)   # round(16*1.5), NOT 912
        self.assertFalse(n["thin"])

    def test_node_cpu_budgets_hosted_multiplier(self):
        # The canonical hosted budget = local tight budget x multiplier, and it
        # is UNSET exactly when the local budget is (a defect/thin node never
        # emits a hosted number either).
        rows = []
        cpus = [(10, 2), (12, 3), (11, 2), (13, 4), (20, 5), (9, 1)]  # max cpu=25
        for i, (u, s) in enumerate(cpus):
            rows.append({"timestamp": f"2026-08-03T0{i}:00:00Z", "git_sha": "a" * 40,
                         "step": "build.x", "elapsed_s": 30 + i,
                         "user_s": u, "sys_s": s})
        # thin node -> both budgets UNSET.
        rows.append({"timestamp": "2026-08-03T00:00:00Z", "git_sha": "b" * 40,
                     "step": "build.y", "elapsed_s": 5, "user_s": 1, "sys_s": 1})
        self._write_step_profiles(rows)

        out = {r["node"]: r for r in
               query.node_cpu_budgets(str(self.parent), None, None, 5,
                                      hosted_multiplier=2.0)}
        # local tight = round(25*1.5)=38; hosted = round(38*2)=... derived from
        # the same base (25*1.5*2=75), NOT from the rounded local value.
        self.assertEqual(out["build.x"]["suggested_cpu_timeout"], 38)
        self.assertEqual(out["build.x"]["suggested_cpu_timeout_hosted"], 75)
        self.assertEqual(out["build.x"]["hosted_multiplier"], 2.0)
        # thin -> both UNSET, hosted never fabricated from a defect/thin node.
        self.assertIsNone(out["build.y"]["suggested_cpu_timeout"])
        self.assertIsNone(out["build.y"]["suggested_cpu_timeout_hosted"])

        # a tighter multiplier flows through.
        out15 = {r["node"]: r for r in
                 query.node_cpu_budgets(str(self.parent), None, None, 5,
                                        hosted_multiplier=1.5)}
        self.assertEqual(out15["build.x"]["suggested_cpu_timeout_hosted"], 56)  # round(25*1.5*1.5)

    def test_node_cpu_budgets_all_kill_node_unset(self):
        # A node whose ONLY samples are kills yields no budget at all — never a
        # number derived from the defect. n_samples falls to 0 -> thin -> UNSET.
        rows = [{"timestamp": f"2026-08-03T0{i}:00:00Z", "git_sha": "a" * 40,
                 "step": "test.always_livelocks", "elapsed_s": 600.0,
                 "user_s": 595.0, "sys_s": 5.0, "ok": "False",
                 "timed_out": "True", "cpu_timed_out": "False", "oom_kills": 0}
                for i in range(6)]
        self._write_step_profiles(rows)
        out = {r["node"]: r for r in
               query.node_cpu_budgets(str(self.parent), None, None, 5)}
        n = out["test.always_livelocks"]
        self.assertEqual(n["n_samples"], 0)
        self.assertEqual(n["n_excluded_kill"], 6)
        self.assertIsNone(n["suggested_cpu_timeout"])
        self.assertTrue(n["thin"])

    def _write_github_step_profiles(self, node_rows):
        # ci-perf artifacts downloaded from GitHub land under
        # store_dir/gha-profiles -> discover_step_profiles tags these origin=github
        # (source=github-ciperf). Still cpu-bearing (runner output), but a
        # DIFFERENT environment than the local box.
        prof = (self.parent / "ignored" / "ci-hub" / "gha-profiles")
        prof.mkdir(parents=True, exist_ok=True)
        path = prof / "step_profiles_gh_class.csv"
        cols = ["timestamp", "git_sha", "step", "elapsed_s", "user_s", "sys_s"]
        for r in node_rows:
            for k in r:
                if k not in cols:
                    cols.append(k)
        with open(path, "w", newline="") as fh:
            w = csv.DictWriter(fh, fieldnames=cols)
            w.writeheader()
            for r in node_rows:
                w.writerow(r)

    def _prof_row(self, step, elapsed_s, sha="a" * 40, ts="2026-08-03T00:00:00Z",
                  **extra):
        row = {"timestamp": ts, "git_sha": sha, "step": step,
               "elapsed_s": elapsed_s}
        row.update(extra)
        return row

    def test_kill_taxonomy_livelock_vs_contention_vs_oom(self):
        # Same wall-budget kill, opposite cause, opposite verdict — the cpu/wall
        # ratio at the kill is the ONLY discriminator. Plant one of each and a
        # genuine pass, and assert both the summary counts and the per-kill ratio.
        self._write_step_profiles([
            # LIVELOCK: cpu ~= wall (a full core burned) -> retry-futile. This is
            # the measured detcore_misc signature (600.013 wall / 599.986 cpu).
            self._prof_row("test.detcore_misc", 600.013, user_s=590.0, sys_s=9.986,
                           timed_out="True", cpu_timed_out="True"),
            # CONTENTION: high wall, low cpu -> the step was waiting -> retry-valid.
            self._prof_row("test.waiter", 600.0, user_s=30.0, sys_s=30.0,
                           timed_out="True", cpu_timed_out="False"),
            # OOM is a MEMORY kill, orthogonal to the spin question -> own bucket
            # regardless of ratio (a parallel build can have cpu >> wall).
            self._prof_row("build.dbi_release", 50.0, user_s=1800.0, sys_s=100.0,
                           timed_out="False", oom_kills="1"),
            # A genuine PASS: no kill flag -> contributes a ratio but NO kill row
            # (proves the classifier does not manufacture a kill from a pass).
            self._prof_row("test.ok", 10.0, user_s=9.0, sys_s=0.5),
        ])
        res = query.kill_taxonomy(str(self.parent), None, None)
        self.assertEqual(res["n_kills"], 3)  # the pass is excluded
        self.assertEqual(res["summary"]["livelock"], 1)
        self.assertEqual(res["summary"]["contention"], 1)
        self.assertEqual(res["summary"]["oom"], 1)
        by = {k["node"]: k for k in res["kills"]}
        self.assertEqual(by["test.detcore_misc"]["verdict"], "livelock")
        self.assertAlmostEqual(by["test.detcore_misc"]["cpu_wall_ratio"], 1.0,
                               places=2)
        self.assertEqual(by["test.waiter"]["verdict"], "contention")
        self.assertAlmostEqual(by["test.waiter"]["cpu_wall_ratio"], 0.1, places=2)
        self.assertEqual(by["build.dbi_release"]["verdict"], "oom")
        # node_ratios covers EVERY node with a ratio, pass or kill (the ratio is
        # informative on passes too, per requirement 1).
        ratio_nodes = {n["node"] for n in res["node_ratios"]}
        self.assertIn("test.ok", ratio_nodes)

    def test_kill_taxonomy_records_path_provenance_and_splits_populations(self):
        # PREREQUISITE for any no_result split: every record carries which PATH
        # produced it, and the summary splits by source. Two paths, same node,
        # opposite ratios -> their ratios must NOT be pooled and the mix must be
        # visible, or a split would produce a precise-looking meaningless number.
        self._write_step_profiles([  # runner-native (local box): a spin
            self._prof_row("test.x", 100.0, user_s=95.0, sys_s=0.0,
                           timed_out="True", cpu_timed_out="True"),
        ])
        self._write_github_step_profiles([  # github-ciperf: same node, a wait
            self._prof_row("test.x", 100.0, user_s=5.0, sys_s=0.0,
                           timed_out="True", cpu_timed_out="False"),
        ])
        res = query.kill_taxonomy(str(self.parent), None, None)
        self.assertEqual(res["n_kills"], 2)
        srcs = {k["source"] for k in res["kills"]}
        self.assertEqual(srcs, {"runner-native", "github-ciperf"})
        # by_source splits the mix: livelock came from the box, contention from GH.
        self.assertEqual(res["by_source"]["runner-native"]["livelock"], 1)
        self.assertEqual(res["by_source"]["github-ciperf"]["contention"], 1)
        # node_ratios are keyed per (source, node) -> two rows for one node name,
        # NOT one pooled ratio that averages a 0.95 spin with a 0.05 wait.
        xrows = {n["source"]: n for n in res["node_ratios"] if n["node"] == "test.x"}
        self.assertEqual(set(xrows), {"runner-native", "github-ciperf"})
        self.assertAlmostEqual(xrows["runner-native"]["p50_ratio"], 0.95, places=2)
        self.assertAlmostEqual(xrows["github-ciperf"]["p50_ratio"], 0.05, places=2)

    def test_kill_taxonomy_ambiguous_band(self):
        # A ratio in [0.3, 0.8) is neither a clean spin nor a clean wait -> it is
        # surfaced as 'ambiguous' rather than force-bucketed, so a reader audits it.
        self._write_step_profiles([
            self._prof_row("test.mid", 100.0, user_s=50.0, sys_s=0.0,
                           timed_out="True", cpu_timed_out="False"),
        ])
        res = query.kill_taxonomy(str(self.parent), None, None)
        self.assertEqual(res["summary"]["ambiguous"], 1)
        self.assertEqual(res["kills"][0]["verdict"], "ambiguous")

    def test_kill_taxonomy_cgroup_usec_fallback(self):
        # A row carrying only the cgroup counter (no user_s/sys_s) still yields a
        # ratio: cpu.usage_usec / 1e6 -> cpu-seconds.
        self._write_step_profiles([
            self._prof_row("test.cg", 100.0, user_s="", sys_s="",
                           timed_out="True", cpu_timed_out="True",
                           **{"cpu.usage_usec": "95000000"}),  # 95 cpu-s
        ])
        res = query.kill_taxonomy(str(self.parent), None, None)
        k = res["kills"][0]
        self.assertAlmostEqual(k["cpu_s"], 95.0, places=2)
        self.assertAlmostEqual(k["cpu_wall_ratio"], 0.95, places=2)
        self.assertEqual(k["verdict"], "livelock")

    def test_kill_taxonomy_no_wall_is_unknown_not_a_divide(self):
        # No/zero wall -> ratio is None (never divide by a missing denominator);
        # a kill with no ratio is 'unknown', not silently dropped.
        self._write_step_profiles([
            self._prof_row("test.nowall", "", user_s=5.0, sys_s=0.0,
                           timed_out="True", cpu_timed_out="False"),
        ])
        res = query.kill_taxonomy(str(self.parent), None, None)
        self.assertEqual(res["n_kills"], 1)
        self.assertIsNone(res["kills"][0]["cpu_wall_ratio"])
        self.assertEqual(res["kills"][0]["verdict"], "unknown")

    def _gha_wf(self, sha, concl, created, updated, wf="W", status="completed",
                run_id=None):
        return {"repo": "r/x", "run_id": run_id or (sha + concl),
                "run_attempt": "1", "workflow_name": wf, "head_branch": "main",
                "head_sha": sha, "status": status, "conclusion": concl,
                "created_at": created, "updated_at": updated}

    def _write_ledger(self, rows):
        path = self.parent / "ignored" / "validate-run-ledger.jsonl"
        with open(path, "w") as fh:
            for r in rows:
                fh.write(json.dumps(r) + "\n")

    def _full_pass_row(self, sha, **over):
        row = {"schema_version": 5, "commit": sha,
               "commit_anchored": True, "tree_dirty": False,
               "selection_mode": "full", "profile": "full", "result": "pass",
               "executed_tests": 42, "filtered_tests": 0,
               "producer": "hermit-validate-sh",
               "base_sha": "b" * 40, "base_tree": "c" * 40,
               "reverie_base_sha": "d" * 40,
               "reverie_base_tree": "e" * 40,
               "admission": "ci-hub-validate-lock",
               "concurrent_validates": 0,
               "concurrency_proof": "validate_lock_owner_ancestry",
               "cwd": str(self.producer_repo),
               "coverage": {"planned_test_nodes": 1,
                            "executed_test_nodes": 1,
                            "zero_executed_nodes": [],
                            "absent_nodes": []}}
        row.update(over)
        return row

    def test_green_split_conclusion_only_when_no_ledger(self):
        # A green-by-conclusion commit with NO ledger receipt: all green time is
        # conclusion-only, ledger-corroborated is 0 (the reverie case).
        self._write_gha([
            self._gha_wf(self.producer_sha, "success", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:00:00Z"),
            self._gha_wf("b" * 40, "success", "2026-08-03T01:00:00Z",
                         "2026-08-03T01:00:00Z"),
        ])
        res = query.green_time(str(self.parent), "r/x", None, ["W"])
        self.assertGreater(res["green_pct"], 0.0)
        self.assertEqual(res["green_ledger_pct"], 0.0)
        self.assertEqual(res["green_ledger_hours"], 0.0)
        self.assertAlmostEqual(res["green_conclusion_only_pct"],
                               res["green_pct"], places=2)

    def test_green_split_ledger_corroborated_when_full_pass_row_exists(self):
        # POSITIVE: a full-pass ledger row at the exact green commit SHA moves
        # that slice into green_ledger. Bracket the mechanism firing.
        self._write_gha([
            self._gha_wf(self.producer_sha, "success", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:00:00Z"),
            self._gha_wf("b" * 40, "success", "2026-08-03T01:00:00Z",
                         "2026-08-03T01:00:00Z"),
        ])
        self._write_ledger([self._full_pass_row(self.producer_sha)])
        res = query.green_time(str(self.parent), "r/x", None, ["W"])
        self.assertGreater(res["green_ledger_hours"], 0.0)
        # combined green is never silently summed away: sub-buckets add to green.
        self.assertAlmostEqual(
            res["green_ledger_hours"] + res["green_conclusion_only_hours"],
            res["green_hours"], places=2)

    def test_green_split_filtered_tests_nonzero_still_corroborates(self):
        # POSITIVE: aggregate filtered_tests is diagnostic. A full run may filter
        # tests outside its planned DAG nodes, while complete per-node coverage
        # proves that every planned test-bearing node actually ran.
        self._write_gha([
            self._gha_wf(self.producer_sha, "success", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:00:00Z"),
            self._gha_wf("b" * 40, "success", "2026-08-03T01:00:00Z",
                         "2026-08-03T01:00:00Z"),
        ])
        self._write_ledger([self._full_pass_row(
            self.producer_sha,
            schema_version=5,
            filtered_tests=3,
            coverage={"planned_test_nodes": 2, "zero_executed_nodes": [],
                      "absent_nodes": []},
        )])
        res = query.green_time(str(self.parent), "r/x", None, ["W"])
        self.assertGreater(res["green_ledger_hours"], 0.0)

    def test_green_split_incomplete_coverage_does_not_corroborate(self):
        # NEGATIVE: an absent planned node keeps the slice conclusion-only even
        # when aggregate counts are positive. Coverage, not filtered count, is
        # the binding evidence for completeness.
        self._write_gha([
            self._gha_wf(self.producer_sha, "success", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:00:00Z"),
            self._gha_wf("b" * 40, "success", "2026-08-03T01:00:00Z",
                         "2026-08-03T01:00:00Z"),
        ])
        self._write_ledger([self._full_pass_row(
            self.producer_sha,
            schema_version=5,
            filtered_tests=3,
            coverage={"planned_test_nodes": 2, "zero_executed_nodes": [],
                      "absent_nodes": ["test.missing"]},
        )])
        res = query.green_time(str(self.parent), "r/x", None, ["W"])
        self.assertEqual(res["green_ledger_hours"], 0.0)

    def test_green_time_red_reign_is_fixed_hour(self):
        # Commit A: run created+terminal-failure at 00:00; commit B: created
        # 01:00 (bounds A's reign to exactly 1h), success. A completes instantly
        # so its whole 1h reign is red; B's success tail runs to now.
        self._write_gha([
            self._gha_wf("a" * 40, "failure", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:00:00Z"),
            self._gha_wf("b" * 40, "success", "2026-08-03T01:00:00Z",
                         "2026-08-03T01:00:00Z"),
        ])
        res = query.green_time(str(self.parent), "r/x", None, ["W"])
        self.assertEqual(res["samples"], 2)
        self.assertAlmostEqual(res["red_hours"], 1.0, places=1)
        # denominator is fully accounted: the four states sum to total.
        self.assertAlmostEqual(
            res["green_hours"] + res["red_hours"] + res["no_result_hours"]
            + res["gap_hours"], res["total_hours"], places=1)
        self.assertEqual(res["current_state"], "green")
        self.assertEqual(res["definition_date"], query.GREEN_TIME_DEFINITION_DATE)

    def test_same_timestamp_uses_newest_run_id(self):
        # Two opposite conclusions at one exact head must not depend on CSV/API
        # order. GitHub run IDs break a timestamp tie deterministically.
        self._write_gha([
            self._gha_wf(
                "a" * 40,
                "failure",
                "2026-08-03T00:00:00Z",
                "2026-08-03T00:00:00Z",
                run_id="10",
            ),
            self._gha_wf(
                "a" * 40,
                "success",
                "2026-08-03T00:00:00Z",
                "2026-08-03T00:00:00Z",
                run_id="11",
            ),
            self._gha_wf(
                "b" * 40,
                "success",
                "2026-08-03T01:00:00Z",
                "2026-08-03T01:00:00Z",
                run_id="12",
            ),
        ])
        res = query.green_time(str(self.parent), "r/x", None, ["W"])
        self.assertEqual(res["red_hours"], 0.0)
        self.assertEqual(res["current_state"], "green")

    def test_green_time_pending_tip_is_gap_not_green(self):
        # The owner's example: main has zero reds but the current tip's
        # authoritative run is still pending -> the tip reign is GAP, never green.
        self._write_gha([
            self._gha_wf("a" * 40, "success", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:00:00Z"),
            self._gha_wf("b" * 40, "", "2026-08-03T01:00:00Z",
                         "2026-08-03T01:00:00Z", status="in_progress"),
        ])
        res = query.green_time(str(self.parent), "r/x", None, ["W"])
        self.assertEqual(res["current_state"], "gap")
        self.assertEqual(res["current_reason"], "pending")
        self.assertGreater(res["gap_hours"], 0.0)
        self.assertEqual(res["red_hours"], 0.0)  # zero reds, yet not fully green

    def test_green_time_cancelled_is_no_result_not_red(self):
        # A supersede/manual cancel is a destroyed answer, not a failure: it is
        # no_result, never red, and never green.
        self._write_gha([
            self._gha_wf("a" * 40, "cancelled", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:00:00Z"),
            self._gha_wf("b" * 40, "success", "2026-08-03T01:00:00Z",
                         "2026-08-03T01:00:00Z"),
        ])
        res = query.green_time(str(self.parent), "r/x", None, ["W"])
        self.assertAlmostEqual(res["no_result_hours"], 1.0, places=1)
        self.assertEqual(res["red_hours"], 0.0)

    def test_green_time_denominator_excludes_cancelled_and_no_conclusion(self):
        # Hand-computable window:
        #   00-01 success    -> 1 green authoritative hour
        #   01-03 cancelled  -> 2 NO-RESULT hours, excluded
        #   03-04 no concl.  -> 1 NO-RESULT hour, excluded
        #   04-05 failure    -> 1 red authoritative hour
        # The final success only bounds the preceding reign. Green-time is
        # therefore 1 / (1 + 1) = 50%, never 1 / the four-hour mixed window.
        self._write_gha([
            self._gha_wf("a" * 40, "success", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:00:00Z", run_id="1"),
            self._gha_wf("b" * 40, "cancelled", "2026-08-03T01:00:00Z",
                         "2026-08-03T01:00:00Z", run_id="2"),
            self._gha_wf("c" * 40, "", "2026-08-03T03:00:00Z",
                         "2026-08-03T03:00:00Z", run_id="3"),
            self._gha_wf("d" * 40, "failure", "2026-08-03T04:00:00Z",
                         "2026-08-03T04:00:00Z", run_id="4"),
            self._gha_wf("e" * 40, "success", "2026-08-03T05:00:00Z",
                         "2026-08-03T05:00:00Z", run_id="5"),
        ])
        res = query.green_time(str(self.parent), "r/x", None, ["W"])
        self.assertAlmostEqual(res["green_hours"], 1.0, places=1)
        self.assertAlmostEqual(res["red_hours"], 1.0, places=1)
        self.assertAlmostEqual(res["no_result_hours"], 3.0, places=1)
        self.assertAlmostEqual(res["authoritative_run_hours"], 2.0, places=1)
        self.assertAlmostEqual(res["green_pct"], 50.0, places=1)
        self.assertAlmostEqual(res["red_pct"], 50.0, places=1)

        # Independent recomputation from the classified intervals: do not call
        # green_time's denominator arithmetic. This brackets the published
        # value against a second implementation over the identical window.
        tl = query.state_timeline(str(self.parent), "r/x", None, ["W"])
        independent = {"green": 0.0, "red": 0.0}
        for iv in tl["intervals"]:
            if iv["state"] in independent:
                independent[iv["state"]] += iv["end"] - iv["start"]
        independent_denom = independent["green"] + independent["red"]
        independent_pct = 100.0 * independent["green"] / independent_denom
        self.assertAlmostEqual(res["green_pct"], independent_pct, places=8)

    def test_only_no_result_has_zero_authoritative_denominator(self):
        # Both a literal cancellation and a completed run with no conclusion
        # are NO-RESULT. With no positive green/red answer, there is no
        # authoritative denominator and no percentage may be fabricated.
        self._write_gha([
            self._gha_wf("a" * 40, "cancelled", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:00:00Z", run_id="1"),
            self._gha_wf("b" * 40, "", "2026-08-03T01:00:00Z",
                         "2026-08-03T01:00:00Z", run_id="2"),
        ])
        res = query.green_time(str(self.parent), "r/x", None, ["W"])
        self.assertEqual(res["authoritative_run_hours"], 0.0)
        self.assertIsNone(res["green_pct"])
        self.assertIsNone(res["red_pct"])
        self.assertGreater(res["no_result_hours"], 0.0)
        self.assertIn("NO-RESULT and gap excluded", res["note"])

    def test_green_time_neutral_is_no_result_not_green(self):
        self._write_gha([
            self._gha_wf("a" * 40, "neutral", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:00:00Z"),
            self._gha_wf("b" * 40, "success", "2026-08-03T01:00:00Z",
                         "2026-08-03T01:00:00Z"),
        ])
        res = query.green_time(str(self.parent), "r/x", None, ["W"])
        self.assertAlmostEqual(res["no_result_hours"], 1.0, places=1)
        self.assertEqual(res["current_state"], "green")

    def _write_jobs(self, rows):
        path = self.parent / "ignored" / "ci-hub" / "gha-jobs.csv"
        cols = ["repo", "run_id", "job_id", "name", "conclusion",
                "started_at", "completed_at"]
        with open(path, "w", newline="") as fh:
            w = csv.DictWriter(fh, fieldnames=cols, extrasaction="ignore")
            w.writeheader()
            for r in rows:
                w.writerow({c: r.get(c, "") for c in cols})

    def test_green_time_case7_job_failed_before_cancel_is_red(self):
        # Seventh case: run-level cancelled, but a job FAILED at 00:30, before the
        # run's cancel at 00:40 -> ORDERING says the failure was independent ->
        # the reign is RED, not no_result.
        self._write_gha([
            self._gha_wf("a" * 40, "cancelled", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:40:00Z", run_id="R1"),
            self._gha_wf("b" * 40, "success", "2026-08-03T01:00:00Z",
                         "2026-08-03T01:00:00Z", run_id="R2"),
        ])
        self._write_jobs([
            {"repo": "r/x", "run_id": "R1", "job_id": "j1", "name": "build",
             "conclusion": "failure", "completed_at": "2026-08-03T00:30:00Z"},
        ])
        res = query.green_time(str(self.parent), "r/x", None, ["W"])
        # reign 00:00-01:00: pending until the run resolves at 00:40 (0.67h gap),
        # then the verdict 00:40-01:00 (0.33h) is RED via the ordering promotion.
        self.assertAlmostEqual(res["red_hours"], 0.33, places=1)
        # gap_hours now also carries the unobserved tail past the store's
        # observation horizon, which grows with wall-clock. Assert the OBSERVED
        # gap -- the quantity this test is actually about.
        self.assertAlmostEqual(
            res["gap_hours"] - res["store_stale_hours"], 0.67, places=1)
        self.assertEqual(res["no_result_hours"], 0.0)
        self.assertEqual(res["job_level_red_promotions"], 1)

    def test_green_time_case7_job_killed_by_cancel_stays_no_result(self):
        # A job killed BY the cancel (its red conclusion completes AFTER the
        # cancel moment) is not an independent failure -> stays no_result.
        self._write_gha([
            self._gha_wf("a" * 40, "cancelled", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:40:00Z", run_id="R1"),
            self._gha_wf("b" * 40, "success", "2026-08-03T01:00:00Z",
                         "2026-08-03T01:00:00Z", run_id="R2"),
        ])
        self._write_jobs([
            {"repo": "r/x", "run_id": "R1", "job_id": "j1", "name": "build",
             "conclusion": "failure", "completed_at": "2026-08-03T00:55:00Z"},
        ])
        res = query.green_time(str(self.parent), "r/x", None, ["W"])
        # completed AFTER the 00:40 cancel -> killed by it -> stays no_result.
        self.assertAlmostEqual(res["no_result_hours"], 0.33, places=1)
        self.assertEqual(res["red_hours"], 0.0)
        self.assertEqual(res["job_level_red_promotions"], 0)

    def test_green_time_case7_propagated_gate_failure_stays_no_result(self):
        # ROOT-CAUSE guard: a cancel-in-progress kills test-debug at 00:39:50; the
        # require-all aggregation gate then completes=failure at 00:40:00 BECAUSE a
        # required dep was cancelled. Its failure is PROPAGATED, not an independent
        # verdict (run-30873193855 / hermit-238b false red). Ordering against the
        # cancel ONSET (earliest cancelled-sibling completion) + the started_at
        # guard (the gate STARTS after its dep resolves) leaves it no_result.
        self._write_gha([
            self._gha_wf("a" * 40, "cancelled", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:40:00Z", run_id="R1"),
            self._gha_wf("b" * 40, "success", "2026-08-03T01:00:00Z",
                         "2026-08-03T01:00:00Z", run_id="R2"),
        ])
        self._write_jobs([
            {"repo": "r/x", "run_id": "R1", "job_id": "j1", "name": "test-debug",
             "conclusion": "cancelled", "started_at": "2026-08-03T00:20:00Z",
             "completed_at": "2026-08-03T00:39:50Z"},
            {"repo": "r/x", "run_id": "R1", "job_id": "j2",
             "name": "Require every portable DAG job to succeed or be deselected",
             "conclusion": "failure", "started_at": "2026-08-03T00:39:55Z",
             "completed_at": "2026-08-03T00:40:00Z"},
        ])
        res = query.green_time(str(self.parent), "r/x", None, ["W"])
        self.assertAlmostEqual(res["no_result_hours"], 0.33, places=1)
        self.assertEqual(res["red_hours"], 0.0)
        self.assertEqual(res["job_level_red_promotions"], 0)

    def test_green_time_case7_independent_failure_with_cancelled_sibling_is_red(self):
        # The genuine case the guard must still catch: a job FAILED at 00:30, then
        # an EXTERNAL newer push cancelled the run, killing a sibling at 00:40. The
        # failure both completed AND started before the cancel onset -> independent
        # -> RED, even though a cancelled sibling exists.
        self._write_gha([
            self._gha_wf("a" * 40, "cancelled", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:40:00Z", run_id="R1"),
            self._gha_wf("b" * 40, "success", "2026-08-03T01:00:00Z",
                         "2026-08-03T01:00:00Z", run_id="R2"),
        ])
        self._write_jobs([
            {"repo": "r/x", "run_id": "R1", "job_id": "j1", "name": "test-release",
             "conclusion": "failure", "started_at": "2026-08-03T00:20:00Z",
             "completed_at": "2026-08-03T00:30:00Z"},
            {"repo": "r/x", "run_id": "R1", "job_id": "j2", "name": "test-debug",
             "conclusion": "cancelled", "started_at": "2026-08-03T00:20:00Z",
             "completed_at": "2026-08-03T00:40:00Z"},
        ])
        res = query.green_time(str(self.parent), "r/x", None, ["W"])
        self.assertAlmostEqual(res["red_hours"], 0.33, places=1)
        self.assertEqual(res["no_result_hours"], 0.0)
        self.assertEqual(res["job_level_red_promotions"], 1)

    def test_green_time_case7_inert_without_job_store(self):
        # No gha-jobs.csv -> the discriminator is inert and cancelled stays
        # no_result (conservative), identical to the run-level-only behavior.
        self._write_gha([
            self._gha_wf("a" * 40, "cancelled", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:40:00Z", run_id="R1"),
            self._gha_wf("b" * 40, "success", "2026-08-03T01:00:00Z",
                         "2026-08-03T01:00:00Z", run_id="R2"),
        ])
        res = query.green_time(str(self.parent), "r/x", None, ["W"])
        self.assertAlmostEqual(res["no_result_hours"], 0.33, places=1)
        self.assertEqual(res["red_hours"], 0.0)
        self.assertEqual(res["job_level_red_promotions"], 0)

    def test_ingest_jobs_scopes_to_cancelled_authoritative_main_and_joins(self):
        # Ingester (C) end-to-end: only cancelled authoritative-MAIN runs are
        # fetched for jobs, and the resulting gha-jobs.csv drives query.py's
        # seventh case by file contract (no cross-module import of internals).
        def row(base, **over):
            base.update(over)
            return base
        self._write_gha([
            # candidate: cancelled, main, authoritative "W"
            self._gha_wf("a" * 40, "cancelled", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:40:00Z", run_id="R1"),
            # bounds R1's reign; not a candidate (success)
            self._gha_wf("b" * 40, "success", "2026-08-03T01:00:00Z",
                         "2026-08-03T01:00:00Z", run_id="R2"),
            # not a candidate: cancelled but non-authoritative workflow (same
            # commit as R1 so it adds no spurious reign boundary).
            self._gha_wf("a" * 40, "cancelled", "2026-08-03T00:10:00Z",
                         "2026-08-03T00:20:00Z", wf="Other", run_id="R3"),
            # not a candidate: cancelled authoritative but NOT on main
            row(self._gha_wf("d" * 40, "cancelled", "2026-08-03T00:10:00Z",
                             "2026-08-03T00:20:00Z", run_id="R4"),
                head_branch="pr-branch"),
        ])
        cand = ingest.cancelled_authoritative_runs(
            str(self.parent), "r/x", ["W"], None)
        self.assertEqual(cand, ["R1"])

        calls = []

        def fake_fetch(repo, run_id):
            calls.append(run_id)
            return [{"id": "j1", "run_id": run_id, "run_attempt": "1",
                     "name": "build", "status": "completed",
                     "conclusion": "failure",
                     "completed_at": "2026-08-03T00:30:00Z"}]

        orig = ingest.fetch_jobs
        ingest.fetch_jobs = fake_fetch
        try:
            ingest.ingest_jobs("r/x", str(self.parent), workflows=["W"],
                               since=None, refetch=False, max_runs=100)
            self.assertEqual(calls, ["R1"])  # scoped: only the one candidate
            # The join makes query.py promote the cancelled run to red (case 7).
            res = query.green_time(str(self.parent), "r/x", None, ["W"])
            self.assertEqual(res["job_level_red_promotions"], 1)
            self.assertAlmostEqual(res["red_hours"], 0.33, places=1)
            # Idempotent: a second pass re-fetches nothing (terminal run cached).
            calls.clear()
            ingest.ingest_jobs("r/x", str(self.parent), workflows=["W"],
                               since=None, refetch=False, max_runs=100)
            self.assertEqual(calls, [])
        finally:
            ingest.fetch_jobs = orig

    def test_green_time_commit_without_authoritative_run_is_gap_no_record(self):
        # Commit B ran only a NON-authoritative workflow: no authoritative check
        # record -> gap(no-record), never carried-forward as A's green.
        self._write_gha([
            self._gha_wf("a" * 40, "success", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:00:00Z"),
            self._gha_wf("b" * 40, "success", "2026-08-03T01:00:00Z",
                         "2026-08-03T01:00:00Z", wf="Other"),
        ])
        res = query.green_time(str(self.parent), "r/x", None, ["W"])
        self.assertEqual(res["current_state"], "gap")
        self.assertEqual(res["current_reason"], "no-record")
        self.assertAlmostEqual(res["green_hours"], 1.0, places=1)  # only A's reign

    def test_green_time_ignores_non_main(self):
        self._write_gha([
            self._gha_wf("a" * 40, "success", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:00:00Z"),
        ])
        # override branch to feature by rewriting the single row
        self._write_gha([{**self._gha_wf("a" * 40, "success",
                          "2026-08-03T00:00:00Z", "2026-08-03T00:00:00Z"),
                          "head_branch": "feature"}])
        res = query.green_time(str(self.parent), "r/x", None, ["W"])
        self.assertEqual(res["samples"], 0)
        self.assertIsNone(res["green_pct"])

    def test_green_time_multi_workflow_requires_all_success(self):
        # Two authoritative workflows at commit A: one success, one failure ->
        # combined red (green requires ALL).
        self._write_gha([
            self._gha_wf("a" * 40, "success", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:00:00Z", wf="W1"),
            self._gha_wf("a" * 40, "failure", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:00:00Z", wf="W2"),
            self._gha_wf("b" * 40, "success", "2026-08-03T01:00:00Z",
                         "2026-08-03T01:00:00Z", wf="W1"),
            self._gha_wf("b" * 40, "success", "2026-08-03T01:00:00Z",
                         "2026-08-03T01:00:00Z", wf="W2"),
        ])
        res = query.green_time(str(self.parent), "r/x", None, ["W1", "W2"])
        self.assertAlmostEqual(res["red_hours"], 1.0, places=1)  # A's reign red
        self.assertEqual(res["current_state"], "green")           # B all-success

    def test_green_time_trend_buckets_per_day(self):
        self._write_gha([
            self._gha_wf("a" * 40, "failure", "2026-08-01T00:00:00Z",
                         "2026-08-01T00:00:00Z"),
            self._gha_wf("b" * 40, "success", "2026-08-02T00:00:00Z",
                         "2026-08-02T00:00:00Z"),
        ])
        tr = query.green_time_trend(str(self.parent), "r/x", None, ["W"], "day")
        self.assertEqual(tr["bucket"], "day")
        self.assertGreaterEqual(len(tr["buckets"]), 1)
        # first day bucket is the failure reign -> 0% green
        self.assertEqual(tr["buckets"][0]["green_pct"], 0.0)

    def test_green_time_append_log_writes_jsonl(self):
        self._write_gha([
            self._gha_wf("a" * 40, "success", "2026-08-03T00:00:00Z",
                         "2026-08-03T00:00:00Z"),
        ])
        res = query.green_time(str(self.parent), "r/x", None, ["W"])
        path = str(self.parent / "gtlog.jsonl")
        query.append_green_time_log(str(self.parent), res, path)
        query.append_green_time_log(str(self.parent), res, path)
        with open(path) as fh:
            lines = [json.loads(x) for x in fh if x.strip()]
        self.assertEqual(len(lines), 2)  # appends, never truncates
        self.assertEqual(lines[0]["repo"], "r/x")
        self.assertIn("green_pct", lines[0])
        for field in ("window_start", "window_end_utc", "green_hours",
                      "authoritative_run_hours", "window_hours",
                      "denominator_definition", "no_result_hours"):
            self.assertIn(field, lines[0])


    def _gha_run(self, **over):
        base = {
            "repo": "rrnewton/hermit", "run_id": "1", "run_attempt": "1",
            "workflow_name": "CI", "head_branch": "main", "pull_requests": "",
            "status": "completed", "conclusion": "success",
            "created_at": "2026-08-03T00:00:00Z", "queue_s": "0", "run_s": "10",
            "html_url": "https://x/1",
        }
        base.update(over)
        return base

    def test_recent_runs_newest_first_and_pr_ref(self):
        self._write_gha([
            self._gha_run(run_id="a", created_at="2026-08-03T01:00:00Z"),
            self._gha_run(run_id="b", created_at="2026-08-03T03:00:00Z",
                          pull_requests="1561"),
            self._gha_run(run_id="c", created_at="2026-08-03T02:00:00Z"),
        ])
        res = query.recent_runs(str(self.parent), None, None, None, None, 10)
        ids = [r["run_id"] for r in res["runs"]]
        self.assertEqual(ids, ["b", "c", "a"])           # newest first
        self.assertEqual(res["runs"][0]["ref"], "#1561")  # PR beats branch
        self.assertEqual(res["runs"][1]["ref"], "main")

    def test_recent_runs_since_status_and_limit(self):
        self._write_gha([
            self._gha_run(run_id="old", created_at="2026-08-01T00:00:00Z"),
            self._gha_run(run_id="q1", created_at="2026-08-03T01:00:00Z",
                          status="queued", conclusion=""),
            self._gha_run(run_id="q2", created_at="2026-08-03T02:00:00Z",
                          status="queued", conclusion=""),
        ])
        # since drops the 08-01 row; status keeps only queued; limit caps output.
        res = query.recent_runs(str(self.parent), None, "2026-08-02",
                                None, "queued", 1)
        self.assertEqual(res["total_matched"], 2)   # both queued survive filter
        self.assertEqual(res["shown"], 1)           # limit
        self.assertEqual(res["runs"][0]["run_id"], "q2")
        self.assertEqual(res["runs"][0]["conclusion"], "queued")  # falls back to status

    def test_recent_runs_queue_outlier_and_slowest(self):
        # A realistic shape: many instant-start runs (p95 stays 0, so the 300s
        # floor governs) plus one stuck-for-hours run that must be flagged.
        rows = [self._gha_run(run_id=f"fast{i}",
                              created_at=f"2026-08-03T03:{i:02d}:00Z",
                              queue_s="0") for i in range(30)]
        rows.append(self._gha_run(run_id="stuck",
                                  created_at="2026-08-03T01:00:00Z",
                                  queue_s="26558", run_s="137"))
        self._write_gha(rows)
        res = query.recent_runs(str(self.parent), None, None, None, None, 10,
                                slowest=True)
        self.assertEqual(res["runs"][0]["run_id"], "stuck")   # slowest first
        self.assertTrue(res["runs"][0]["queue_outlier"])
        self.assertFalse(res["runs"][1]["queue_outlier"])
        self.assertEqual(res["window_outliers"], 1)
        # Rendered table marks the outlier and never truncates the workflow name.
        out = query.render_recent(res, 10)
        self.assertIn("!", out)
        self.assertIn("26558", out)

    def test_queued_run_gets_offline_lower_bound_not_zero(self):
        # A still-queued run has run_started_at == created_at (GitHub placeholder)
        # => queue_s == 0. The listing must replace that misleading 0 with the
        # snapshot-anchored lower bound, keep queue_s itself untouched, and flag
        # the run as an outlier off the lower bound (not the stored 0).
        created = "2026-08-03T00:00:00Z"
        self._write_gha([
            self._gha_run(run_id="term", created_at="2026-08-03T05:00:00Z",
                          conclusion="success", queue_s="0"),
            self._gha_run(run_id="stuck", created_at=created,
                          run_started_at=created,  # placeholder == created
                          status="queued", conclusion="", queue_s="0"),
        ])
        # Pin the snapshot clock: set the store mtime to created + 2h.
        store = self.parent / "ignored" / "ci-hub" / "gha-runs.csv"
        snap = query._epoch(created) + 7200
        os.utime(store, (snap, snap))

        res = query.recent_runs(str(self.parent), None, None, None, None, 10,
                                slowest=True)
        stuck = next(r for r in res["runs"] if r["run_id"] == "stuck")
        term = next(r for r in res["runs"] if r["run_id"] == "term")
        self.assertEqual(stuck["queue_s"], 0.0)              # stored value untouched
        self.assertEqual(stuck["queue_lower_bound_s"], 7200)  # snapshot - created
        self.assertTrue(stuck["queue_outlier"])              # flagged off the LB
        self.assertIsNone(term["queue_lower_bound_s"])       # terminal: no LB
        self.assertEqual(res["runs"][0]["run_id"], "stuck")  # slowest = LB wins
        # queue_s percentiles must stay measured-only (the LB never leaks in).
        self.assertEqual(res["queue_p95_s"], 0.0)
        out = query.render_recent(res, 10)
        self.assertIn(">=7200", out)
        self.assertIn("still queued as of snapshot", out)


class HelperTest(unittest.TestCase):
    def test_percentile_nearest_rank(self):
        self.assertEqual(query.percentile([1, 2, 3, 4], 50), 2)
        self.assertEqual(query.percentile([1, 2, 3, 4], 95), 4)
        self.assertIsNone(query.percentile([], 50))


if __name__ == "__main__":
    unittest.main()


class GreenTimeObservationHorizonTest(unittest.TestCase):
    """A stale store must not carry the last known state forward as fact.

    The metric partitions main's wall-clock into green/red/no_result/gap. Before
    the horizon cap, the LAST commit's reign ran to `now`, so any time after the
    store's newest row inherited that commit's verdict. Measured on the live
    store 2026-08-06, that was 58.81h -- 53.3% of the whole denominator --
    entirely extrapolated. When the last verdict is `success` this reports hours
    nobody observed as GREEN, which is exactly the claim the metric exists to
    refuse: absence of a record is not a green period.
    """

    WF = "CI (GitHub-managed portable)"

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.parent = Path(self.tmp.name)
        (self.parent / "ignored" / "ci-hub").mkdir(parents=True)
        self.addCleanup(self.tmp.cleanup)

    def _write_gha(self, rows):
        path = self.parent / "ignored" / "ci-hub" / "gha-runs.csv"
        with open(path, "w", newline="") as fh:
            w = csv.DictWriter(fh, fieldnames=ingest.GHA_COLUMNS,
                               extrasaction="ignore")
            w.writeheader()
            for r in rows:
                w.writerow({c: r.get(c, "") for c in ingest.GHA_COLUMNS})

    def _row(self, sha, created, conclusion):
        return {
            "repo": "rrnewton/hermit", "run_id": sha, "run_attempt": "1",
            "workflow_name": self.WF, "event": "push", "head_branch": "main",
            "head_sha": sha, "status": "completed", "conclusion": conclusion,
            "created_at": created, "run_started_at": created,
            "updated_at": created,
        }

    def _timeline(self):
        return query.state_timeline(str(self.parent), "rrnewton/hermit", None,
                                    [self.WF])

    def test_unobserved_tail_is_gap_not_the_last_known_green(self):
        # Last observation is a SUCCESS, deliberately: this is the dangerous
        # case. Everything after it must be gap, never green.
        self._write_gha([
            self._row("a" * 40, "2020-01-01T00:00:00Z", "failure"),
            self._row("b" * 40, "2020-01-01T01:00:00Z", "success"),
        ])
        tl = self._timeline()
        tail = tl["intervals"][-1]
        self.assertEqual(tail["state"], "gap")
        self.assertIn("no observation after", tail["reason"] or "")
        self.assertEqual(tl["observed_through_utc"], "2020-01-01T01:00:00Z")
        self.assertGreater(tl["store_stale_hours"], 0)

        res = query.green_time(str(self.parent), "rrnewton/hermit", None, [self.WF])
        # Years of unobserved time dwarf the 1h observed, so gap must dominate
        # the WINDOW accounting. The headline uses only conclusive
        # authoritative-run hours; do not use it as a store-coverage proxy.
        self.assertGreater(res["gap_pct"], 99.0)
        self.assertLess(res["green_window_pct"], 1.0)

    def test_buckets_stay_mutually_exclusive_and_sum_to_the_denominator(self):
        self._write_gha([
            self._row("c" * 40, "2020-01-01T00:00:00Z", "success"),
            self._row("d" * 40, "2020-01-01T02:00:00Z", "failure"),
        ])
        res = query.green_time(str(self.parent), "rrnewton/hermit", None, [self.WF])
        total = sum(res[k] for k in
                    ("green_hours", "red_hours", "no_result_hours", "gap_hours"))
        self.assertAlmostEqual(total, res["total_hours"], places=1)
        # The headline denominator is conclusive authoritative-run time only.
        self.assertAlmostEqual(
            res["green_hours"] + res["red_hours"],
            res["authoritative_run_hours"], places=1)
        self.assertAlmostEqual(res["green_pct"] + res["red_pct"], 100.0,
                               places=0)
        # Separately, named *_window_pct fields partition the complete window.
        window_pct = sum(res[k] for k in
                         ("green_window_pct", "red_window_pct",
                          "no_result_window_pct", "gap_window_pct"))
        self.assertAlmostEqual(window_pct, 100.0, places=0)
        # The denominator and window must be reportable -- a percentage without
        # them is the unqualified-number defect this metric is meant to avoid.
        for field in ("authoritative_run_hours", "window_hours",
                      "denominator_definition", "window_start",
                      "window_end_utc", "observed_through_utc", "samples"):
            self.assertIsNotNone(res.get(field), f"{field} must be reported")
