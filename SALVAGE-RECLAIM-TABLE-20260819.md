# Salvage / reclaim action table - 2026-08-19

[hermit2, slots, opus-5, devbig014, role=coordinator]

Sizes are btrfs `compsize` DISK usage, measured after `sync` + `btrfs subvolume sync`.
Per-row sizes count shared extents once PER ROW, so they SUM HIGHER than the true total;
the authoritative figure is the single combined invocation at the bottom.

| # | worktree | repo | branch | disk GiB | had work? | preserved at | safe to delete? |
|---|---|---|---|---|---|---|---|
| 1 | `scratch/w14-baseline` | dev-hermit | `DETACHED` | 32.32 | YES | https://github.com/rrnewton/dev-hermit/pull/141 ⚠hooks-bypassed | YES |
| 2 | `hermit/ignored/staging-batch3/hermit` | hermit | `staging/batch-3` | 22.29 | no | - | YES |
| 3 | `scratch/graft-recovery-hermit` | hermit | `recovery/everything-else` | 18.59 | no | - | YES |
| 4 | `experiments/demo5_bisect_20260731/ignored/mswt/p3` | hermit | `DETACHED` | 14.49 | no | - | YES |
| 5 | `experiments/demo5_bisect_20260731/ignored/mswt/p1` | hermit | `DETACHED` | 14.33 | no | - | YES |
| 6 | `experiments/demo5_bisect_20260731/ignored/mswt/p2` | hermit | `DETACHED` | 13.73 | YES | https://github.com/rrnewton/hermit/pull/2316 | YES |
| 7 | `worktrees/regress/reverie` | reverie | `fix/sabre-detour-vdso-getrandom` | 12.28 | no | - | YES |
| 8 | `ignored/helper-238/hermit-wt` | hermit | `DETACHED` | 12.13 | no | - | YES |
| 9 | `hermit/ignored/only-fix` | hermit | `fix/only-shard-nesting` | 12.12 | no | - | YES |
| 10 | `ignored/staging-batch1/hermit` | hermit | `staging/batch-1b` | 11.68 | no | - | YES |
| 11 | `hermit/ignored/land-1595/hermit` | hermit | `land/1595-onto-2caef182` | 11.17 | no | - | YES |
| 12 | `ignored/ratchet-liteinst/hermit` | hermit | `DETACHED` | 10.15 | no | - | YES |
| 13 | `ignored/land-1586/hermit` | hermit | `codex/validate-rust-script-thin-wrapper` | 9.92 | no | - | YES |
| 14 | `worktrees/kvm-canonical-measurement` | hermit | `codex/kvm-canonical-measurement` | 9.83 | no | - | YES |
| 15 | `hermit/ignored/land-1622/hermit` | hermit | `DETACHED` | 9.67 | no | - | YES |
| 16 | `ignored/land-1471b/hermit` | hermit | `land/1471-rebase-2caef182` | 9.55 | no | - | YES |
| 17 | `worktrees/codex-keep-going-hermit-pin` | hermit | `codex/keep-going-agent-utils-pin` | 9.37 | no | - | YES |
| 18 | `ignored/land-1468/hermit` | hermit | `DETACHED` | 9.09 | no | - | YES |
| 19 | `ignored/drainall-validate` | hermit | `DETACHED` | 9.05 | no | - | YES |
| 20 | `ignored/land-1213/hermit` | hermit | `land/1213-rebase` | 9.04 | no | - | YES |
| 21 | `ignored/staging-batch2/hermit` | hermit | `staging/batch-2` | 9.00 | no | - | YES |
| 22 | `ignored/land-1595/hermit` | hermit | `land/1595-verify-verdict` | 9.00 | no | - | YES |
| 23 | `ignored/land-1604/hermit` | hermit | `DETACHED` | 8.98 | no | - | YES |
| 24 | `ignored/land-1624/hermit` | hermit | `DETACHED` | 8.98 | no | - | YES |
| 25 | `scratch/prose-hermit` | hermit | `feat/validate-sh-admission-guards` | 8.94 | YES | https://github.com/rrnewton/hermit/pull/2324 | YES |
| 26 | `hermit/ignored/land-1200/hermit` | hermit | `land/1200-rebase-2caef182b` | 8.92 | no | - | YES |
| 27 | `scratch/coord-rpc-fixes/reverie` | reverie | `perf/coordinator-rpc-guest-side-trim` | 8.60 | no | - | YES |
| 28 | `ignored/land-1443/hermit` | hermit | `land-1443-rebase` | 8.48 | no | - | YES |
| 29 | `ignored/schema5-validate-parent.aP2vbt/repo/ignored/validate-fresh-fLTCNzwZ` | hermit | `DETACHED` | 8.09 | no | - | YES |
| 30 | `ignored/land-1623/hermit` | hermit | `pin-bump/pr-1919-038e993926e4` | 8.07 | no | - | YES |
| 31 | `ignored/band-rebase` | hermit | `DETACHED` | 7.93 | no | - | YES |
| 32 | `ignored/staging-reverie-live/reverie` | reverie | `staging/reverie-drain-live-20260806` | 7.82 | no | - | YES |
| 33 | `ignored/land-1576/hermit` | hermit | `DETACHED` | 6.84 | no | - | YES |
| 34 | `scratch/w4-1719-reconcile` | hermit | `DETACHED` | 5.77 | no | - | YES |
| 35 | `hermit/ignored/land-1445/hermit` | hermit | `land/1445-rebase` | 5.56 | no | - | YES |
| 36 | `worktrees/reverie-maturity/reverie` | reverie | `codex/reverie-backend-maturity-b1-b1.5` | 3.72 | no | - | YES |
| 37 | `worktrees/m2audit/hermit-committed-time-comparator` | hermit | `codex/filter-host-perturbed-committed-time` | 3.18 | no | - | YES |
| 38 | `worktrees/reverie-actions-policy/reverie` | reverie | `sanity-driver/finalize-dbt-background-evidence` | 2.99 | no | - | YES |
| 39 | `worktrees/sabre-clock-exec-reverie-18ab` | reverie | `codex/sabre-proc-self-exe-reexec` | 2.50 | no | - | YES |
| 40 | `ignored/ratchet-sabre/hermit` | hermit | `feat/sabre-forward-signals-to-tool` | 2.48 | YES | https://github.com/rrnewton/hermit/pull/2321 | YES |
| 41 | `worktrees/pressure-iterative-main/hermit` | hermit | `DETACHED` | 2.31 | no | - | YES |
| 42 | `worktrees/demo-ptrace-refresh` | dev-hermit | `codex/demo-ptrace-refresh` | 2.22 | no | - | YES |
| 43 | `ignored/w6-kvm` | hermit | `DETACHED` | 2.01 | no | - | YES |
| 44 | `worktrees/stdio-lseek-capture` | hermit | `codex/stdio-lseek-capture-offset` | 1.98 | no | - | YES |
| 45 | `worktrees/divergence/demo5-origin-parent` | dev-hermit | `DETACHED` | 1.96 | no | - | YES |
| 46 | `worktrees/liteinst-promote-stale-reasons` | hermit | `codex/liteinst-promote-stale-reasons` | 1.77 | YES | https://github.com/rrnewton/hermit/pull/2333 | YES |
| 47 | `ignored/demo-adv-review-2ea84473` | dev-hermit | `DETACHED` | 1.68 | YES | https://github.com/rrnewton/dev-hermit/pull/137 | YES |
| 48 | `worktrees/divergence/hermit-demo5-origin` | hermit | `DETACHED` | 1.54 | no | - | YES |
| 49 | `worktrees/w11flock/reverie` | reverie | `liteinst/zero-ptracer-litmus-negative-half` | 1.40 | no | - | YES |
| 50 | `worktrees/pmu/hermit-failclosed-rebase` | hermit | `codex/fail-closed-rebased-main` | 1.30 | YES | https://github.com/rrnewton/hermit/pull/2337 | YES |
| 51 | `worktrees/sanity-driver/reverie` | reverie | `sanity-driver/m2-dbt-protected-evidence` | 1.29 | no | - | YES |
| 52 | `ignored/dr2` | dev-hermit | `DETACHED` | 1.18 | no | - | YES |
| 53 | `scratch/w5-pr394` | reverie | `DETACHED` | 1.16 | no | - | YES |
| 54 | `worktrees/sanity-driver/hermit` | hermit | `sanity-driver/rehearse-reverie-459` | 1.13 | YES | https://github.com/rrnewton/hermit/pull/2340 | YES |
| 55 | `worktrees/rk-pipesz` | hermit | `fix/determinize-pipe-capacity` | 1.13 | no | - | YES |
| 56 | `hermit/scratch/w5-pinbump` | hermit | `DETACHED` | 1.12 | no | - | YES |
| 57 | `hermit/ignored/cold-fix` | hermit | `DETACHED` | 1.10 | no | - | YES |
| 58 | `ignored/w3-sabre-wt` | hermit | `fix/sabre-scope-message-names-run` | 1.01 | no | - | YES |
| 59 | `scratch/w5-appstrict` | hermit | `DETACHED` | 0.98 | no | - | YES |
| 60 | `scratch/w11-corpus` | hermit | `DETACHED` | 0.95 | no | - | YES |
| 61 | `scratch/w5sabre` | hermit | `fix/sabre-rpc-socket-path-nondeterministic-info` | 0.89 | no | - | YES |
| 62 | `ignored/codex-dbt-verdict-integration` | hermit | `DETACHED` | 0.87 | YES | https://github.com/rrnewton/hermit/pull/2318 | YES |
| 63 | `ignored/w7-corpus` | hermit | `DETACHED` | 0.86 | no | - | YES |
| 64 | `ignored/ci-hub/stress-wt/nightly` | hermit | `DETACHED` | 0.85 | no | - | YES |
| 65 | `ignored/det4-parity/hermit` | hermit | `DETACHED` | 0.83 | no | - | YES |
| 66 | `worktrees/kvm-signal-reverie` | reverie | `codex/kvm-signal-status-teardown` | 0.77 | YES | https://github.com/rrnewton/reverie/pull/463 | YES |
| 67 | `scratch/w15-procfs` | hermit | `w15/procfs-wiring-call-site-guard` | 0.76 | YES | https://github.com/rrnewton/hermit/pull/2328 ⚠hooks-bypassed | YES |
| 68 | `ignored/coalesce-det4/hermit` | hermit | `landing/coalesce-clean7-onto-4c70658e7` | 0.75 | YES | https://github.com/rrnewton/hermit/pull/2317 | YES |
| 69 | `worktrees/m2audit/hermit-landed-sabre-d56112e9` | hermit | `DETACHED` | 0.74 | no | - | YES |
| 70 | `ignored/ratchet-liteinst/reverie` | reverie | `fix/kvm-erestartsys-restart` | 0.71 | no | - | YES |
| 71 | `.git/modules/reverie` | reverie | `main` | 0.61 | no | - | YES |
| 72 | `scratch/au-runner-term` | agent-utils | `containment-provenance-record` | 0.57 | no | - | YES |
| 73 | `worktrees/kvm-lseek-reverie` | reverie | `codex/kvm-captured-stderr-lseek` | 0.55 | no | - | YES |
| 74 | `worktrees/m2audit-sabre/hermit` | hermit | `m2audit/sabre-ratchet-fanout` | 0.54 | YES | https://github.com/rrnewton/hermit/pull/2334 | YES |
| 75 | `worktrees/m2audit/hermit` | hermit | `codex/staged-pressure-test-10-20-way-throughput` | 0.54 | YES | https://github.com/rrnewton/hermit/pull/2335 | YES |
| 76 | `ignored/w16-parity` | hermit | `DETACHED` | 0.52 | no | - | YES |
| 77 | `worktrees/ratchet-dbt/hermit-main` | hermit | `ratchet-dbt/branch-count-must-fail` | 0.52 | no | - | YES |
| 78 | `worktrees/codex-unresolved-summary-audit` | dev-hermit | `codex/unresolved-summary-placeholder-audit` | 0.51 | no | - | YES |
| 79 | `scratch/au-stress-concurrency` | agent-utils | `codex/stress-concurrency-and-stdin` | 0.47 | no | - | YES |
| 80 | `ignored/w1-kvm-main` | hermit | `DETACHED` | 0.42 | no | - | YES |
| 81 | `worktrees/kvm-lseek-root-cause` | hermit | `codex/kvm-lseek-offset-root-cause` | 0.40 | no | - | YES |
| 82 | `worktrees/hermit-pinbump` | hermit | `gdbserver-sequentialized-guest-on-run-path_20260818` | 0.38 | no | - | YES |
| 83 | `ignored/ratchet-sabre/reverie-probe` | reverie | `feat/sabre-forward-signals-to-tool` | 0.37 | YES | https://github.com/rrnewton/reverie/pull/462 | YES |
| 84 | `scratch/statfix` | hermit | `fixture/stat-metadata-identity` | 0.37 | YES | https://github.com/rrnewton/hermit/pull/2326 | YES |
| 85 | `worktrees/pmu/hermit` | hermit | `codex/sabre-clock-coordinator-time` | 0.37 | no | - | YES |
| 86 | `scratch/w15-parity` | hermit | `DETACHED` | 0.36 | no | - | YES |
| 87 | `scratch/failclosed` | hermit | `fail-closed-by-default` | 0.33 | YES | https://github.com/rrnewton/hermit/pull/2323 | YES |
| 88 | `ignored/node-d16a0f1c-hermit` | hermit | `DETACHED` | 0.28 | no | - | YES |
| 89 | `worktrees/sabre-clock-exec-reverie` | reverie | `codex/sabre-clock-exec-self` | 0.28 | no | - | YES |
| 90 | `worktrees/reverie-pinned` | reverie | `gdbserver-thaw-on-pinned-base_20260818` | 0.28 | no | - | YES |
| 91 | `/var/tmp/pdeath-probe.3505340` | hermit | `DETACHED` | 0.27 | no | - | YES |
| 92 | `scratch/timerfix` | hermit | `fixture/timer-family-identity` | 0.27 | YES | https://github.com/rrnewton/hermit/pull/2327 | YES |
| 93 | `scratch/sockfix` | hermit | `fixture/socket-epoll-ordering` | 0.27 | YES | https://github.com/rrnewton/hermit/pull/2325 | YES |
| 94 | `worktrees/m2audit/hermit-promotion-d56112e9` | hermit | `DETACHED` | 0.27 | no | - | YES |
| 95 | `worktrees/kvm-signal-waitstatus` | hermit | `codex/kvm-signal-waitstatus-teardown` | 0.25 | YES | https://github.com/rrnewton/hermit/pull/2332 | YES |
| 96 | `scratch/au-wall/wt` | agent-utils | `codex/wall-timeout-derivation` | 0.23 | no | - | YES |
| 97 | `worktrees/divergence-range/hermit` | hermit | `fix/record-divergence-turn-range` | 0.21 | no | - | YES |
| 98 | `worktrees/codex-keep-going-agent-utils-hermit-base` | agent-utils | `codex/keep-going-hermit-pin-minimal` | 0.20 | no | - | YES |
| 99 | `scratch/au-cputo-mult` | agent-utils | `codex/cpu-timeout-platform-multiplier` | 0.20 | no | - | YES |
| 100 | `scratch/au-cgroups/wt` | agent-utils | `codex/remove-cgroups-noop-flag` | 0.20 | no | - | YES |
| 101 | `worktrees/pmu/reverie` | reverie | `codex/pmu-skid-trial-1000-vs-2000` | 0.19 | no | - | YES |
| 102 | `scratch/au-worktrees/cgroups-231b` | agent-utils | `codex/cgroups-small-default-cap` | 0.19 | no | - | YES |
| 103 | `worktrees/ratchet-dbt/hermit-verdict` | hermit | `DETACHED` | 0.18 | YES | https://github.com/rrnewton/hermit/pull/2338 ⚠hooks-bypassed | YES |
| 104 | `scratch/au-parity` | agent-utils | `codex/rust-cpu-timeout-and-enforcement-crosscheck` | 0.16 | no | - | YES |
| 105 | `worktrees/codex-tick-render-failure-agent-utils-replay` | agent-utils | `codex/tick-repeated-render-failure-replay` | 0.16 | no | - | YES |
| 106 | `ignored/w16-brk` | reverie | `kvm-brk-arena-not-mmap-base` | 0.15 | no | - | YES |
| 107 | `worktrees/codex-agent-utils-wrkslots-help` | agent-utils | `codex/wrkslots-help-descriptions` | 0.13 | no | - | YES |
| 108 | `/tmp/hermit-cgroup-land-cd9` | hermit | `codex/land-cgroup-guard` | 0.13 | no | - | YES |
| 109 | `ignored/hermit-cgroup-land-verify` | hermit | `DETACHED` | 0.13 | no | - | YES |
| 110 | `worktrees/codex-tick-render-failure-agent-utils` | agent-utils | `codex/tick-repeated-render-failure` | 0.13 | no | - | YES |
| 111 | `/tmp/codex-dbt-verdict-integration` | hermit | `DETACHED` | 0.12 | YES | https://github.com/rrnewton/hermit/pull/2343 | YES |
| 112 | `worktrees/combined-reverify/hermit` | hermit | `codex/corrected-combined-candidate-pmu-ready` | 0.10 | no | - | YES |
| 113 | `worktrees/wall-series-bulk-ancestry` | dev-hermit | `codex/wall-series-bulk-ancestry` | 0.08 | no | - | YES |
| 114 | `worktrees/codex-dev-hermit-lint-main` | dev-hermit | `codex/dev-hermit-lint-main` | 0.08 | YES | https://github.com/rrnewton/dev-hermit/pull/143 | YES |
| 115 | `worktrees/codex-dev-hermit-lint-current` | dev-hermit | `codex/dev-hermit-lint-current` | 0.08 | no | - | YES |
| 116 | `worktrees/reverie-maturity/hermit` | hermit | `DETACHED` | 0.06 | no | - | YES |
| 117 | `worktrees/sanity-driver/reverie-combined` | reverie | `sanity-driver/dbt-combined-measurement` | 0.06 | no | - | YES |
| 118 | `scratch/au-cputo-unboxed` | agent-utils | `codex/cpu-timeout-unboxed-fallback` | 0.05 | no | - | YES |
| 119 | `ignored/validate-producer/1618/hermit` | hermit | `DETACHED` | 0.05 | no | - | YES |
| 120 | `ignored/hermit-boxprobe` | hermit | `probe-runner-boxing` | 0.05 | no | - | YES |
| 121 | `ignored/w16-cifix` | hermit | `ci-keep-prebuilt-artifacts-when-a-rerun-is-possible` | 0.05 | no | - | YES |
| 122 | `hermit/scratch/w5-relax` | hermit | `DETACHED` | 0.04 | no | - | YES |
| 123 | `worktrees/owner-directive-current-state` | dev-hermit | `codex/owner-directive-current-state` | 0.04 | no | - | YES |
| 124 | `scratch/recovered-agent-utils-primary-20260805/au-mypy-fix` | agent-utils | `cgroups-optout-land` | 0.03 | no | - | YES |
| 125 | `scratch/au-worker-exc` | agent-utils | `worker-thread-exception-fails-loudly` | 0.03 | YES | https://github.com/rrnewton/agent-utils/pull/72 | YES |
| 126 | `scratch/au-cpuset-allocator` | agent-utils | `codex/cpuset-reservation-allocator` | 0.03 | no | - | YES |
| 127 | `scratch/au-parity-derived` | agent-utils | `runner-derived-capabilities-and-exclusivity` | 0.03 | no | - | YES |
| 128 | `worktrees/ledger/hermit` | hermit | `codex/requalify-disabled-but-passing-compat-cells` | 0.03 | no | - | YES |
| 129 | `scratch/au-runner-features` | agent-utils | `runner-known-failure-allowlist` | 0.03 | YES | https://github.com/rrnewton/agent-utils/pull/71 ⚠hooks-bypassed | YES |
| 130 | `scratch/au-cli-fix` | agent-utils | `codex/fix-reexec-abspath` | 0.03 | no | - | YES |
| 131 | `worktrees/demo5-info-admission` | dev-hermit | `codex/demo5-info-admission` | 0.03 | no | - | YES |
| 132 | `scratch/main-tooling` | dev-hermit | `DETACHED` | 0.03 | no | - | YES |
| 133 | `scratch/au-pr6` | agent-utils | `pr6-check` | 0.03 | no | - | YES |
| 134 | `worktrees/bpf-denial-classification` | dev-hermit | `codex/bpf-denial-classification` | 0.03 | YES | https://github.com/rrnewton/dev-hermit/pull/142 | YES |
| 135 | `ignored/au-ambizero-sol-loc` | agent-utils | `DETACHED` | 0.02 | no | - | YES |
| 136 | `.git/modules/agent-utils` | agent-utils | `DETACHED` | 0.02 | no | - | YES |
| 137 | `ignored/producer-definition-main-fix` | dev-hermit | `fix/producer-definition-transition-health` | 0.02 | no | - | YES |
| 138 | `worktrees/coord/hermit` | hermit | `DETACHED` | 0.02 | no | - | YES |
| 139 | `hermit/ignored/cold-main` | hermit | `DETACHED` | 0.02 | no | - | YES |
| 140 | `hermit/ignored/cold-msg` | hermit | `DETACHED` | 0.02 | no | - | YES |
| 141 | `worktrees/per-backend-ci/hermit` | hermit | `codex/per-backend-ci` | 0.02 | no | - | YES |
| 142 | `ignored/one-source-validate-ledger-parent` | dev-hermit | `cihub-auditor/one-source-final-reader` | 0.02 | YES | https://github.com/rrnewton/dev-hermit/pull/138 ⚠hooks-bypassed | YES |
| 143 | `ignored/obligation-admission-fix` | dev-hermit | `DETACHED` | 0.02 | no | - | YES |
| 144 | `worktrees/ratchet-native/hermit` | hermit | `ratchet-native/measure-native-envelope` | 0.02 | no | - | YES |
| 145 | `ignored/schema5-1980-squash` | dev-hermit | `DETACHED` | 0.02 | YES | https://github.com/rrnewton/dev-hermit/pull/139 ⚠hooks-bypassed | YES |
| 146 | `worktrees/ptrace-startup-ack/hermit` | hermit | `codex/ptrace-scheduler-start-ack` | 0.02 | no | - | YES |
| 147 | `worktrees/pcfix/hermit` | hermit | `ci/parityc-fix-fixtures-and-compile-gate` | 0.02 | YES | https://github.com/rrnewton/hermit/pull/2336 ⚠hooks-bypassed | YES |
| 148 | `ignored/health-actions-disabled` | dev-hermit | `cc-health-knows-actions-disabled_20260819` | 0.02 | no | - | YES |
| 149 | `worktrees/demos-presentability` | dev-hermit | `demos/pinned-hermit-binary` | 0.02 | no | - | YES |
| 150 | `worktrees/m2audit-combined/dev-hermit` | dev-hermit | `codex/register-hermit-pr2305-producer-current` | 0.02 | no | - | YES |
| 151 | `worktrees/cc-exec-control` | dev-hermit | `cc-exec-control_20260818` | 0.02 | no | - | YES |
| 152 | `worktrees/actions-disabled-branch-protection-conflict` | dev-hermit | `codex/actions-disabled-branch-protection-conflict` | 0.02 | no | - | YES |
| 153 | `ignored/safehermit-build` | dev-hermit | `cc-safehermit_20260819` | 0.02 | no | - | YES |
| 154 | `ignored/disk-runbook` | dev-hermit | `cc-disk-exhaustion-runbook_20260819` | 0.02 | no | - | YES |
| 155 | `worktrees/pr-staleness-gates` | dev-hermit | `codex/pr-staleness-gates` | 0.02 | no | - | YES |
| 156 | `worktrees/codex-reverie-final-frame-diagnosis` | dev-hermit | `codex/reverie-final-frame-diagnosis` | 0.02 | no | - | YES |
| 157 | `worktrees/evidence-discard-doc` | dev-hermit | `codex/evidence-survives-outer-failure` | 0.02 | no | - | YES |
| 158 | `ignored/rk-outage` | dev-hermit | `fix/census-outage-on-unreadable-authority` | 0.02 | no | - | YES |
| 159 | `worktrees/codex-cold-review-validate-infra-parent` | dev-hermit | `codex/cold-review-validate-infra` | 0.02 | no | - | YES |
| 160 | `worktrees/tolerance-audit-inventory` | dev-hermit | `codex/tolerance-audit-inventory` | 0.02 | no | - | YES |
| 161 | `worktrees/m2audit-combined/hermit` | hermit | `codex/combined-2305-2304-dbt-disable-landing` | 0.02 | no | - | YES |
| 162 | `worktrees/ratchet-dbt/hermit` | hermit | `ratchet-dbt/measure-dbt-envelope` | 0.02 | no | - | YES |
| 163 | `ignored/pr124-runner-rebind-fix` | dev-hermit | `fix/canonical-ledger-allowlist-transition` | 0.02 | no | - | YES |
| 164 | `ignored/sync-clean-nested-submodules` | dev-hermit | `codex/sync-clean-nested-submodules` | 0.02 | no | - | YES |
| 165 | `ignored/w16-aupin` | dev-hermit | `DETACHED` | 0.02 | no | - | YES |
| 166 | `worktrees/m2audit-record-getpid/hermit` | hermit | `DETACHED` | 0.01 | no | - | YES |
| 167 | `worktrees/landing-predicate/hermit` | hermit | `codex/latch-validation-failures` | 0.01 | no | - | YES |
| 168 | `ignored/release-worktree-fail-closed` | dev-hermit | `fix/release-worktree-fail-closed-child-repos` | 0.01 | no | - | YES |
| 169 | `worktrees/bucket-timeout/hermit` | hermit | `codex/build-manifest-harness-before-gate` | 0.01 | no | - | YES |
| 170 | `ignored/receipt-gap-finalizer` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 171 | `scratch/w15-boundedops-fresh-main` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 172 | `scratch/sol-a1` | dev-hermit | `sol-slots/repin-agent-utils` | 0.01 | no | - | YES |
| 173 | `scratch/sol-a1-pre` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 174 | `ignored/valrs-demo08` | dev-hermit | `valrs/demo08-test-reconcile` | 0.01 | no | - | YES |
| 175 | `ignored/w6-bitwise` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 176 | `scratch/w5wind` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 177 | `scratch/w5-pinrace` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 178 | `scratch/w5-csvcount` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 179 | `ignored/w28-reconcile` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 180 | `ignored/w3-sc-wt` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 181 | `scratch/w5-flagmatrix` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 182 | `ignored/w16-ledger` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 183 | `ignored/w16-skill` | dev-hermit | `w16/validate-cpu-budget-skill` | 0.01 | no | - | YES |
| 184 | `ignored/w3-ratchet-wt` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 185 | `ignored/w3-mtl-wt` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 186 | `ignored/w16-esrch` | dev-hermit | `w16/detcore-misc-esrch-evidence` | 0.01 | no | - | YES |
| 187 | `ignored/w3-persist-wt` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 188 | `scratch/scorecard-baseline` | dev-hermit | `scorecard/pre-tightening-baseline-20260806` | 0.01 | no | - | YES |
| 189 | `ignored/w3-relay-wt` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 190 | `ignored/w3-tax-wt` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 191 | `ignored/w3-reset-wt` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 192 | `scratch/w5-parent-fresh` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 193 | `ignored/w16-acceptance` | dev-hermit | `w16/release-0.3-acceptance-contract` | 0.01 | no | - | YES |
| 194 | `ignored/w3-lifecycle-wt` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 195 | `scratch/w5-pr72` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 196 | `worktrees/sabre-divergence-ratchet/hermit` | hermit | `codex/sabre-divergence-ratchet` | 0.01 | YES | https://github.com/rrnewton/hermit/pull/2339 | YES |
| 197 | `worktrees/regress/hermit` | hermit | `feat/sched-affinity-observed-value-and-ci` | 0.01 | no | - | YES |
| 198 | `worktrees/ratchet-kvm-reverie-erestartsys` | reverie | `DETACHED` | 0.01 | YES | https://github.com/rrnewton/reverie/pull/464 | YES |
| 199 | `worktrees/codex-real-2/reverie` | reverie | `DETACHED` | 0.01 | no | - | YES |
| 200 | `worktrees/codex-validatelock/reverie` | reverie | `DETACHED` | 0.01 | no | - | YES |
| 201 | `worktrees/codex-emptybucket/reverie` | reverie | `DETACHED` | 0.01 | no | - | YES |
| 202 | `worktrees/compat-envelope-program/reverie` | reverie | `DETACHED` | 0.01 | no | - | YES |
| 203 | `worktrees/coord/reverie` | reverie | `DETACHED` | 0.01 | no | - | YES |
| 204 | `worktrees/helper-validator/reverie` | reverie | `DETACHED` | 0.01 | no | - | YES |
| 205 | `ignored/w16-gate` | reverie | `gate-no-result-says-so` | 0.01 | no | - | YES |
| 206 | `worktrees/ci-hub-trust/reverie` | reverie | `DETACHED` | 0.01 | no | - | YES |
| 207 | `worktrees/sol-producer/reverie` | reverie | `ci/stamp-ledger-producer` | 0.01 | no | - | YES |
| 208 | `ignored/w3-394-wt` | reverie | `DETACHED` | 0.01 | no | - | YES |
| 209 | `scratch/w11-gatefix` | dev-hermit | `w11/verifier-dep-closure-bracket` | 0.01 | no | - | YES |
| 210 | `worktrees/order-violation-demotion/hermit` | hermit | `codex/demote-order-violation-chaos` | 0.01 | no | - | YES |
| 211 | `worktrees/egress-probe/hermit` | hermit | `fix/sabre-fallback-is-loud` | 0.01 | YES | https://github.com/rrnewton/hermit/pull/2331 | YES |
| 212 | `worktrees/ledger-2304-remeasure/hermit` | hermit | `DETACHED` | 0.01 | no | - | YES |
| 213 | `scratch/au-fetch-once` | agent-utils | `codex/planner-one-shot-fetch` | 0.01 | no | - | YES |
| 214 | `worktrees/slot79/hermit` | hermit | `codex/dbi-to-dbt-live-references` | 0.01 | no | - | YES |
| 215 | `worktrees/ledger-hello-signals-remeasure/hermit` | hermit | `DETACHED` | 0.01 | no | - | YES |
| 216 | `worktrees/pressure-iterative/hermit` | hermit | `DETACHED` | 0.01 | no | - | YES |
| 217 | `worktrees/pressure-iterative-authorized/hermit` | hermit | `DETACHED` | 0.01 | no | - | YES |
| 218 | `worktrees/codex-dbt-wait4-polling/hermit` | hermit | `codex/dbt-wait4-published-exit-recheck` | 0.01 | YES | https://github.com/rrnewton/hermit/pull/2330 | YES |
| 219 | `worktrees/codex-dbt-verdict-compose/hermit` | hermit | `codex/dbt-verdict-compose-839cc973` | 0.01 | YES | https://github.com/rrnewton/hermit/pull/2329 | YES |
| 220 | `worktrees/codex-dbt-verdict-path/hermit` | hermit | `codex/dbt-verify-verdict-artifact` | 0.01 | no | - | YES |
| 221 | `worktrees/w11flock/hermit` | hermit | `w11/parity-probe-strict-comparator` | 0.01 | YES | https://github.com/rrnewton/hermit/pull/2342 ⚠hooks-bypassed | YES |
| 222 | `worktrees/scorecard-reasons/hermit` | hermit | `codex/scorecard-reasons-promotion` | 0.01 | YES | https://github.com/rrnewton/hermit/pull/2341 | YES |
| 223 | `worktrees/val1147/hermit` | hermit | `DETACHED` | 0.01 | no | - | YES |
| 224 | `worktrees/w7/hermit` | hermit | `DETACHED` | 0.01 | no | - | YES |
| 225 | `scratch/optooling-repro-wt` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 226 | `.claude/worktrees/agent-adf7f9c02fa053316` | dev-hermit | `worktree-agent-adf7f9c02fa053316` | 0.01 | no | - | YES |
| 227 | `scratch/sabre-parent-land` | dev-hermit | `DETACHED` | 0.01 | no | - | YES |
| 228 | `scratch/parent-land-lifecycle-test` | dev-hermit | `DETACHED` | 0.01 | YES | https://github.com/rrnewton/dev-hermit/pull/140 | YES |
| 229 | `worktrees/vfix/hermit` | hermit | `fix/test-inventory-ignores-generated-files` | 0.01 | no | - | YES |
| 230 | `ignored/validate-producer/1627/hermit` | hermit | `DETACHED` | 0.01 | no | - | YES |
| 231 | `worktrees/divergence/hermit` | hermit | `codex/ptrace-red-cell-first-divergence-survey` | 0.01 | no | - | YES |
| 232 | `worktrees/codex-real-2/hermit` | hermit | `DETACHED` | 0.01 | no | - | YES |
| 233 | `worktrees/pressure-fixes/hermit` | hermit | `codex/pressure-disabled-native-timeout-classification` | 0.01 | no | - | YES |
| 234 | `worktrees/codex-emptybucket/hermit` | hermit | `codex/prebuilt-manifest-planner-empty-bucket` | 0.01 | no | - | YES |
| 235 | `ignored/envcaus` | hermit | `env-block-retry-three-state-verdict` | 0.01 | no | - | YES |
| 236 | `worktrees/sabre-shutdown-order/hermit` | hermit | `codex/sabre-scheduler-shutdown-order` | 0.01 | no | - | YES |
| 237 | `worktrees/ci-hub-trust/hermit` | hermit | `cihub-auditor/schema5-edae` | 0.00 | no | - | YES |
| 238 | `ignored/det4-detlogdiff/hermit` | hermit | `feat/cross-backend-detlog-diff-harness` | 0.00 | YES | https://github.com/rrnewton/hermit/pull/2319 | YES |
| 239 | `worktrees/printlogs-logdiff/hermit` | hermit | `feature/log-diff-print-logs` | 0.00 | no | - | YES |
| 240 | `worktrees/m2audit-node-fixture/hermit` | hermit | `codex/node-v8-jit-direct-runtime` | 0.00 | no | - | YES |
| 241 | `worktrees/codex-sabre-rpc-tmpdir/hermit` | hermit | `codex/sabre-rpc-visible-tmpdir` | 0.00 | no | - | YES |
| 242 | `worktrees/parity-implementation` | hermit | `codex/ptrace-golden-parity` | 0.00 | no | - | YES |
| 243 | `worktrees/per-backend-ratchet84/hermit` | hermit | `codex/per-backend-ratchet-84` | 0.00 | no | - | YES |
| 244 | `worktrees/scorecard-reasons-pmu-scope/hermit` | hermit | `codex/liteinst-pmu-scope` | 0.00 | no | - | YES |
| 245 | `worktrees/receipt-2263b/hermit` | hermit | `DETACHED` | 0.00 | no | - | YES |
| 246 | `worktrees/receipt-2258/hermit` | hermit | `DETACHED` | 0.00 | no | - | YES |
| 247 | `worktrees/codex-cpuid2258/hermit` | hermit | `land/cpuid-pr2258` | 0.00 | no | - | YES |
| 248 | `worktrees/codex-overnight-lane1/hermit` | hermit | `DETACHED` | 0.00 | no | - | YES |
| 249 | `worktrees/receipt-2263/hermit` | hermit | `DETACHED` | 0.00 | no | - | YES |
| 250 | `worktrees/receipt-relay-probe-20260815a/hermit` | hermit | `DETACHED` | 0.00 | no | - | YES |
| 251 | `worktrees/dbt-typed-outcome/hermit` | hermit | `codex/dbt-typed-verification-outcome` | 0.00 | no | - | YES |
| 252 | `worktrees/verify-diagnostics/hermit` | hermit | `codex/verify-first-run-diagnostics` | 0.00 | no | - | YES |
| 253 | `/tmp/hermit-da85-manifest-cli` | hermit | `DETACHED` | 0.00 | no | - | YES |
| 254 | `worktrees/m2audit-cbe2304/hermit` | hermit | `codex/cbe-2304-cold-validate` | 0.00 | no | - | YES |
| 255 | `worktrees/triage-qemu-cells/hermit` | hermit | `DETACHED` | 0.00 | no | - | YES |
| 256 | `worktrees/triage-state-cells/hermit` | hermit | `DETACHED` | 0.00 | no | - | YES |
| 257 | `worktrees/triage-scheduler-cells/hermit` | hermit | `DETACHED` | 0.00 | no | - | YES |
| 258 | `worktrees/engine-triage/hermit` | hermit | `DETACHED` | 0.00 | no | - | YES |
| 259 | `worktrees/slot07/hermit` | hermit | `DETACHED` | 0.00 | no | - | YES |
| 260 | `worktrees/slot06/hermit` | hermit | `DETACHED` | 0.00 | no | - | YES |
| 261 | `worktrees/lander/hermit` | hermit | `DETACHED` | 0.00 | no | - | YES |
| 262 | `worktrees/codex-validatelock/hermit` | hermit | `DETACHED` | 0.00 | no | - | YES |
| 263 | `worktrees/compat-envelope-program/hermit` | hermit | `rescue/registry-drift-compat-envelope-checkout` | 0.00 | no | - | YES |
| 264 | `worktrees/helper-validator/hermit` | hermit | `pin-bump/pr-2059-99437f05e823-helper-validator-3799914` | 0.00 | no | - | YES |
| 265 | `scratch/au-parallel-experiment` | agent-utils | `codex/dag-runner-core-allocator` | 0.00 | YES | https://github.com/rrnewton/agent-utils/pull/70 | YES |
| 266 | `worktrees/sol-producer/hermit` | hermit | `ci/delete-dead-parallel-gate-path` | 0.00 | no | - | YES |
| 267 | `ignored/w7-apps` | hermit | `fix/application-harness-typed-strict-verification` | 0.00 | YES | https://github.com/rrnewton/hermit/pull/2322 ⚠hooks-bypassed | YES |
| 268 | `ignored/det4-linuxboot/hermit` | hermit | `ci/dedicated-linux-boot-lane` | 0.00 | YES | https://github.com/rrnewton/hermit/pull/2320 | YES |
| 269 | `ignored/w8-gateprov/hermit` | hermit | `fix/merge-gate-verifier-registry-provisioning` | 0.00 | no | - | YES |
| 270 | `ignored/w1-pin` | hermit | `DETACHED` | 0.00 | no | - | YES |
| 271 | `ignored/operandA-1559/hermit` | hermit | `DETACHED` | 0.00 | no | - | YES |
| 272 | `hermit/ignored/prelude-fitness-drainall` | hermit | `DETACHED` | 0.00 | no | - | YES |
| 273 | `ignored/validate-producer/1623/hermit` | hermit | `land/1623-onto-a7933358` | 0.00 | no | - | YES |
| 274 | `ignored/reverie-pin-bump/hermit` | hermit | `feat/reverie-pin-bump-55f6876a` | 0.00 | no | - | YES |
| 275 | `ignored/land-1471/hermit` | hermit | `codex/backend-parity-rlimit-identity` | 0.00 | no | - | YES |
| 276 | `ignored/validate-producer/1549/hermit` | hermit | `DETACHED` | 0.00 | no | - | YES |
| 277 | `ignored/land-fchown-gap/hermit` | hermit | `dbi-file-metadata-fchown-gap` | 0.00 | no | - | YES |
| 278 | `hermit/ignored/land-clusters/hermit` | hermit | `rebase/pr-1380` | 0.00 | no | - | YES |
| 279 | `scratch/highband-rebase/hermit` | hermit | `drain/pr-1445` | 0.00 | no | - | YES |
| 280 | `scratch/au-plan-mech` | agent-utils | `codex/pr-landing-planner-mechanism-conflicts` | 0.00 | no | - | YES |
| 281 | `scratch/au-plan-archive` | agent-utils | `codex/pr-landing-planner-archive-plans` | 0.00 | no | - | YES |
| 282 | `scratch/reverie-kvm17` | reverie | `codex/kvm-round18-parity` | 0.00 | no | - | YES |
| 283 | `scratch/cgroups-optout-au` | agent-utils | `cgroups-opt-out-small-default` | 0.00 | no | - | YES |
| 284 | `.git/modules/liteinst2` | liteinst2 | `main` | 0.00 | no | - | YES |
| 285 | `scratch/recovered-agent-utils-primary-20260805/au-cap-land` | agent-utils | `codex/cgroups-cap-land` | 0.00 | no | - | YES |
| 286 | `scratch/au-estimator` | agent-utils | `codex/ci-dag-runner-dryrun-estimator` | 0.00 | no | - | YES |
| 287 | `scratch/au-defcap/wt` | agent-utils | `codex/small-default-cap-231b` | 0.00 | no | - | YES |
| 288 | `worktrees/codex-validatelock/liteinst2` | liteinst2 | `DETACHED` | 0.00 | no | - | YES |
| 289 | `worktrees/codex-emptybucket/liteinst2` | liteinst2 | `DETACHED` | 0.00 | no | - | YES |
| 290 | `worktrees/compat-envelope-program/liteinst2` | liteinst2 | `DETACHED` | 0.00 | no | - | YES |
| 291 | `worktrees/coord/liteinst2` | liteinst2 | `DETACHED` | 0.00 | no | - | YES |
| 292 | `worktrees/helper-validator/liteinst2` | liteinst2 | `DETACHED` | 0.00 | no | - | YES |
| 293 | `worktrees/ci-hub-trust/liteinst2` | liteinst2 | `DETACHED` | 0.00 | no | - | YES |
| 294 | `worktrees/sol-producer/liteinst2` | liteinst2 | `DETACHED` | 0.00 | no | - | YES |
| 295 | `scratch/liteinst2-clean-separation` | liteinst2 | `impl-liteinst-clean-separation` | 0.00 | YES | https://github.com/rrnewton/liteinst2/pull/24 | YES |
| 296 | `scratch/liteinst2-m1` | liteinst2 | `codex/liteinst-m5-stress` | 0.00 | no | - | YES |
| 297 | `hermit/ignored/pdeathsig` | hermit | `fix/sabre-info-host-timing-solo` | 0.00 | no | - | HOLD — timeouts (pid 3089765) |
| 298 | `worktrees/cc-coherent-timebase` | hermit | `cc-coherent-timebase` | 0.00 | no | - | NO - live agent — runner (pids 3390948, 3569148) |
| 299 | `worktrees/ratchet-kvm-envelope` | hermit | `ratchet-kvm/envelope` | 0.00 | no | - | NO - live agent — ratchet-kvm (pid 899610) |
| 300 | `/home/newton/temp/hermit-liteinst-artifact-repro` | hermit | `DETACHED` | 0.00 | no | - | HOLD — ncdu browser (pid 146478) - not an agent |
| 301 | `worktrees/au-wrkslots-fix` | ? | `DETACHED` | 0.00 | no | - | HOLD — slots (me) - holds agent-utils PR #69 |


## Totals

- worktrees measured: **296** (outermost candidates; 2 nested inside another candidate were folded in)
- **AUTHORITATIVE RECLAIM (single combined compsize, extents deduped): 396.2 GiB / 425.4 GB**
- same set, uncompressed (what du would roughly say): 1076.5 GiB — du overstates **2.72x**
- same set, referenced/logical: 1465.7 GiB
- sum of per-row figures: 449.2 GiB (inflated by shared extents; do not quote this)
