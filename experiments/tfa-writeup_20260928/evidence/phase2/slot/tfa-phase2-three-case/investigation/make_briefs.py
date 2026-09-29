#!/usr/local/fbcode/platform010/bin/python3.12
"""Build each investigator brief: the exact fixed prompt, then a controller supplement."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

E3 = Path(
    "/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/"
    "tfa-phase2-three-case"
)
INV = E3 / "investigation"
CHECKOUTS = Path("/home/newton/work/tfa/phase2-cases/investigators")
SCHEMA = Path("/home/newton/work/tfa/fbsource/fbcode/testinfra/tfa/report.schema.v1.json")


def sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


SUPPLEMENT = """
===== END OF EXACT FIXED PROMPT =====

# Controller supplement for this diagnostic run

This supplement is from the controller (the operator running TFA phase two). It
overrides the fixed prompt above only where a point below says so. Everything
else in the fixed prompt stands.

## S1. Status of this run
- This is a diagnostic proof-of-concept investigation for owner study. The
  production gate, signed Codex 0.156.0, is BLOCKED (36/40 containment tests).
  Nothing you produce qualifies a patch or makes that gate green.
- You are the only investigator for case `{case}`. Two other Claude Opus 5.5
  sessions investigate the other cases in their own checkouts. Do not read,
  write, or run anything for them.

## S2. Witness commands (overrides `controller_pass_command`, `controller_fail_command`, `working_directory`)
- Do not use `controller_*_command` from the fixed facts. It routes through the
  packaged-Hermit `buck run ... run-witness` path, which is blocked here.
- Do not use the fixed `working_directory` or any other path under `/tmp`. The
  owner has forbidden all execution under `/tmp`.
- The measured non-`/tmp` PASS and FAIL reproducers for this case are in the
  unposted initial comment below.
  - The PASS witness is sched-seed {pass_seed} and the FAIL witness is
    sched-seed {fail_seed}. Each was confirmed on 2 decided attempts, with 0
    censored.
  - Relocation summary: `{summary}` (sha256 `{summary_sha}`).
- Do not re-run those commands verbatim. Their report and verify-log paths are
  preserved evidence and must not be overwritten.
- To reproduce, use the controller replay runner. It replays the measured
  witness argv. It rewrites only the attempt paths (the cwd same-path mount,
  `--tmp`, `--sh-report` and `--verify-log-dir`) into a new directory you name,
  creates a fresh owned empty `controller-cwd` there, verifies the pinned
  safehermit, Hermit and ELF hashes, classifies the run, and writes
  `receipt.json`:

      {witness} --case {case} --template pass --out {agent_out}/witness/<new-label>
      {witness} --case {case} --template fail --out {agent_out}/witness/<new-label>

  Use a new label for every attempt; the runner refuses an existing directory.
  Optional flags:
  - `--seed N` replays the same argv with a different `--sched-seed`. Do not
    use the fresh stress seeds `{fresh}`. They are reserved for the
    controller's post-fix stress.
  - `--note TEXT` records your intent in the receipt.
- A replay's path differs from the measured command, so each replay is a new
  measurement. Report what it actually shows.
  - `CENSORED` is a bound firing. It is neither PASS nor FAIL.
  - If a replay does not reproduce a witness, record that honestly. Do not
    re-run until the result looks right.
- Runs take up to about 4 minutes, with a 240-second outer bound. Never run
  more than one Hermit replay at a time.
- The runner script is `{runner}`, and its helpers are
  `{helpers}`. Read them but do not modify them.

## S3. Your checkout (overrides "read-only" in contract items 4 and 7)
- Your own writable checkout is `{checkout}`. It is an Eden clone at
  `7f239d5b37dfbecf32fed144468da461f998add5`, and `sl status` was empty at
  start.
- The owner authorizes you to draft the fix by editing source files in this
  checkout only.
- Everything else in items 4 and 7 still applies:
  - Do not commit, amend, rebase, shelve, goto, revert, purge, or run any other
    source-control mutation.
  - Do not touch any other checkout, especially
    `/home/newton/work/tfa/fbsource` and `/home/newton/work/tfa/phase2-cases/<case>`.
  - Do not quarantine tests, weaken assertions, add sleeps or retries, or
    change the expected verdict.
- When your fix is final, write it to `{agent_out}/patch.diff` with
  `sl diff --reason "export TFA {case} draft fix - sl help diff" > {agent_out}/patch.diff`
  run in your checkout. It must match your checkout's changes exactly. The
  controller will apply `patch.diff` in its own separate checkout.
- Do not invoke Buck or Arc, as the fixed prompt says. The controller builds,
  then stress-tests the fixed ELF against both witness schedules and the fresh
  stress seeds after you finish.

## S4. Deterministic inputs
- The difference hints and the unposted initial comment are reproduced
  verbatim below. Their files are:
  - `{hints}` (sha256 `{hints_sha}`)
  - `{comment}` (sha256 `{comment_sha}`)
- Historical phase-one evidence, read-only: `{authoritative}`.
- Report schema: `{schema}`.

## S5. Outputs
- Write these into `{agent_out}`:
  - `hypotheses.json`: 2 to 5 ranked hypotheses, each with supporting and
    contradicting evidence and a next experiment, and exactly one top
    hypothesis.
  - `patch.diff`
  - `validation.json`
  Also write a short human-readable `notes.md` there, covering the root cause,
  the trigger versus the originating invariant, and what you ran.
- Do not write `report.json`. Return the report as your final message, as JSON
  that satisfies the schema, with `qualification="candidate-only"`.
- You cannot build, so you cannot measure the fixed test.
  - Set `original_fail_now_passes` and `original_pass_still_passes` to `false`,
    meaning "not demonstrated by the investigator".
  - For each validation you could not run, give its proposed argv, status
    `FAIL`, passed 0, total 0, and an artifact string beginning `NOT RUN:` that
    says why.
  - The controller's stress results are authoritative.

## S6. Hard limits
- Set `TMPDIR={tmpdir}` on every shell command. Never write under `/tmp`.
- Do not use grep, find or rg over fbsource; they time out on this monorepo.
  Instead:
  - read files directly by path;
  - use the `search_files` MCP tool;
  - use `sl log <file>` or `sl blame`, each with `--reason`.
- Do not spawn subagents, agentctl sessions, Codex, or any other model or
  harness.
- Publish nothing:
  - no comments, diffs, `jf`, `arc diff`, or Phabricator/GitHub writes;
  - no reviewers;
  - no network posting.
- Do not modify anything under `{e3}` except inside `{agent_out}`.
- If EdenFS shows an infosec or protected-folder warning, answer no. Stop and
  say so in your final report.
- Do not kill processes you did not start.

## S7. Unposted initial comment (verbatim)

{comment_text}

## S8. Deterministic difference hints (verbatim JSON)

```json
{hints_text}```
"""


def main() -> None:
    registry: dict[str, object] = {"schema": "tfa.investigation-registry.v1", "cases": {}}
    for case in ("thrift", "asyncssl", "httpcoro"):
        case_dir = INV / case
        agent_out = case_dir / "agent-output"
        tmpdir = case_dir / "tmpdir"
        tmpdir.mkdir(exist_ok=True)
        fixed = case_dir / "fixed-prompt.txt"
        summary = E3 / "relocated-v2" / case / "summary.json"
        witness = json.loads(summary.read_text())["witness"]
        stage = E3 / "stages" / case / "stage1-2-relocated"
        hints = stage / "difference-hints.json"
        comment = stage / "initial-comment.md"
        facts = json.loads(fixed.read_text().split("do not reinterpret):\n", 1)[1])
        values = {
            "case": case,
            "pass_seed": witness["pass"]["seed"],
            "fail_seed": witness["fail"]["seed"],
            "summary": summary,
            "summary_sha": sha(summary),
            "witness": E3 / "witness" / "witness.sh",
            "runner": E3 / "witness" / "run_witness.py",
            "helpers": E3 / "relocated" / "run_relocated.py",
            "agent_out": agent_out,
            "fresh": facts["fresh_stress_seeds"],
            "checkout": CHECKOUTS / case,
            "hints": hints,
            "hints_sha": sha(hints),
            "comment": comment,
            "comment_sha": sha(comment),
            "authoritative": E3 / "authoritative" / case,
            "schema": SCHEMA,
            "tmpdir": tmpdir,
            "e3": E3,
            "comment_text": comment.read_text(),
            "hints_text": hints.read_text(),
        }
        brief = case_dir / "brief.md"
        brief.write_text(fixed.read_text() + SUPPLEMENT.format(**values))
        registry["cases"][case] = {
            "agent_name": f"tfa-inv-{case}",
            "harness": "claude",
            "model": "claude-opus-5-5",
            "checkout": str(CHECKOUTS / case),
            "agent_output": str(agent_out),
            "tmpdir": str(tmpdir),
            "fixed_prompt": {"path": str(fixed), "sha256": sha(fixed)},
            "brief": {"path": str(brief), "sha256": sha(brief)},
            "fixed_prompt_is_prefix_of_brief": brief.read_bytes().startswith(
                fixed.read_bytes()
            ),
            "relocation_summary": {"path": str(summary), "sha256": sha(summary)},
            "hints": {"path": str(hints), "sha256": sha(hints)},
            "initial_comment": {"path": str(comment), "sha256": sha(comment)},
            "witness_runner": {
                "path": str(values["runner"]),
                "sha256": sha(Path(values["runner"])),
            },
            "witness_wrapper": {
                "path": str(values["witness"]),
                "sha256": sha(Path(values["witness"])),
            },
        }
    (INV / "registry.json").write_text(json.dumps(registry, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
