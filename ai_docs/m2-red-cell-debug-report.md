# Hermit M2 red-cell debugging record

## Immediate accounting and three bounded reproductions

This report currently contains **43 populated cell/probe-mode sections**,
representing **42 unique `(guest or cell, backend, mode)` identities**.  The
difference is `c-programs/kcmp-eperm` under DBT verify: one section preserves
the historical direct `no_result` probe and another preserves its
exact-current manifest requalification.  Thirty-one sections are manifest-cell
observations and twelve are direct or integration probes.

Conservatively classifying the 43 sections by their strongest recorded status
(exact-current uncommitted recoveries count as recovered evidence, not as
landed `main` behavior):

- **33 measured green or recovered false-red sections**;
- **3 unresolved measured-red sections**: `language-runtimes/node-v8-jit`
  under ptrace verify, `record_rs_sched_yield`, and the fork/kill/waitpid
  SIGCHLD control;
- **7 `no_result`, evidence-capability, or not-freshly-requalified sections**.

After merging the duplicate historical/current DBT `kcmp-eperm` identity, the
42 unique identities classify as 33 green/recovered, 3 measured red, and 6
`no_result`/capability/unqualified.  The seven section-level boundary entries
are DBT `fork-exec-pipeline`, historical DBT echo/cat/direct-kcmp, KVM
echo/true/cat; direct-kcmp merges with its green exact-current identity in the
unique count.  Every section contains measured
material and records a command or shared exact envelope, a verdict or precise
comparison boundary, and retained evidence; known identity or requalification
limits are stated explicitly.

### Validation anchor and current review boundary

The strongest combined uncommitted receipt is now exact for the final registry
lifecycle fix.  It is bound to dirty Hermit HEAD
`d601651a3a0f4865b72af50a9c5967eaf60c351a`, whole-worktree fingerprint
`455174bb78b7eba03376febc6245bfa11fffa54b8442cd74cf5d7655360567c4`,
and release-binary SHA-256
`73571dacd63ff68a32d3f3f7d8a00d26cc4cd9e9d1f3ca5f0fdac5bbb6d40342`.
The canonical summary is
`scratch/exec-final-validation-post-lifecycle-20260816-SOuxof/canonical-summary.tsv`
(SHA-256
`d353070bbb3626b861e1bf6ee022442569fbe9b1b04765549671739d41df4c66`).
The complete evidence manifest SHA-256 is
`42c456677832531dcee31fa004ef44c329a3e5820b273f9642e4421ed60b63db`.

| Probe | Result on fingerprint `455174bb...` |
| --- | --- |
| `system-utils/example-date` replay | matched, 509/509 INFO |
| `system-utils/example-devrand` replay | matched, 634/634 INFO |
| `system-utils/clock-exec-continuity` replay | matched, 268/268 INFO |
| `data-handling/shell-pipeline` replay | matched, 4227/4227 INFO |
| `poll` partial-copyout control | matched, 115/115 INFO |
| `ppoll` partial-copyout/timeout control | matched, 115/115 INFO |
| invalid-`nfds` `poll`/`ppoll` control | matched, 95/95 INFO |

All seven commands exited 0, reported `bitwise_parity=true`, had null
first-divergence fields, and together compared **5963/5963 INFO messages**.
The record format is exactly `0x10d`; older formats are rejected rather than
decoded under the new poll/ppoll/exec schemas.  Both lifecycle regressions
passed 1/1 (2/2 total), the Hermit library target passed 150/150, and format,
Clippy, and diff checks passed.

The full serial record/replay integration target passed **53/54**.  Its sole
failure was the already tracked `record_rs_sched_yield` shutdown INFO-order
divergence; an isolated rerun reproduced the same failure.  This is preserved
as a current measured red below rather than removed from the denominator.
Primary logs are `full-record-replay-serial.log` (SHA-256
`a8b72d24a00733633bc4511ffe7c146bfce3474d26fe503bc817fa104d5de982`)
and `sched-yield-rerun.log` (SHA-256
`a345b629c5106d71ae4e9640e9aaf031564749021b1a853713589c12bd5714d1`).

The lifecycle edit moves materialization-scope creation before registration
and stores controller-owned `OwnedFd` inode pins.  Independent review
**ACCEPTS the selected ptrace replay scope**: the former scope-leak and
inode-reuse HIGH findings are closed, the focused tests pass, and the selected
canonical replay set is 7/7 exact-current.

Where older ptrace replay sections say “exact-current,” that phrase means
exact for their retained measurement-time source and binary.  The anchor above
is authoritative for the final exec/poll/lifecycle selected set; unrelated DBT
and LiteInst anchors retain their own stated identities.

The accepted scope still has four explicit Linux-semantics boundaries:

| Boundary | Current behavior | Selected M2 effect |
| --- | --- | --- |
| non-UTF-8 root argv/environment | metadata lossily rewrites bytes, so replay can observe different data | none; selected inputs are ASCII/UTF-8 |
| cwd symlink absent from exec topology | replay can substitute a plain directory and lose parent/alias topology | none in the seven selected replay cells |
| redundant `/proc`/`dev-fd` separators or `.` | successful exec is rejected after the fact; recording fails closed with no valid replay | none; selected probes use canonical spellings |
| execute-only executable | snapshot reopening requires read access; recording fails closed with no valid replay | none; selected images are readable |

This is code-review acceptance, not landing authorization.  The new
`execve`/`execveat` record/replay support still requires
`AUTONOMOUS-BOT-IMPLEMENTED` at its dispatch/classification entry and
`TODO-HUMAN-REVIEW(PR-id)` at the determinization block, using the real
introducing PR number.  No PR ID should be fabricated before the PR exists.

The first two reproductions below are current unresolved failures.  The third
is an explicitly labeled **PRE-FIX SNAPSHOT** retained because it is a useful
fast demonstration of the now-repaired partial-copyout defect.  It must not be
run with the current uncommitted binary expecting a failure.

### 1. CURRENT ptrace red: `language-runtimes/node-v8-jit` verify

Expected runtime on this host: about 7 seconds, bounded to 45 seconds.  A fresh
run on dirty base `d601651a3a0f4865b72af50a9c5967eaf60c351a`, release-binary
SHA-256
`9e05cf590d2cdc08e4e2d1d93924ea6e0400869f16ab2489cc4c902aafa26c55`,
compared 91,056 versus 91,024 INFO messages and diverged at scheduler turn
2275 / virtual nanoseconds `1767227246419325000`.

```bash
r="$PWD/target/ignored/user-node-v8-$(date +%s)"
timeout --kill-after=10s 45s env \
  HERMIT_BIN="$PWD/target/release/hermit" E2E_RESULT_ROOT="$r" \
  E2E_RUN_ID=repro E2E_KEEP_VERIFY_LOGS=1 \
  ./ci/test_harness.sh run --lane portable --mode verify --backend ptrace \
  --test language-runtimes/node-v8-jit --include-manual
v="$r/runs/repro/language-runtimes-node-v8-jit-verify-ptrace/verify-1.json"
jq '{verdict,bitwise_parity,compared_log_messages,first_divergent_scheduler_turn,first_divergent_virtual_nanoseconds}' "$v"
l="${v%/verify-1.json}/verify-logs/verify-1"
target/release/hermit log-diff --no-color --limit 1 --syscall-history 8 \
  "$l"/run1_log_* "$l"/run2_log_*
```

Expected result: the harness exits 1.  Both runs first finish syscall 219,
`wait4(...)=ERESTARTSYS`.  At the first mismatch one run remains in
`InternalIOPolling` for `wait4`, while the other finishes syscall 220 as
`wait4(...)=Ok(49)`.  The exact turn/poll count may move, but the failure class
is host child-reap visibility during deterministic `wait4` polling, not an
instruction-value mismatch.  Fresh retained evidence:

- `target/ignored/m2-forensics-node/runs/node-v8-current-20260816/language-runtimes-node-v8-jit-verify-ptrace/verify-1.json`
  (SHA-256
  `113d572c60889b54c63aa7ad96c02f261d86605a29c00e4b4c958cdba493be05`);
- the adjacent `verify-logs/verify-1/run1_log_WMrSz` and
  `run2_log_7BEwl`.

No cleanup beyond the harness's normal process-group teardown is required.

### 2. CURRENT ptrace red: fork/kill/waitpid SIGCHLD control

Expected runtime: 0.03 seconds in the retained run, bounded to 30 seconds.
The tiny guest forks a child, sends it `SIGKILL`, and verifies `waitpid` reports
that signal.  Both guest runs exit 0; the canonical INFO ordering diverges.

```bash
timeout --preserve-status -k 5s 30s ./target/release/hermit \
  --log info --backend ptrace run --strict --verify --keep-logs \
  --verify-log-dir target/ignored/red-cell-sigchld-control-20260816T1300Z/logs \
  --verify-json target/ignored/red-cell-sigchld-control-20260816T1300Z/verdict.json \
  -- target/ignored/red-cell-sigchld-control-20260816T1300Z/fork_kill_waitpid_control
```

Expected result: exit 1, canonical divergence with 128/128 INFO messages,
first scheduler turn 5 and virtual nanoseconds `1767225600005050760`.  Both
streams match through `clone(...)=Ok(5)`.  At the first mismatch run 1 emits
`logically_kill` for child 5 while run 2 first finishes syscall 34,
`kill(5, SIGKILL)=Ok(0)`.  This is a child-death/SIGCHLD event-order divergence,
not a differing guest result.  Retained evidence root:
`target/ignored/red-cell-sigchld-control-20260816T1300Z/`; verdict SHA-256
`ec654dbe6cbc4e7518bc600e734dca6e7880f4e7f25176b2e969d63d32180c04`.
That is the retained run's exact first boundary; a repeat can move the first
mismatch later into `wait4` child-reap visibility.  No special cleanup is
required.

### 3. PRE-FIX SNAPSHOT (binary `9e05cf59...`): ptrace `poll` partial-`revents` copyout

This retained failure was produced on dirty base
`d601651a3a0f4865b72af50a9c5967eaf60c351a` by Hermit release-binary SHA-256
`9e05cf590d2cdc08e4e2d1d93924ea6e0400869f16ab2489cc4c902aafa26c55`.
Its authoritative artifact root is
`target/ignored/poll-red-cell-current-20260816T125558Z/timed/poll/`.  The
command below records the historical shape; do not substitute the current
`target/release/hermit` and still expect the red result.

Expected historical runtime: 0.07 seconds, bounded to 30 seconds.  The guest places two
`pollfd` records across a writable/read-only page boundary.  Linux writes
`POLLIN` to the first `revents`, faults on the second, and returns `EFAULT`.

```bash
d=$(mktemp -d "$PWD/target/ignored/user-poll-copyout.XXXXXX")
TMPDIR="$d" timeout --kill-after=3s 30s target/release/hermit \
  --log info --backend ptrace record start --strict --record-timeout 20 \
  --verify --verify-json "$d/verify.json" --data-dir "$d/data" \
  target/ignored/poll-red-cell-current-20260816T125558Z/revents_probe poll \
  >"$d/stdout" 2>"$d/stderr"
```

Historical expected result: exit 1, 99/99 canonical INFO messages, and first scheduler
turn 8 near virtual nanoseconds `1767225600004629625`.  The authoritative
guest-visible difference is earlier: recording returns `EFAULT` with
`revents0=1`, while replay returns the same `EFAULT` but leaves the initial
sentinel `revents0=4660` (`0x1234`).  The recorder stores only the error and
drops Linux's partial copyout, so replay cannot restore it.  Retained evidence:
`target/ignored/poll-red-cell-current-20260816T125558Z/timed/poll/`; verdict
SHA-256
`285949ffb7e00c30302f879f51dced6c735d537d1b6c2a5dfc403de70bd19fad`.
No special cleanup is required.  The uncommitted fix now passes the equivalent
canonical probe at 115/115 INFO messages; see the detailed poll section below.

This is the durable log-first record for cells examined during the M2 sanity
investigation.  It lives in the `dev-hermit/ai_docs` workspace rather than the
Hermit product repository.  Every cell section records the exact backend and
mode, the strongest observed verdict, the last matching event, the first
divergent event (or the precise boundary that prevented comparison), the
implicated syscall/instruction, the root-cause classification, and retained
evidence.

## Evidence rules

- INFO/DETLOG/COMMIT logs are examined before source.
- The first divergence is authoritative; later differences are treated as
  downstream noise.
- A timeout, panic, launch refusal, or missing evidence frame is recorded as
  `no_result`, not invented as a log divergence.
- Scorecard-policy red is distinguished from a measured execution failure.
- Non-KVM L2 requires bare `--strict --verify --verify-json`, nonzero canonical
  INFO on both sides, and `bitwise_parity: true`.  KVM is currently
  output/status-only and cannot establish L2.
- Commands, source SHAs, relaxations, and artifact paths are retained whenever
  available.  Historical observations are labeled when they predate the final
  dirty worktree.

## Forensic history: why the old high rates and current scorecard disagree

The apparent collapse is mostly a standards-and-denominator discontinuity,
amplified by work that was closed or left on branches without landing.  It is
not evidence that thousands of programs were freshly run and suddenly became
nondeterministic.

**Old 200-cell measurements (August 1).**  The retained full-corpus source,
`compat-envelope/SCORECARD-CURRENT.md`, records raw strict/verify outcomes of
156/200 for DBI, 130/200 for KVM, 164/200 for SaBRe, and 118/200 for LiteInst.
Its headline cross-backend percentages use only the 179 cells whose ptrace arm
passed: DBI 76.0% stdout parity / 86.6% determinism, KVM 62.6% / 72.1%, SaBRe
78.8% / 91.6%, and LiteInst 60.3% / 65.9%.  Those executions are real history,
but all 1,000 backend rows are `legacy-unqualified`, the parity observable is
the legacy stdout comparison, and qualified measured coverage is 0/179 for
every backend.  They therefore cannot be compared numerically with today's
full canonical INFO standard or cited as current L2.

**Current scorecard population.**  At landed Hermit
`302a1a9c0fde564db0292d1ea5cabc91343e79bc`, the scorecard has 5,376 cells:
170 green and 5,206 red.  Only 625 cells are enabled; 4,751 are disabled.
Equivalently, the red population is 455 enabled-red plus 4,751 disabled-red,
so **4,751/5,206 = 91.3% of all reds are disabled/unrun**, not 5,206 measured
runtime failures.  Anchors are
`ai_docs/evidence/verify-main-green-and-scorecard-20260815/README.md` and
`ai_docs/QUALIFICATION_TALLY_20260815.md`.

**Task-graph and landing forensics.**  On devbig014, the initiating snapshot
was `hermit` 5,020/5,348 closed and `hermit2` 950/1,047; a final live recheck
still showed 5,020/5,348 for `hermit` but had advanced to 1,023/1,050 for
`hermit2`, so these are moving databases rather than immutable receipts.  The
earlier local term search found 100/104 matching `hermit` tasks and 43/49
matching `hermit2` tasks already closed.  A separate durable audit found only
3/106 implemented-task claims both landed and fully meeting their stated goal
(`ai_docs/2026-08-04-implemented-task-closure-audit.md`).

Representative records show why “closed” is not a mechanical success signal:
`wire-tier-evidence-into-the-scorecard-so-a-green-cell-requires-evidence` is
closed with an IMPLEMENTED note for PR #94 at
`03264242aca464f24e476a6eb11635d538f18893`, followed by a note that the PR
could not be reopened because its source branch was deleted;
`sync-parent-before-any-landing` is closed while preserving a schema-version
collision between poll PR #2272 and ppoll PR #2264; and
`overnight-lane1-drive-cells-to-landing` is closed with multiple exact-head
validation attempts recorded as `TRUNCATED`/no verdict.  Those notes are useful
forensics, but their status fields do not prove merged behavior.

The devbig030 snapshot supplied by the remote forensic pass was `hermit1`
3,618/4,278 closed and active `hermit3` 530/692.  Of 33 implemented BACKLOG
non-lane tasks, 30 were not landed; four tasks were closed with unmerged PRs.
Only 16 of 75 open PRs were scorecard-relevant, and 68 claimed cell additions
collapsed to 60 unique cells after overlap.  Representative implementation
commits that exist locally but are not ancestors of `origin/main` are:

- `ba1eb2ad0e28d6cb2f5129dee55c4a2698a0bbc9`, DBT queued-signal resolution;
- `c2fbd504ade4798536bedc37c36d75b6043455bb`, KVM comparable L2 evidence;
- `441c86886521c884911ac28ca4d106d121d6d480`, `ppoll` partial-`revents`
  copyout on `EFAULT`.

Remote-only history also identified unlanded KVM RDTSC ref `5bd1b852`.
Durable backend records remain in
`ai_docs/DBT_CANONICAL_RERUN_11_20260815.md`,
`ai_docs/DBT_VERIFY_47_QUALIFICATION_20260815.md`, and
`ai_docs/QUALIFICATION_TALLY_20260815.md`; raw experiments remain in
`experiments/verify-strict-clean-control-divergence_20260806/`,
`experiments/detlog-stack-heap-parity-179_20260807/`, and
`experiments/kvm-verify-slot-screening_20260815/`.

**Causal synthesis.**  The old rates answer “did this legacy run/output check
pass on the 179-cell ptrace frontier?”; the current scorecard asks for a much
stronger evidence envelope over a 5,376-cell Cartesian inventory and paints
disabled cells red.  Meanwhile, substantial backend work remained unlanded or
was administratively closed without merge ancestry.  The manually measured
current unresolved failures in this report are much narrower:
child-death/SIGCHLD and `wait4` ordering, DBT fork/exec teardown, and KVM's
missing internal INFO comparator.  Record/replay partial syscall copyout was a
real Linux-semantics defect, but the current uncommitted implementation repairs
it and has canonical evidence below.  Many other red rows sampled here were
selection/evidence debt and turned green when actually run.

## Cell index

| Cell / probe | Backend / mode | Classification | Detailed section |
| --- | --- | --- | --- |
| `system-utils/date-nanoseconds` | ptrace / verify | measured regression, fixed | [section](#system-utilsdate-nanoseconds--ptrace-verify) |
| `c-programs/record-replay-file-state` | ptrace / verify | historical filesystem-precondition divergence; final canonical green | [section](#c-programsrecord-replay-file-state--ptrace-verify) |
| `system-utils/example-date` | ptrace / verify | unsupported scorecard red; measured canonical green | [section](#system-utilsexample-date--ptrace-verify) |
| `system-utils/example-devrand` | ptrace / verify | unsupported scorecard red; measured canonical green | [section](#system-utilsexample-devrand--ptrace-verify) |
| `language-runtimes/example-python-random` | ptrace / verify | unsupported scorecard red; measured canonical green | [section](#language-runtimesexample-python-random--ptrace-verify) |
| `determinism-stress-c/fork-tree` | ptrace / verify | policy-only red; measured canonical parity under declared relaxations | [section](#determinism-stress-cfork-tree--ptrace-verify) |
| `c-programs/syscall-file-io` | ptrace / verify | policy-only red; measured canonical parity under declared relaxations | [section](#c-programssyscall-file-io--ptrace-verify) |
| `backend-parity-c/file-io-roundtrip` | ptrace / replay | policy-only red; retained earlier snapshot L2, outside post-lifecycle seven | [section](#backend-parity-cfile-io-roundtrip--ptrace-replay) |
| `backend-parity-c/fork-exec-pipeline` | ptrace / replay | policy-only red; retained earlier snapshot L2, outside post-lifecycle seven | [section](#backend-parity-cfork-exec-pipeline--ptrace-replay) |
| `language-runtimes/example-python-random` | ptrace / replay | policy-only red; retained earlier snapshot L2, outside post-lifecycle seven | [section](#language-runtimesexample-python-random--ptrace-replay) |
| `system-utils/date-nanoseconds` | ptrace / replay | stale divergence rationale; retained earlier snapshot L2, outside post-lifecycle seven | [section](#system-utilsdate-nanoseconds--ptrace-replay) |
| `system-utils/random-device` | ptrace / replay | stale divergence rationale; retained earlier snapshot L2, outside post-lifecycle seven | [section](#system-utilsrandom-device--ptrace-replay) |
| `system-utils/clock-determinism` | ptrace / replay | unsubstantiated policy red; retained earlier snapshot L2, outside post-lifecycle seven | [section](#system-utilsclock-determinism--ptrace-replay) |
| `language-runtimes/node-v8-jit` | ptrace / verify | measured canonical divergence | [section](#language-runtimesnode-v8-jit--ptrace-verify) |
| `data-handling/shell-pipeline` | ptrace / replay | pre-fix `EEXIST` divergence; exact-current L2, 4227/4227 INFO | [section](#data-handlingshell-pipeline--ptrace-replay) |
| `system-utils/example-date` | ptrace / replay | pre-fix relative-exec divergence; exact-current L2, 509/509 INFO | [section](#system-utilsexample-date--ptrace-replay) |
| `system-utils/example-devrand` | ptrace / replay | pre-fix relative-exec divergence; exact-current L2, 634/634 INFO | [section](#system-utilsexample-devrand--ptrace-replay) |
| `system-utils/clock-exec-continuity` | ptrace / replay | pre-fix `/proc/self/exe` divergence; exact-current L2, 268/268 INFO | [section](#system-utilsclock-exec-continuity--ptrace-replay) |
| `record_rs_sched_yield` integration probe | ptrace / record-replay verify | current reproduced end-of-run scheduler/log-order divergence | [section](#record_rs_sched_yield--ptrace-recordreplay-integration) |
| `backend-parity-c/fork-exec-pipeline` | DBT / verify | measured divergence and later teardown no-result | [section](#backend-parity-cfork-exec-pipeline--dbt-verify) |
| `backend-parity-c/file-io-roundtrip` | DBT / verify | policy-only red; measured canonical L2 | [section](#backend-parity-cfile-io-roundtrip--dbt-verify) |
| `language-runtimes/example-python-random` | DBT / verify | inventory-only red; measured canonical L2 | [section](#language-runtimesexample-python-random--dbt-verify) |
| `/bin/echo hello` | DBT / verify | false red from evidence plumbing, fixed for selected scope | [section](#binecho-hello--dbt-verify) |
| `/bin/true` | DBT / verify | historical evidence-plumbing false red; exact-current green | [section](#dbt-bin-true) |
| `/bin/cat /dev/null` | DBT / verify | historical evidence-plumbing false red; exact guest not requalified | [section](#dbt-bin-cat) |
| `c-programs/kcmp-eperm` direct probe | DBT / verify | historical evidence-plumbing false red | [section](#dbt-direct-kcmp) |
| `backend-parity-c/host-identity` | DBT / exact-current verify | canonical L2, 108/108 INFO | [section](#dbt-exact-host-identity) |
| `c-programs/add-key-enosys` | DBT / exact-current verify | canonical L2, 93/93 INFO | [section](#dbt-exact-add-key) |
| `c-programs/cachestat-enosys` | DBT / exact-current verify | canonical L2, 93/93 INFO | [section](#dbt-exact-cachestat) |
| `c-programs/futex-waitv-enosys` | DBT / exact-current verify | canonical L2, 93/93 INFO | [section](#dbt-exact-futex-waitv) |
| `c-programs/get-robust-list-self` | DBT / exact-current verify | canonical L2, 93/93 INFO | [section](#dbt-exact-get-robust-list) |
| `c-programs/ioctl-fioclex` | DBT / exact-current verify | canonical L2, 121/121 INFO | [section](#dbt-exact-ioctl-fioclex) |
| `c-programs/kcmp-eperm` | DBT / exact-current verify | canonical L2, 95/95 INFO | [section](#dbt-exact-kcmp) |
| `c-programs/keyctl-enosys` | DBT / exact-current verify | canonical L2, 93/93 INFO | [section](#dbt-exact-keyctl) |
| `c-programs/listmount-enosys` | DBT / exact-current verify | canonical L2, 93/93 INFO | [section](#dbt-exact-listmount) |
| `/bin/echo hello` | KVM / verify | guest passes; policy-correct non-L2 | [section](#binecho-hello--kvm-verify) |
| `/bin/true` | KVM / verify | guest passes; output/status-only non-L2 | [section](#kvm-bin-true) |
| `/bin/cat /dev/null` | KVM / verify | guest passes; output/status-only non-L2 | [section](#kvm-bin-cat) |
| `custom/fork-kill-waitpid-control` | ptrace / verify | current canonical child-death event-order divergence | [section](#sigchld-fork-kill-waitpid-control) |
| `poll` partial-`revents` copyout control | ptrace / record-replay verify | pre-fix guest-visible divergence; exact-current matched 115/115 INFO | [section](#poll-partial-revents-copyout) |
| `ppoll` partial-`revents` copyout control | ptrace / record-replay verify | pre-fix replay `no_result`; exact-current matched 115/115 INFO | [section](#ppoll-partial-revents-copyout) |
| `poll`/`ppoll` invalid-`nfds` control | ptrace / record-replay verify | exact-current matched 95/95 INFO | [section](#poll-ppoll-invalid-nfds) |
| `system-utils/record-getpid` | LiteInst host hybrid / verify | exact-current canonical green under declared relaxations | [section](#liteinst-record-getpid) |

## `system-utils/date-nanoseconds` — ptrace verify

**Manifest and command.** `tests/e2e/manifests/system-utils.toml`, program
`tests/e2e/system-utils/date-nanoseconds.sh`, with effective arguments:

    --log=info run --backend=ptrace --strict --verify \
      --verify-json <report> --keep-logs --verify-log-dir <dir>

The harness receipt declares `no-virtualize-cpuid` and
`max-timeslice=disabled`.  The current generated cell is enabled and green.

**Observed verdicts.** Clean SHA
`1b98ccef5d5098bbc3e72fba689b1a026aaeb780` passed canonical comparison with
981/981 INFO messages.  Clean SHA
`d601651a3a0f4865b72af50a9c5967eaf60c351a` returned `no_result`: run 1
panicked before a second comparison log existed.  A dirty-tree fix on base
`d601651a...` then passed with 989/989 INFO messages; its binary SHA-256 was
`53991c288e70d482570686de83e8b76e61dc5f23493b34cda6edaf7c8bd92c59`.

**First bad boundary.** The last completed event was dettid 3 syscall 227,
`rt_sigprocmask(...)=Ok(0)`.  The next event was syscall 228,
`clone(CloneFlags(0x1200011), ..., 0x7ffff7f80a10, 0)`.  The normal fork child,
dettid/detpid 5, was incorrectly logged as a root thread, self-registered via
`CreateChildThread`, and waited on
`ParentContinue { parent: 5, child: 5 }`.  The parent then logged `dtid 5
cloned, continuing parent + register new thread`; the duplicate scheduler
priority insertion panicked at `detcore/src/tool_global.rs:1276` with
`assertion failed: old_prio.is_none()`.

The implicated DetTime was `syscalls:228`, `syscall_nanos:Some(7947500)`,
`rcbs:0`, `nondet_instrs:20`, and
`starting_micros:1767225600000000`.  There was no post-divergence COMMIT.  The
good path instead commits child `MemAddrSpace(DetPid(5))` at turn 9, parent
`ParentContinue { parent: 3, child: 5 }` at turn 10, and finishes clone as
`Ok(5)`.

**Root cause and status.** The first-bad commit replaced authoritative
`guest.is_root_thread()` classification with an equality heuristic.  A normal
fork-child leader satisfies `dettid == detpid == tool_detpid`, so it was
registered twice.  The fix restores backend-authoritative root classification
and retains only a narrowly gated DBT-root fallback.  The retained fixed
`detcore/src/lib.rs` SHA-256 is
`0f342710e41e40c6d8fc973f2716f372018a4263ecb856141f55cda3f1c84cc7`.
This regression is fixed; the cell now passes ptrace L2 under the declared
harness relaxations.

**Artifacts.** `ignored/compat-envelope/m2-green-nondbt-d601-U9SHD6LM/`:

- `bisect.log`
- `diagnostic-date-trace/verify-logs/run1_log_TV6O5` (SHA-256
  `d6a225f4d9facdb46d2c7d9108c03b3f81d5de82e5df30f5ab369e5b409b7211`)
- `diagnostic-date-trace/stderr`
- `regression-bad-d601-bare-identical/verify.json`
- `regression-good-1b98-canonical/verify.json`
- `fixed-date-nanoseconds-final/results.jsonl`

The fixed verification report is
`ignored/e2e/runs/local-1786866351-220595/system-utils-date-nanoseconds-verify-ptrace/verify-1.json`
(SHA-256
`5a40e1e92d8800a6b568e427908e663aacefa1b5633b7617fde5c6df3d5fb106`),
with logs under `verify-logs/verify-1/`.

## `c-programs/record-replay-file-state` — ptrace verify

**Old red status and historical first divergence.**  The scorecard row was
enabled/red with no observations, while the manifest enabled ptrace but had
`ci=false` without a reason.  Unlike the policy-only reds below, a retained
historical run did contain a real log difference.  At clean source SHA
`5738e1dcb38293ec26762d3c62e21b145e39e494`, binary SHA-256
`ec04a2d3c226bd89e2e816a919d65bb933c564bc47c99117f6987d4cf63a9825`,
INFO message 94 and syscall 36 were the first divergence:

    run 1: unlink("/var/tmp/hermit-record-file-state/data") = Ok(0)
    run 2: unlink("/var/tmp/hermit-record-file-state/data") = Err(ENOENT)

Syscall 37, `rmdir("/var/tmp/hermit-record-file-state")`, consequently
diverged at message 96 with the same `Ok(0)` versus `ENOENT` shape.  The prior
completed syscalls matched.  The guest deliberately ignores those initial
cleanup return values; a stale path existed before run 1, run 1 removed it,
and run 2 therefore observed a different external filesystem precondition.
Hermit does not promise to determinize a changing host filesystem.  This was
not a scheduler or instruction divergence.  Five serial reruns at the same
clean SHA and binary passed the historical comparator.

**Final green command and verdict.**  The exact-current harness command was:

    E2E_RESULT_ROOT="$PWD/target/ignored/final-replay-validation" \
      E2E_RUN_ID=verify-record-replay-file-state \
      ./ci/test_harness.sh run --lane portable --mode verify \
      --backend ptrace --test c-programs/record-replay-file-state --ci-only

The effective Hermit command used INFO logging, ptrace, `run --strict
--verify --verify-json`, and the manifest relaxations
`no-virtualize-cpuid` and `max-timeslice=disabled`.  The final report is green:
`verified=true`, `bitwise_parity=true`, `verdict=matched`, and 248/248
canonical INFO messages.  Both first-divergent fields are null.  Thus the
final pair has no syscall, instruction, scheduler-turn, or virtual-time
divergence.  Because relaxations are present, this is canonical parity under
the declared envelope rather than a no-relaxation L2 claim.

**Final identity and artifacts.**  Hermit base SHA
`d601651a3a0f4865b72af50a9c5967eaf60c351a`, dirty source tree, executable
SHA-256
`09227dd8a8a5cd28f4561f6786cc164661aa824ea330c68160a7a56abe03fcdf`,
and test-source SHA-256
`b3766729d2d3b320e72c72ff2b09e07bbf121a8632e94b7c2088c566d1c884ce`.
Final evidence:

- `target/ignored/final-replay-validation/verify-record-replay-file-state/results.jsonl`
- `target/ignored/final-replay-validation/runs/verify-record-replay-file-state/c-programs-record-replay-file-state-verify-ptrace/verify-1.json`

Historical failure and five reruns are under
`worktrees/triage-state-cells/hermit/ignored/e2e/triage-state-cells/`.

## `system-utils/example-date` — ptrace verify

**Why it was red.**  The scorecard row was enabled/red with an empty
observation list.  The manifest enabled ptrace for verify but had `ci=false`
and no failure rationale.  No retained failing syscall or instruction
supported the red status; this was a bookkeeping/policy red.

**Command and verdict.**  The retained diagnostic command was:

    HERMIT_BIN=target/release/hermit ./tests/manifest-cli.rs run \
      system-utils/example-date --mode verify --backend ptrace --log info \
      -- --verify-strict

It used `no-virtualize-cpuid` and `max-timeslice=disabled`.  The canonical
report matched 704/704 INFO messages with guest exit 0.  The first divergent
scheduler turn and virtual nanoseconds are null, and the retained log says no
substantive differences were found.  There is no syscall or instruction
divergence for this measured pair.  This is canonical parity under explicit
relaxations, not a no-relaxation L2 claim.

**Identity limitation and artifacts.**  The diagnostic ran before the final
rebase, at dirty base `bbafa443315c42d441bcb388f9608a27943e81fc` using
`target/release/hermit`; that early probe did not capture the executable
digest, so it is diagnostic rather than immutable exact-head evidence.  The
guest `examples/date.sh` content SHA-256 was
`1ee206523805af6daaec13126611c237aa9deb9ce02b079dbdd1b1113c65a097`.
Artifacts:

- `ignored/e2e-commands/work/system-utils-example-date/captures/verify.json`
- `/tmp/ptrace-verify-strict-system-utils-example-date.info`

## `system-utils/example-devrand` — ptrace verify

**Why it was red.**  The scorecard row was enabled/red with no observations;
the manifest enabled ptrace but set `ci=false` without a failure rationale.
No failing log backed the red status.

**Command and verdict.**  The retained command was:

    HERMIT_BIN=target/release/hermit ./tests/manifest-cli.rs run \
      system-utils/example-devrand --mode verify --backend ptrace --log info \
      -- --verify-strict

With `no-virtualize-cpuid` and `max-timeslice=disabled`, canonical comparison
matched 830/830 INFO messages and the guest exited 0.  Both first-divergence
fields are null; there is no measured syscall, instruction, scheduler, or
virtual-time divergence.  This is a relaxed-envelope canonical result.

**Identity limitation and artifacts.**  Dirty base
`bbafa443315c42d441bcb388f9608a27943e81fc`; the early probe did not retain
the Hermit binary digest.  Guest `examples/devrand.sh` content SHA-256:
`00b605c8021ff67eb7d1615275b828b970b31f7715316ed363a3bd6ce3500f6c`.
Artifacts:

- `ignored/e2e-commands/work/system-utils-example-devrand/captures/verify.json`
- `/tmp/ptrace-verify-strict-system-utils-example-devrand.info`

## `language-runtimes/example-python-random` — ptrace verify

**Why it was red.**  The scorecard row was enabled/red with no observations;
the manifest listed ptrace but had `ci=false` and no recorded failure.  The
red was not supported by a failing event.

**Command and verdict.**  The retained command was:

    HERMIT_BIN=target/release/hermit ./tests/manifest-cli.rs run \
      language-runtimes/example-python-random --mode verify \
      --backend ptrace --log info -- --verify-strict

With `no-virtualize-cpuid` and `max-timeslice=disabled`, canonical comparison
matched 3305/3305 INFO messages, guest exit 0.  Both divergence fields are
null and no syscall or instruction divergence exists in this pair.  This is
canonical parity under relaxations.

**Identity limitation and artifacts.**  Dirty base
`bbafa443315c42d441bcb388f9608a27943e81fc`; no executable digest was retained
for this early diagnostic.  Guest `examples/rand.py` content SHA-256:
`e1a32fd938403f394f36555be180f2b17dd497859f28e9aa9ac0dadcd0372343`.
Artifacts:

- `ignored/e2e-commands/work/language-runtimes-example-python-random/captures/verify.json`
- `/tmp/ptrace-verify-strict-language-runtimes-example-python-random.info`

## `determinism-stress-c/fork-tree` — ptrace verify

**Why it was red.**  The cell was enabled/red with an empty observation list,
but its manifest had `ci=false`, so it was absent from the expected CI plan.
There was no retained failed syscall, instruction, or comparator result behind
the red row.  This was a selection-policy red, not evidence that the guest did
not run.

**Exact command.**  The fresh retained run used:

    timeout --kill-after=10s 150s env \
      HERMIT_BIN="$PWD/target/release/hermit" \
      E2E_RESULT_ROOT="$PWD/target/ignored/m2-red-cell-debug" \
      E2E_RUN_ID=ptrace-fork-tree-logs-20260816 \
      E2E_KEEP_VERIFY_LOGS=1 \
      ./ci/test_harness.sh run --lane portable --mode verify \
      --backend ptrace --test determinism-stress-c/fork-tree --include-manual

The effective Hermit command was INFO-level ptrace `run --strict --verify
--verify-json --keep-logs --verify-log-dir`.  The portable manifest declared
`no-virtualize-cpuid` and `max-timeslice=disabled`; there were no other
relaxations.

**Verdict and divergence boundary.**  Both guests exited 0.  Canonical
comparison and a separate `hermit log-diff --no-color --syscall-history 5`
both matched all 2990/2990 INFO messages.  The JSON reports
`bitwise_parity=true`, and both first-divergence fields are null.  Therefore
there is no divergent syscall, instruction, scheduler turn, or virtual time in
this measured pair.  This is canonical parity under the two declared
portable-lane relaxations, not a no-relaxation claim.

The pair exercised 13 processes, nested forks, 100 identity syscalls per
process, child/grandchild exit-status validation, and `wait4`.  Both streams
ended after syscall 146, `write(...)=Ok(113)`, followed by COMMIT turn 71 for
process exit.  Each run used 72 scheduler turns and finished at virtual time
`1767225613.784125000s`.

**Identity, artifacts, and cleanup.**  Hermit base
`d601651a3a0f4865b72af50a9c5967eaf60c351a`, dirty source tree; executable
SHA-256
`9597f0b6a9b407a3eeda2c572c9e903ae062310ac1d26621bccdb7cf2b59a22c`;
guest-source SHA-256
`04cd48f271f152353ca330c0cb703b5e7cc2012397c0c6649db05f9ff176b6c2`;
compiled fixture SHA-256
`d25044678ab9cf4881d86d4d5f1f2f0f82f9271bae4c77d5091fe2bf532cbb37`.
Primary retained evidence:

- `target/ignored/m2-red-cell-debug/ptrace-fork-tree-logs-20260816/results.jsonl`
  (SHA-256
  `cb3370adfa1315dfe0cd64cacb4ed7157c611ee4ab98c3581115eebc11d1016f`)
- `target/ignored/m2-red-cell-debug/runs/ptrace-fork-tree-logs-20260816/determinism-stress-c-fork-tree-verify-ptrace/verify-1.json`
  (SHA-256
  `afc5db53ca00e4b86cd49b7a474235286b930c940380bda282f950ae5a19bf50`)
- the same directory's `verify-logs/verify-1/run1_log_yvkh6` and
  `run2_log_aD2R0` (SHA-256 respectively
  `04ba021565afbdfc349eaff0c0c23e8a6f3a909db61910811c1bd6c18ae7d1ed`
  and
  `a81ac1047a159d469094be554c04c46a3408ba635e04a9082a4c09948566335f`)
- `verify-logs/verify-1/manual-log-diff.json` (SHA-256
  `e7b230439e6fcb7b55714c38c433e372bef316104ce95ba7fa5c431edc6af560`)

The raw log hashes differ because they retain real wall-clock prefixes; the
canonical comparator matched their full INFO remainder.  A post-run process
check found no matching Hermit or guest process.

## `c-programs/syscall-file-io` — ptrace verify

**Why it was red.**  This row was likewise enabled/red with no observations
but excluded from the expected plan by `ci=false`.  No failure rationale or
failed event existed.  The red was policy-only.

**Exact command.**  The fresh retained run used:

    timeout --kill-after=10s 150s env \
      HERMIT_BIN="$PWD/target/release/hermit" \
      E2E_RESULT_ROOT="$PWD/target/ignored/m2-red-cell-debug" \
      E2E_RUN_ID=ptrace-syscall-file-io-logs-20260816 \
      E2E_KEEP_VERIFY_LOGS=1 \
      ./ci/test_harness.sh run --lane portable --mode verify \
      --backend ptrace --test c-programs/syscall-file-io --include-manual

The effective envelope was INFO-level ptrace `run --strict --verify
--verify-json --keep-logs --verify-log-dir`, with the declared
`no-virtualize-cpuid` and `max-timeslice=disabled` relaxations only.

**Verdict and divergence boundary.**  Both guests exited 0; canonical verify
and the independent log diff matched 155/155 INFO messages, with
`bitwise_parity=true` and null first-divergence fields.  No syscall,
instruction, schedule, or virtual-time divergence exists in this pair.  The
matched path includes file creation, write/readback, `fallocate`, truncation,
two renames, `symlinkat`, `readlinkat`, unlinking, and expected `ENOENT`
results.  Both runs ended after syscall 59, `write(...)=Ok(27)`, then COMMIT
turn 7 for exit; each used eight scheduler turns and ended at virtual time
`1767225600.501125000s`.

**Identity, artifacts, and cleanup.**  The Hermit base, dirty-tree state, and
executable are the same as the preceding cell.  Guest-source SHA-256 is
`40185202e631dd418e2e812a6b573199bae792170126552ed542f8cf79767299`;
compiled fixture SHA-256 is
`87a3bb19d275a40828a3549df8ddc270c3f9f80ca461dfc3202cf39457d0c5f7`.
Primary evidence:

- `target/ignored/m2-red-cell-debug/ptrace-syscall-file-io-logs-20260816/results.jsonl`
  (SHA-256
  `0c07f4e16c339301cea1978089dc4bba1ad532a996d2d9b7032442cbf16e973a`)
- `target/ignored/m2-red-cell-debug/runs/ptrace-syscall-file-io-logs-20260816/c-programs-syscall-file-io-verify-ptrace/verify-1.json`
  (SHA-256
  `e18de18b5d63131da5af9f4c85fd9c08b94215af44cd80593c073a23749e715d`)
- the same directory's `verify-logs/verify-1/run1_log_INCqk` and
  `run2_log_wHV6W` (SHA-256 respectively
  `7a44af663bc63acf97776b5a44535bb3c6297b132d6f74589f8b75b0202eea93`
  and
  `28a035259da364ec3709255b56c3cc1b26734c9865c7a3d622e60d6b66797d6c`)
- `verify-logs/verify-1/manual-log-diff.json` (SHA-256
  `9bdba358cee4489133f783edcc95e7a875215d8d1b5afd3328acb5f61a9b29c9`)

Again, only the real wall-clock prefixes make the retained raw-log digests
different.  No matching Hermit or guest process remained after the run.

## `backend-parity-c/file-io-roundtrip` — ptrace replay

**Why it was red.**  The scorecard row was disabled/red.  Its manifest said
only that record/replay qualification was separate from strict verification;
it named no failed syscall or instruction.  This was policy exclusion, not a
measured runtime failure.

**Command and verdict.**  Exact-current command:

    E2E_RESULT_ROOT="$PWD/target/ignored/final-replay-validation" \
      E2E_RUN_ID=replay-file-io-roundtrip ./ci/test_harness.sh run \
      --lane portable --mode replay --backend ptrace \
      --test backend-parity-c/file-io-roundtrip --ci-only

The effective envelope was INFO logging and `record start --strict --verify
--verify-json --record-timeout 90`.  No relaxations were declared.  The record
and replay streams matched 122/122 canonical INFO messages, guest exit 0, with
both first-divergence fields null.  No syscall or instruction divergence
exists; this is L2 for this named ptrace cell.

**Identity and artifacts.**  Dirty base `d601651a...`, executable SHA-256
`09227dd8a8a5cd28f4561f6786cc164661aa824ea330c68160a7a56abe03fcdf`,
test-source SHA-256
`4f4cec85cc5d11a52a031bd0f469abed5d687db10f568e130d9bd7dc07b4d89f`.
Artifacts:

- `target/ignored/final-replay-validation/replay-file-io-roundtrip/results.jsonl`
- `target/ignored/final-replay-validation/runs/replay-file-io-roundtrip/backend-parity-c-file-io-roundtrip-replay-ptrace/verify-1.json`

## `backend-parity-c/fork-exec-pipeline` — ptrace replay

**Why it was red.**  The disabled/red row carried only the generic policy that
record/replay qualification was separate from strict verification.  There was
no ptrace replay failure attached to it.

**Command and verdict.**  Exact-current command:

    E2E_RESULT_ROOT="$PWD/target/ignored/final-replay-validation" \
      E2E_RUN_ID=replay-fork-exec-pipeline ./ci/test_harness.sh run \
      --lane portable --mode replay --backend ptrace \
      --test backend-parity-c/fork-exec-pipeline --ci-only

The INFO-level strict record/replay comparison matched 203/203 canonical INFO
messages, exit 0.  Both divergence fields are null and there was no syscall,
instruction, scheduler-turn, or virtual-time divergence.  No relaxations were
declared; this is L2 for this named ptrace cell.

**Identity and artifacts.**  Dirty base `d601651a...`, executable SHA-256
`09227dd8a8a5cd28f4561f6786cc164661aa824ea330c68160a7a56abe03fcdf`,
test-source SHA-256
`445aec142be46b866c33711b40ebfae1c84c38e6a5853e35dad313e53f86e750`.
Artifacts:

- `target/ignored/final-replay-validation/replay-fork-exec-pipeline/results.jsonl`
- `target/ignored/final-replay-validation/runs/replay-fork-exec-pipeline/backend-parity-c-fork-exec-pipeline-replay-ptrace/verify-1.json`

## `language-runtimes/example-python-random` — ptrace replay

**Why it was red.**  The disabled/red policy claimed that strict verification
already owned the examples-directory parity contract.  It did not record a
replay failure.

**Command and verdict.**  Exact-current command:

    E2E_RESULT_ROOT="$PWD/target/ignored/final-replay-validation" \
      E2E_RUN_ID=replay-example-python-random ./ci/test_harness.sh run \
      --lane portable --mode replay --backend ptrace \
      --test language-runtimes/example-python-random --ci-only

The INFO-level strict record/replay streams matched 2056/2056 canonical INFO
messages, exit 0.  Both first-divergence fields are null.  No relaxations were
declared and no syscall or instruction divergence exists; this is L2 for this
named ptrace replay cell.

**Identity and artifacts.**  Dirty base `d601651a...`, executable SHA-256
`09227dd8a8a5cd28f4561f6786cc164661aa824ea330c68160a7a56abe03fcdf`.
The harness direct-command identity SHA-256 was
`ef629b34dd6df74d063e327f7f49a534cd51931621267420f40a305b36ea8c75`.
Artifacts:

- `target/ignored/final-replay-validation/replay-example-python-random/results.jsonl`
- `target/ignored/final-replay-validation/runs/replay-example-python-random/language-runtimes-example-python-random-replay-ptrace/verify-1.json`

## `system-utils/date-nanoseconds` — ptrace replay

**Why it was red.**  The disabled/red manifest prose claimed replay diverged
while reading timezone files, but no retained failing receipt or first
divergence supported that sentence.  It was stale policy for the exact-current
binary.

**Command and verdict.**  Exact-current command:

    E2E_RESULT_ROOT="$PWD/target/ignored/final-replay-validation" \
      E2E_RUN_ID=replay-date-nanoseconds ./ci/test_harness.sh run \
      --lane portable --mode replay --backend ptrace \
      --test system-utils/date-nanoseconds --ci-only

The INFO-level strict record/replay comparison matched 871/871 canonical INFO
messages, exit 0.  Both first-divergence fields are null; the current pair has
no timezone-read, syscall, instruction, scheduler, or virtual-time divergence.
No relaxations were declared; this is L2 for the named ptrace replay cell.

**Identity and artifacts.**  Dirty base `d601651a...`, executable SHA-256
`09227dd8a8a5cd28f4561f6786cc164661aa824ea330c68160a7a56abe03fcdf`,
test-source SHA-256
`0f3bd1eeb3d13d75b7d11db3ca773bb0dc200b1e201a4b81c8e65a4a58b02b13`.
Artifacts:

- `target/ignored/final-replay-validation/replay-date-nanoseconds/results.jsonl`
- `target/ignored/final-replay-validation/runs/replay-date-nanoseconds/system-utils-date-nanoseconds-replay-ptrace/verify-1.json`

## `system-utils/random-device` — ptrace replay

**Why it was red.**  The disabled/red policy claimed that the `hexdump`
process tree diverged during replay, but no retained failing log or divergence
coordinate supported that claim.  It is stale for the exact-current binary.

**Command and verdict.**  Exact-current command:

    E2E_RESULT_ROOT="$PWD/target/ignored/final-replay-validation" \
      E2E_RUN_ID=replay-random-device ./ci/test_harness.sh run \
      --lane portable --mode replay --backend ptrace \
      --test system-utils/random-device --ci-only

The INFO-level strict record/replay streams matched 886/886 canonical INFO
messages, exit 0.  Both divergence fields are null and no syscall or
instruction divergence exists.  No relaxations were declared; this is L2 for
this named ptrace replay cell.

**Identity and artifacts.**  Dirty base `d601651a...`, executable SHA-256
`09227dd8a8a5cd28f4561f6786cc164661aa824ea330c68160a7a56abe03fcdf`,
test-source SHA-256
`61321a5e85b9ab248bd3b4d01a8678bff01b0674563c74bce763e51f4b1da4cf`.
Artifacts:

- `target/ignored/final-replay-validation/replay-random-device/results.jsonl`
- `target/ignored/final-replay-validation/runs/replay-random-device/system-utils-random-device-replay-ptrace/verify-1.json`

## `system-utils/clock-determinism` — ptrace replay

**Why it was red.**  The disabled/red policy said only that a clock replay
divergence was tracked separately.  It named no failing syscall, instruction,
or retained artifact.  The current execution disproves a current divergence
for this exact binary and guest.

**Command and verdict.**  Exact-current command:

    E2E_RESULT_ROOT="$PWD/target/ignored/final-replay-validation" \
      E2E_RUN_ID=replay-clock-determinism ./ci/test_harness.sh run \
      --lane portable --mode replay --backend ptrace \
      --test system-utils/clock-determinism --ci-only

The INFO-level strict record/replay streams matched 123/123 canonical INFO
messages, exit 0.  Both first-divergence fields are null.  There is no clock
syscall, nondeterministic instruction, scheduler, or virtual-time divergence
in the measured pair.  No relaxations were declared; this is L2 for the named
ptrace replay cell.

**Identity and artifacts.**  Dirty base `d601651a...`, executable SHA-256
`09227dd8a8a5cd28f4561f6786cc164661aa824ea330c68160a7a56abe03fcdf`,
test-source SHA-256
`f4583495bd8dcceef783dd175d28c32c91891e6f62c9b24ce7a7c285ab0a1019`.
Artifacts:

- `target/ignored/final-replay-validation/replay-clock-determinism/results.jsonl`
- `target/ignored/final-replay-validation/runs/replay-clock-determinism/system-utils-clock-determinism-replay-ptrace/verify-1.json`

## `language-runtimes/node-v8-jit` — ptrace verify

**Manifest and command.** `tests/e2e/manifests/language-runtimes.toml`, program
`tests/e2e/language-runtimes/node-v8-jit.sh`, using the same canonical ptrace
verify arguments and harness relaxations as the preceding cell.  The generated
cell is enabled/red, while the manifest deliberately has `ci=false` with the
recorded `wait4` divergence reason.  It is both policy-unselected and measured
red.

**Verdict and first divergence.** Both guests exited 0, but canonical
comparison failed with 90,894 versus 90,878 INFO messages.  The JSON identifies
scheduler turn 2131 and virtual time `1767227246351825000ns` as the first
divergent point.  Before that point, child detpid 49 exited, SIGCHLD interrupted
dettid 25, syscall 219 `wait4(...)` returned `ERESTARTSYS`, and the restarted
wait entered `InternalIOPolling`.  Both runs committed turn 2131 for dettid 25
at `1_767_227_246.351_825_000s`.

At INFO message 80,454, run 1 emitted `NONCOMMIT turn 2132`, dettid 25,
`poll_attempt:15`, `fyi:"wait4"`; run 2 instead finished syscall 220,
`wait4(-1,...)=Ok(49)`.  Run 1 did not reap pid 49 until COMMIT turn 2147.  The
first divergence is therefore syscall-level `wait4` readiness, not a known
instruction divergence.

**Root cause and status.** `handle_wait4` converts blocking waits into host
`WNOHANG` retries controlled by deterministic `InternalIOPolling`.  Logical
child exit and SIGCHLD are deterministic, but physical kernel visibility of
the reapable child is not; one run observed it sixteen scheduler turns later.
The fix must gate completion on deterministic child/waitable state rather than
host polling.  This remains unresolved and is independent of the fixed root
registration bug.

**Source and artifacts.** Dirty tree on base
`d601651a3a0f4865b72af50a9c5967eaf60c351a`, binary SHA-256
`7de376628fb7a147254a43066c9915095440ea9569c4f5d77822537cf64556b5`.
Evidence is under
`ignored/compat-envelope/m2-green-nondbt-d601-U9SHD6LM/fixed-node-v8-jit-isolated/`:

- `results.jsonl`
- `cell-artifacts/language-runtimes-node-v8-jit-verify-ptrace/verify-1.json`
  (SHA-256
  `25ad55ca0e6a527e4b7035637102bb23df6cfb4d616d733653516e4dd66ab0d0`)
- `.../captures/verify-1.stderr`
- `.../verify-logs/verify-1/run1_log_Jgvp4`
- `.../verify-logs/verify-1/run2_log_sAFFR`

## `data-handling/shell-pipeline` — ptrace replay

**Manifest and command.** `tests/e2e/manifests/data-handling.toml`, generated
command:

    target/release/hermit --log=info --backend ptrace record start --strict \
      --verify --verify-json <verify.json> --data-dir <recording> \
      --record-timeout 60 -- tests/e2e/data-handling/shell-pipeline.sh --run

The environment fixed `LC_ALL=C` and `TZ=UTC`.  The scorecard marks replay
disabled/red because verify nominally owns the fixture, but this manual run
proved a real replay failure.

**First divergence.** Replay diverged on thread 7 at syscall event 58 and the
bounded command eventually exited 124.  The last matching sequence was the
`/usr/bin/mkdir -p` child calling `mkdir("/tmp")` and then
`mkdir("hermit-e2e", 0777)`.  Recording next expected
`mkdir("shell-pipeline", 0777)`; replay instead issued `write(2,...,7)` and
entered mkdir's error path.  The retained replay report contains no scheduler
turn, virtual time, or RIP.

**Root cause and uncommitted fixed status.** Replay initially created only `/tmp` and
`/var/tmp`.  A recorded `mkdir(...)=EEXIST` was returned without physically
materializing the directory because side-effect injection ran only for a
recorded success.  `mkdir -p` therefore believed `hermit-e2e` existed although
it was absent from the replay root.

The fix adds a record/replay `MkdirEvent` containing the original result and a
fact stating whether `EEXIST` referred to an actual directory.  Recording
classifies the final component with a temporary
`O_PATH|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC` descriptor.  Replay materializes the
complete missing path inside the confined replay root only for that recorded
directory case, refuses to replace or follow regular files and symlinks, and
still returns the recorded `EEXIST` to the guest.

An earlier uncommitted validation command was:

    art=target/ignored/replay-mkdir-eexist-exact-current.CziBZ3
    guest_tmp=$(mktemp -d /tmp/hermit-shell-pipeline-exact-current.XXXXXX)
    mkdir -p "$guest_tmp/guest-tmp"
    LC_ALL=C E2E_TMPDIR="$guest_tmp/guest-tmp" timeout 90s \
      target/release/hermit record start --strict \
      --data-dir "$art/recording" --verify \
      --verify-json "$art/verdict.json" -- \
      tests/e2e/data-handling/shell-pipeline.sh --run

With CLI logging default/unset and no relaxations, that earlier snapshot exited
0 and matched 4215/4215 canonical INFO messages.  The post-lifecycle
exact-current validation used INFO logging and the same strict, no-relaxation
record/replay envelope.  It exited 0 and matched **4227/4227** canonical INFO
messages with `bitwise_parity=true`, guest exit 0, and null scheduler-turn and
virtual-time divergence fields.  This is L2 for this exact ptrace replay cell
at fingerprint `455174bb...` and release binary `73571dac...`.

**Publication boundary shared by the exec/mkdir fixes below.**  The completed
receipt binds to dirty Hermit HEAD
`d601651a3a0f4865b72af50a9c5967eaf60c351a`, whole-worktree fingerprint
`455174bb78b7eba03376febc6245bfa11fffa54b8442cd74cf5d7655360567c4`,
and release-binary SHA-256
`73571dacd63ff68a32d3f3f7d8a00d26cc4cd9e9d1f3ca5f0fdac5bbb6d40342`.
The implementation is uncommitted and is not on `main`; audit-tag/PR-id
finalization is pending the eventual PR.  Independent review accepts the
selected ptrace replay implementation after the lifecycle repair, subject to
the explicit limitations in the top review boundary.

**Source and artifacts.** Observed source HEAD
`fa7c2c8f1818f3b629c8995375e9c70682a5b90b`; worktree cleanliness was not
attested.  Evidence:

- `/tmp/ptrace-rr-data-handling-shell-pipeline.info` (SHA-256
  `1bfc60e2fb8905b44312ddbd7c08c7218a5d2fa9f21a5eccf91d7a3e6384c49c`)
- `/tmp/hermit-report-76QCxi.diff` (SHA-256
  `5b9d1a30836ed45a4fe7fd38fc2d3ffa281d18df72793224fc0b1d98625f3cd7`)

Earlier uncommitted evidence:

- `target/ignored/replay-mkdir-eexist-exact-current.CziBZ3/verdict.json`
  (SHA-256
  `3b8e194940ec6424474b2debb9b4e1de51d59c491a0e70678ba11ed30ef5baaa`)
- `target/ignored/replay-mkdir-eexist-exact-current.CziBZ3/stderr`
  (SHA-256
  `8f9a3d1c4acb3b01cd27f8a1595e38a9069e726147c83e16ee4643c356c73a35`)

The run used dirty base `d601651a3a0f4865b72af50a9c5967eaf60c351a`
and release-binary SHA-256
`58beb9d71905cb21d8498c35a0877b8f3eb4fb9ca5ed4b010ae1c8dd10cb3ab8`.
All stale shell-pipeline process groups were cleaned; the final no-orphan check
was empty.

Exact-current validation evidence is under
`/home/newton/work/dev-hermit/scratch/exec-final-validation-post-lifecycle-20260816-SOuxof/`:

- `canonical/shell-pipeline/verify.json` SHA-256
  `30393186809f2be918ea9cc4afc36f5c68364c3d2f5cc69b8cbac44a86527c11`;
- `canonical/shell-pipeline/stderr` SHA-256
  `63c68db5a22438b9ff61b84638f0edcfae118cc84a6972c97f50782e06bb647a`;
- `full-record-replay-serial.log` SHA-256
  `a8b72d24a00733633bc4511ffe7c146bfce3474d26fe503bc817fa104d5de982`
  (53/54 integration tests passed; only the separately tracked
  `record_rs_sched_yield` shutdown INFO-order failure remained).

## `system-utils/example-date` — ptrace replay

**Manifest and command.** `tests/e2e/manifests/system-utils.toml`:

    target/release/hermit --log=info --backend ptrace record start --strict \
      --verify --verify-json <verify.json> --data-dir <recording> \
      --record-timeout 60 -- sh -c ./examples/date.sh

The cell is policy-disabled/red, but the run measured a real replay failure.

**First divergence.** An earlier retained run reported thread 3 event 93.  The
fresh pre-fix reproduction at `/tmp/hermit-relative-before.M21Xo7` located the
same boundary more completely at event 130: the streams matched through
`execveat(AT_FDCWD,"./examples/date.sh",...)`; recording next entered the
interpreter loader with `mmap(NULL,8192,RW,PRIVATE|ANON,-1,0)`, while replay
called `newfstatat(AT_FDCWD,"./examples/date.sh",...)`.  No scheduler turn,
virtual time, or instruction address was retained.  The pre-fix JSON remained
`no_result`.

**Root cause and uncommitted fixed status.** `Recorder::record_exec_path` discarded
non-absolute targets, while replay staged only recorded absolute executables.
The relative script and its shebang target were absent from the replay chroot,
so the child took a shell lookup/stat failure path.  The fix resolves relative
`AT_FDCWD` targets against the recording-time guest cwd, snapshots the exact
executable bytes and dependencies into the recording, and stages that snapshot
at the guest-absolute destination during replay rather than consulting the
live host path.

The focused requalification command used INFO-level ptrace, strict
record/replay, canonical bare `--verify`, no relaxations, and a 90-second
recording timeout:

    timeout --kill-after=5s 120s target/release/hermit --log=info \
      --backend ptrace record start --strict --verify \
      --verify-json /tmp/hermit-exec-path-exact.oTbORq/example-date/verify.json \
      --data-dir /tmp/hermit-exec-path-exact.oTbORq/example-date/recording \
      --record-timeout 90 -- /bin/sh -c ./examples/date.sh

That earlier snapshot exited 0 and matched 1037/1037 INFO messages.  The
exact-current receipt identified in the preceding shell-pipeline section
reran the same guest and command shape.  It exited 0 and matched
**509/509** canonical INFO messages with `bitwise_parity=true`, guest exit 0,
and null scheduler-turn and virtual-time divergence fields.  This is L2 for
this exact ptrace replay guest at fingerprint `455174bb...` and release binary
`73571dac...`, with INFO logging and no relaxations; it is not a claim that the
implementation is on `main`.

**Source and artifacts.** Observed source HEAD
`fa7c2c8f1818f3b629c8995375e9c70682a5b90b`; worktree cleanliness was not
attested.  Evidence:

- `/tmp/ptrace-rr-system-utils-example-date.info` (SHA-256
  `097bf6d8eeaf7e4fc687f9c3ef454d3983d13e5e685ff77ddc1c5c9171c85915`)
- `/tmp/hermit-report-sW1tXn.diff` (SHA-256
  `c1d12fdb5b619dd31c89f00505139d219a40033582d106664e48663db708fc80`)
- `ignored/e2e-commands/work/system-utils-example-date/captures/verify.json`

Earlier uncommitted evidence uses dirty base
`d601651a3a0f4865b72af50a9c5967eaf60c351a` and release-binary SHA-256
`58beb9d71905cb21d8498c35a0877b8f3eb4fb9ca5ed4b010ae1c8dd10cb3ab8`:

- `/tmp/hermit-exec-path-exact.oTbORq/example-date/verify.json` (SHA-256
  `a1e638d642cc8f0ebd4f2f868b0db44c26595d2fe9523da3fd43b04d7e67436d`)
- `/tmp/hermit-exec-path-exact.oTbORq/example-date/stderr` (SHA-256
  `ce0391f4f1d237743912cec0030fc24a3c075436e831b269494632b549a5d262`)

The run's unique-path orphan check was empty.

Completed receipt evidence:

- `/home/newton/work/dev-hermit/scratch/exec-final-validation-post-lifecycle-20260816-SOuxof/canonical/example-date/verify.json`
  SHA-256
  `e1ad73cc91d478cc88a9402ac2b95e20e065996699916394ca296198c4028d0a`;
- adjacent `stderr` SHA-256
  `7a9f63f57f804374069665cd2701c852d80dbb3ab906dd2521f0646f8250ad0e`.

## `system-utils/example-devrand` — ptrace replay

**Manifest and command.** `tests/e2e/manifests/system-utils.toml`, with the same
replay flags and timeout as `example-date`, running `sh -c
./examples/devrand.sh`.  The cell is policy-disabled/red and independently
measured red.

**First divergence.** Exit 1, thread 3, syscall event 93.  Event 92 was
`execveat(AT_FDCWD,"./examples/devrand.sh",...)`.  Recording expected the
interpreter-loader `mmap(NULL,8192,...)`; replay issued
`newfstatat(AT_FDCWD,"./examples/devrand.sh",...)`.  No turn, virtual time, or
RIP was retained.

**Root cause and status.** This is the same relative-child-exec staging defect
as `example-date`, not `/dev/urandom` nondeterminism.  The exact-byte snapshot
and guest-absolute staging fix above repairs this cell as well.  The earlier
uncommitted command was the `example-date` command with guest `/bin/sh -c
./examples/devrand.sh`; INFO logging, strict canonical record/replay, a
90-second recording timeout, and no relaxations.  It exited 0 and matched
1171/1171 INFO messages.  The exact-current validation snapshot reran the
guest with release binary `73571dac...` and matched **634/634**
canonical INFO messages with `bitwise_parity=true`, exit 0, and null
scheduler/vtime divergence fields.  This is L2 for this exact ptrace replay
guest at fingerprint `455174bb...`.

**Source and artifacts.** Observed source HEAD
`fa7c2c8f1818f3b629c8995375e9c70682a5b90b`; worktree cleanliness was not
attested.  Evidence:

- `/tmp/ptrace-rr-system-utils-example-devrand.info` (SHA-256
  `f8f5784c253efa80afb9f1bedee69039fbdf275b8cd503d7ce664a094263131f`)
- `/tmp/hermit-report-3kmhFE.diff` (SHA-256
  `2f6c3f6d1972ac91b7496e2542b19c1cfa3e8fd1844a89ef5cae8a3456197579`)
- `ignored/e2e-commands/work/system-utils-example-devrand/captures/verify.json`

The earlier focused base and binary identity match `example-date`.  Evidence:

- `/tmp/hermit-exec-path-exact.oTbORq/example-devrand/verify.json` (SHA-256
  `af8876f37875a332307774d49b126c6ad09b2b78f8508264d46e483f6f5250c8`)
- `/tmp/hermit-exec-path-exact.oTbORq/example-devrand/stderr` (SHA-256
  `5cfb94a1e66874d09f49bcb21cc6d28e5f4dd7775cba697b19563e8dcaf30753`)

The unique-path orphan check was empty.

Completed receipt evidence:

- `/home/newton/work/dev-hermit/scratch/exec-final-validation-post-lifecycle-20260816-SOuxof/canonical/example-devrand/verify.json`
  SHA-256
  `801f226b5dd0823db611345351227d729fffc2b33df6b2507374b85afa994807`.

## `system-utils/clock-exec-continuity` — ptrace replay

**Why it was red.**  The policy-disabled row does contain a reproducible
record/replay failure, but the failure is not clock nondeterminism.  A fresh,
single record plus single replay with targeted ptrace TRACE isolated an
incorrectly staged `/proc/self/exe` replacement image.

**Exact commands.**  For readability, `ART` below expands to the exact retained
directory.  The commands used no determinism relaxations:

    ART=/home/newton/work/dev-hermit/scratch/clock-exec-continuity-replay-20260816-LOyXJO
    mkdir -p "$ART/home" "$ART/xdg-config" "$ART/tmp" \
      "$ART/fixtures" "$ART/data"
    cp -a tests/e2e/xdg-config/. "$ART/xdg-config/"
    setsid timeout --kill-after=10s 110s env \
      LC_ALL=C TZ=UTC HOME="$ART/home" \
      XDG_CONFIG_HOME="$ART/xdg-config" E2E_TMPDIR="$ART/tmp" \
      E2E_FIXTURE_DIR="$ART/fixtures" \
      RUST_LOG='info,reverie_ptrace::syscall=trace,reverie_ptrace::task=trace,hermit::replayer=trace,detcore::record_or_replay=trace' \
      RUST_BACKTRACE=1 \
      /home/newton/work/dev-hermit/hermit/target/release/hermit \
      --log=info --log-file "$ART/record.trace.log" --backend ptrace \
      record start --strict --data-dir "$ART/data" --record-timeout 90 -- \
      /home/newton/work/dev-hermit/hermit/ignored/e2e-commands/work/system-utils-clock-exec-continuity/guest \
      >"$ART/record.stdout" 2>"$ART/record.stderr"

    setsid timeout --kill-after=10s 110s env \
      LC_ALL=C TZ=UTC HOME="$ART/home" \
      XDG_CONFIG_HOME="$ART/xdg-config" E2E_TMPDIR="$ART/tmp" \
      E2E_FIXTURE_DIR="$ART/fixtures" \
      RUST_LOG='info,reverie_ptrace::syscall=trace,reverie_ptrace::task=trace,hermit::replayer=trace,detcore::record_or_replay=trace' \
      RUST_BACKTRACE=1 \
      /home/newton/work/dev-hermit/hermit/target/release/hermit \
      --log=info --log-file "$ART/replay.trace.log" --backend ptrace \
      replay --data-dir "$ART/data" \
      --autopilot acdba75f581e4f1db89a3cc7998caad4 \
      >"$ART/replay.stdout" 2>"$ART/replay.stderr"

The recording exited 0 and printed generations 0, 1, and 2 followed by
`clock exec continuity holds across 2 execs`.  Replay reproduced generation 0
byte-for-byte, then failed before generation 1 could read the clock.  The
retained traces contain 268 recording INFO messages and 138 replay INFO
messages; these are event-stream counts, not a successful parity comparison.

**Detected divergence, event 57.**  The replay failure report first rejects
thread 3 syscall event 57.  The preceding recorded and replayed event was the
same `close(3)`.  Recording next expected:

    mmap(NULL, 12288, PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANON, -1, 0)

Replay instead issued:

    openat(AT_FDCWD, "/lib64/libgcc_s.so.1", O_CLOEXEC)

The actual `openat` commits at replay scheduler turn 7, on virtual time
`1767225600.005490270s`.  This is the point where the replayer detects a
different syscall number, but it is downstream of the first semantic
difference.

**First semantic divergence, Detcore syscall 49 / turn 6.**  Both streams
match through `close(3)` at Detcore syscall 48.  At syscall 49 they both issue
`openat` with the same raw register values, including the same pathname pointer
`0x7ffff7fba750`, flags, and result `Ok(3)`, but the pointed-to bytes differ:

- recording: `openat(..., "/lib64/libc.so.6", O_CLOEXEC)` and COMMIT turn 6 at
  `1767225600.004826520s`;
- replay: `openat(..., "/lib64/libunwind-x86_64.so.8", O_CLOEXEC)` and COMMIT
  turn 6 at `1767225600.004827940s`.

The record/replay comparator currently accepts this event because its syscall
match is based on the syscall number and raw argument registers rather than
the pointed-to pathname contents.  That delays the reported failure until
event 57, when the replacement images finally request different syscall
numbers.  Thus event 57 is the detection point; syscall 49 is the first
log-visible semantic divergence.

**Root cause.**  The wrong image is selected still earlier, at the successful
guest `execve("/proc/self/exe", ...)` after Detcore syscall 40 and before
post-exec syscall 42.  The recording's `exec_paths` file contains the original
absolute guest path followed by `/proc/self/exe` twice.  During replay,
`populate_recorded_exec_paths` evaluates `/proc/self/exe` in the Hermit
controller, then copies that file into the replay chroot at the same path.
Consequently, the guest's self-exec loads the Hermit CLI rather than the
recorded C guest.

ELF dependencies make the diagnosis conclusive: the C guest and the recording
copy each declare only `libc.so.6`; the Hermit CLI declares
`libunwind-x86_64.so.8`, `libgcc_s.so.1`, `libm.so.6`, `libc.so.6`, and
`ld-linux-x86-64.so.2`.  Replay opens `libunwind` at syscall 49 and then
`libgcc_s` at detected event 57, exactly matching the controller binary's
loader path.  This is an exec-path staging defect, not a clock-value,
virtual-time-continuity, PMU, or scheduling defect.  Weakening or coarsening
virtual time would be wrong.  The fix direction is to resolve procfs self-exec
aliases to the recorded executable and to compare pointed-to path contents at
the first `openat` mismatch.

**Fix and exact-current verdict.**  The implemented replay path now records
the traced guest's `/proc/<pid>/exe` bytes while preserving `/proc/self/exe` as
the guest-visible destination, stores the image and dependencies by content,
and stages those recorded bytes rather than resolving the controller's live
magic link.  The earlier focused invocation used the same INFO-level,
strict, no-relaxation record/replay envelope as `example-date`, after compiling:

    cc -std=c11 -O0 -g -Wall -Wextra -Werror -D_GNU_SOURCE \
      tests/c/clock_exec_continuity.c -o \
      /tmp/hermit-exec-path-exact.oTbORq/clock_exec_continuity

The run exited 0 and matched 268/268 canonical INFO messages with
`bitwise_parity=true`; scheduler-turn and virtual-time divergence fields are
null.  Continuous, fine-grained virtual time was not weakened.  This is L2 for
the exact ptrace replay guest, INFO logging, relaxations none.  The completed
combined receipt binds that result to fingerprint `455174bb...` and release
binary `73571dac...`.

**Instruction evidence boundary.**  No exact guest RIP was captured.  The
record/replay configuration hardcodes an empty `stacktrace_event`, replay has
no event selector, and the targeted ptrace syscall TRACE records arguments but
not instruction pointers.  A second GDB replay was not run because this probe
was deliberately limited to exactly one recording and one replay.  The
replacement image and its dynamic-loader `DT_NEEDED` branch are nevertheless
identified by the trace and ELF dependency evidence.

**Identity, artifacts, and cleanup.**  Hermit base
`d601651a3a0f4865b72af50a9c5967eaf60c351a`, dirty tree; Hermit executable
SHA-256
`9597f0b6a9b407a3eeda2c572c9e903ae062310ac1d26621bccdb7cf2b59a22c`;
compiled guest and recorded `exe` SHA-256
`d9297ce46a5db4d844775dcc9f450306099096cc16c2fb88afeedd7111f28988`;
guest-source SHA-256
`58efc8a739a05b8c8173ff8d1a7050e4e1dbabf15c99cfe137e1c4b1114b8116`.
Artifact root:

`/home/newton/work/dev-hermit/scratch/clock-exec-continuity-replay-20260816-LOyXJO/`

Key evidence hashes:

- `record.trace.log`:
  `493d1ba9c6dfbe794231d7e1316a56f6221123239e1ae05ad2448bd578f312a2`
- `replay.trace.log`:
  `57670fbcd07d666d3d8974320c09c13514c673a988648abfbc7774b75483c458`
- `desync.diff`:
  `705f438375dc0debc8dec9a0b4079035e469a52f32d51fa2076910182b22a7a4`
- `data/acdba75f581e4f1db89a3cc7998caad4/exec_paths`:
  `a2016232b424082f239b6203ee1f1a49b22a62a7310cb94bf59c65ad55c8b7ae`
- `data/acdba75f581e4f1db89a3cc7998caad4/thread/3`:
  `d8390f2942f3bdfc161e3df90842234a3abf38d1b11e12c26fd72119a79943d1`

A post-run process-group check found no process from this recording or replay.
Earlier uncommitted evidence is under
`/tmp/hermit-exec-path-exact.oTbORq/clock-exec-continuity/`, using release
binary SHA-256
`58beb9d71905cb21d8498c35a0877b8f3eb4fb9ca5ed4b010ae1c8dd10cb3ab8`
and compiled guest SHA-256
`392f17f964319e119f801c02ee605bb30b6bfeed11f74162235e1d548e162423`:

- `verify.json` SHA-256
  `c22cc6a994960c08ad8b256dc2711b37945356a8909cab0123bf6ffd5e30c8a9`
- `stderr` SHA-256
  `776d8a654d601d6f8ce00c7e811a9fbb9fb17974efa89f5aaf369ccf73fd0bb3`

Its unique-path orphan check was empty.

Completed exact-current receipt evidence:

- `/home/newton/work/dev-hermit/scratch/exec-final-validation-post-lifecycle-20260816-SOuxof/canonical/clock-exec-continuity/verify.json`
  SHA-256
  `c22cc6a994960c08ad8b256dc2711b37945356a8909cab0123bf6ffd5e30c8a9`.

<a id="record_rs_sched_yield--ptrace-recordreplay-integration"></a>
## `record_rs_sched_yield` — ptrace record/replay integration

**Probe and reproducibility.**  This incidental red came from the focused
Hermit integration suite, not an E2E scorecard cell:

    cargo test -p hermit --test record_replay -- --nocapture --test-threads=1

The suite passed 43/44 tests.  Three immediate targeted invocations of
`cargo test -p hermit --test record_replay record_rs_sched_yield --
--nocapture` then produced pass, pass, fail.  The test internally runs the
`rustbin_sched_yield` guest through `hermit record start --verify` with a
30-second recording timeout and a 45-second outer timeout.

The exact-current lifecycle receipt reproduced this boundary twice: the full
serial target passed 53/54 with only `record_rs_sched_yield` failing, and an
immediate isolated rerun failed again.  These runs used fingerprint
`455174bb...`; their logs are
`scratch/exec-final-validation-post-lifecycle-20260816-SOuxof/full-record-replay-serial.log`
(SHA-256
`a8b72d24a00733633bc4511ffe7c146bfce3474d26fe503bc817fa104d5de982`)
and `sched-yield-rerun.log` (SHA-256
`a345b629c5106d71ae4e9640e9aaf031564749021b1a853713589c12bd5714d1`).

**First divergence.**  The retained failing pair has 381 recording INFO
messages versus 380 replay INFO messages, while both contain 315 DETLOG
messages and 50 identical COMMIT turns.  The streams match through root dettid
3 entering `sched_yield()` after completed syscall 142
`rt_sigprocmask(...)=Ok(0)`, child dettid 5 committing `exit_group` at turn 49,
and both deterministic threads being removed.  Recording then emits
`scheduler (step2_process_blocked): zero threads left anywhere, fizzling.`
twice; replay emits it once before both log `run queue empty, exiting
sched_loop`, shutdown, and the same final report.  The duplicate recording
message is the first and only substantive INFO-stream mismatch.

There is no divergent syscall result, guest instruction, exec path, COMMIT
sequence, or virtual time.  This is an intermittent end-of-run scheduler/log
emission race, independent of the exec-image staging fixes above.  It remains
an unresolved validation red rather than evidence that `sched_yield` itself
returns nondeterministically.

**Artifacts.**  Latest failing logs:

- `/tmp/record_log_Ss1mB` (SHA-256
  `61daa3537621d4ba64f1baefc1321053af865e72a92e71e6970d0f0c64ee0489`)
- `/tmp/replay_log_cnZmn` (SHA-256
  `dd0a4194d13a64f976fffffac3bc054e768b14bdc5e4b2edbae6d8aa8d41d3a3`)

## `backend-parity-c/fork-exec-pipeline` — DBT verify

**Invocation and backend reality.** Earlier manifest runs used:

    hermit --log=info run --backend=dbt --strict --verify \
      --verify-json <report> [--keep-logs --verify-log-dir <dir>] -- <fixture>

The DBT path loaded the real Detcore tool under DynamoRIO.  The manifest
recorded `no-virtualize-cpuid` and `max-timeslice=disabled`.  The receipt binds
Hermit source SHA `d601651a3a0f4865b72af50a9c5967eaf60c351a` (dirty), binary
SHA-256 `2af7be8a7c46dcd0f3324680b8ccd0b6bb62632972fd88a00c32ff5452abfe6c`,
and fixture SHA-256
`445aec142be46b866c33711b40ebfae1c84c38e6a5853e35dad313e53f86e750`.

**Earlier completed runs: evidence-order divergence.** Both guests exited 0
and produced nonempty protected evidence.  A retained comparison had 319/319
INFO records and reported first divergent scheduler turn 8 at virtual time
`1_767_225_600.182_000_000s`.  Both streams matched through root dettid 3
finishing syscall 33, `close(4)=0`, entering syscall 34,
`read(3,...,64)`, and scheduler `NONCOMMIT turn 9` while polling that read.
At INFO message 90, run 1 emitted `DETLOG SCHEDRAND: seeding scheduler
runqueue with seed 0`; run 2 emitted `COMMIT turn 10 ... InternalIOPolling`
at `1_767_225_600.182_500_000s`.  Both records occur in both streams, but in
different order.  Other retained pairs moved the first mismatch to child
dettid 4 startup/`AT_RANDOM` versus root dettid 3 read-poll turns.

This is a cross-sender evidence serialization ordering defect between the
root scheduler and child-image sender, not a differing syscall result or
guest instruction.  The comparator's syscall context merely names the
still-pending pipe read.

Artifacts are under `target/dbt-final-gate.3zPRzJ/`:

- `backend-parity-c-fork-exec-pipeline.json`, SHA-256
  `fb0fc9ad8e3cca8ba8c8459fd66a9f6b0adf6114cc34ad6280e27854c23d14c5`
- retained logs, SHA-256
  `066ee5e8be87d7b79265ac62eb0a5a81ff9df02873724f64aa16e2c0c32a63df`
  and `f49e048ad7cc1f75c479a8b7ea3243d953d3493bc8b313530930d197f287c39c`
- comparator stderr, SHA-256
  `0a41c1e3fa6a05f361ea76840533a46b1851a3134ae26983ed53c99fcba6e463`

**Later/current phase: teardown no-result.** The later direct probe used
`--backend dbt --strict --verify --verify-json`, default CLI logging with
canonical INFO evidence, and no explicit relaxations.  It stopped after
`:: DBT Run1...`; the JSON remained `no_result` and both decoded log files
were zero bytes.  Thus there is no matching event, scheduler turn, virtual
time, syscall, or guest-instruction divergence for this phase: one complete
finalized run was never returned.

The process tree proved that the fixture and `/bin/echo -n
hermit-fork-exec` had launched.  The captured host-runtime wait boundary was:

    State: S (sleeping)
    wchan: __futex_wait
    syscall: 202 (futex), RIP 0x7ffff7d3b1ed
    #0-#6: addresses mapped inside libdynamorio.so
    #7  dynamorio_app_take_over
    #8  dynamorio_app_take_over

This is a product teardown/lifecycle deadlock, not host infrastructure and not
a guest syscall divergence.  `client.c:event_exit` requests Detcore shutdown
while DynamoRIO all-thread exit synchronization can suspend and wait on the
background scheduler.  Current `detcore-dbt/src/lib.rs:998-1002` only resets
readiness and sets `RUNTIME_SHUTDOWN`; the bounded quiescence/UAF guard in
`client.c:3520-3550` safely omits FINAL evidence if the background remains
active but does not make fork/exec complete.  Experimental teardown changes
were reverted.  This cell remains red/`no_result` and is outside the selected
M2 single-process DBT scope.

Timeout evidence:

- `target/dbt-acceptance-final-valid.JeHVfB/backend-parity-c-fork-exec-pipeline.stderr`,
  SHA-256
  `61ab932237d28f371b9fa0273035c4aeca21dd55bb753356a54f16ae271c6cb2`
- pending JSON, SHA-256
  `aef37a7cccb959c03d465d2400cf9fca5b9058a333023f350a4ba84eb5b6369b`
- `target/dbt-fork-stack-diag.4YjwRT/gdb-932171.txt`, SHA-256
  `0f6853469fb56c96738322f9f4acf12431d5c5a0fd100db6f3831accee1ce631`
- `/proc` snapshot, SHA-256
  `bb71326d11523856868dbeeb2ee1b8b3c31f5f972ae3da7f4bff4e763bc643bc`

## `backend-parity-c/file-io-roundtrip` — DBT verify

**Why it was red.**  The scorecard row was disabled/red with no observations.
The manifest disabled DBT only because new top-of-funnel coverage was being
established on ptrace first and said DBT would ratchet separately; it named no
failed syscall or instruction.  A fresh direct run proves that this is
selection/inventory debt, not a runtime failure for this guest.

**Exact command and backend reality.**  The retained canonical invocation was:

    DBT_SAMPLE_DIR=target/ignored/dbt-red-cell-samples/run.LBWRUP/file-io-roundtrip-canonical
    mkdir -p "$DBT_SAMPLE_DIR/logs"
    env -u RUST_LOG -u HERMIT_LOG -u HERMIT_LOG_FILE \
      timeout --signal=TERM --kill-after=10s 120s \
      target/release/hermit run --backend dbt --strict --verify \
      --verify-json "$DBT_SAMPLE_DIR/verify.json" --keep-logs \
      --verify-log-dir "$DBT_SAMPLE_DIR/logs" -- \
      /home/newton/work/dev-hermit/hermit/target/ignored/final-replay-validation/runs/replay-file-io-roundtrip/backend-parity-c-file-io-roundtrip-replay-ptrace/fixtures/program \
      >"$DBT_SAMPLE_DIR/stdout" 2>"$DBT_SAMPLE_DIR/stderr"

Logging was unset/default at the CLI; the protected evidence stream contained
INFO.  There were no relaxation flags.  Stderr explicitly reports `Detcore
Tool active` and execution under DynamoRIO's `drrun`, so this is the real
`Detcore<DbtGuest>` path rather than a launch-only adapter.

**Verdict and divergence boundary.**  Guest exit was 0 and stdout reported
`wrote=1024 read=1024 checksum=130560 end=1024 stat=1024 shrunk=512`.
Protected framed evidence was present, nonempty, authenticated and accepted;
the JSON reports canonical comparison, `bitwise_parity=true`, and 131/131 INFO
messages.  An independent command also matched all events:

    target/release/hermit log-diff --no-color --limit 0 \
      --syscall-history 12 \
      --json "$DBT_SAMPLE_DIR/log-diff.jsonl" \
      "$DBT_SAMPLE_DIR/logs/dbt-run1_log_G76tN" \
      "$DBT_SAMPLE_DIR/logs/dbt-run2_log_oUnco"

Both first-divergence fields are null; the two raw logs are byte-identical.
There is no divergent syscall, instruction, scheduler turn, or virtual time.
The matched core path includes `write(3, ..., 1024)`, `fsync`, end/rewind
`lseek`, eight 128-byte reads plus EOF, `fstat`, `ftruncate(512)`, `close`, and
`unlink`.  Both streams end with syscall 55, `write(1,...,74)=Ok(74)`, and
COMMIT turn 6 for exit.  This is L2 for this exact DBT guest, default log
setting, relaxations none; it does not qualify arbitrary DBT process trees.

**Identity, artifacts, and cleanup.**  Hermit base
`d601651a3a0f4865b72af50a9c5967eaf60c351a`, dirty; Reverie base
`ee6716a65d41e8f1d65ee32efa4aafa910b9cf29`, dirty.  Binary SHA-256 values:

- Hermit: `9597f0b6a9b407a3eeda2c572c9e903ae062310ac1d26621bccdb7cf2b59a22c`
- Detcore DBT tool: `5ba6f8f0c3eb3b442b024ae9886ce1b3adc03e9890b8fc38571c5881bc42cd4d`
- Reverie DBT client:
  `3f17f853187e4b5443825546ec08998c53845386183373430f47ef0a066cc45e`
- C source:
  `4f4cec85cc5d11a52a031bd0f469abed5d687db10f568e130d9bd7dc07b4d89f`
- compiled guest:
  `2523487b42715ab5d5e4ad573753785898985e7a6747bcdf8b81a2ac0539f6be`

Artifact root:
`target/ignored/dbt-red-cell-samples/run.LBWRUP/file-io-roundtrip-canonical/`.
The verify JSON SHA-256 is
`f722e0208242f5354e1b3c2dc4f19247d2e5aa4f42b2118a9470858a2843b202`;
both retained log files have SHA-256
`fa2b5f998179a096fd2e38579c9775e25245329d49a3f1f196534ccfe13e3a0f`;
the independent log-diff JSON has SHA-256
`0e88e14e59516b7a478d6e3f8ea11666610abbdd93d6930c4b1dfcdd3ba8c769`.
A post-run check found no `drrun`, DynamoRIO, Detcore DBT, or Reverie DBT
process left behind.

## `language-runtimes/example-python-random` — DBT verify

**Why it was red.**  This row was enabled/red with no observations, while its
manifest enabled DBT but set the verify mode `ci=false`.  It therefore had no
expected-plan receipt despite no recorded backend failure.  The red was
inventory-only.

**Exact command and backend reality.**  The retained run was:

    DBT_SAMPLE_DIR=target/ignored/dbt-red-cell-samples/run.LBWRUP/example-python-random
    mkdir -p "$DBT_SAMPLE_DIR/logs"
    env -u RUST_LOG -u HERMIT_LOG -u HERMIT_LOG_FILE \
      timeout --signal=TERM --kill-after=10s 120s \
      target/release/hermit run --backend dbt --strict --verify \
      --verify-json "$DBT_SAMPLE_DIR/verify.json" --keep-logs \
      --verify-log-dir "$DBT_SAMPLE_DIR/logs" -- ./examples/rand.py \
      >"$DBT_SAMPLE_DIR/stdout" 2>"$DBT_SAMPLE_DIR/stderr"

As above, logging was default/unset, no relaxations were supplied, and the
banner confirms the real shared Detcore tool under DynamoRIO.

**Verdict and divergence boundary.**  Guest exit was 0 and both runs produced
the same ten random integers.  Protected evidence validated and canonical
comparison matched 3299/3299 INFO messages with `bitwise_parity=true`.
Independent `log-diff --no-color --limit 0 --syscall-history 12` also matched;
the raw retained logs are byte-identical and both divergence fields are null.
There is no syscall, instruction, scheduler, or virtual-time divergence.

The stream includes deterministic user-PRNG seed 0 and matching `getrandom`
calls of 8, 24, and 2496 bytes.  Both runs finish at syscall 1497,
`munmap(...,151552)=Ok(0)`, followed by COMMIT turn 181 for exit.  This is L2
for this exact DBT Python workload, default log setting, relaxations none.

**Identity, artifacts, and cleanup.**  Hermit, Reverie, DBT tool, and client
identities are the same as the preceding cell.  `examples/rand.py` SHA-256 is
`e1a32fd938403f394f36555be180f2b17dd497859f28e9aa9ac0dadcd0372343`.
Artifact root:
`target/ignored/dbt-red-cell-samples/run.LBWRUP/example-python-random/`.
The verify JSON SHA-256 is
`e46064667d4310566fd66272208f78d8614e5478f4c140818756eb920628f7b2`;
both logs have SHA-256
`626520251dc727af84b5dc4b43dba1c2ce2592a03b873e8ede13b4ba18948485`;
the independent log-diff JSON has SHA-256
`12c21ec76f01e2f94a4ac300d74ed20e7a0831305e133dde406937e54672a899`.
The shared post-run process check found no DBT/DynamoRIO orphan.

## `/bin/echo hello` — DBT verify

**Historical command.** This predates protected evidence and used stale binary
`ge2f9a36b1ad8`, INFO logging, and no relaxations:

    timeout --signal=TERM --kill-after=5s 120s \
      target/release/hermit --backend dbt --log info run --strict \
      --verify --verify-strict \
      --verify-json /tmp/hermit-backend-audit.7VyfIC/dbt-echo.verify.json \
      -- /bin/echo hello

**Guest reality and no-result boundary.** The real Detcore tool executed the
guest twice.  Stdout was `hello\n`; each run reported 113 syscalls, 112
rewritten, and memory hash `9175dd372546a296`.  The last guest event was
`exit_group(0)`, with COMMIT turn 33 at virtual time
`1_767_225_600.974_500_000s`.  Nevertheless the JSON published
`verified=false`, `verdict=no_result`, `comparison=null`, and no guest exit
code or INFO counts.

The exact boundary was old CLI control flow: the DBT dispatch returned from
`run_dbt` before the common verifier, leaving the pre-stamped pending JSON.
This was an evidence-publication defect, not guest nondeterminism.

**Manual first divergence.** Reconstructing canonical INFO exposed a second
systemic defect.  The first mismatch was message 6, COMMIT turn 0 at identical
virtual time, differing only in physical host PID/DetPid (`567709` versus
`613507`).  The first syscall, `brk(NULL)`, returned the same value.  Replacing
that unstable identity with stable `DetPid(3)` left 286/286 INFO messages with
no substantive difference.  No syscall or instruction divergence was found.

Protected framed DBT evidence and the ordinary canonical comparator are now
wired: exact-current `/bin/true` and nine selected cells below pass with
nonzero INFO evidence.  This exact echo command was not requalified against
the final dirty snapshot, so its current status is “false-red class fixed;
exact echo not freshly certified.”

Artifacts under `/tmp/hermit-backend-audit.7VyfIC/`:

- `dbt-echo.verify.json`, SHA-256
  `aef37a7cccb959c03d465d2400cf9fca5b9058a333023f350a4ba84eb5b6369b`
- `dbt-echo-verify.info`, SHA-256
  `3abf3eb2c51544a3f68f905c2220b30ecba79d6bea46997c3b092668e32f700e`
- PID-normalized logs, identical SHA-256
  `fb18dbd231559290e2978b070c2e0e94eeececfe4b5dd1c3b53c5ae1ff6a1948`
- `dbt-next-logdiff.stderr`, SHA-256
  `ab28631da0ff37a51ff07d6700f8707195b91413e5151b9c4267bbadf4ef2a0f`

### HISTORICAL KNOWN-BAD DBT control: `/usr/bin/kill -0 1`

This is preserved for diagnosis only; it is **not a current repro** and is not
one of the three commands at the top.  A clean historical worktree at
`2b38d8e629ee582db6a59f340b1a3c8980fd85c5` used a Hermit binary with SHA-256
`e57604881668ca9c5bfbfbd063e5388f3bcce3f0bed17c059f1b81b5b4220f78`:

    timeout --kill-after=5s 30s \
      /home/newton/work/dev-hermit/worktrees/coord/hermit/target/release/hermit \
      --log info run --backend dbt --strict --verify \
      --verify-json /tmp/m2-historical-dbt-kill-info.v9Vqhv/verify.json \
      -- /usr/bin/kill -0 1

The guest reached syscall event 109, but DBT exposed the host PID namespace:
`kill(1, 0)=Err(ESRCH)`.  The command completed in about one second yet the old
adapter left `verdict=no_result`, `comparison=null`, and no INFO counts.  Thus
the precise product defect was guest-visible PID semantics, while the precise
verification boundary was the historical DBT evidence-publication return.
Retained files are `/tmp/m2-historical-dbt-kill-info.v9Vqhv/verify.json`
(SHA-256
`cbd2e05784aa6f50e4816ccaea4c5e5547f99e3f8b4dc078bbaba996e60e40a7`)
and adjacent `stderr` (SHA-256
`a369ec6cd410c76fe318b7fa8d72e20481e419661e9a24bfb45bd1e6ba02544e`).

<a id="dbt-bin-true"></a>
## `/bin/true` — DBT verify

**Command and envelope.** The historical command was the preceding DBT echo
command with guest `/bin/true`; backend DBT, INFO logging, strict canonical
verify, and no relaxations.

The real Detcore path completed 32 syscalls, rewrote 31, and produced matching
memory hash `cbf29ce484222325`.  Its last event was `exit_group(0)`, COMMIT turn
4 at `1_767_225_600.276_000_000s`.  The old adapter nevertheless published
the same pending `no_result` JSON, so there was no compared stream and no
syscall/instruction divergence to name.  This is the same evidence-plumbing
false red as DBT echo.

Current protected evidence passes `/bin/true` with status 0, `matched`,
`bitwise_parity=true`, and 80/80 INFO records.  Receipt:
`target/ignored/final-dbt-validation/runs/exact-current.ljJVhw/bin-true/verify.json`,
SHA-256
`b44225aded7260bbd4be032013af791ebf6f5b1bcfb3439ad7ba70ba57127ba0`.
Historical INFO SHA-256:
`3dc96c094b28a8cb381be0a27eddb7e63c6bbbb9ac374c48acceb14bda2da21c`.

<a id="dbt-bin-cat"></a>
## `/bin/cat /dev/null` — DBT verify

**Command and envelope.** Historical DBT, INFO logging, strict canonical
verify, no relaxations, guest `/bin/cat /dev/null`.

The real Detcore path completed 119 syscalls, rewrote 118, and produced
matching memory hash `cbf29ce484222325`.  The last event was `exit_group(0)`,
COMMIT turn 33 at `1_767_225_601.016_000_000s`.  The adapter still published
`no_result`; comparison never began, so no syscall or instruction divergence
exists.  Protected evidence fixes the common adapter class, but this exact cat
guest was not rerun in the final selected set.

Artifacts:
`/tmp/hermit-backend-audit.7VyfIC/dbt-cat.verify.json`, SHA-256
`aef37a7cccb959c03d465d2400cf9fca5b9058a333023f350a4ba84eb5b6369b`,
and `dbt-cat-verify.info`, SHA-256
`c6366cf56a5f31a5a036dd244cb4418cf61bca5c498dd71deea5d607083593d2`.

<a id="dbt-direct-kcmp"></a>
## DBT direct `c-programs/kcmp-eperm` probe

**Command and envelope.** Historical stale binary `ge2f9a36b1ad8`, INFO
logging, strict verify, no relaxations:

    env LC_ALL=C TZ=UTC ./target/release/hermit --log=info run \
      --backend dbt --strict --verify --verify-json <out>.json -- <kcmp>

The guest exited 0 through the real Detcore tool, completed 39 syscalls, and
reported identical memory hash `5d9478fc8d30b012`.  The process returned 0
but JSON remained `no_result` with zero compared INFO messages.  The precise
boundary was again the old adapter return before the verifier; there was no
measured syscall or instruction divergence.

The exact-current manifest cell now passes 95/95 canonical INFO, documented
separately below.  Historical dossier:
`ai_docs/DBT_ADAPTER_PROBE_20260815.md`, SHA-256
`96b6b4e4324563589e63a7fa698d17c0aca93f8dc6e82b3e4059a0884c9175d6`.

## Exact-current selected DBT cells — shared evidence envelope

All nine manifest cells below used:

    hermit run --backend dbt --strict --verify \
      --verify-json <path> -- <guest>

The CLI log level was default; protected evidence and comparison scope were
INFO.  There were no explicit relaxations.  Every run exited 0 with
`verified=true`, `verdict=matched`, `bitwise_parity=true`, nonzero INFO on
both sides, and no first divergence.  Artifact root:
`target/ignored/final-dbt-validation/runs/exact-current.ljJVhw/`.  These are
L2 results for the named selected single-process cells only; they do not cover
fork/exec, arbitrary process trees, the whole corpus, or stress levels.

<a id="dbt-exact-host-identity"></a>
### Exact-current DBT: `backend-parity-c/host-identity`

108/108 INFO records matched; status 0 and no divergence.  This directly
confirms that the historical physical-PID identity mismatch is fixed for this
cell.  Receipt `backend-parity-c-host-identity/verify.json`, SHA-256
`91671313f256a62c2aa54369d1feda024b207d7ff35b2acc380b9f004d3116f9`.

<a id="dbt-exact-add-key"></a>
### Exact-current DBT: `c-programs/add-key-enosys`

93/93 INFO records matched; status 0 and no divergence.  Receipt
`c-programs-add-key-enosys/verify.json`, SHA-256
`1151a5926fb8a4b3b2aa72eaa3ac3ad98e83add4dab720aa4088f00edafe242e`.

<a id="dbt-exact-cachestat"></a>
### Exact-current DBT: `c-programs/cachestat-enosys`

93/93 INFO records matched; status 0 and no divergence.  Receipt
`c-programs-cachestat-enosys/verify.json`, SHA-256
`1151a5926fb8a4b3b2aa72eaa3ac3ad98e83add4dab720aa4088f00edafe242e`.

<a id="dbt-exact-futex-waitv"></a>
### Exact-current DBT: `c-programs/futex-waitv-enosys`

93/93 INFO records matched; status 0 and no divergence.  Receipt
`c-programs-futex-waitv-enosys/verify.json`, SHA-256
`1151a5926fb8a4b3b2aa72eaa3ac3ad98e83add4dab720aa4088f00edafe242e`.

<a id="dbt-exact-get-robust-list"></a>
### Exact-current DBT: `c-programs/get-robust-list-self`

93/93 INFO records matched; status 0 and no divergence.  Receipt
`c-programs-get-robust-list-self/verify.json`, SHA-256
`1151a5926fb8a4b3b2aa72eaa3ac3ad98e83add4dab720aa4088f00edafe242e`.

<a id="dbt-exact-ioctl-fioclex"></a>
### Exact-current DBT: `c-programs/ioctl-fioclex`

121/121 INFO records matched; status 0 and no divergence.  Receipt
`c-programs-ioctl-fioclex/verify.json`, SHA-256
`efd9012e698a85376a07aa1f4b9604d936941dbde3575e0fad85eb56eaabd7bb`.

<a id="dbt-exact-kcmp"></a>
### Exact-current DBT: `c-programs/kcmp-eperm`

95/95 INFO records matched; status 0 and no divergence.  This overturns the
historical direct probe's `no_result` for the selected manifest cell.  Receipt
`c-programs-kcmp-eperm/verify.json`, SHA-256
`acd9b2ba8b1e7fc0c0bd7147f6b97ffccfec591568ceaf57fafc9d0160e75b7b`.

<a id="dbt-exact-keyctl"></a>
### Exact-current DBT: `c-programs/keyctl-enosys`

93/93 INFO records matched; status 0 and no divergence.  Receipt
`c-programs-keyctl-enosys/verify.json`, SHA-256
`1151a5926fb8a4b3b2aa72eaa3ac3ad98e83add4dab720aa4088f00edafe242e`.

<a id="dbt-exact-listmount"></a>
### Exact-current DBT: `c-programs/listmount-enosys`

93/93 INFO records matched; status 0 and no divergence.  Receipt
`c-programs-listmount-enosys/verify.json`, SHA-256
`1151a5926fb8a4b3b2aa72eaa3ac3ad98e83add4dab720aa4088f00edafe242e`.

## `/bin/echo hello` — KVM verify

**Historical command and envelope.** KVM `KvmGuest<Detcore>`, INFO logging,
strict verify, no relaxations:

    timeout --signal=TERM --kill-after=5s 120s \
      target/release/hermit --backend kvm --log info run --strict \
      --verify --verify-strict \
      --verify-json /tmp/hermit-backend-audit.7VyfIC/kvm-echo.verify.json \
      -- /bin/echo hello

The guest ran twice; stdout and exit status matched.  Detcore completed syscall
113, entered `exit_group(0)`, and committed turn 33 at
`1_767_225_600.018_666_000s`.  The complete matching run report ended at
virtual time `1_767_225_600.019_166_000s`.

The product JSON correctly reported output-only evidence:
`verified=true`, `verdict=matched`, `bitwise_parity=false`,
`comparison.compare_logs=false`, and no INFO count.  A manual canonical diff
found exactly one mismatch, INFO message 292, after all guest and Detcore
events: `hermit::kvm` host lifecycle timings (`prepare_us`, `execution_us`,
and related wall durations).  There was no guest syscall or instruction
divergence.

Current source demotes this host telemetry from INFO to DEBUG at
`hermit-cli/src/lib.rs:1445-1453`.  KVM nevertheless remains non-L2 because
`verify.rs:720-723` deliberately skips internal log comparison and compares
only status/stdout/stderr.  The remaining red is evidence capability/policy,
not an echo execution failure.

Artifacts under `/tmp/hermit-backend-audit.7VyfIC/`:

- `kvm-echo.verify.json`, SHA-256
  `6abbb8fb70893d1ca215c10536124a997b6cca39ec7998e9e5bf97a3300e9ade`
- retained logs, SHA-256
  `bad2678a41f3eea705fec7d1baff97f868abfa6a49ff431b2740580c80de2874`
  and `b0abfbd0275e0df2bbfb9e3023dfe48af91c108de7166cb780f8e7758d93e90b`
- `kvm-canonical-logdiff.stderr`, SHA-256
  `91744a1dc9ab265c20e2f7f68e8c107c9504abfa60fe5e1792102a58e1234454`

<a id="kvm-bin-true"></a>
## `/bin/true` — KVM verify

**Command and envelope.** Historical KVM `KvmGuest<Detcore>`, INFO logging,
strict verify, no relaxations, guest `/bin/true`.

The guest exited 0.  A retained single-run log reached syscall 32,
`munmap(...)`, then `exit_group(0)` and COMMIT turn 4 at
`1_767_225_600.002_798_000s`; final virtual time was
`1_767_225_600.003_298_000s`.  The verify result matched output/status but set
`compare_logs=false` and `bitwise_parity=false`.  Thus no two-run first
divergence exists: the exact boundary is the KVM policy branch that skips INFO
comparison.  Status remains guest-pass/non-L2.

Artifacts: `/tmp/hermit-backend-audit.7VyfIC/kvm-true.verify.json`, SHA-256
`6abbb8fb70893d1ca215c10536124a997b6cca39ec7998e9e5bf97a3300e9ade`,
and `kvm-true.info`, SHA-256
`1daa93b20f2714b89ec5c037fba85d7627c717f5517fbade6813c71b410e4185`.

<a id="kvm-bin-cat"></a>
## `/bin/cat /dev/null` — KVM verify

**Command and envelope.** Historical KVM `KvmGuest<Detcore>`, INFO logging,
strict verify, no relaxations, guest `/bin/cat /dev/null`.

The guest exited 0.  A retained single-run log completed syscall 119,
`close(2)=0`, then `exit_group(0)` and COMMIT turn 33 at
`1_767_225_600.018_749_000s`; final virtual time was
`1_767_225_600.019_249_000s`.  Verify matched output/status but skipped INFO,
so there is no measured syscall or instruction divergence.  The precise
non-L2 boundary is again `compare_logs=false` in the KVM verify policy.

Artifacts: `/tmp/hermit-backend-audit.7VyfIC/kvm-cat.verify.json`, SHA-256
`6abbb8fb70893d1ca215c10536124a997b6cca39ec7998e9e5bf97a3300e9ade`,
and `kvm-cat.info`, SHA-256
`fee2e4d06684c50b1e6c3ccc4a64e1fc28e2ee300cb81067d64762e6bd6ae0ce`.

<a id="sigchld-fork-kill-waitpid-control"></a>
## `custom/fork-kill-waitpid-control` — ptrace verify

**Command and envelope.**  Current dirty Hermit base
`d601651a3a0f4865b72af50a9c5967eaf60c351a`, ptrace backend, INFO logging,
strict canonical verify, no relaxations:

    timeout --preserve-status -k 5s 30s ./target/release/hermit \
      --log info --backend ptrace run --strict --verify --keep-logs \
      --verify-log-dir target/ignored/red-cell-sigchld-control-20260816T1300Z/logs \
      --verify-json target/ignored/red-cell-sigchld-control-20260816T1300Z/verdict.json \
      -- target/ignored/red-cell-sigchld-control-20260816T1300Z/fork_kill_waitpid_control

The source forks, has the child pause, sends `SIGKILL`, waits for that child,
and checks `WIFSIGNALED`/`WTERMSIG`.  Source SHA-256 is
`5cf89248f7510eb4c2d9eb7182d3a059cad7750cb4e666401baa90d5a498f696`;
guest binary SHA-256 is
`e86c5130dfeeeb75e3c6ef2797ba1352e6db96501a579734747630e021f46351`.

**Measured verdict and first divergence.**  Runtime was 0.03 seconds and the
command exited 1.  Both guest executions exited 0 and produced 128/128 INFO
messages.  Canonical comparison first diverged at scheduler turn 5, virtual
nanoseconds `1767225600005050760`.  Both streams matched through child dettid 5
finishing `set_robust_list` and parent dettid 3 finishing syscall 33,
`clone(...)=Ok(5)`.  Parent syscall 34 was `kill(5, SIGKILL)`.  At INFO message
101/100, run 1 first emitted scheduler `logically_kill` for child 5, whereas
run 2 first emitted `finish syscall #34: kill(5, 9)=Ok(0)`.

The guest result and syscall return agree; only the relative publication of
child-death cleanup and the parent's kill completion differs.  This is a real
SIGCHLD/child-lifecycle event-order nondeterminism.  Because the disputed
events race, a fresh run may move the first mismatch later into `wait4` child
reap visibility; that is the same failure cluster, not a different syscall
value defect.

**Artifacts.**  Root
`target/ignored/red-cell-sigchld-control-20260816T1300Z/`: `verdict.json`
SHA-256
`ec654dbe6cbc4e7518bc600e734dca6e7880f4e7f25176b2e969d63d32180c04`,
`log-diff.txt` SHA-256
`5e07c936c7f4779a432ed6220cfa7cd21ba7487b1efeb0360d68f12396be3e6c`,
and run logs SHA-256
`767bb54fd30243d8b9ea72586c3f99bc84d57b566d2c000fd07221608045790e`
and
`2f3817366ac4080f7517ca3eace0954c60e255fd226a841658c954725d97ad3b`.

<a id="poll-partial-revents-copyout"></a>
## `poll` partial-`revents` copyout control — ptrace record/replay verify

**Command and construction (PRE-FIX SNAPSHOT).**  This retained ptrace
record/replay failure used INFO logging, strict canonical verify, no
relaxations, and release-binary SHA-256
`9e05cf590d2cdc08e4e2d1d93924ea6e0400869f16ab2489cc4c902aafa26c55`:

    d=$(mktemp -d "$PWD/target/ignored/user-poll-copyout.XXXXXX")
    TMPDIR="$d" timeout --kill-after=3s 30s target/release/hermit \
      --log info --backend ptrace record start --strict --record-timeout 20 \
      --verify --verify-json "$d/verify.json" --data-dir "$d/data" \
      target/ignored/poll-red-cell-current-20260816T125558Z/revents_probe poll \
      >"$d/stdout" 2>"$d/stderr"

The probe places two `pollfd` structures across a writable/read-only page
boundary.  Both descriptors are ready.  Linux writes `POLLIN` to the first
`revents`, faults while copying the second, and returns `EFAULT`.  Source
SHA-256 is
`4587ff84acf2ed303d89632088b550849a8a958f8a454db4ab8d901a6e3d957a`;
binary SHA-256 is
`47d66212762926e9bd08239e51e06ffe5ae835d22c6dbddf631f4060aa93932d`.

**Measured verdict and first divergence.**  The retained run took 0.07
seconds, exited 1, and compared 99/99 INFO messages.  Syscall event 36 is
identical in both logs as `poll(..., 2, 3456)=EFAULT`, but the guest-visible
memory is not: recording prints `revents0=1`, replay leaves the initial
sentinel `revents0=4660` (`0x1234`).  The first INFO mismatch is the downstream
COMMIT for stdout at scheduler turn 8, with prior committed virtual times
`1767225600004629615` versus `1767225600004629645`.  The semantic first
divergence is therefore the partial copyout from syscall 36, before that
COMMIT.  Three subsequent repetitions reproduced the same output difference.

**Root cause.**  `hermit-cli/src/recorder/network.rs::handle_poll` constructs
the event through `result.and_then(...)`.  On `Err(EFAULT)` it records only the
error and never reads the prefix the kernel already copied out.  Replay sees
the error event before `handle_poll` can restore any `PollFd` array, leaving
the sentinel in guest memory.  This violates Linux's observable partial-copy
semantics; it is not scheduler noise.

**Artifacts.**  Root
`target/ignored/poll-red-cell-current-20260816T125558Z/timed/poll/`:
`verify.json` SHA-256
`285949ffb7e00c30302f879f51dced6c735d537d1b6c2a5dfc403de70bd19fad`,
`stderr` SHA-256
`ee1d6924a3862c1a8cf07fafad83470738d85a0d4183b0da8e4fd5a1509acf62`,
and record/replay logs SHA-256
`c5b86796cda6e8bd475a63761ffae9185909c7a6574910326e20a7ccac13d3bc`
and
`0833103efdc0667a82dfd87e2d7972ea068dc23dbebfbf9229fb6f7a10273842`.

**Recovered exact-current uncommitted status.**  The `0x10d` event schema records the exact
result, pointer presence, and readable `pollfd` prefix even when Linux returns
`EFAULT`; replay restores that prefix before returning the recorded error.  On
fingerprint `455174bb...` with release binary `73571dac...`, the canonical
probe exited 0 and matched **115/115** INFO
messages with `bitwise_parity=true` and null divergence fields.  Evidence:
`scratch/exec-final-validation-post-lifecycle-20260816-SOuxof/canonical/poll/verify.json`
(SHA-256
`4d461a529ce2a99ad67e53757b485bb5a7407e5ac83ab241bdf9903047896b7e`).

<a id="ppoll-partial-revents-copyout"></a>
## `ppoll` partial-`revents` copyout control — ptrace record/replay verify

**Command and envelope (PRE-FIX SNAPSHOT).**  The same `9e05cf59...` binary
selects `ppoll` when passed `plain`:

    d=$(mktemp -d "$PWD/target/ignored/user-ppoll-copyout.XXXXXX")
    TMPDIR="$d" timeout --kill-after=3s 30s target/release/hermit \
      --log info --backend ptrace record start --strict --record-timeout 20 \
      --verify --verify-json "$d/verify.json" --data-dir "$d/data" \
      target/ignored/poll-red-cell-current-20260816T125558Z/revents_probe plain \
      >"$d/stdout" 2>"$d/stderr"

**Precise no-result boundary.**  The retained run took 0.43 seconds and exited
1.  Recording again observes `EFAULT` with `revents0=1`; replay leaves 4660.
Unlike `poll`, replay detects the changed fixed-width output before canonical
log comparison: on thread 3 at syscall event 40 it expected
`write(1, ..., 94)` but got `write(1, ..., 97)`.  The replayer panics at
`hermit-cli/src/replayer.rs:877`; the panic crosses the non-unwinding
`clone_with_stack` callback, aborts, and the container reports `SIGSEGV`.
Consequently JSON is correctly `verdict=no_result`, `comparison=null`, with no
INFO counts or first scheduler turn.  Three subsequent repetitions reached
the same `no_result` boundary.

The underlying Linux-semantics defect is the same `result.and_then(...)`
partial-copyout loss in `handle_ppoll`; the extra panic is downstream replay
desynchronization handling, not the first cause.  Unlanded commit
`441c86886521c884911ac28ca4d106d121d6d480` addresses this class but is not an
ancestor of `origin/main`.

**Artifacts.**  Root
`target/ignored/poll-red-cell-current-20260816T125558Z/timed/ppoll/`:
`verify.json` SHA-256
`aef37a7cccb959c03d465d2400cf9fca5b9058a333023f350a4ba84eb5b6369b`,
`stderr` SHA-256
`48e79e8897bf90594703eb2377ba1f6a4501793263d1b236133f8549d20e7295`,
and `tmp/hermit-report-N1QL8y.diff` SHA-256
`df8633e07545799209a268e7d044bbadc84e426e0c52d1d62f0e2757639a4cc8`.

**Recovered exact-current uncommitted status.**  Schema `0x10d` separately records the
`ppoll` result, readable `pollfd` prefix, timeout-pointer presence, and exact
remaining timeout; replay restores both guest-visible outputs without
rounding or freezing virtual time.  On fingerprint `455174bb...` with release
binary `73571dac...`, the canonical probe exited 0 and matched **115/115** INFO
messages with `bitwise_parity=true` and null divergence fields.  Evidence:
`scratch/exec-final-validation-post-lifecycle-20260816-SOuxof/canonical/ppoll/verify.json`
(SHA-256
`4d461a529ce2a99ad67e53757b485bb5a7407e5ac83ab241bdf9903047896b7e`).

<a id="poll-ppoll-invalid-nfds"></a>
## `poll`/`ppoll` invalid-`nfds` control — ptrace record/replay verify

**Boundary and recovered verdict.**  This control passes an invalid descriptor
count and requires Linux's exact `EINVAL` behavior without attempting a bogus
array copy.  It guards the error path adjacent to the partial-copyout repair;
there was no retained pre-fix semantic divergence for this input.  On the
exact-current snapshot (`455174bb...`, binary `73571dac...`, schema
`0x10d`) the canonical command exited 0 and matched **95/95** INFO messages
with `bitwise_parity=true` and null scheduler-turn and virtual-time divergence
fields.

**Artifacts.**  Evidence is
`scratch/exec-final-validation-post-lifecycle-20260816-SOuxof/canonical/invalid-nfds/verify.json`
(SHA-256
`acd9b2ba8b1e7fc0c0bd7147f6b97ffccfec591568ceaf57fafc9d0160e75b7b`)
and adjacent `stderr` (SHA-256
`54460386b39adf0beec4aa22aa633fa20a7e55654b65938cb85bf95c3e12c793`).

<a id="liteinst-record-getpid"></a>
## `system-utils/record-getpid` — LiteInst host-hybrid verify

**Command and backend reality.**  The exact manifest command is printed by:

    ./tests/manifest-cli.rs get system-utils/record-getpid \
      --mode verify --backend liteinst --lane portable --log info

It compiles `tests/c/getpid.c`, then runs INFO-level strict canonical verify
with `--backend liteinst --no-virtualize-cpuid --max-timeslice=disabled`.
Stderr makes the architecture explicit:
`[liteinst host hybrid] activation verified (traps=1, hooks=31); Detcore Tool
active in ptrace host`.  This is Reverie-LiteInst preprocessing plus the ptrace
Detcore Tool, not a separate full Detcore backend.

**Measured verdict.**  The command exited 0, stdout was `My pid: 3`, and JSON
reported `verified=true`, `verdict=matched`, `bitwise_parity=true`, canonical
INFO comparison, and 908/908 compared messages.  Both divergence fields are
null.  This is exact-current canonical parity for this cell under the two
declared relaxations; it is not evidence for arbitrary LiteInst workloads and
is not an unrelaxed project-wide L2 claim.

**Artifacts.**  Root
`target/ignored/m2-red-cell-debug/liteinst-record-getpid-20260816.cC9v5q/`:
`verify.json` SHA-256
`ca1e55cdd9c4dff568daadb0e42154711acc312d98c4e8f1b3247e53bf162457`,
`stderr` SHA-256
`a07c4a84c9434127703d0210696bcdba56be616beabff47afae10bc73f9916d7`,
`stdout` SHA-256
`7d65259dd0f4861331834b3f73013d851b44389c6162f00a24a0176bba121251`,
and run logs SHA-256
`4252d10c13ee28937f28613ab3a362daf30ccfc7e8718cfd836f9eaa40f9652b`
and
`2489178203ff2300cc5d66f328f44c537969c5c9e4dd17be03c0ac34a052f769`.

## Current backend source snapshot

The final diagnosis is against dirty Hermit HEAD
`d601651a3a0f4865b72af50a9c5967eaf60c351a`:

- `detcore-dbt/src/lib.rs` SHA-256
  `d0cfbe06bd706e83bd77b2d451ed2e3ce6a8d770659b7ab2c2a3ecb4ea8e818f`
- `hermit-cli/src/bin/hermit/backends.rs` SHA-256
  `ac04a6e7297517104be12ef3b7ed1f506044aace831fd663d8a5ebfefdea342c`
- `hermit-cli/src/bin/hermit/verify.rs` SHA-256
  `3f8f020d2bddb355b894420c254cffb1370ba3d4d4dc21f50b414dda738d8a0c`
- `hermit-cli/src/lib.rs` SHA-256
  `ace39e870ec6fd931192324c279fc568d7da9333db8b3460c90f5543e0a9ef91`

Relevant Reverie source is dirty HEAD
`ee6716a65d41e8f1d65ee32efa4aafa910b9cf29`:

- `reverie-dbt/native/client.c` SHA-256
  `5fd7ccc07ea188a0c7b0653cb8d4c43ac97bd2cf438e1adcabe7483532c53a37`
- `reverie-dbt/src/launcher.rs` SHA-256
  `8f9af73a474fc889ca6e616ad3eabe70d47d8d3c0c7f7d28419c50dee8437fbe`
- `reverie-dbt/src/lib.rs` SHA-256
  `0c763a39d467772da502bb853d401d55d438aed37cdec27f29fe24618daa29ed`
- `reverie-dbt/src/evidence.rs` SHA-256
  `c3a7a0c5b201e82ece0469fed44f8910bce27726ed7e1a81669a631e25ceeb09`

## Maintenance rule

Add a section here whenever a new scorecard cell is manually examined.  Do not
mark a red cell "nondeterministic" without recording its first divergent
event; if comparison never began, record the exact `no_result` boundary
instead.
