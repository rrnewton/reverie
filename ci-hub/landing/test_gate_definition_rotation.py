#!/usr/bin/env python3
"""Mutation brackets for the sanctioned gate-rotation public command."""

from __future__ import annotations

from copy import deepcopy
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile

SOURCE_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))
import gate_definition_rotation as G  # noqa: E402

BASE, HEAD, OLD, NEW = "a" * 40, "b" * 40, "c" * 40, "d" * 40


def run(*args: str, cwd: Path | None = None, env: dict[str, str] | None = None,
        check: bool = True) -> subprocess.CompletedProcess[str]:
    proc = subprocess.run(args, cwd=cwd, env=env, text=True, capture_output=True)
    if check and proc.returncode != 0:
        raise AssertionError(f"{' '.join(args)} failed: {proc.stdout}\n{proc.stderr}")
    return proc


def good() -> dict:
    return {
        "candidate": {
            "repo": G.REPO, "pr": 2071,
            "base": BASE, "head": HEAD, "old_blob": OLD, "new_blob": NEW,
            "allowed_paths": [G.WORKFLOW_PATH],
        },
        "observed": {
            "repo": G.REPO, "pr": 2071, "remote_main": BASE, "pr_base": BASE,
            "pr_head": HEAD, "expected_head": HEAD, "head_descends_base": True,
            "old_blob": OLD, "new_blob": NEW, "changed_paths": [G.WORKFLOW_PATH],
            "approvals": {"claude": None, "codex": HEAD},
            "malformed_review_binding": False, "request_changes_at_head": False,
            "local_ledger_qualified": True, "definition_only_self_refusal": True,
        },
    }


def bracket_eight_refusal_classes() -> None:
    assert G.assess(good()).eligible
    cases = [
        ({"candidate": {}}, "repository or PR"),
        ({"observed": {**good()["observed"], "pr": 9}}, "repository or PR"),
        ({"observed": {**good()["observed"], "remote_main": "e" * 40}}, "main moved"),
        ({"observed": {**good()["observed"], "pr_head": "e" * 40}}, "exact head"),
        ({"observed": {**good()["observed"], "new_blob": "e" * 40}}, "workflow blob"),
        ({"observed": {**good()["observed"], "changed_paths": [G.WORKFLOW_PATH, "x"]}}, "path set"),
        ({"observed": {**good()["observed"], "approvals": {"claude": None, "codex": None}}},
         "required exact-head"),
        ({"observed": {**good()["observed"], "local_ledger_qualified": False}}, "local ledger"),
        ({"observed": {**good()["observed"], "definition_only_self_refusal": False}}, "solely"),
    ]
    for patch, text in cases:
        case = good()
        case.update(patch)
        verdict = G.assess(case)
        assert not verdict.eligible and any(text in reason for reason in verdict.refusals)


class Harness:
    """Real consumer+lander code with inert local GitHub and repository seams."""

    def __init__(self, root: Path) -> None:
        self.root = root
        self.state_path = root / "state.json"
        self.real_git = shutil.which("git") or "/usr/bin/git"
        for rel in [
            "ci-hub/landing/gate_definition_rotation.py",
            "ci-hub/landing/land-pr.sh",
            "ci-hub/landing/merge-gate-status.jq",
            "ci-hub/landing/cherry_pick_context.py",
            "ci-hub/health/approval_binding.py",
            "ci-hub/check_outcome.py",
        ]:
            dest = root / rel
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(SOURCE_ROOT / rel, dest)
        (root / "ci-hub/remediation").mkdir(parents=True, exist_ok=True)
        (root / "ci-hub/bin").mkdir(parents=True, exist_ok=True)
        (root / "ignored").mkdir()
        self._repos()
        self._write_shims()
        self.reset()

    def _git(self, *args: str, cwd: Path | None = None) -> str:
        return run(self.real_git, *args, cwd=cwd).stdout.strip()

    def _repos(self) -> None:
        remote, seed = self.root / "remote.git", self.root / "seed"
        self._git("init", "--bare", str(remote))
        self._git("init", str(seed))
        self._git("config", "user.email", "fixture@example.invalid", cwd=seed)
        self._git("config", "user.name", "Gate Fixture", cwd=seed)
        workflow = seed / G.WORKFLOW_PATH
        workflow.parent.mkdir(parents=True)
        workflow.write_text("name: old-gate\n")
        self._git("add", G.WORKFLOW_PATH, cwd=seed)
        self._git("commit", "-m", "base", cwd=seed)
        self._git("branch", "-M", "main", cwd=seed)
        self._git("remote", "add", "origin", str(remote), cwd=seed)
        self._git("push", "origin", "main", cwd=seed)
        self.base = self._git("rev-parse", "HEAD", cwd=seed)
        self.old_blob = self._git("rev-parse", f"HEAD:{G.WORKFLOW_PATH}", cwd=seed)

        wt = self.root / "worktrees/lander/hermit"
        wt.parent.mkdir(parents=True)
        self._git("clone", "--branch", "main", str(remote), str(wt))
        self._git("config", "user.email", "fixture@example.invalid", cwd=wt)
        self._git("config", "user.name", "Gate Fixture", cwd=wt)
        self._git("checkout", "-b", "fixture/rotation", cwd=wt)
        (wt / G.WORKFLOW_PATH).write_text("name: independently-reviewed-new-gate\n")
        self._git("add", G.WORKFLOW_PATH, cwd=wt)
        self._git("commit", "-m", "candidate", cwd=wt)
        self.head = self._git("rev-parse", "HEAD", cwd=wt)
        self.new_blob = self._git("rev-parse", f"HEAD:{G.WORKFLOW_PATH}", cwd=wt)
        self._git("push", "origin", "HEAD:fixture/rotation", cwd=wt)

        self._git("checkout", "--orphan", "other", cwd=seed)
        self._git("rm", "-rf", ".", cwd=seed)
        workflow.parent.mkdir(parents=True, exist_ok=True)
        workflow.write_text("name: unrelated-base\n")
        self._git("add", G.WORKFLOW_PATH, cwd=seed)
        self._git("commit", "-m", "other", cwd=seed)
        self.other = self._git("rev-parse", "HEAD", cwd=seed)
        self._git("push", "origin", "HEAD:other", cwd=seed)
        self._git("fetch", "origin", "other", cwd=wt)

        self._git("clone", "--branch", "main", str(remote), str(self.root / "hermit"))
        rev_remote, rev_seed = self.root / "reverie.git", self.root / "reverie-seed"
        self._git("init", "--bare", str(rev_remote))
        self._git("init", str(rev_seed))
        self._git("config", "user.email", "fixture@example.invalid", cwd=rev_seed)
        self._git("config", "user.name", "Gate Fixture", cwd=rev_seed)
        (rev_seed / "README").write_text("fixture\n")
        self._git("add", "README", cwd=rev_seed)
        self._git("commit", "-m", "base", cwd=rev_seed)
        self._git("branch", "-M", "main", cwd=rev_seed)
        self._git("remote", "add", "origin", str(rev_remote), cwd=rev_seed)
        self._git("push", "origin", "main", cwd=rev_seed)
        self._git("clone", "--branch", "main", str(rev_remote), str(self.root / "reverie"))

    def fresh_state(self) -> dict:
        return {
            "root": str(self.root), "real_git": self.real_git, "pr": 2071,
            "branch": "fixture/rotation", "base": self.base, "head": self.head,
            "other": self.other, "old_blob": self.old_blob, "new_blob": self.new_blob,
            "allowed_paths": [G.WORKFLOW_PATH], "approvals": True,
            "rejection": False, "ledger": True, "self_refusal": True,
            "merge_fail": False, "merged": False, "variable": "",
        }

    def reset(self, **updates: object) -> None:
        self._git("--git-dir", str(self.root / "remote.git"), "update-ref", "refs/heads/main", self.base)
        state = self.fresh_state()
        state.update(updates)
        self.state_path.write_text(json.dumps(state))
        for path in (self.root / ".landing-lock", self.root / ".landing-lock.owner"):
            path.unlink(missing_ok=True)

    def state(self) -> dict:
        return json.loads(self.state_path.read_text())

    def main_sha(self) -> str:
        return self._git("--git-dir", str(self.root / "remote.git"), "rev-parse", "refs/heads/main")

    def env(self) -> dict[str, str]:
        return os.environ | {
            "PATH": f"{self.root / 'bin'}:{os.environ.get('PATH', '')}",
            "GATE_FIXTURE_STATE": str(self.state_path),
            "CI_HUB_POST_LAND_AUDIT": "0",
            "CI_HUB_LANDING_LOG_DIR": str(self.root / "logs"),
            "LANDER_MODEL": "fixture",
        }

    def command(self, *extra: str) -> subprocess.CompletedProcess[str]:
        return run(
            sys.executable, str(self.root / "ci-hub/landing/gate_definition_rotation.py"),
            "--pr", "2071", "--branch", "fixture/rotation",
            "--checkout", str(self.root / "worktrees/lander/hermit"), "--execute",
            *extra, env=self.env(), check=False,
        )

    def _write_shims(self) -> None:
        proxy = self.root / "bin/with-proxy"
        proxy.parent.mkdir()
        proxy.write_text(PROXY_SHIM)
        proxy.chmod(proxy.stat().st_mode | stat.S_IXUSR)
        hub = self.root / "ci-hub/ci-hub"
        hub.write_text(CI_HUB_SHIM)
        hub.chmod(hub.stat().st_mode | stat.S_IXUSR)
        arm = self.root / "ci-hub/remediation/land_and_arm.py"
        arm.write_text(ARM_SHIM)
        arm.chmod(arm.stat().st_mode | stat.S_IXUSR)


PROXY_SHIM = r'''#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
p = pathlib.Path(os.environ["GATE_FIXTURE_STATE"])
s = json.loads(p.read_text()); a = sys.argv[1:]
if a[0] == "git": os.execv(s["real_git"], [s["real_git"], *a[1:]])
if a[0] != "gh": raise SystemExit(97)
a = a[1:]
def save(): p.write_text(json.dumps(s))
def comments():
    out=[]
    if s["approvals"]:
        out.append({"body":"[adversarial-reviewer agent, codex]\nAPPROVED-AT: codex "+s["head"]+
          "\nAPPROVED-AT: claude "+s["head"]})
    if s["rejection"]: out.append({"body":"REQUEST CHANGES AT "+s["head"]})
    return out
if a[0] == "api":
    endpoint=next((x for x in a[1:] if x.startswith("repos/")), "")
    if endpoint.endswith("/pulls/2071"):
        print(json.dumps({"head":{"sha":s["head"]},"base":{"sha":s["base"]},
          "merged_at":"2099-01-01T00:00:00Z" if s["merged"] else None,
          "merge_commit_sha":s["head"] if s["merged"] else None}))
    elif "/issues/2071/comments" in endpoint:
        print(json.dumps([comments()] if "--slurp" in a else comments()))
    elif "/pulls/2071/reviews" in endpoint: print("[[]]")
    elif endpoint.endswith("/check-runs?per_page=100"):
        conclusion="failure" if s["self_refusal"] else "success"
        print(json.dumps({"check_runs":[{"name":"merge-gate-v4","head_sha":s["head"],
          "status":"completed","conclusion":conclusion,"completed_at":"2099-01-01T00:00:00Z",
          "details_url":"https://example.invalid/runs/1/job/42"}]}))
    elif endpoint.endswith("/actions/jobs/42"):
        print(json.dumps({"steps":[{"name":"Require the registered v4 gate definition",
          "conclusion":"failure" if s["self_refusal"] else "success"}]}))
    elif "/actions/workflows/merge-gate.yml/runs" in endpoint or "/actions/runs?head_sha=" in endpoint:
        print('{"workflow_runs":[]}')
    elif "/actions/variables/MERGE_GATE_V4_BLOB" in endpoint and "PATCH" in a:
        value=next(x.split("=",1)[1] for x in a if x.startswith("value=")); s["variable"]=value; save(); print("{}")
    elif endpoint.endswith("/actions/variables/MERGE_GATE_V4_BLOB"):
        print(json.dumps({"value":s["variable"]}))
    elif "/contents/.github/workflows/merge-gate.yml?ref=main" in endpoint:
        out=subprocess.check_output([s["real_git"],"--git-dir",s["root"]+"/remote.git","rev-parse",
          "refs/heads/main:.github/workflows/merge-gate.yml"],text=True).strip()
        print(json.dumps({"sha":out}))
    else: print("null")
    raise SystemExit(0)
if a[:2] == ["pr","view"]:
    field=a[a.index("--json")+1]
    if field=="number,headRefOid,labels,mergeable":
      print(json.dumps({"number":2071,"headRefOid":s["head"],
        "labels":[{"name":"passed-review-codex"}],
        "mergeable":"MERGEABLE"})); raise SystemExit(0)
    if field=="number,headRefOid,body,labels,commits":
      print(json.dumps({"number":2071,"headRefOid":s["head"],"body":"## Summary\nfixture",
        "labels":[{"name":"passed-review-codex"}],
        "commits":[]})); raise SystemExit(0)
    values={"headRefName":s["branch"],"headRefOid":s["head"],"isDraft":"false",
      "labels":"", "mergeCommit":s["head"]}
    print(values[field]); raise SystemExit(0)
if a[:2] == ["pr","ready"]: raise SystemExit(0)
if a[:2] == ["pr","comment"]: raise SystemExit(0)
if a[:2] == ["pr","merge"]:
    if s["merge_fail"]: print("fixture merge refused",file=sys.stderr); raise SystemExit(1)
    assert "--rebase" in a and "--admin" not in a and a[a.index("--match-head-commit")+1] == s["head"]
    subprocess.check_call([s["real_git"],"--git-dir",s["root"]+"/remote.git","update-ref",
      "refs/heads/main",s["head"]]); s["merged"]=True; save(); print("merged"); raise SystemExit(0)
raise SystemExit(96)
'''

CI_HUB_SHIM = r'''#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
root=pathlib.Path(__file__).resolve().parents[1]; p=pathlib.Path(os.environ["GATE_FIXTURE_STATE"])
s=json.loads(p.read_text()); a=sys.argv[1:]
if a[0]=="validate-status":
  if not s["ledger"]: raise SystemExit(2)
  print(json.dumps({"repo":"rrnewton/hermit","sha":s["head"],"verdict":"VALIDATED",
    "qualifying_count":1,"newest_qualifying":{"sha":s["head"],"profile":"full",
    "selection_mode":"full","coverage_satisfied":True,"failures":0,"executed_tests":42,
    "coverage":{"planned_test_nodes":2,"executed_test_nodes":2,"absent_nodes":[],"zero_executed_nodes":[]}}})); raise SystemExit(0)
if a[0]=="inherit-obligations": print("INHERITED REMEDIATION OBLIGATIONS: 0"); raise SystemExit(0)
if a[:2]==["land-lock","run"]:
  agent=a[a.index("--agent")+1]; pr=a[a.index("--pr")+1]; cmd=a[a.index("--")+1:]
  stat=pathlib.Path(f"/proc/{os.getpid()}/stat").read_text(); ticks=stat[stat.rfind(")")+2:].split()[19]
  boot=pathlib.Path("/proc/sys/kernel/random/boot_id").read_text().strip()
  (root/".landing-lock").write_text(f"agent={agent}\npr={pr}\nhost=fixture\n")
  (root/".landing-lock.owner").write_text(f"host=fixture\nboot_id={boot}\npid={os.getpid()}\nstart_ticks={ticks}\n")
  rc=subprocess.run(cmd).returncode
  (root/".landing-lock").unlink(missing_ok=True); (root/".landing-lock.owner").unlink(missing_ok=True)
  raise SystemExit(rc)
raise SystemExit(95)
'''

ARM_SHIM = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
root=pathlib.Path(__file__).resolve().parents[2]; s=json.loads(pathlib.Path(os.environ["GATE_FIXTURE_STATE"]).read_text())
if sys.argv[1]=="prepare": raise SystemExit(0)
if sys.argv[1]=="complete":
  d=root/"ignored/ci-hub/land-intents"; d.mkdir(parents=True,exist_ok=True)
  (d/"rrnewton-hermit-pr2071.json").write_text(json.dumps({"landed_sha":s["head"]})); raise SystemExit(0)
raise SystemExit(2)
'''


def public_mutation_brackets() -> None:
    with tempfile.TemporaryDirectory(prefix="gate-rotation-live-", dir="/tmp") as name:
        h = Harness(Path(name))
        refusal_updates = [
            {"base": h.other}, {"head": h.other}, {"rejection": True},
            {"ledger": False}, {"self_refusal": False},
        ]
        for index, updates in enumerate(refusal_updates, 1):
            h.reset(**updates)
            proc = h.command()
            assert proc.returncode != 0, (index, proc.stdout, proc.stderr)
            assert h.main_sha() == h.base and h.state()["variable"] == "", index

        h.reset()
        forged = h.command("--_inner")
        assert forged.returncode == 2 and "unrecognized arguments: --_inner" in forged.stderr
        assert h.main_sha() == h.base and h.state()["variable"] == ""

        h.reset()
        direct = run(
            str(h.root / "ci-hub/landing/land-pr.sh"), "2071", "fixture/rotation",
            "--no-rebase", "--gate-rotation", "--_inner", "--agent", "gate-definition-rotation",
            env=h.env(), check=False,
        )
        assert direct.returncode == 2 and "outside its recorded land-lock subtree" in direct.stderr
        assert h.main_sha() == h.base and h.state()["variable"] == ""

        h.reset()
        ordinary = run(
            str(h.root / "ci-hub/landing/land-pr.sh"), "2071", "fixture/rotation",
            "--no-rebase", "--foreground", "--agent", "gate-definition-rotation",
            env=h.env(), check=False,
        )
        assert ordinary.returncode != 0
        assert "SELF_AUTHORIZATION_REFUSED" in ordinary.stdout
        assert h.main_sha() == h.base and h.state()["variable"] == ""

        h.reset(merge_fail=True)
        failed = h.command()
        assert failed.returncode != 0 and h.main_sha() == h.base and h.state()["variable"] == ""

        h.reset()
        passed = h.command()
        assert passed.returncode == 0, (passed.stdout, passed.stderr)
        events = [line.split()[-1] for line in passed.stdout.splitlines() if "GATE-ROTATION-EVENT " in line]
        assert events == [
            "collect_outer", "verify_outer", "enter_land_lock", "verify_final",
            "normal_non_admin_merge", "refresh_main", "prove_ancestry",
            "read_main_gate_blob", "patch_registration", "verify_registration",
        ], events
        assert h.main_sha() == h.head and h.state()["variable"] == h.new_blob


def entry_point_enumeration() -> None:
    landing = SOURCE_ROOT / "ci-hub/landing"
    assert not (landing / "land-direct-main.sh").exists()
    consumer = (landing / "gate_definition_rotation.py").read_text()
    lander = (landing / "land-pr.sh").read_text()
    assert 'add_argument("--_inner"' not in consumer
    assert consumer.count('"PATCH"') == 1
    assert consumer.count('runtime.register_after_ancestry(snapshot)') == 1  # recovery command
    assert '--register-only' in lander and 'POST-LAND REGISTRATION PENDING' in lander
    assert lander.count('gh pr merge "$PR"') == 1
    assert '--match-head-commit "$HEAD"' in lander and "gh pr merge" in lander
    assert "gh pr merge" not in "\n".join(
        line for line in lander.splitlines() if "--admin" in line and not line.lstrip().startswith("#")
    )
    assert "gate-definition change requires the sanctioned --gate-rotation checks" in lander
    assert "gate-rotation authority" not in lander
    assert "GATE-DEFINITION-ROTATION-AUTHORIZED" not in consumer
    assert "SELF_AUTHORIZATION_REFUSED" in lander


def main() -> int:
    bracket_eight_refusal_classes()
    public_mutation_brackets()
    entry_point_enumeration()
    print("gate-definition-rotation: safety_classes=8/8 mutation_refusals=5/5 "
          "public_e2e=1/1 exact_events=10/10 failed_land_no_preregister=1/1 "
          "direct_inner_refused=2/2 entry_points=3/3")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
