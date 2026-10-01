# ccbench harness textpack: how to reproduce (sub-crashcortex, 2026-09-30)

Node: devvm64412.cln0 (OnDemand), fbsource eeb1dcb38e9f. Everything runs as $USER without root.

1. Kernel mirror:
   `git clone --mirror https://git.internal.tfbnw.net/repos/git/rw/kernel/linux.git /data/users/$USER/linux-full.git`
   then `git --git-dir=... commit-graph write --reachable`.
2. QEMU (user space):
   `buck2 build fbcode//antlir/vm/runtime:qemu-system-x86_64 fbcode//antlir/vm/runtime:roms`
   Copy the binary as `tools/bin/qemu-system-x86_64.real`, the ROMs into `tools/share/qemu/`, and install the wrapper
   `tools/bin/qemu-system-x86_64` (in the pack). The wrapper forces `-nic none` unless slirp is `restrict=on`.
3. Skill pin:
   - baseline skill = `~/.claude/agent-market/skills/kernel-investigation` (SKILL.md sha256 2566402676642ccc…);
   - copy it to `harness/plugins/ccbench-baseline/skills/kernel-investigation`, then
     `sed -i "s#~/.claude/skills/kernel-investigation#<that dir>#g" SKILL.md subagents/*.md` (SKILL.md → sha256 30178b5b…);
   - variant skills = `fbcode/users/dcostantino/kernel_investigator/prompts/skills/{kverify,kreview}.md` copied to
     `harness/skills-variant/<name>/SKILL.md`.
4. Build one case:
   - `ccbench_bundle.py --mid M --host H --time T --out cases/M` (Hive; needs `meta presto.query`);
   - `ccbench_tree.sh linux-full.git cases/M <crash_sha12> <cutoff>`;
   - write `cases/M/tree.json` = `{"crash_full": ..., "cutoff": ...}`. Use `cutoff_derive` from the CSV for derive mode.
5. Run: `ccbench_run.py --case cases/M --arm arms/baseline.json --run-id X --wall 3600 --token-cap 40000000 [--allowlist 1.1]`.
   If the supervisor dies, use `ccbench_finalize.py`.
6. Grade:
   - `grade_prep.py --run-id X --case cases/M --sealed sealed/M.json --out grading/<blind>.json --blind-id <blind>`;
   - copy small reproducer artifacts to `grading/<blind>-artifacts/`;
   - run the `ccbench-grade-pilot` workflow script (v1.1: pass `mode`, `case_repo` and `cutoff` per run).
7. Sample pipeline:
   - `fetch_posts.py`, `fetch_comments.py`, `parse_posts.py`;
   - workflow `ccbench-extract-posts`;
   - `verify_fixes.py`;
   - workflows `ccbench-ground-truth` and `ccbench-crashfirst-mine`;
   - `collect_journals.py`;
   - `select_cases.py pool`;
   - workflow `ccbench-families`;
   - `select_cases.py select`;
   - `postprocess_csv.py`.
