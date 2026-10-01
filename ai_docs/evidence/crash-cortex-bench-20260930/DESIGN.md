# Crash Cortex Debug Benchmark: Phase 1 design, 100-case sample, 3-case pilot

Owner: newton. Coordinator: coord2 (dev-hermit). Author: sub-crashcortex (OnDemand devvm64412.cln0, fbsource `eeb1dcb38e9f`).
Date: 2026-09-30. Status: see §0.

Companion files:
- `cases_100.csv` is the answer key: MID, pin, kernel, root cause, fix, family, fairness. It is sealed and must never be shown to an agent under test.
- `cases_100_inputs.csv` holds the agent-visible columns only.
- `pilot/` contains the per-run manifests and grades.

Everything below is reproducible from the scripts in `harness/`, pasted alongside this doc.

---------------------------------------------------------------------------------------------------

## 0. Status and headline numbers

**Phase 1 is done**: design, 100-case sample, and a 3-case pilot in two modes. Nothing was run at scale. No diffs,
no reviewer pings, and no changes to dcostantino's package. GitHub is denied for this agent identity, so everything is
in private Phabricator pastes (IDs in the final report). coord2 can commit them to dev-hermit.

| What | Result (provenance) |
|---|---|
| Sample | **100 cases**: 97 tier A + 3 tier B known outcomes, i.e. a landed fix on a release/upstream branch that is absent from the pinned crash kernel. Every fix and pin was git- and Hive-verified by script (`select_cases.py`). 32 defect families, ≤5 per family. **54 timing-dependent, 30 strict races.** Sources: 43 CrashCortex history, 57 mined from tree + Hive. All 100 pins bundle-ready in Hive. `cases_100.csv` (answer key) + `cases_100_inputs.csv` + 26 spares. |
| Bundle | `ccbench-bundle/v1`: the exact pre-agent `/tmp/kinv-logs/<MID>/` file set of kernel_investigator, rebuilt from the Hive mirrors, sha256-manifested; ground truth sealed separately. Kernel source = per-case **time-cut** repo (find mode: crash time; derive mode: 1 day before the fix was first authored). |
| Harness | `ccbench_run.py`: production prompt text + fixed offline section + one capability section per arm; `claude --safe-mode --strict-mcp-config --permission-mode dontAsk` + offline allowlist; live 60 min / 40M-token caps; manifest with CLI-authoritative tokens/$ and a leak audit. The OS-level netns layer still needs an approved container runner (§4.2). |
| Grading | deterministic facts + 2 blind LLM judges (Opus, Sonnet), third judge on disagreement (exercised once), human SME on a 10% sample. |
| Pilot, find mode | All 3 cases scored R1 = 3/3 and R2 = 3/3 (the fix was in the tree and was found). Mean **40.0 min, 16.52M tokens, $15.26**. Score 0.87. |
| Pilot, derive mode (primary) | R1 mean 2.0, R2 mean 1.33. One case repeated the original bot's mitigation-instead-of-root-cause error. Mean **47.7 min, 18.89M tokens, $17.38**. Score **0.50**, so the benchmark discriminates. |
| Reproducer | 6/6 attempted, 4/6 ran a guest workload, 0/6 genuinely reproduced (1 via self-injected faults). 2/6 A/B claims were unsupported and 1 was wrong. This is the weakest capability and the best A/B target. |
| Cost, 100 × (baseline + 1 variant) | derive mode ≈ **$3.6k expected** (agents $3.48k + judges), ≈ $4.6k wall-cap bound, ≈ $7.5k hard ceiling; ×2 with repeats; ≈ 13 h on 4 OnDemand nodes per repeat (§7). |
| Key findings | (1) kernel_investigator's `--allowedTools` restricted mode does not restrict under the Meta wrapper. (2) The auto-installed `meta` plugin can read the WP answer posts, so any past-incident eval must use `--safe-mode`. (3) Scuba `kernel_logview` keeps about 67 days; the Hive mirror keeps ≥12 months. (4) Derive mode is needed to avoid a find-the-fix ceiling. (5) Pretraining contamination risk for pre-cutoff upstream fixes. |


---------------------------------------------------------------------------------------------------

## 1. What we are building and why

The goal is to measure a kernel-debugging agent's *debug capability in isolation*. Given only the frozen evidence a
human would have had at crash time, we ask:

1. Does it find the originating defect?
2. Does it propose the landed (or an equivalent) fix?
3. Does it build and run a reproducer and a real A/B?
4. What does all of that cost?

We then run A/B experiments where two arms differ in exactly one capability (e.g. Hermit, drgn with symbols,
`/kverify` and `/kreview` installed properly). Arms are scored blind against sealed ground truth.

Non-goals for phase 1: large runs (we pilot 3 cases), production changes to `fbcode//users/dcostantino/kernel_investigator`
(the harness wraps it from outside), and new diffs or reviewer pings.

## 2. The 100-case sample

### 2.1 Sources

| Source | What we used | Provenance |
|---|---|---|
| Kernel AI Investigations (WP group 4508446156040829) | All 277 CrashCortex posts (2026-03-06 .. 2026-09-24) plus all comments. That is 272 reports, 220 distinct MIDs. | `meta workplace.post list --group-id 4508446156040829` (paged), `meta workplace.comment list` |
| Crash data | Hive mirror `infrastructure.scuba_kernel_logview` (full rows incl. dmesg/all_cpus/kdump_dmesg; retained from 2025-10-06). The Scuba table itself only goes back to 2026-07-25. 216/220 MIDs are still present in Hive. | `meta presto.query execute --namespace infrastructure` |
| Kernel tree | Full mirror of `https://git.internal.tfbnw.net/repos/git/rw/kernel/linux.git` (6.7 GB, 4 711 branches, 7 769 tags, cloned 2026-09-30). | `git clone --mirror`; commit-graph written |
| Tree-mined cases | Landed human fixes (2025-10-01..) whose changelog carries a crash signature, mapped to Hive MIDs with the same frames on kernels lacking the fix. | ground-truth workflow, "Mine" phase |

### 2.2 Ground-truth tiers (known outcome = tier A or B only)

- **A: independent.** A landed fix on a release branch (`v6.9/6.11/6.13/6.16/6.19/7.1-fb`) or an upstream mirror
  (`linus-upstream`, `net-next-upstream`) meets all of the following:
  - it is human-authored (not a CrashCortex-pipeline patch);
  - its changelog describes this crash's mechanism or signature (read and quoted);
  - it is not an ancestor of the crash-kernel commit (`git merge-base --is-ancestor`).

  A case also qualifies as tier A when a human expert comment names the fix or confirms the root cause.
- **B: known but less independent.** Either the landed fix is a CrashCortex-derived patch that was reviewed and landed,
  or a human acknowledged the diagnosis and a landed fix matches it.
- **C: bot claim only.** Not a known outcome; not sampled.
- **X: excluded.** Reasons: hardware/firmware fault; fix only in an out-of-tree vendor or BPF-scheduler repo;
  claimed fix already in the crash kernel; answer visible in the raw input; no crash data.

Every A/B verdict was produced by an adjudicator agent that ran the git checks itself. An independent skeptic agent,
instructed to refute when uncertain, then re-checked it. Only skeptic-upheld cases, or cases with a corrected tier
still in A/B, are sampled.

### 2.3 Stratification and fairness rules

- Defect families are canonical clusters of the adjudicated family labels. There are at most 5 cases per family.
- At least 20 cases must be timing-, race-, contention- or feedback-loop-dependent.
- Exclude cases whose raw input reveals the answer (e.g. an MCE/GHES banner, a log line naming the fix).
  Exclude cases whose diagnosis needs out-of-tree source that is not in the bundle.
- **Two cutoff modes per case.** Both columns are in the CSV. The primary mode is derive.
  - **derive** (`cutoff_derive`): the kernel tree is cut one day before the earliest authoring of *any* variant
    (same subject, any exposed branch) of the ground-truth fix. The agent must work out the fix itself. This is the
    "debug capability in isolation" measurement.
  - **find** (`cutoff` = crash time): the tree holds exactly what a human could have searched at crash time. For 77/100
    cases the fix already existed then, so this measures CrashCortex Phase 5 (fix search, backport identification).

  The evidence bundle is identical in both modes; only the time-cut repo differs (§3.3). `fix_available_at_cutoff`
  records whether the fix existed at crash time.
- Each case also records `bot_was_right`, i.e. whether the original CrashCortex post got the root cause or fix right.
  Some cases are ones where CrashCortex was wrong and a human corrected it, e.g. MID 3c212664, where Rik van Riel named
  `54262cda76de`. These are the most valuable A/B discriminators.

### 2.4 Result: the 100 cases (`cases_100.csv`)

Pipeline, with the provenance of each number:

1. Extraction: 272 posts, 220 MIDs. 28 agents, schema-validated. Workflow `wf_1f0fff84-069`.
2. Deterministic prefilter: drop 55.
   - 40 were out-of-tree vendor driver cases (NVIDIA/AMD/BDE) with no in-tree fix;
   - 11 were hardware/firmware;
   - 4 had no Hive crash data.
3. The remaining 165 MIDs were adjudicated, one agent each, running git on the full mirror. Result: A=66, B=7, C=79, X=13.
4. Skeptic pass on all 73 A/B cases: 49 upheld; 14 A→B; 6 B→C; 4 A→A with a corrected claim.
   After the skeptics: **A=52, B=15, C=85, X=13.** Workflow `wf_35ea35f5-588`.
5. Mining outside the CrashCortex history:
   - fix-first: 8 subsystem miners;
   - crash-first: 8 miners over the top 563 fleet MIDs (≥5 hosts, 2025-10-06..2026-09-30) that CrashCortex never investigated.

   82 cases were mined and 81 were upheld by skeptics. Workflows `wf_35ea35f5-588` and `wf_9c71e80a-e1e`.
6. Deterministic re-verification of every candidate (`select_cases.py pool`):
   - the pin row exists in Hive;
   - each GT fix exists and is contained in a release or upstream branch;
   - each GT fix is **not** an ancestor of the pinned crash-kernel commit.

   Result: 138 candidates, 135 fix-verified, **126 eligible** (answer not visible, no out-of-tree source needed).
7. Families: two independent clusterings (mechanism-first and code-area-first) and a reconciler produced **33 canonical
   families** over the 126. Workflow `wf_7b008c18-9db`.
8. Stratified greedy selection:
   - order: tier A → CrashCortex-wrong/partial → timing → CrashCortex source;
   - cap of 5 per family;
   - then a Hive check that every pin row is bundle-ready.

   Operator-authored fixes (dcostantino, 3 cases) were demoted to tier B before selection.

**Composition:**
- Tiers: 97 A + 3 B.
- 32 families, max 5 per family.
- Sources: 43 from CrashCortex history, 40 fix-first mined, 17 crash-first mined.
- Timing: **54 timing-dependent**. Of these, **30 are strict races** (`race_subset`: data race, UAF from concurrent paths,
  lock-order deadlock, memory ordering). The 30 are the intended Hermit-arm subset.
- Kernels: 6.16 (39), 6.13 (35), 6.9 (9), 7.1 (6), 6.11 (4), 6.12 (3), 6.19 (2), 6.18 (2).
- Pins: crash times 2025-10-15 .. 2026-09-30; median 145 affected hosts.
- **100/100 pin rows are bundle-ready in Hive.** For one (CCB-004) the backtrace column is near-empty but the trace is in dmesg.
- Fix availability: 77 cases have the fix in-tree at crash time (find mode). 23 have it only later. The median derive
  cutoff is 64 days before the crash.
- `bot_was_right` (CrashCortex cases): 29 yes, 13 partial, 1 no. Only 5 cases carry a human comment. Selection bias: known
  outcomes are easier to establish where the bot named the fix. The 57 tree-mined cases, which CrashCortex never saw,
  offset this.
- Mechanisms: UAF/lifetime 25, data race 17, lock-hold/starvation 14, invariant/accounting 14, NULL/missing check 12,
  other 7, livelock/feedback 4, allocation-failure 4, lock-order 2, refcount 1.
- The 3 pilot cases are CCB-012 (1e340e89), CCB-011 (3c212664) and CCB-020 (102ffa7b).
- `cases_spares.csv` holds 26 more eligible cases (13 A, 13 B) for substitutions.

**CSV columns** (answer key, sealed):
- identity: `case_id`, `mid`, `source`;
- family: `family_id`, `family_name`, `subsystem`, `mechanism_class`, `mechanism_detail`, `timing_dependent`, `race_subset`;
- crash and pin: `category_key`, `kernel_full`, `crash_commit`, `rep_host`, `rep_time`, `rep_time_utc`, `cutoff` (find),
  `cutoff_derive`, `cutoff_derive_utc`, `n_hosts`;
- ground truth: `gt_tier`, `known_root_cause`, `fix_commits`, `fix_authors`, `fix_landed_on` (branches plus commit date),
  `fix_available_at_cutoff`, `bot_was_right`, `human_signal`;
- fairness: `answer_visible_in_input`, `difficulty`, `fair_test_rationale`;
- provenance: `evidence`, `pin_method`, `bundle_ready`, `pin_row_sizes`, `original_post`, `original_report_paste`.

`cases_100_inputs.csv` holds only what a harness needs to build bundles: `case_id`, `mid`, `kernel_full`,
`crash_commit`, `rep_host`, `rep_time`, `cutoff`.

**Families in the sample:**

| Family | Name | Cases | Timing | Strict race |
|---|---|---|---|---|
| F01 | btrfs qgroup: GFP_ATOMIC extent_changeset ulist_add failure under spinlock -> BUG_ON | 5 | 3 | 0 |
| F02 | sched/fair EEVDF: unclamped reweight_entity vlag -> avg_vruntime overflow -> rq-lock hard lockup | 5 | 0 | 0 |
| F03 | btrfs block group: bg_list refcount race in create_pending_block_groups -> UAF | 5 | 5 | 5 |
| F04 | btrfs extent buffer: unbounded writeback-inhibit xarray walk without resched | 5 | 0 | 0 |
| F06 | mm page tables: concurrent modification race (khugepaged collapse vs PT_RECLAIM, pagewalk PUD split vs refault) | 5 | 5 | 5 |
| F07 | btrfs extent_map lifecycle: map freed while still on modified_extents/tree (LOGGING flag split, debug BUG) | 5 | 5 | 1 |
| F09 | btrfs background workers: race/stale state -> infinite loop (kill_all_delayed_nodes xarray race, discard worker) | 4 | 4 | 3 |
| F10 | bpf arena: VMA dup/remap without vm_open refcount -> double close UAF | 4 | 0 | 0 |
| F11 | bpf arena: GUARD_SZ/kern_vm_start misalignment on 64K pages -> infinite page-table walk | 4 | 0 | 0 |
| F12 | mm/slub: obj_exts (memcg/alloc-tag) free-path unbounded recursion -> stack overflow | 4 | 0 | 0 |
| F13 | mm/swap: swap_reclaim_full_clusters unbounded scan without cond_resched | 4 | 4 | 0 |
| F14 | net NIC drivers (bnxt, idpf): NULL deref of rings/vport on down or resetting interface | 4 | 1 | 1 |
| F15 | sched: wrong sched_class/ownership state assumed on class or mode switch (scx migration threads, mm_cid) | 4 | 1 | 0 |
| F08 | btrfs extent-map shrinker: root->inodes xa_lock held across cond_resched_lock -> lock starvation | 3 | 3 | 0 |
| F16 | fuse readahead: folio lifecycle bugs (splice-move refcount underflow, max_pages==0 ZERO_SIZE_PTR) | 3 | 1 | 0 |
| F17 | fs clients (fuse-uring, pNFS flexfiles): wrong NULL/ERR_PTR check or request-less entry -> NULL deref | 3 | 1 | 0 |
| F18 | core/mm/PCI: missing NULL check on absent or uninitialized state (cgroup rstat, swap_cgroup, AER info) | 3 | 0 | 0 |
| F22 | sched_ext: stale dsq->first_task after unlink -> UAF via scx_bpf_dsq_peek | 3 | 0 | 0 |
| F24 | mlx5: TX queue/channel lifecycle and FW data validation (reporter UAF, BQL reset, fw_tracer format) | 3 | 2 | 0 |
| F25 | drivers: current-message/transfer state race between completion and error/timeout paths (ipmi, tegra QSPI) | 3 | 1 | 1 |
| F19 | printk/IPI storm: unrate-limited WARN/AER prints or kfence static-key IPI broadcast -> soft lockup/CSD stall | 2 | 0 | 0 |
| F21 | sched_ext: spurious cgrp_moving_from WARN under rq lock -> netconsole printk self-deadlock | 2 | 1 | 1 |
| F26 | btrfs subpage: relocation folio private freed by reclaim race -> UAF/NULL deref | 2 | 2 | 2 |
| F27 | iommu/amd: completion-wait sequence allocation race -> timeouts and dom->lock convoy | 2 | 2 | 2 |
| F28 | perf/x86 PMU: event pointer vs active_mask state race -> NULL event deref | 2 | 2 | 2 |
| F29 | tracing probes (uprobes, fprobe): register/unregister race -> NULL deref | 2 | 2 | 1 |
| F30 | workqueue: watchdog false-positive stall from memory reordering | 2 | 2 | 2 |
| F31 | timers/migration: tmigr_handle_remote_up hierarchy livelock | 2 | 2 | 2 |
| F32 | block/net: stale reference after async wakeup or queue change (rq_qos on-stack wait, kTLS anchor) -> UAF | 2 | 2 | 2 |
| F05 | mm list_lru / cgroup writeback: long list spinlock hold in shrinker walk or inode switch -> soft lockup | 1 | 1 | 0 |
| F23 | sched fair/rt: starvation or IPI livelock (CFS throttle of preempted RCU reader, RT push self-IPI) | 1 | 1 | 0 |
| F33 | mm THP: invalid PMD migration entry dereferenced in split_huge_pmd_locked | 1 | 1 | 0 |

The earlier dev-hermit triage (246 reports / 33 families / 43 timing-dependent / 13 race families) was not readable from
this node because GitHub is denied. The 33-family taxonomy here was rebuilt independently and should be reconciled
against it; see §9.


## 3. Offline evidence bundle (format `ccbench-bundle/v1`)

### 3.1 What is frozen per case

Built by `harness/ccbench_bundle.py --mid M --host H --time T` (pin = `--representative-host/--representative-time`).

| File (in `evidence/`) | Content | Source |
|---|---|---|
| `backtrace.txt`, `dmesg.txt`, `kdump_dmesg.txt` | Exactly what kernel_investigator's collector writes | kernel_logview row (Hive) |
| `drgn_output.txt` | `=== Stack Trace / All CPUs / Modules / Crashing Task / Memory Info ===`, same builder as `collector._save_collection_data` + `mcp_collector.mcp_data_to_drgn` | same row (`all_cpus` column) |
| `bandicoot.json` | kdump_dmesg[:8000], makedumpfile_log[:2000], bpf_users, tw_jobs, bt_noinline, host_timeline[:5000], MCE fields | same row + netcons |
| `host_timeline.txt` | netcons ±300 s, ≤200 lines (same window and cap as `_query_host_timeline`) | `infrastructure.scuba_netcons` |
| `collection.json` | mid, host, crash_time, category_key, kernel, lsst, 10-host sample | derived |

Other files in the case directory:
- `context.json`: prompt fields, i.e. category, exception type, kernel, models %, hosts affected, host sample, panic, backtrace. The population used is the MID's rows in the 30 days before the cutoff.
- `crash_row.json`: the full raw row.
- `MANIFEST.json`: format, pin, cutoff, `kernel_full`, the exact SQL of every query, and sha256 plus size of every file.

Never frozen into the bundle: the original report, patches, `representative_triage.json`, the post, `kinv-bundle` tarballs
(they contain `report.md`, `kernel_fix.patch` and `reproducer/`), and any file written after the agent started.

### 3.2 What is NOT in v1, and why

| Item | Status | Plan |
|---|---|---|
| vmcore / live drgn | excluded by design. It lives on the crash host and needs the `--live-vmcore` ACL. | v2: "vmcore excerpt" = pre-run drgn script outputs captured by the collector while the host still has the core (tasks, locks, the crashing object). Frozen as `vmcore_excerpt.txt`. |
| crash-kernel symbols | Partly covered: backtraces are already symbolized (file:line) and the exact source is in the tree. The release fbpkg `kernel.<uname>` exists (e.g. `kernel.6.16.1-0_fbk2_hardened_0_gf40efc324cc8:1`, 699.7 MiB) but is an antlir btrfs sendstream that needs root (`btrfs receive`) to unpack. | v2 bundle builder runs as a job with root. It extracts `vmlinux`, `System.map` and module `.ko` debug files into `symbols/`. That is the "drgn + symbols" arm. |
| OOB console | Hive mirror keeps about 18 h, so old cases have none. | Capture at incident time going forward. |
| ODS metrics | not frozen in v1. The production prompt's ODS section is omitted for every arm. | v2: `ods.json` (the fixed key set the agent queries today, ±1 h). |
| kdump_all_cpus | Scuba-only. `all_cpus` from the row is used. | capture at incident time. |

Capture urgency: the Hive kernel_logview mirror currently reaches back to 2025-10-06 and netcons roughly 3+ months.
Bundles for the 100 cases should be built within days, not months.

### 3.3 Kernel source: a time-cut repo per case (`harness/ccbench_tree.sh`)

- A bare repo shares objects with the full mirror (alternates). It exposes only these refs:
  - `crash`: the exact crash-kernel commit (`_g<sha12>` of the uname);
  - each release/upstream branch at its newest commit ≤ cutoff (`rev-list -1 --before=@cutoff`);
  - tags created ≤ cutoff.
- There are no personal or scratch branches (they carry bot-drafted fixes) and there is no remote, so `git fetch` has nowhere to go.
- The agent's cwd is a worktree at `crash`. Example (MID 3c212664, cutoff 2026-06-23): `v6.13-fb` ends 2026-06-22,
  so Rik's fix `54262cda76de` (committed 2026-06-11) is findable, while anything after the crash is not.
- Mode: `find` uses cutoff = crash time. `derive` uses `cutoff_derive` = (earliest author time of any GT-fix variant on
  any exposed ref) − 1 day. Verified on the 3 pilot cases: after the derive cut, 0 refs contain any GT fix variant and
  `git log --all --grep=<fix subject>` returns 0 hits. The find-mode repos had 6–10 hits each.
- Residual risk: the alternates object store still holds post-cutoff objects, reachable only by exact SHA or by
  whole-store enumeration. The leak audit flags `cat-file --batch-all-objects`, `fsck --unreachable` and
  `rev-list --all --objects`. v2 option: materialize a real per-cutoff pack (`git clone --no-local` of the ref-filtered
  repo, ~5 GB each), or build one per cutoff-week bucket.

### 3.4 Storage

- Case dirs (`/data/users/<runner>/ccbench/cases/<MID>/`: evidence, context, manifest, tree.json) are small, 0.2–1 MB of
  evidence each. They are uploaded as `ccbench-case-<MID>.tar.zst` to Everstore/Manifold (`jf upload`, as the existing
  kinv bundles are), and the handle is recorded in the case table.
- The kernel mirror is shared, one per runner host.
- Ground truth lives in `sealed/<MID>.json` (mode 0700, a separate directory, never mounted into a run). The answer-key CSV
  is stored with the same restriction.
- Prompts reference neither sealed files nor paste IDs.

## 4. Sandboxed harness (`harness/ccbench_run.py`)

### 4.1 One run

1. Stage only `evidence/*` into `/tmp/kinv-logs/<MID>/`, verifying sha256 against `MANIFEST.json`. The production prompt and
   the `/kernel-investigation` skill hard-code this path. Only one run per MID may be active per host (lock).
2. `git worktree add --detach <run>/linux crash` from the time-cut repo. The cwd is the kernel tree, as in production.
3. Render the prompt. There are three parts:
   - **(a)** the production text of `prompts.build_investigation_prompt` with `live_vmcore=False` and no ODS section.
     It includes the "use /kernel-investigation", "reproduce in QEMU", "run /kverify" and "write report.md" instructions.
   - **(b)** a fixed offline-environment section, `offline_env.md`, byte-identical across arms. It says:
     - there is no network or fleet;
     - the tree is cut at the benchmark cutoff (find mode: crash time; derive mode: before the fix was authored);
     - where the skill lives;
     - mkosi is replaced by `make` + QEMU 8.1.2 with KVM;
     - Phase 7 duplicate check = `unchecked`;
     - the caps, and "write report.md early".
   - **(c)** the arm's capability section. This is the only arm-dependent text.
4. Launch the agent:

   ```
   claude -p <prompt> --model claude-opus-4-8[1m] --effort max --safe-mode --strict-mcp-config
          --permission-mode dontAsk --allowedTools <offline allowlist> --output-format stream-json --verbose
          --session-id <uuid> --add-dir /tmp/kinv-logs/<MID> --add-dir <skill dir> [...]
   ```

   The model and effort are production's `config.AGENT_MODEL` and `AGENT_EFFORT`. The run gets a fresh `CLAUDE_CONFIG_DIR`,
   and `PATH` is prefixed with the benchmark tools dir.
5. Supervise:
   - wall cap 3600 s;
   - token cap 40M, where tokens = Σ over API calls (input + cache_creation + cache_read + output), deduped by message id.
     The live stream under-counts output tokens by about 1%, because stream events carry start-of-message usage. The
     manifest therefore reports the CLI's final `modelUsage` totals and cost as authoritative and uses the live sum only
     to enforce the cap. The live sum is parsed from stream-json;
   - either cap kills the process group;
   - no auto-resume (production resumes up to 3×).
6. Collect and audit:
   - copy `/tmp/kinv-logs/<MID>` (minus build trees) to `<run>/out`, plus `worktree.diff`;
   - write `manifest.json`: wall, tokens, $ at list price, CLI-reported cost, tool histogram, permission denials,
     leak-audit hits, prompt/arm/offline/bundle hashes, crash commit, cutoff;
   - then delete the staged dir and worktree.

### 4.2 Isolation layers (what the pilot has, what production benchmarking needs)

| Layer | Pilot (this node) | Needed for the 100×2 run |
|---|---|---|
| L1 Claude config | `--safe-mode` + `--strict-mcp-config`: no user/plugin MCP servers, no plugin skills or agents, no hooks, no CLAUDE.md. Verified: with a fresh config the Meta wrapper otherwise auto-installs `meta@agent-market`, which exposes `knowledge_filtered_search`/`knowledge_load`. Those can read the WP answer posts and the report pastes. | same |
| L2 Tool permissions | `--permission-mode dontAsk` + explicit allowlist, and everything else is denied. Verified: `curl` denied, `git`/`cat`/`python3` allowed. **Finding:** kernel_investigator's own `--allowedTools` "restricted mode" does *not* restrict under the Meta wrapper's default permission mode. A test with only `Read`/`Bash(ls:*)` allowed still ran `curl https://www.internalfb.com` (HTTP 302). **Pilot friction:** dontAsk always denies shell `for` loops and running agent-built host binaries. The find-mode pilot had 19/13/0 denials (for-loops, `cd`+multi-line compounds, and guest-image tools `mkfs.btrfs`/`dd`/`truncate`/`fallocate`). Allowlist v1.1 adds the guest-image tools. | Inside a real netns container, run with `bypassPermissions`: the container, not the allowlist, is the boundary. This also removes the for-loop friction. |
| L3 Data | Bundle only. Time-cut repo, no remote. QEMU wrapper refuses guest networking unless slirp `restrict=on`, and defaults to `-nic none`. | same |
| L4 Network | **Not enforced on this node.** An allowed `python3` could still open sockets. The agent-security guardrails refuse namespace/sudo tooling for this agent identity, so I did not attempt it. Mitigated by the post-hoc leak audit, which invalidates a run on any hit. | Run each arm in a container/VM with a netns whose only egress is the model gateway. The kernel mirror is mounted read-only, the case dir read-only, and `/tmp/kinv-logs` is a private tmpfs, which also removes the "one run per MID" limit. Options: a Tupperware/Sandcastle job with a netns, or an OnDemand with root. **Needs coord2/owner approval.** |

### 4.3 Fixed prompt, seed and repeats

- Claude Code exposes no sampling seed or temperature. Instead we fix everything else and record hashes:
  - model id and CLI version (2.1.285);
  - effort;
  - prompt sha256;
  - skill sha256 (`kernel-investigation/SKILL.md` from agent-market `2566402676642ccc…`; paths rewritten to the pinned copy → `30178b5b…`);
  - offline section sha256 (v1.0 `6b95fa64…` for the find-mode pilot; v1.1 `1fe9b0f4…`, which only changes the cutoff wording, for the derive-mode pilot);
  - arm sha256;
  - bundle-manifest sha256.
- Nondeterminism is handled statistically: paired design (same 100 cases in both arms), ≥2 repeats per case per arm for
  the headline comparison, and case-level bootstrap CIs on the paired deltas.

### 4.4 Capability toggles (arms)

An arm is a JSON file: `name`, `skill_dir`, `add_dirs`, `extra_bash`, `extra_tools`, `capability_section`.
Two arms must differ in exactly one of these.

| Arm | Differs from baseline by | Status |
|---|---|---|
| `baseline` | Production-faithful: `/kernel-investigation` skill + production prompt. `/kverify` and `/kreview` are referenced but not installed, as in production today (the previous round lost about 27 min to a filesystem crawl for them). | defined, piloted |
| `variant-kskills` | `/kverify` and `/kreview` exposed as installed skills: the package's `prompts/skills/{kverify,kreview}.md` plus paths in the capability section. | defined, not run |
| `variant-drgn-symbols` | `symbols/` (vmlinux + debuginfo of the crash kernel) + `drgn` usage notes. Needs the v2 bundle. | designed |
| `variant-hermit` | Hermit binary on PATH + allowlist entry + a skill file (deterministic or chaos scheduling for userspace reproducers and guest workloads). | designed |
| `variant-mkosi` | kernel-mcp mkosi/QEMU MCP tools (privileged daemon) instead of plain `make`+QEMU. | needs root runner |

## 5. Blind grading

### 5.1 Rubric (per run)

| # | Criterion | Scale | How |
|---|---|---|---|
| R1 | Root cause (originating defect) | 0 wrong · 1 right subsystem/fatal mechanism only · 2 right function/structure, mechanism incomplete · 3 matches GT | judges vs `sealed.root_cause` |
| R2 | Fix | 0 none/wrong · 1 mitigation/symptom only · 2 equivalent per `equivalent_fix_criteria` · 3 exact (cites a GT sha/subject, or a functionally identical patch) | deterministic sha/subject citation + patch file/function overlap vs GT fix, then judges |
| R3 | Reproducer | attempted · actually ran (a QEMU invocation whose output shows a boot, or boot logs in `reproducer/`) · reproduced the failure | deterministic evidence from the transcript and artifacts, then judges |
| R4 | A/B verdict | n/a · correct (fail-before/pass-after artifacts and the tested patch is R2≥2) · unsupported (claimed without artifacts) · wrong | judges |
| R5 | Cost/time | wall s, tokens, $ (list), exit reason (cap hit?) | `manifest.json` |
| R6 | Calibration | stated confidence vs R1/R2 (high-confidence-wrong flagged) | judges |
| R7 | Integrity | leak-audit hits (run invalid if any hit reached a live source), permission denials, hallucinated tools | manifest |

Headline score = 0.4·R1/3 + 0.3·R2/3 + 0.1·[R3 ran] + 0.1·[R4 correct] + 0.1·R6. Cost is reported separately and never
folded into the score. A/B comparisons use paired per-case deltas on the score and on each criterion, plus success@cap.

### 5.2 Who grades and how disagreements resolve

1. `grade_prep.py` computes deterministic facts: GT sha/subject cited; patch vs GT file/function overlap; QEMU
   invocations and boot evidence; cost. It also writes a blinded copy of the report (arm-revealing tool names masked,
   run ID replaced by a hash).
2. Two independent LLM judges grade from `{sealed GT, blinded report, patch, facts}` without the arm label.
   The judges are claude-opus-5-5 and claude-sonnet-5-5, i.e. a different model and tier from the agent under test.
3. If the judges disagree on any R1/R2 point, or on R3 or R4, a third judge adjudicates at high effort and sees both
   rationales.
4. Anything still split, and a random 10% sample, goes to a human kernel SME: the fix author where possible, otherwise
   riel/boris/mharmstone-level reviewers via coord2. Their verdict is final and is used to measure judge accuracy.

## 6. Pilot: 3 cases, baseline arm, 60 min / 40M-token caps

### 6.1 Setup (provenance)

| | |
|---|---|
| Cases | CCB-012 = MID 1e340e89… (btrfs `root->inodes` xa_lock starvation, 6.16, pin devvm20945.cco0 @ 1781007878). CCB-011 = MID 3c212664… (EEVDF `reweight_entity` vlag overflow → rq-lock hard lockup, 6.13-fbk16, pin twshared69669.13.pnb1 @ 1782239178). CCB-020 = MID 102ffa7b… (kTLS strparser stale skb → NULL deref in softirq, 6.11-fbk10, pin twshared46327.01.lla3 @ 1784135568). |
| Why these | All three are tier A with human-authored fixes: Boris Burkov `3a2268b38193`; Rik van Riel `54262cda76de`/`ccd3bc00e3e5`, named by Rik in the WP thread; Jakub Kicinski `4ab26bce3969`. There are 3 families and 3 subsystems (fs, sched, net), and all three are timing-dependent. In CCB-011 the original CrashCortex post proposed a mitigation and missed the fix. |
| Bundles | built from Hive by `ccbench_bundle.py`: 7 evidence files each, 0.4–0.9 MB, sha256-manifested. Case dirs: `/data/users/newton/ccbench/cases/<MID>` (find) and `<MID>__derive` (derive). |
| Agent | `claude -p` 2.1.285, `claude-opus-4-8[1m]`, effort max (production values), `--safe-mode --strict-mcp-config --permission-mode dontAsk`, allowlist v1.0. The three runs ran concurrently on one 166-core OD. |
| Grading | `grade_prep.py` facts → judges A (Opus, session default) and B (Sonnet), both blind. The third judge was not needed: A and B agreed on every R1–R4 criterion. Workflow `wf_8f9ad025-7e0`. |

### 6.2 Results: find mode (tree cut at crash time; `pilot-*-baseline-r1`)

| Case | Wall | Tokens (CLI modelUsage) | $ (list, CLI) | R1 root cause | R2 fix | R3 reproducer | R4 A/B | R6 | Score |
|---|---|---|---|---|---|---|---|---|---|
| CCB-012 btrfs | 42.1 min | 14.08M (13.61M cache-read, 0.30M cache-write, 0.17M out) | $14.02 | 3 | 3 (cites `3a2268b38193`; patch hunks identical) | attempted; 2 guest boots; **trigger never ran** (no `mkfs.btrfs` in allowlist v1.0) | n/a | ok (medium stated) | 0.80 |
| CCB-011 sched | 35.5 min | 17.92M | $15.10 | 3 | 3 (backport of `ccd3bc00e3e5`, byte-identical to `54262cda76de`) | ran (baseline + fixed guests booted, 120 s workload), not reproduced | **unsupported**: claimed "FIX VERIFIED" with no fail-before evidence; the "arithmetic A/B" numbers were copied from the fix changelog, not computed | ok | 0.90 |
| CCB-020 tls | 42.1 min | 17.57M | $16.66 | 3 | 3 (cites `4ab26bce3969`; identical hunk) | ran (nolibc PID-1 kTLS workload), not reproduced | n/a (says "REPRODUCER INADEQUATE") | ok (high) | 0.90 |
| **Total / mean** | 40.0 min mean | 49.57M / 16.52M | **$45.78 / $15.26** | 3.0 | 3.0 | 3/3 attempted, 2/3 ran, 0/3 reproduced | 0 correct, 1 unsupported | | 0.87 |

Integrity and leak audit:
- 0 leak-audit hits on all runs. Two false positives came from C source written by Write; the audit now scans command
  and path inputs only.
- 0 network commands.
- All 3 runs ended on their own (`terminal_reason: completed`). No cap was hit.

Judge notes worth keeping:
- CCB-011's patch re-attached Rik's `Signed-off-by` to modified text and used a local-tree `Fixes:` sha.
- CCB-012 honestly capped its confidence at medium because it had no A/B.

### 6.3 What the pilot proved and what it exposed

- **Pipeline works end to end.** Bundle (Hive) → time-cut tree → isolated run → manifest → deterministic facts →
  2 blind judges → grade. No live fleet, Scuba, vmcore, WP or Phabricator access occurred.
- **Find mode shows a ceiling effect.** All 3 fixes were already in the time-cut tree, and the agent located and
  cherry-picked each one; the reports cite the commits. Find mode therefore measures fix search, and a strong baseline
  saturates it. This is why derive mode is primary (§2.3, §6.4).
- **The reproducer is now the discriminating axis.** 0/3 reproduced and 1/3 made an unsupported A/B claim. The harness
  must provide a guest-userland kit (mkfs.*, busybox, stress-ng static). Allowlist v1.1 adds mkfs/dd/truncate/fallocate;
  v2 adds a static busybox/stress-ng image. This is also where a Hermit-style controlled-scheduling arm should show a
  difference on the 30 race cases.
- **Harness bugs found and fixed.**
  - The runner crashed in its post-run audit on string `message` events. The agents had already exited, and the runs
    were re-finalized with `ccbench_finalize.py`.
  - The live token sum under-counts output by about 1%; the manifest now uses CLI `modelUsage`.
  - The leak audit had false positives on file bodies.
- **Cost is lower than the previous online rerun** (89.9 min / 24.0M tokens for 4594a5b3). There is no collection, triage,
  owner, WP or diff stage, and the baseline gives up on `/kverify` quickly (one failed `Skill` call) instead of crawling the
  filesystem for 27 min.

### 6.4 Extra pilot condition: derive mode (tree cut 1 day before the fix was authored; `pilot-*-baseline-derive-r2`)

Same 3 cases, same arm, caps and allowlist v1.0. Only the time-cut repo differs: 0 refs hold any GT-fix variant.
Graded by the same blind two-judge workflow (`wf_2696e972-bb0`). CCB-020 needed the third judge (R1 2 vs 1; R4 wrong vs
unsupported).

| Case | Wall | Tokens | $ | R1 | R2 | R3 | R4 | R6 | Score |
|---|---|---|---|---|---|---|---|---|---|
| CCB-012 btrfs | 55.1 min | 18.49M | $17.55 | **3** (right function + PREEMPT_NONE `cond_resched_lock` mechanism + virt_spin_lock amplifier; missed the sibling `btrfs_find_first_inode`) | **2** equivalent: drops the lock every 100 skipped inodes (GT drops it on every skip, in 2 sites) | ran (2 boots, preempt=none + nopvspin, 33k shrinker triggers), not reproduced | n/a (honest: "NOT A/B-verified") | ok (medium) | 0.80 |
| CCB-011 sched | 41.0 min | 13.92M | $15.28 | **1**: named the WARN-under-rq-lock console path as the originating defect; never mentions `reweight_entity`/vlag | **1**: `printk_deferred` around the WARN = mitigation. **This is exactly the original CrashCortex bot's error on this MID.** | attempted, no boot evidence | **unsupported**: "FIX VERIFIED" from a circular A/B that toggles only its own harness printk | overconfident (high) | 0.23 |
| CCB-020 tls | 46.9 min | 24.28M | $19.30 | **2**: right function/state (`tls_strp_read_sock` skips `tls_strp_load_anchor_with_queue`), mechanism wrong | **1**: backported a different real upstream fix (`491deb9b8c4a`, alloc_page failure path) | ran **and reproduced a panic**, but via its own fault injection | **wrong**: fail-before/pass-after is real only for the injected scenario, not this crash | overconfident (high) | 0.47 |
| **Mean** | 47.7 min | 18.89M | **$17.38** | 2.0 | 1.33 | 3/3 attempted, 2/3 ran, 1/3 "reproduced" (injected) | 0 correct | | **0.50** |

**Find vs derive on the same cases and arm: 0.87 vs 0.50.** R1 goes 3.0 → 2.0 and R2 goes 3.0 → 1.33. Derive mode
discriminates, and it reproduces a real historical failure mode: on CCB-011, symptom-mitigation instead of the
originating defect, which is what CrashCortex itself did in production. Derive mode costs about 14% more per run
($17.38 vs $15.26) and takes about 19% longer.

Rubric fix found here, applied in v1.1: judges must be told the **mode and cutoff**, and must verify "no upstream
fix exists" claims against the case's time-cut repo, not the full mirror. In derive mode both judges flagged the agents'
"no upstream fix" statements as false. They were true for the tree the agent saw. The flag did not change any R1/R2
score, but it would bias R6/integrity.

First derive attempt (`*-derive-r1`): all three runs were SIGKILLed together at 20:19Z, about 38 min in (§8.6). There is
no CLI total for them. The live estimate is $29.5 at list price, an undercount by the ~1% output tokens (≈$3–4/run).

**Total pilot spend** (9 agent runs incl. the killed ones, 13 judge agents):
- find $45.78 + derive-r2 $52.13 + derive-r1 ≈ $30–40 (live estimate) ≈ **$130–140** for agent runs;
- judges ≈ 0.55M tokens.



## 7. Cost estimate for 100 cases × (baseline + 1 variant)

All $ figures are list price, taken from the CLI's own `modelUsage.costUSD` with `costBasis: list`. I checked them against
the published claude-opus-4-8 rates: $5/M input, $25/M output, $0.50/M cache read, $10/M 1-hour cache write.
Internal chargeback may differ.

| Item | Basis (provenance) | 100 cases × (baseline + 1 variant) = 200 runs |
|---|---|---|
| Agent runs, expected | pilot mean: find mode **$15.26 / 16.52M tokens / 40.0 min per run** (3 runs, §6.2); derive mode (the recommended primary) **$17.38 / 18.89M / 47.7 min per run** (3 runs, §6.4) | **derive ≈ $3.48k**, 3.8B tokens, 159 agent-hours (find ≈ $3.05k) |
| Agent runs, wall-cap bound | every run uses the full 60 min at the derive-pilot token rate (0.40M tokens/min → 23.8M, ≈$21.9) | ≈ $4.4k |
| Agent runs, hard ceiling | every run hits the 40M-token cap (pilot blended $0.92/M tokens) | ≈ $7.4k |
| Repeats | recommended 2 per case per arm for the headline A/B (the Claude CLI has no seed) | ×2 → ≈ $7.0k expected (derive) |
| Judges | pilot: 6 judge agents = 228k tokens total (≈38k per judge); 2 judges per run + 30% third-judge allowance | ≈ 18M tokens, **≈ $0.1–0.2k** |
| Human SME | 10% random sample + unresolved disagreements (≈ 30 runs × 15 min) | ≈ 7–8 SME-hours |
| Compute | 3 concurrent runs per 166-core OnDemand, as in the pilot (each run builds 1–4 guest kernels at -j160) | ≈ 53 node-hours per repeat (derive), ≈ 13 h wall on 4 nodes; cap-bound 67 node-hours |
| Bundles | 3 Presto queries + time-cut repo per case (≈ 20–40 s); evidence 0.2–1 MB per case; shared 6.7 GB mirror; ≈ 10 GB transient per run (worktree + builds) | ≈ 1 h, ≈ $0 |

**Total for 100 × (baseline + 1 variant), 1 repeat, derive mode:** about **$3.6k expected** (agents $3.48k + judges ≈ $0.1–0.2k).
It is **≈ $4.6k** if every run hits the 60-min wall cap and **≈ $7.5k** at the hard 40M-token ceiling. Find mode is
≈ $3.2k expected. With the recommended 2 repeats: **≈ $7.2k expected** (derive). Wall time is about 13 h on 4
OnDemand nodes per repeat.

For comparison, the previous round's online reruns were 38–90 min and 24–32M tokens per crash. At the same blended rate
that is ≈ $22–30 per run, or $4.4–5.9k for 200 runs.


## 8. Findings worth acting on (no action taken this round)

1. **kernel_investigator "restricted mode" is not restrictive** under the Meta `claude` wrapper. In `-p` mode with the
   wrapper's default permission mode, tools outside `--allowedTools` still run. Verified with `curl`. Production agents
   therefore have full Bash and all auto-installed plugin tools. A fix is to add `--permission-mode dontAsk` (and
   `--strict-mcp-config` plus an explicit `--mcp-config`). Not changed this round (no diffs to dcostantino's package).
2. **Answer-leak channel for any benchmark:** the auto-installed `meta` plugin's knowledge tools can read the Kernel AI
   Investigations posts and report pastes. Any evaluation of CrashCortex on past incidents must run with `--safe-mode`
   (or an equivalent) or it is contaminated.
3. **Data retention:**
   - Scuba `kernel_logview` holds about 67 days (min time 2026-07-25 on 2026-09-30);
   - the Hive mirror holds ≥12 months;
   - netcons Hive holds ≥3 months;
   - the OOB mirror about 18 h.

   kernel-mcp's `get_crash_detail` is Scuba-only, which is why MIDs older than about 30–60 days SKIP without a pin.
   The benchmark bundle builder reads Hive.
4. The crash-kernel fbpkg holds symbols but needs root to unpack (sendstream). A symbols arm needs a root-capable
   bundle builder.
5. **Pretraining contamination in derive mode.** Hiding a fix from the tree does not hide it from the model. Upstream
   fixes public before the agent model's training cutoff (e.g. `4ab26bce3969`, July 2025) may be recalled. Mitigations:
   - report derive-mode results split by `fix public before model cutoff` vs after;
   - Meta-internal fixes (FBONLY, internal branches; e.g. `54262cda76de`) and fixes newer than the model cutoff are
     clean;
   - grade citations of a hidden upstream SHA as "recalled" rather than "derived".
6. **Stray-process kills.** The first derive-mode pilot attempt (`*-derive-r1`) was SIGKILLed on all three runs at the
   same moment (20:19Z), at about 38 min. There was no OOM (user cgroup `oom_kill 0`), and the cause is undetermined: the
   runs were in systemd scopes owned by the driving agent's tool calls. The runs were relaunched with `setsid nohup`.
   In the production runner, run each agent under a supervisor that owns its own scope, i.e. the container of §4.2.

## 9. Asks for coord2

1. **Commit these pastes to dev-hermit** `ai_docs/crash-cortex-benchmark-20260930/`. GitHub is denied for this agent
   identity: fwdproxy returns 403 `github.com denied by filter {agent_role: default_agent}`, and lore.kernel.org and
   git.kernel.org are denied too. The paste IDs are in the final report.
2. **Isolation for the real run (L4).** Either approve a container/VM runner (netns with model-gateway-only egress,
   read-only mirror and case mounts, private `/tmp`, root for mkosi and sendstream symbol extraction), or explicitly
   accept L1–L3 + leak audit as sufficient for phase 2.
3. **Taxonomy reconciliation.** Paste `experiments/crash-cortex-report_20260819` (246 reports / 33 families) so the
   33 families here can be mapped onto it.
4. **Decisions:**
   - primary mode derive (recommended) vs find;
   - repeats per case per arm (recommended 2);
   - which variant goes first. Options: `variant-kskills` (cheapest, ready); a Hermit arm on the 30-case race subset;
     `variant-drgn-symbols` (needs the root-capable bundle builder).
5. **Budget** (§7) and **human SME graders.** I pinged nobody. The fix authors in the sample include Leo Martins,
   Boris Burkov, Rik van Riel, Breno Leitao, Omar Sandoval and Jakub Kicinski.
6. **Relay to dcostantino** (no diff this round): `--allowedTools` restricted mode is ineffective under the Meta wrapper
   (§8.1).

