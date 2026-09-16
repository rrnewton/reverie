# Bounded paired execution — 2026-09-04

## Result and limits

The unchanged three seeded samples establish L2 repeat parity for **ptrace 3/3**
and **explicit host-hybrid LiteInst 3/3**, at Hermit
`4d8f866102882b6eeabdf99cc7e81433cc3c95c5` and Reverie
`320412c5967790939ebe405c73e394ffd9c41459`. All six first attempts returned
strict JSON `bitwise_parity=true`, `verdict=matched`, nonzero left/right INFO
counts, guest exit 0, and no signal. No retries or failures occurred. The
comparison is BitwiseInfoV1 canonical repeat parity, **not equality between
backends**. Log level INFO, relaxations none, IO-buffer comparison enabled.

Frozen 340 and cached subset 253 remain unchanged. Only three members were
measured per backend here; **337/340 remain unmeasured by this execution**.
No corpus-wide current success rate is inferred. Static labels remain included.
The requested in-process path was not executed and still needs >=306/340.
These results cannot qualify its architecture. The earlier setup-only artifact
`host-hybrid-execution.json` is historical: this new run supplies its missing
legacy artifact through the normal staging helper without rewriting history.

No first failure exists in this sample. Historical startup-map demotions were
not reproduced, but one repeat-pair per cell does not clear historical flakiness
or authorize editing red labels. Next implementation scope remains the separately
owned typed Detcore preload/CLI wiring and qualified in-process preemption,
not speculative fixes to these three now-observed hybrid samples. See
`architecture-audit.md`; no inactive Detcore DSO was substituted.

## Commands, resources, provenance

Raw root (outside product repo):
`/home/newton/work/dev-hermit/ignored/liteinst-host-hybrid-pairs-20260904`.
Run UTC interval: `2026-09-04T20:33:29.926287397Z` to
`2026-09-04T20:33:46.369486722Z` on `devbig014`, x86_64 Linux
`7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`.

Existing supported staging command is recorded literally in
`stage-legacy-runtime.json`: `scripts/stage-liteinst-runtime.sh dev` with the
external stable runtime path and own private target. It used locked dependencies,
`CARGO_NET_OFFLINE=true`, `CARGO_BUILD_JOBS=2`, and the checked pinned source.
No dependency fetch or product edit. It succeeded in about 114 seconds under the
900-second step cap; the helper generated the authentic adjacent revision marker.
`stage.log` retains admission, cgroup audit, duration and build completion;
intermediate Cargo progress was buffered by dagrun rather than separately saved.

Admission commands, executed from the assigned product slot:

```bash
DAGRUN_ENGINE=python /home/newton/work/dev-hermit/agent-utils/bin/dagrun run \
  --dag /home/newton/work/dev-hermit/experiments/liteinst_architecture_20260904/stage-legacy-runtime.json \
  --max-steps 1 --max-cpus 2 --max-mem 8G --admission 0 \
  --no-profile-feedback --run-timeout 960 --perf-dir "$PAIR_RAW/stage-profile"

DAGRUN_ENGINE=python /home/newton/work/dev-hermit/agent-utils/bin/dagrun run \
  --dag /home/newton/work/dev-hermit/experiments/liteinst_architecture_20260904/run-host-hybrid-pairs.json \
  --max-steps 1 --max-cpus 2 --max-mem 8G --admission 0 \
  --no-profile-feedback --keep-going --run-timeout 420 --perf-dir "$PAIR_RAW/pairs-profile"
```

The execution environment is retained in raw `execution-start.txt`:
`PAIR_RAW` is the raw root; `PAIR_HARNESS` and `HERMIT_BIN` are the actual
`test-harness` and `hermit` under
`/home/newton/work/dev-hermit/ignored/liteinst-detcore-isolated-20260904/target/debug`.
`HERMIT_LITEINST_RUNTIME=$PAIR_RAW/runtime/libreverie_liteinst.so`,
`E2E_RESULT_ROOT=$PAIR_RAW/e2e`, `E2E_BUILD_ROOT=$PAIR_RAW/guests`,
`E2E_KEEP_VERIFY_LOGS=1`, `HERMIT_LOG_MAX_BYTES=67108864`.
`RUST_LOG` and timeout multiplier overrides were unset. The official runner
compiled directly into per-cell fixtures, so the configured build root was unused.

Each exact `test-harness run` selector and exit capture is in
`run-host-hybrid-pairs.json`. `--include-manual` selects the existing cells,
without editing labels or execution flags. The official runner supplied
`--log info run --base-env=minimal --backend <backend> --strict --verify-strict
--verify --verify-json <report> --keep-logs --verify-log-dir <dir>` plus its
normal isolated workdir and guest environment. Full literal argv, environment,
cwd, output, status, timing, and report are in `e2e/<run_id>/results.jsonl`.
No ad-hoc Hermit invocation or safehermit double wrapping occurred.

Both admissions were GRANTED, with observed outer `memory.max=8589934592`,
swap disabled and `cpu.max=200000 100000`: bandwidth limits, not CPU pinning.
Six serial steps had 60-second outer caps. Official per-cell defaults stayed
57 wall seconds and 22 CPU seconds. The official retry facility permits two
attempts, but every cell succeeded on the first. Twelve retained verify logs
were found, with no truncation marker. Raw `evidence-check.txt` records the
independent typed-report/hash/ELF checks. No command remains running.

## Actual guest ELF identities

Official source recipes use `cc -std=c11 -O2 -g -Wall -Wextra -Werror`,
then existing cell cflags, absolute C source, and `-o <fixtures/program>`.
The two backend-parity guests add `-D_GNU_SOURCE`; ioctl adds no cflags.
Each pair's independently compiled artifacts is byte-identical. `file` and
`readelf -l` prove all three distinct guests dynamically linked with PT_INTERP
`/lib64/ld-linux-x86-64.so.2`, not a guess from manifest metadata:

| Guest | Source | Compiled ELF SHA-256 |
| --- | --- | --- |
| ioctl-fioclex | tests/c/ioctl_fioclex.c | cc423f5ebeb336b64b75c53d53c7f83e388580bf336bd1a18032c1a1eaea87e0 |
| o-tmpfile-anon | tests/backend-parity/fixtures/o_tmpfile_anon.c | 3b2f12ab7d43328b2115148ed4f1f3b4e26914455d6d811166e41b9c8973bdf2 |
| record-lock | tests/backend-parity/fixtures/record_lock.c | a29b1d9cb5e3b09481d206c5806c8cb00d4db9ebcf05962110bd7514d7c1584e |

This is a dynamic-only **sample** of three; it does not reclassify unbuilt frozen
rows. Runner `test_sha256` is the source digest, not the compiled ELF digest.
Raw `SHA256SUMS` joins sources, staging inputs, frozen artifacts, binary identity
metadata, runtime/marker, six result rows, six strict JSON reports, compiled
ELFs, captures and retained logs. Each result row's embedded report hash was
independently matched to the on-disk report. Binary/runtime hashes were unchanged
after execution. The binary's short embedded build SHA agrees with the checked
source and build logs; this is not a clean full-workspace validation receipt.

Works? YES for the six named repeat-verification observations, not intended
in-process completion. UX sensible? YES for these runs: current output explicitly
identifies LiteInst host-hybrid, retained log locations and comparison policy.
No product files, labels, task status, commits or remotes were changed.
