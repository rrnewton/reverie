# assertions-3 handoff — 2026-09-17

Successor to recon-2 in this slot. Their handoff,
`HANDOFF-recon-2-20260917.md`, is still worth reading and I have NOT deleted it.
Read it first; this file says what changed under it.

Facts and evidence are in the TaskGraph notes on
`four-named-assertions-across-four-backends-are-what-keeps-main-red` and on the
pull requests. This is the reading.

---

## Where the four stand now

| backend | state |
|---|---|
| **DBT** | **Root-caused, and GREEN ON MAIN.** 3/3 at `cb5b4da5e`, 105 and 99 guest syscalls. My PR 3061 is CLOSED as superseded — see below. |
| **LiteInst** | Fixed and landed by its lane, hermit#3048. A defective test, not a product defect. |
| **KVM** | **Fixed on main, measured.** `24 passed; 0 failed; 89 filtered` at plain `31289f459` / pin `596b9ade`, against the historical `23 passed; 1 failed; 89 filtered`. Credit is the KVM lane's landed `cdf828dde`; I only supplied the run they said was still pending. |
| **SaBRe** | Still failing. Reproducible on demand now. The live repair is hermit#3054 — **which does not currently run at all**; see below. |

## DBT — what recon-2 had right, and the one thing they had wrong

They found the failing assertion, `DR_ASSERT(evidence_callback_depth == 0)` at
reverie `reverie-dbt/native/client.c:3995`, and that is exactly right.

**Their framing of it as container-versus-host was confounded, and the confound
was their own control.** `detcore-dbt/src/lib.rs:999` `prepare_native_client()`
has two branches: the packaged `install_pkg` client when
`hermit_resources::install_dir()` resolves, and otherwise a cmake build of
Reverie's client. Hermit's packaged producer has passed `-mtls-dialect=gnu`
since `f0e404b6c`; Reverie's on-demand producer did not. Setting
`HERMIT_INSTALL_DIR` to hold the runtime constant is precisely the input that
selects the first branch. So their host run and the validate run loaded
**different client binaries**, and the difference is the whole defect.

I would not have caught it without their note recording exactly what they set.
That is an argument for writing the method down, not just the result.

The mechanism, each link checked separately:

1. Measured in the pinned image: gcc 15.3.0 compiles the client's
   `_Thread_local evidence_callback_depth` to `R_X86_64_TLSDESC` by default and
   to `R_X86_64_DTPMOD64` + `__tls_get_addr` with `-mtls-dialect=gnu`.
2. Vendored DynamoRIO's x86 `tlsdesc_resolver`, `core/unix/module_elf.c:1442-1448`,
   is `ASSERT_NOT_IMPLEMENTED(false); return 0;` — and in release the assert
   compiles out, so the zero is silent.
3. A zero TLS offset aliases the counter onto `%fs:0`, the TCB self-pointer. It
   is never zero, and every `enter()` corrupts the TCB. (That also predicts the
   older SIGSEGV-with-core era. I did not re-run the old pin to confirm.)
4. `test.hermit_unit` forwards 24 env names and `HERMIT_INSTALL_DIR` is not one
   of them, and under nextest the binary is `target/debug/deps/hermit-<hash>`, so
   `built_in_place` looks for `target/debug/install_pkg` while the packaged tree
   is at `target/install_pkg`. On-demand branch, always.

The repair is Reverie `6ae69f57`. Hermit pinned `596b9ade` while I was
diagnosing this and now pins `4db9ddb6`, a descendant of `6ae69f57`, so **main
has the fix as of `cb5b4da5e`** and I verified it there.

## The habit that actually did the work today

recon-2 closed with "an instrument that returns a confident answer about the
wrong thing", and the defence being to check a result against something known
independently. **Both of today's substantive findings are that same shape, and
one of them is in their own handoff.**

- DBT: a control that held everything constant *except* the artifact under test.
- SaBRe: their vDSO exclusion reads `glibc is 2.34`. That is the **host**. The
  pinned image is **glibc 2.42**, past the 2.41 that adopts `vgetrandom`. Their
  handoff instructs the next reader not to restart on coverage-gap theories and
  treats the vDSO as closed. **That instruction is wrong and should not be
  followed.**

So the rule generalises past instruments to *machines*: check which one you
measured. The reading was correct for the host and the failure is in the
container.

## SaBRe — reproduction is unblocked; here is the recipe

recon-2 was stopped by staging, not understanding. Driving the node the way
validate does works. It must be **one** container invocation, because `target/`
is a separate volume that does not survive between them:

```bash
ci/hermetic/run-in-pinned-root.sh --src . --out <dir> --src-rw \
  --cargo-home ~/.cargo -- bash -lc '
    cd /src
    export CARGO_BUILD_JOBS=8 HERMIT_INSTALL_FORCE_RESTAGE=<tag>
    cargo build --release --locked -p hermit --features third-party-backends \
      -p detcore-dbt -p detcore-sabre -p hermit-install
    HERMIT_SABRE_TEST_BINARY=$PWD/target/release/hermit \
    HERMIT_SABRE_BINARY=$PWD/target/install_pkg/rsrcs/sabre \
      cargo test --release --locked -p hermit --features third-party-backends \
        --test sabre_examples -- --nocapture --test-threads=1'
```

What it says at Reverie `6ae69f57`: `FAILED. 5 passed; 1 failed` in 7.95s. The
ptrace digest `088f0992f02f426933bd4c0248fed209` is byte-identical to recon-2's
two earlier readings; the SaBRe digest is a third distinct value.

**The pin alone does not fix SaBRe.** Three SaBRe commits ride along in
`596b9ade..6ae69f57` and it still fails. A Hermit-side change is required, which
is what hermit#3054 is.

Two further facts for whoever continues:

- glibc 2.42's public `getrandom` is a **14-byte thunk**, not an
  implementation: `endbr64; mov ecx,0x1; jmp <getrandom_vdso>`. The vDSO is the
  guest's actual route in this image.
- Reverie rewriter.c:448 *does* patch the syscall sites inside
  `__vdso_getrandom`, but only `if (sbr_bootstrap_enabled())`, which reads
  `REVERIE_SABRE_BOOTSTRAP_V1` — **a name that appears nowhere in hermit**. So
  that rewrite is currently unreachable. hermit#3054's `sabre_bootstrap.rs` and
  `supports_loader_bootstrap() -> true` are what would arm it.

I did **not** determine why the current non-transparent libc detour fails to
intercept. Reading says it should install. That question may be moot if 3054
lands, since it deliberately makes that detour transparent.

### hermit#3054 does not run at its own head, and that is the immediate blocker

I used the recipe above on `c961f85503dee4629c00981a7707afe39da71a3e` to produce
the "actual guest evidence" its description says it is waiting for. It got worse
rather than better:

| head | result |
|---|---|
| main + pin `6ae69f57` | `5 passed; 1 failed` in **7.95s**, only getrandom, at the parity assertion |
| 3054 `c961f8550` | `2 passed; 4 failed` in **0.45s**, four methods, all at the run-failed panic |

```
/src/target/install_pkg/rsrcs/sabre: error while loading shared libraries:
libz.so.1: cannot open shared object file: No such file or directory
```

Its installer change stages a Cargo-built loader (`sha256 e4c4903d…`) in place of
the gitlink-built one (`sha256 36516529…`), and the new one cannot resolve zlib.
**libz IS in the image** — the point is that this image has **no global library
search path at all**: `LD_LIBRARY_PATH` is unset and `ldconfig -p` is empty, so
everything resolves purely by baked-in RPATH/RUNPATH. So it is an RPATH gap, not
a missing dependency.

There is a precedent for the fix in-tree:
`hermit-install/native-client/CMakeLists.txt` sets `$ORIGIN`-relative
`BUILD_RPATH`/`INSTALL_RPATH` plus `LINKER:--disable-new-dtags` for this exact
class of problem.

I did not `readelf -d` the staged loader — `target/` is a separate volume and it
was gone by the time I looked — so "lacks a zlib RUNPATH" is an inference from
the loader's own error plus the absent search path, not a direct read. One
`readelf -d` settles it for whoever has the build in hand.

Hermit's fail-closed check behaved correctly throughout: it refused with "no
syscall was intercepted … not a Hermit guarantee" rather than reporting a clean
run.

## The pull requests, and a coordination point that needs a human

- **hermit#3061** (mine): **CLOSED as superseded, and how that happened is the
  transferable part.** While it sat unreviewed, `cb5b4da5e` landed for the
  LiteInst lane and moved main's pin to Reverie `4db9ddb6` — a **descendant** of
  the `6ae69f57` I was adopting, carrying the same `-mtls-dialect=gnu` line. So
  main picked up the DBT repair as a side effect of somebody else's LiteInst pin
  bump, and merging mine would have been a pin **downgrade**, which
  `check-reverie-pin.rs` clause 2 forbids.

  **GitHub reported only `CONFLICTING`.** That reads as "rebase me", not "you are
  done". Had I rebased on that prompt instead of reading what had landed, I would
  have pushed a downgrade into review. A pull request whose subject is a *pin*
  can be satisfied by an unrelated pin bump and nothing announces it — when a
  pin PR goes conflicting, read the new pin before rebasing.

  I verified main rather than inferring it: 3/3 at `cb5b4da5e`, thread-tree
  `[3]`, 105 and 99 syscalls, against the `596b9ade` control's 0 syscalls.
- **hermit#3058** (recon-2's): I reviewed it as a non-author and approved. One
  correction worth carrying: the DynamoRIO assertion text **was** in the
  retained validate log, at `test.hermit_unit.log` lines 359 and 764 — in the
  `..._status_...` section, which calls `run_with_backend` and does not capture
  stderr, so it flowed through. The capturing site is the one that lost it. That
  narrows "could not be read from the run at all" to a reading failure rather
  than a retention failure. 3058 is still right to land.
- **hermit#3054** (draft, another author): **contains my pin move already**,
  plus the SaBRe bootstrap repair and installer work — 24 files, Trigger 2 and 3
  human review, two exact-head approvals. 3061 duplicates its pin hunks and will
  conflict.

That question resolved itself: main took the pin independently, so there is no
landing-order decision left. 3054 should rebase; its pin hunks are now behind
main's.

## KVM — how to run it, since two official attempts could not

The test self-guards on `/dev/kvm` and **a skip reports as a pass**, so the
validate node refuses unless the device actually opens. Do the same:

```bash
if ! exec 9<>/dev/kvm; then echo "REFUSING"; exit 1; fi; exec 9<&-
cargo test --locked -p hermit --features third-party-backends --test cli \
  -- --nocapture --test-threads=1 run_kvm_
```

I ran it at plain main and at my pin branch; both give 24/24 in ~8.5s. The
`89 filtered` matches the historical failing run, so the denominators agree and
it is a like-for-like comparison rather than a narrower run that got further.

The historical exit-127 guest loader failure is **not** retrospectively
explained by this. I observed that it no longer happens, not why.

## Traps that cost me time, beyond the ones recon-2 already listed

- `cargo metadata` in a fresh hermit worktree dies with `no matching package
  named dagrun` until you `git submodule update --init --recursive agent-utils`.
- `tg-note-verified` refuses an unnamespaced `/tmp` path. Use
  `/tmp/<agent-id>-<purpose>.md`.
- Committing a pin below the Reverie tip trips an advisory. It is a choice, not
  an error: `HERMIT_PIN_BELOW_MASTER_ACK=1 git commit ...`, and say why in the
  body.
- **`pgrep -f 'run-in-pinned-root'` matches its own command line.** I used it to
  ask whether a container was still going and it answered RUNNING every single
  time, including when nothing was running; a wait loop built on it never
  exited. No result here depends on it — the logs are the evidence — but it cost
  wall-clock. Capture the pid and use `ps -p <pid>`. This is recon-2's own
  closing lesson, an instrument confidently answering about the wrong thing, and
  I walked into it while writing about it.

## Where things are on disk

- `ignored/assertions-3/evidence/` — the **12 retained logs**, with
  `SHA256SUMS.txt` beside them and the same hashes in the task notes.
- **I removed the worktrees and the container output volumes deliberately.**
  They were 24 GB of build scratch against 112 KB of evidence, and this project
  has had disk-fill incidents. Nothing is lost: every result is in the logs, and
  each worktree is one `git worktree add --detach <path> <sha>` away. If you
  recreate one, remember
  `git submodule update --init --recursive agent-utils` or `cargo metadata` dies
  with `no matching package named dagrun`.
- Live status index: `/home/newton/temp/assertions-3-status.md`.

---

# Second task, same lane: structured-test-results-refused...hides-what-failed

## What was actually left of it

Two of its three obligations were already discharged before I arrived, and
saying so plainly matters because the title still reads as though the whole
thing is open.

- **Action, "why does the refusal fire":** answered. A stray
  `.hermit-verify-summary-*` file changed source identity, prepared executables
  were declared stale, the nodes refused before running. Fixed by hermit#3026,
  merged.
- **Verify (a), a failing node names its test ids:** satisfied by live evidence.
- **Verify (b), a result that cannot be written must read as
  could-not-determine:** the open half. That was hermit#3027, blocked.

## hermit#3066, and why it is a replacement rather than an update

Main moved a long way under 3027. `has_product_failure_evidence` now reads the
typed `test_results_error` with completion guards, and the real-lane assertion
that was red through runs 1824–158a89f6 **passes at current main**. So the
AU2781 contract gap and the fixture failure were repaired by others. 3027 was
written against the old AU14589 contract, still carried the since-landed 3026
files, and was conflicting — fixing it in place would have left nothing of it.
I closed it with each recorded finding answered, then wrote hermit#3066: one
file, rebased, mergeable.

The change reclassifies **only** the never-written cause to `no_result`. Two
points that are not cosmetic:

- **`no_result`, not `understood_infrastructure_failure`.** Nothing diagnosed a
  cause, and that bucket is projected with a named cause it would leave empty.
  This was the 3027 reviewer's own suggested alternative.
- **A hazard none of the reviews named.** A node that exited **nonzero** with no
  report DID fail a condition; the missing report means the failure cannot be
  NAMED, and naming is not the same as existing. So the predicate independently
  requires `returncode == Some(0)` and no collected wall/CPU/OOM breach.
  Reclassifying purely on "the report is absent" would have excused real
  failures — quietly, and in the direction that makes a red tree look green.

## The two things to inherit

**The count will not move, and the task's framing implies it will.** In run 1819
the refusal was 17 of 26 failures. At current main the stale-executable
signature is gone and the named DBT and KVM assertions pass. 3066 is what makes
a FUTURE refusal read honestly; it is not what unblocks measurement today.
Anyone expecting a large immediate drop in the failure count will be
disappointed and may conclude the fix did not work.

**The rust-script warm cache will lie to you, and it lied to me.** My first
control reported the self-test still PASSING with the repair disabled — which
would have meant my new assertions could not fail, the exact defect 3027's
author caught in themselves. It was the previous binary. **Re-run
`./ci/prepare-rust-scripts.sh` after editing an *included module* such as
`scripts/lib/validate_classification.rs`;** the freshness check does not catch
it. Every number I report was taken after a forced rebuild, and the control then
failed correctly with the two new tests while all three pre-existing
classification tests still passed — which is what rules out goalpost moving.

## The task is OPEN on purpose, and there is a policy conflict behind it

hermit's AGENTS.md says `implemented` but not `landed` is a valid CLOSED state.
The parent's `close-task` offers only a code reference that must already be an
ancestor of main, or `--disposition` meaning nothing to land. There is no path
for the state hermit's policy names as valid, and both refused me.

I did not route around it. `--disposition 'superseded by hermit#3066'` would
have been accepted and would have been false — 3066 is this task's own work, and
recording it that way erases exactly the landing debt the verifier exists to
keep visible. **The row should close via `close-task --code` the moment 3066
lands.** Read the open status as landing debt, not as work outstanding.

## SaBRe, continued from above — the loader now loads

3054's author fixed the `libz.so.1` blocker I reported (Reverie `226c3e31`,
loader CMakeLists only — the RPATH class I predicted). Confirmed: `ldd` on the
staged loader resolves everything from the nix store.

At the new head `7aeca0e0c` the suite is still **2 passed / 4 failed in 0.52s**,
now on four bootstrap refusals rather than a missing library:

| test | refusal |
|---|---|
| `sabre_libc_getrandom_is_deterministic` | `bootstrap mapped object identity mismatch` |
| `sabre_non_racy_examples_verify_current_envelope` | `argv pointer leaves initial stack` |
| `sabre_root_pid_matches_ptrace` | `ambiguous bootstrap ELF load bias` |
| `sabre_scheduler_empty_info_precedes_fallback_completed_info` | same |

Against 5 passed / 1 failed at plain main + `6ae69f57`, so **three tests that
passed now fail** and no SaBRe guest executes. The getrandom determinism
question is still unmeasured at that head — not confirmed, not refuted. Whether
each refusal is a bootstrap defect or a host property it is correctly rejecting,
I could not tell from outside it, and I said so there rather than guessing.

## Cross-lane question, answered structurally

`validate_checkout_cleanup` and `wrkslots_audit` are health-tick reminders in
`ci-hub/health/tick-hub.yaml`, run as plain `python3` by the parent's tick. They
are not dagrun validate nodes and never declare a `result_manifests` structured
report, so the refusal mechanism **cannot** reach them. That is a structural
negative rather than "I looked and did not see it".

---

# Third thread: SaBRe root-caused far enough to hand over cleanly

`sabre_leaks_libc_getrandom` (P1) now carries the full characterisation in its
own description, so it survives this row.

## The detour was never the mechanism, and that is measured

I spent a long time reasoning about whether SaBRe's
`#[detour(lib = "libc", func = "getrandom")]` installs on glibc 2.42's 14-byte
thunk, eliminating candidate after candidate from source — instruction space,
`JUMP_SIZE`, the safe-instruction predicate, library-name matching, `.dynsym`
presence, section headers, IFUNC, the bootstrap bypass. Every elimination held
and they all pointed the same way: it *should* install.

**Stop reasoning and look.** A guest that resolves `getrandom` with `dlsym` and
prints the first four bytes at that address, from inside itself:

| environment | backend | entry bytes | output |
|---|---|---|---|
| container | ptrace | `f3 0f 1e fa` | `088f0992…` |
| container | sabre | `f3 0f 1e fa` | `e57db31d…` |
| host | ptrace | `f3 0f 1e fa` | `12ab3768…` |
| host | sabre | `f3 0f 1e fa` | `12ab3768…` |

Unpatched under SaBRe **everywhere** — and the host is determinized anyway. The
detour is not the operative mechanism, in either environment. Both branches of
the partition I had carefully set up were wrong, because the partition assumed
the detour mattered.

The probe is self-controlled: its container ptrace arm reproduces `088f0992…`,
the Detcore digest measured five times before, so the instrument reads what it
claims to. Source retained at
`ignored/assertions-3/evidence/detour-probe-source.rs` — drop it into any build.

## The contradiction is resolved: the comment is wrong

Same technique, one step further — dump the function BODY, not just the entry.

Host, inside `getrandom` at offset +16:

```
ptrace   b8 3e 01 00 00  0f 05      mov eax,0x13e ; syscall
sabre    e9 a0 62 38 fe  90 90      jmp rel32 ; nop ; nop
```

**SaBRe rewrites the syscall instruction.** Following the jump lands on the
relocated `mov` plus SaBRe's trampoline preamble (`48 8d 64 24 80` red-zone
skip, `9c` pushfq, `41 5b`).

Container: **exactly one line of the entire dump differs, and it is the result.**
The 14-byte thunk and the head of `getrandom_vdso` are byte-identical under both
backends. SaBRe modified nothing the guest executes.

So the mechanism is syscall-instruction rewriting, and glibc 2.41 moved that
instruction off the executed path into a userspace ChaCha behind
`getrandom_vdso`. Nothing left to rewrite.

**`detcore-sabre`'s comment is factually wrong.** It says that site is one
"SaBRe deliberately does not rewrite" and offers that as the detour's reason for
existing. That single false sentence is why this took two lanes: it asserts the
working mechanism does not work, which makes the host's behaviour inexplicable
and sends every reader hunting for a fourth thing. **Fix the comment when you
fix the code** — it will mislead the next person exactly as it misled us.

Adjacent: `registers_libc_getrandom_detour` asserts the detour is *registered*.
Registration is not installation, and installation demonstrably does not happen.
Every other `#[detour]` in that crate rests on the same assumption.

## The lesson I keep re-learning, stated once more because I paid for it twice

recon-2's closing note was about instruments that answer confidently about the
wrong thing. Today that cost me three separate readings:

- `pgrep -f '<pattern>'` matching its own command line, so a wait loop never
  exited;
- a warm rust-script cache running the previous binary, so a control reported
  PASSED with the repair disabled;
- and this one — an elimination chain that was individually correct at every
  step and collectively aimed at a component that turned out to be inert.

The third is the interesting one, because nothing was *wrong*. Each elimination
holds. The defect was that I never checked whether the thing I was eliminating
causes for was doing anything at all. **Before proving why a mechanism fails,
prove it runs.** One four-byte read settled what a day of source reading could
not.

## Also landed this session, outside hermit

dev-hermit `e3bebab66` — a reviewer of hermit#3066 found that my change creates
the first ledger gate shape with `result=no_result` AND `exit_code=0`, and the
parent's `gate_kind()` had no mapping for `no_result`, so its exit-status
fallback would count that node as a **pass**. That is the exact inversion 3066
exists to prevent, arriving through a different repository. Fixed, tested in
both directions, verified by content on `origin/main`, task closed against the
SHA.

Worth carrying: a reviewer who follows a value **out of the repository** catches
things the author structurally cannot. I was reading hermit's classifier; the
consequence landed in the parent's reader.

The wider gap there is still open and deliberately so: the same fallback reads
*any* unrecognised `result` with exit 0 as a pass. I did not change it because I
could not establish which result strings reach `gate_kind` as gate rows — a grep
over validate JSON also matches E2E cell records, which is the wrong
denominator. Recorded on `dev_hermit_gate_kind`.

**Postscript on that gate_kind landing.** The reviewer did not stop at my commit
— they added a second one, `190842c1`, and the task closed against theirs. What
they added is precisely the piece I declined: the denominator, measured over the
whole tracked ledger at `origin/main`, 2,579 rows and 80,652 gate records, plus
verification against the real producer artifact rather than a hand-built dict.
I had refused to widen the fix without the right population; they went and got
the population. That is the better answer, and it is worth noting that the
refusal and the follow-through were both necessary — declining to guess is not
the same as declining to know.

They now hold `persist-the-per-node-reason-so-a-no-result-can-be-attributed-after-the-fact`,
the sequel. I gave that lane three data points from this work: reasons DO flow
intact for the total-interception-failure class and for backend launch refusals
(two positives, no gap observed), and one negative that bounds the task — the
SaBRe getrandom defect produces no reason **anywhere**, not because a reason was
dropped but because nothing at node level failed. It is a coverage-shaped blind
spot, not a persistence-shaped one, and reason persistence cannot close it. The
instrument that would is per-syscall coverage: did a syscall the tool subscribed
to ever reach it.

---

# The end of the SaBRe thread: the obvious fix was tried and measured insufficient

Two more results after the root cause, both at hermit#3054's heads.

## The scoreboard was re-measured, because a relayed claim decays

I had reported DBT green at `cb5b4da5e` and KVM green at `31289f459`, and that
was relayed upward. Main then took four commits including a Reverie pin bump to
`114b3094`. **A pin move is exactly what regresses those**, so I re-ran all four
named assertions at one head, `7f84482cd`, in a single build:

```
DBT       ok.     3 passed; 0 failed; 339 filtered   2.71s   105 guest syscalls
KVM       ok.    24 passed; 0 failed;  89 filtered   9.72s
LiteInst  ok.     1 passed; 0 failed;  23 filtered   5.25s   named test, by name
SaBRe     FAILED. 5 passed; 1 failed;   0 filtered   8.14s
```

It held. But the habit is the point: **your own earlier measurement is a claim,
and it ages.** If you relay something upward, own re-checking it when the base
moves.

## 3054's mechanism is armed, firing, and still does not fix it

Their newest head `0ad33fe44` cleared every bootstrap refusal I had reported
(2 passed/4 failed → 5 passed/1 failed, back to main's shape). So I asked whether
the vDSO rewrite was actually happening. A guest reads its own `[vdso]` mapping
from `/proc/self/maps` and counts syscall instructions across all 8192 bytes:

| | ptrace | sabre |
|---|---|---|
| `vdso-syscall-insns` | **7** | **3** |
| `vdso-hash` | `47f1d953ade5ae60` | `7adb60e57fe3a7cf` |
| `bytes` | `088f0992…` | `4ec85582…` fresh |

**SaBRe rewrote four of the kernel vDSO's seven syscall sites.**
`rewriter.c:448` is reached, the gate is armed, the mechanism runs — and
getrandom still leaks, a sixth distinct SaBRe digest against a seventh
byte-identical ptrace one.

## The conclusion, and it is bigger than a failing test

`vgetrandom` does not take per-call bytes from a syscall. The kernel allocates an
opaque per-thread state through a syscall and **populates that memory** with
seed material; the vDSO generates from a userspace ChaCha over it, issuing no
syscall per call.

Detcore determinizes syscall **return values**. Entropy arriving as
kernel-written **memory** is outside that model. So rewriting the syscalls
*around* the vDSO lets Detcore see the allocation and the fallbacks — plausibly
the four sites that vanished — while the bytes the guest gets come from state
Detcore never substituted.

Flagged as the leading explanation with its basis, not as measured fact. What IS
measured is that the rewrite happens and the output is still host entropy.

**Any fix must either keep the guest off the vDSO path entirely, or emulate the
state page's contents rather than the syscalls around it.** Both are larger than
a rewrite gate. I told 3054 so, and suggested decoupling — land what is measured
to work, track getrandom parity separately, rather than hold the PR behind a
test this design cannot satisfy.

## What is left unmeasured, precisely

Which four of the seven sites were rewritten, and whether any is the vgetrandom
state allocation. My probe counts and hashes the mapping; it does not attribute
sites to functions. That attribution confirms or refutes the explanation above
and is one more probe. Source is in
`ignored/assertions-3/evidence/vdso-rewrite-probe-source.rs`, reusable in one
build.

## Three probes, one technique

Every step past the source-reading wall came from the same move: **make the
guest read its own memory and tell you.** Entry bytes, function body, vDSO
mapping. Each took one build and settled something a day of reading could not —
and the first one overturned a conclusion I had reached by careful elimination.
When you are reasoning about what a rewriter did to a process, ask the process.

---

# Coda: I caught myself doing the thing, and the catch is the lesson

I closed the last section saying "ask the process." Then I asked the process a
question with no control and believed the answer.

**What happened.** I measured the whole `[vdso]` mapping at 3054's head, saw
syscall instructions drop 7 → 3, and reported to that PR that its bootstrap gate
was firing. Then I read `rewriter.c` more carefully: it detours
`__vdso_getcpu`, `__vdso_time`, `__vdso_gettimeofday` and `__vdso_clock_gettime`
**ungated**, at lines 417-446, *before* the `sbr_bootstrap_enabled()` block at
448. Four ungated detours. Four missing syscalls.

My proof was exactly as consistent with the gate never firing at all. I had
measured one head and compared it to nothing.

**The control settles it, per symbol.** Walking the vDSO's own dynsym from
inside the guest and counting syscall instructions inside each symbol's extent:

```
                    main (gate off)           3054 (gate on)
__vdso_getrandom    2 syscalls, hash          0 syscalls, hash
                    f6b46ea15a3683d7          33c1691575f7091a
                    IDENTICAL to ptrace       ptrace still 2 / f6b46ea1
```

At main, `__vdso_getrandom` is byte-identical — every change there is one of the
four ungated neighbours. At 3054's head, **both** of its syscall instructions are
gone. The gate fires and reaches getrandom specifically.

So the conclusion I reported was right. **The evidence I gave for it decided
nothing**, and I nearly posted the opposite correction off the same number
facing the other way. Both readings were available from one datum, which is the
definition of a datum that decides nothing.

**And the hard finding is now much sharper.** Removing *every* syscall
instruction inside `__vdso_getrandom` still leaves the output fresh entropy. So
vgetrandom's per-call bytes never come from a syscall: the kernel populates an
opaque per-thread state through a syscall and the vDSO expands it with a
userspace ChaCha. Detcore substitutes syscall **return values** and never
substituted that state page. A syscall-interception determinizer cannot reach
this by intercepting syscalls, however completely. That is a limit of the model,
not a bug in the wiring.

**The rule to inherit, which is narrower than "ask the process".** Asking the
process is necessary and not sufficient. A measurement of one configuration is
not evidence about a mechanism; only a measurement against a configuration where
that mechanism is known to be off is. I ran three probes that worked because
each carried its own control — the ptrace arm — and one that did not, because
its control was a different head I never ran. **When the probe's control is "a
build I didn't make", there is no control.**


---

# The control I owed, and the second defect it turned up

I had been asserting the vDSO was the *sole* leak on the strength of glibc's
structure. That was still an inference. The control is three getrandom routes in
**one** guest process, plain main, pinned container:

| route | SaBRe stable across runs | matches ptrace |
|---|---|---|
| A — inline asm, syscall in the **guest's own** `.text` | **yes** | no |
| B — libc `syscall(SYS_getrandom, …)` | **yes** | no |
| C — libc `getrandom()` via the vDSO | **no** | no |

A and B determinize, so the rewriting works, the guest reaches Detcore, and
Detcore determinizes getrandom in that container. **Only C varies.** The vDSO
path is the whole cause, measured.

## And A and B still do not match ptrace

That is the part worth inheriting. Concatenating the guest's draws in order:

```
ptrace  12ab376880e4f387 b99fd21da39c4e9f 7c2e57e8739e284b 622a3509…
sabre   63b091c267b3dc67 12ab376880e4f387 b99fd21da39c4e9f 7c2e57e8739e284b
```

**SaBRe's stream is ptrace's with exactly one extra 8-byte draw in front.** After
that prefix they agree byte for byte, 24 of 24, verified by string comparison.

This is a **second, independent defect**. It shifts every determinized random
value the guest receives, so any cross-backend randomness parity assertion fails
on it alone — and it survives whatever happens to the vDSO. Anyone who fixes the
vDSO and expects `sabre_libc_getrandom_is_deterministic` to go green will be
surprised.

## The hypothesis, with its fix site named

`detcore-sabre` already has a mechanism whose stated job is to prevent exactly
this. `Plugin::post_load_syscall_pending` carries the comment "the SaBRe-injected
runtime requests its hash seed on the first rewritten syscall after post-load.
Keep that tool-private draw out of Detcore's guest-visible random stream", and
the guard at `lib.rs:159` matches on **shape**:

```rust
call.buflen() == 32 && call.flags() == libc::GRND_NONBLOCK as usize
```

A hash seed is naturally a `u64` — **eight** bytes. The leak is exactly eight
bytes. If the real request is 8 rather than 32, the guard never matches, the draw
is served from the guest's stream, and everything shifts by one. Fits every
number.

**I did not observe the request's buflen or flags**, so it is a hypothesis with a
named check, not a finding. One `dbg!` at that guard settles it.

## What the probe's own failure taught

My first inline asm put the flags argument in `r10` — argument four — and
getrandom takes three, so `rdx` held garbage and the kernel returned `EINVAL`.
The guest printed `A-FAILED -22` and the run refused. **A probe that fails loudly
on its own bug is worth more than one that quietly returns a plausible number**,
and it is worth building them that way deliberately: every one of these probes
prints what it read before it draws a conclusion from it.

---

# I had the second defect backwards, and the corrected version is worse

I reported that SaBRe consumes an **extra** 8-byte draw, and built a hypothesis
about the hash-seed guard at `lib.rs:159`. Both wrong. SaBRe **misses** a draw
that ptrace consumes.

**What settled it: asking Detcore, not the guest.** Running the same guest at
`--log=info` with a retained log shows ptrace serving **two** getrandom calls:

```
getrandom(0x7ffff7fb2200,  8, 1) = Ok(8)    <- glibc's OWN startup draw
getrandom(0x7fffffffe080, 16, 0) = Ok(16)   <- the guest's
```

8 bytes, `GRND_NONBLOCK`, buffer **inside libc** — the stack/pointer guard during
libc init. Then:

```
ptrace guest  12ab376880e4f387 b99fd21da39c4e9f
sabre  guest  63b091c267b3dc67 12ab376880e4f387
```

SaBRe's second half is ptrace's first half. SaBRe's guest read the stream **eight
bytes earlier**, and `63b091c2…` is what ptrace gave the startup draw. That draw
never consumed the stream under SaBRe.

**Why I got it backwards.** I had the two guest outputs and nothing else. A
prefix that appears on one side is equally "an insertion here" or "an absence
there", and those are opposite defects with opposite fixes. Only a view of *what
else Detcore served* separates them, and I hadn't looked. The guest outputs alone
were another datum that decides nothing — the third time this session.

**The corrected finding.** glibc's startup getrandom is determinized under ptrace
and not under SaBRe, so the guest's **stack guard and pointer guard take host
entropy**. That is early-startup interception coverage loss. It affects every
guest, not only getrandom callers, and the stream shift is a side effect of it.
Inference, flagged: the draw lands during libc init before SaBRe finishes
rewriting. It is *not* the documented bootstrap bypass — that needs buflen 32,
this is 8.

**A limit worth knowing before you repeat this.** You cannot see SaBRe's own
DETLOG by setting the env var normally. `detcore-sabre` reads
`DETLOG_FORWARD_ENV` through `sabre::take_private_env` — SaBRe's **private**
environment channel — so `REVERIE_SABRE_HERMIT_FORWARD_DETLOG=1` on the hermit
process does not reach the injected plugin. My SaBRe log stayed at 33 lines with
zero getrandom mentions on both attempts.

**That absence is the log not being forwarded, not an absence of getrandom**, and
I nearly had a fourth wrong conclusion from it. Anyone reading these logs should
treat "no getrandom under SaBRe" as unmeasured.

## Where this leaves the SaBRe task

Three defects now, distinct, in increasing order of reach:

1. **vDSO leak** — glibc ≥ 2.41 routes getrandom through a userspace ChaCha over
   a kernel-populated state page. Measured: rewriting every syscall inside
   `__vdso_getrandom` does not determinize it. A model limit, not a wiring bug.
2. **Early-startup coverage loss** — glibc's init getrandom escapes SaBRe
   entirely; stack and pointer guards take host entropy. Affects every guest.
3. **The inert detour** — `#[detour(lib="libc", func="getrandom")]` never
   installs, defended by a comment asserting the opposite of the truth, with a
   unit test pinning registration rather than installation.

None is fixed. All three are characterised with evidence and named fix sites,
and all three are reported on hermit#3054.

---

# And then the "new" defect turned out to be documented

Before reporting the early-startup gap as a discovery, I checked whether hermit
already knew. It does.

`hermit-cli/src/lib.rs:1771`:

> `preplugin_coverage=absent` is a property of SaBRe's launch order: the guest
> interpreter runs before the Detcore plugin is loaded. It is recorded rather
> than inferred from counters because the ptrace fallback cannot observe that
> interval either.

And https://github.com/rrnewton/hermit/issues/1725, *"Make SaBRe unobserved
dynamic-loading phase loud"*, is **CLOSED** — resolved by making the window
visible, not by closing it. Provenance already recorded: `/bin/true` gave 3
COMMITs under SaBRe against 14 under ptrace, the 11 absent records being the
loader's `libc.so.6` path resolution.

**So the window is known and deliberate. I had presented it as new.** Hermit has
been announcing it on every SaBRe run.

**What is actually new is the category.** #1725 and the comment both characterise
the loss as *missing log records*. The same window also swallows a getrandom
**whose value the guest keeps** — stack and pointer guard seeded from host
entropy, every later determinized value offset. Observability gap in the record;
determinism gap in fact. On glibc ≥ 2.41 specifically, since that is when the
pre-plugin window acquired a getrandom worth keeping.

Filed as `sabre_s_pre_plugin` (P1), wired to block the getrandom row, because it
affects **every** guest and would otherwise be buried.

**One thing I checked and refused to call a defect.**
`preplugin_coverage=absent` is a hardcoded literal, so it can never say anything
else — but that is correct by design and the comment says so. A true statement of
launch order is not a measurement pretending to be one. It would have been easy
and wrong to file it alongside the registration-not-installation test; the
difference is that this one is honest about what it is.

The real gap is narrower and worth carrying: **the mechanism reports that the
window exists and never what fell into it.** Had it named the escaped syscall,
today would have been one run instead of a day of probes. A per-syscall report of
what the pre-plugin window swallowed is the instrument this area is missing, and
it would catch the next one without anybody writing a probe.

## The pattern across today, stated once

Four times I reached a conclusion and four times the check changed it:

| conclusion | what the check did |
|---|---|
| container-vs-host explains DBT | confounded — it was packaged-vs-on-demand client |
| the detour is broken in the container | it is inert **everywhere**; never the mechanism |
| SaBRe consumes an extra draw | backwards — it **misses** one ptrace consumes |
| early-startup loss is a new defect | known, documented, closed as observability |

None of these was carelessness; each was a reasonable reading of real evidence.
What separated the wrong ones from the right ones was always the same: whether
the evidence could have come out differently if the conclusion were false.
**Before believing a result, ask what reading it rules out.** A prefix that
appears on one side, a byte count that drops, a hash that changes — each was
equally consistent with two opposite stories until something else pinned it.

---

# Reading SaBRe's own side, and withdrawing a conclusion because of it

## The unblock, which had defeated two attempts

`detcore-sabre::forward_detlog` writes through `RawStderr`, so the injected
process's DETLOG goes to **stderr**, never to `--log-file`. And hermit arms that
forwarding **itself** at `hermit-cli/src/lib.rs:1617-1619` whenever
`tracing::enabled!(target: "detcore", INFO)`, after `env_remove`-ing any
user-set value.

So the recipe is `--log=info` and **read stderr**. My earlier attempts set
`REVERIE_SABRE_HERMIT_FORWARD_DETLOG=1` by hand — which hermit strips — and read
the log file, which never receives it. Both failures presented as "no getrandom
under SaBRe", which is indistinguishable from the real thing until you know.

## What Detcore actually sees

| backend | inbound syscalls | first syscall | getrandom seen |
|---|---|---|---|
| ptrace | **72** | `brk(NULL)` | 8-byte + 32-byte, both libc buffers |
| sabre | **43** | `fstat(1, …)` | **none** |

ptrace opens on the dynamic loader — `brk`, `mmap`, `access(ld.so.preload)`,
then a long run of `openat`/`newfstatat` through /nix/store. SaBRe's first
syscall is already inside the program's own stdio setup. **29 syscalls of
loading are invisible to SaBRe.** The launch-order claim is now observed rather
than inferred.

## The guest-visible cost, measured directly

| field | ptrace stable | sabre stable | ptrace == sabre |
|---|---|---|---|
| stack-guard | yes | **NO** | no |
| pointer-guard | yes | **NO** | no |
| at-random | yes | yes | **yes** |
| stack-addr | yes | yes | no (fixed 240-byte offset) |
| vdso-addr | yes | yes | yes |

`AT_RANDOM` being identical everywhere is what makes this conclusive: the guards
are not varying because their classical input varied. And the two address
controls being stable rules out "SaBRe is just looser about layout".

## Why I withdrew the "model limit" conclusion

I had written on the sister task that vgetrandom's bytes come from a
kernel-populated state page, that Detcore substitutes syscall *return values*,
and therefore "a syscall-interception determinizer cannot reach this by
intercepting syscalls, however completely". A limit of the model, I said.

The DETLOG refutes it. **The guest's own `getrandom(buf, 16, 0)` produces no
syscall on either backend** — glibc 2.42 serves it from the userspace ChaCha.
And ptrace is *still* deterministic. The only way that holds is that the ChaCha's
**seed** was determinized; ptrace sees a 32-byte draw that SaBRe does not.

So the model is fine. ptrace determinizes the seed and everything downstream
follows. SaBRe never sees it.

**That correction changes what the fix is**, which is why it was worth making
loudly rather than quietly. "Model limit" implies emulating a state page or
keeping the guest off the vDSO. "SaBRe misses the seeding call" implies changing
*when* SaBRe starts intercepting. Different work, different owner, different
size. It also makes `sabre_s_pre_plugin` the plausible **cause** of the getrandom
failure rather than its sibling — rewired accordingly.

I posted the withdrawal on hermit#3054 too, and was explicit about what I was
and was not retracting: the decouple recommendation stands on the measurement
(their head still fails), but the reasoning I gave for it was wrong.

## What would close the pre-plugin task

The stack guard identical across two SaBRe runs **and** equal to ptrace's, in
the pinned container. Five lines of inline asm, retained at
`ignored/assertions-3/evidence/preplugin-guest-state-probe-source.rs`. **Do not
accept a 2.34 host as evidence** — that glibc does not exercise this path the
same way, and a green run there would mean nothing.

## The instrument this area is missing

`preplugin_coverage=absent` is correct and says only **that** the window exists.
Every hour here went on discovering **what** fell into it. A per-syscall report
of what the pre-plugin window swallowed would have made this one run, and would
catch the next instance without anybody writing a probe. If one thing from this
lane gets built, that is the one I would pick.

## Closing the last flagged inference, which was also wrong

I had said the 32-byte draw was `getrandom_vdso` seeding its ChaCha, and flagged
it as unestablished. Disassembling that function's three syscall sites in the
pinned image:

```
0x45bb3   mov eax,0x13e ; syscall    SYS_getrandom — the FALLBACK; buffer and
                                     length come from caller registers
0x45c57   mov eax,0xe   ; syscall    rt_sigprocmask SIG_BLOCK
0x45cbd   mov eax,0xe   ; syscall    rt_sigprocmask SIG_SETMASK
```

One fallback getrandom and a signal-blocking bracket. **No 32-byte draw.** The
guess is refuted; that call's origin is now recorded as unidentified.

**The withdrawal of "model limit" survives it**, because it never rested on
naming the seed — only on the guest's 16-byte call issuing no syscall on either
backend while ptrace is deterministic regardless. Something upstream is
determinized under ptrace; I cannot say which draw and no longer claim to.

**A side fact worth keeping.** `getrandom_vdso` has a real syscall fallback, so
the vDSO path is not unconditional. Whoever tests a launch-order fix gets a
two-sided check from that: a request large enough to take the fallback should
determinize once interception starts early enough, while a small one still would
not. That distinguishes "interception now starts early enough" from "the vDSO
path still bypasses it". It is a diagnostic, **not** a workaround — a determinism
product must not depend on which glibc branch a guest happens to take.

## Where this lane ends

Five self-corrections in one session, each found by a check rather than by a
reviewer:

| I concluded | the check said |
|---|---|
| container-vs-host explains DBT | confounded — packaged vs on-demand client |
| the detour is broken in the container | inert everywhere; never the mechanism |
| SaBRe consumes an extra draw | backwards — it misses one |
| early-startup loss is a new defect | known, documented, closed as observability |
| the 32-byte draw is the ChaCha seed | not from `getrandom_vdso` at all |

The through-line is not carelessness; every one was a fair reading of real
evidence. It is that **a single observation almost never distinguishes two
stories**, and the cheap discipline is to name the other story before believing
the first. When I did that, I was right; when I didn't, I wasn't. Four of the
five were caught by asking what a *control* would look like — a second head, a
second backend, a second configuration — and one by simply reading the source I
was theorising about.

---

# The four-assertions row's premise has expired, and the gate is quiet

## The gate

`ci-hub validate-status` at 14:29Z: last completed run **1838**, finished **7
hours ago**; **0 live in-progress**; prior cadence 1–2 hours across runs
1826–1838; **283 commits since the last full green** (RUN 1777, 2026-09-11).

The ticks **stopped** rather than completed. I did not start one — they are an
operated cadence and why they stopped is an operational question I did not
investigate.

*(I first measured that gap as fourteen hours by comparing an artifact
directory's **local** mtime against a **UTC** clock. It is seven. Same class as
everything else in this handoff, caught before publishing.)*

## The premise

Run 1838 reached real depth (2570 executed, 2390 passed), so its failure list
means something *about its own commit*. Ten failing nodes:

```
check.backend_parity_suites    e2e.manifest_backend_parity_c
check.lint_checks              e2e.manifest_c_programs
check.portability_paths        test.regular_crates
privileged-test.cli_kvm        test.hermit_unit
test.liteinst_strict           test.sabre_examples
```

**Four are this row's backends** — re-measured at `421e08cef`: DBT, KVM,
LiteInst pass; SaBRe fails. **Six were never in its scope.** So "four named
assertions are what keeps main red" was already incomplete when written, and
with three now green it names the *minority* of the remaining red.

**A counting trap in that log.** It also lists `classification.mixed` and
`classification.refused`. Those are **not nodes** — they are fixture lanes run
*inside* `validate_classification`'s own test, deliberately failed and asserted
on. Grep the log for FAIL and you get twelve where the answer is ten. Ironically
they come from the area I spent the morning on.

## One measurement out of three attempts, and the two failures are instructive

| node | result |
|---|---|
| `check.portability_paths` | **PASSES** now, rc 0. Fixed since 1838. Measured. |
| `check.backend_parity_suites` | **Not measured.** My run showed 12 failing assertions — all **my own setup**. The test shells out to `VERIFICATION_REPORT_BIN`, defaulting to `target/debug/verification-report`, a cargo debug binary my ad-hoc run never built. It returned `None` for every record and the test faithfully reported twelve content failures. I nearly filed it as a current red. |
| `check.lint_checks` | **Not measured.** First attempt refused correctly (`rc 75`, uninitialised submodule) — the node fail-closing on my setup, not a lint result. After initialising `third-party/rr` the container died mid-script with no diagnostic, twice. |

I reported the ratio rather than the one green, because "portability_paths is
green" on its own reads as progress on a front I mostly did not measure.

**The lesson for anyone spot-checking DAG nodes by hand:** two of my three
attempts failed on environment, not product. The DAG owns dependencies and
resource bounds for a reason, and a hand-run node that fails is far more likely
to be telling you about your worktree than about main. Both of mine announced it
— one with `rc 75` and an explicit "this is a SETUP condition", one with a
`None` that only looked like data. The first is what good failure reporting looks
like; the second is what this whole session has been about.

## What the row needs, and it is not more spot checks

A fresh full validate at current main. Until then "what keeps main red" is
unanswerable: the newest evidence predates three of the four fixes, and per-node
probing cannot substitute for the reason above.

---

# I measured my own change's population, and it is zero

3066's reviewer measured the ledger before extending my gate_kind fix. I owed
this change the same and had not done it.

**Canonical ledger at `origin/main`: 2,127 rows, 124,738 gate and attempt
records.** Never-written refusals: **146**. Malformed: 0. Cannot-read: 0.
**Every one of the 146 carries `exit_code: 2`. None carries 0.**

hermit#3066 requires `returncode == Some(0)`, so it **reclassifies nothing**
historically. Safe — and **it would not have changed run 1819**, whose 17 nodes
carry exit 2 and stay `ProductFailure`. My commit message claims otherwise;
corrected on the PR before approval.

## The split is the interesting part

- **78 new-contract** — `test_results_error` holds the refusal, `reason`
  preserved as `"exit 2"`. The command's own status survives; `product_failure`
  is defensible.
- **68 old-contract** — pre-AU2781 overwrote `reason` with the refusal text. The
  real cause is **destroyed**; `exit 2` is all that remains and cannot say
  whether it came from the harness refusing or a genuine failure. Calling those
  `product_failure` asserts what the record cannot support — which is exactly
  this task's original argument.

So the task's motivating population is real, and 3066 is aimed *next to* it
rather than at it.

## Two things I refused to do

**Widen 3066 to catch exit 2.** That would overturn the reasoning in its own
commit message on one day's data without a principle to replace it — the
goalpost move I guarded against when writing it. And the discriminator is absent
from the new-contract records, where `"exit 2"` genuinely is the command's
status.

**Classify on nextest's exit convention.** nextest exits 100 for test failures,
so exit 2 on a nextest node is probably the harness refusing. That is one
runner's convention, inferred rather than contracted, and building a
classification on it would be the same fragile reading this area exists to
remove. It was tempting precisely because it would have "worked".

## What it needs

An owner ruling on one sentence: **when a node's nonzero exit IS the harness's
refusal to write results, is that a product failure?** If no, the repair is
narrower than 3066 and belongs at the producer — a distinguishable status for
"refused" rather than reusing the command's exit, which AU2781 already half-did
by preserving `reason`.

3066 should still land: safe, measured, and it makes the exit-0 shape read
honestly. But it is **future-proofing**, and calling it a fix for the run-1819
population would be wrong.

---

# The defect's last live instance: all 507 tests passed, node recorded as a product failure

## The trajectory

Never-written refusals per run, by affected node, from the canonical ledger:

```
2026-09-16T13:44  d0b1468b   16 nodes
2026-09-16T15:53  161261b1    3 nodes
2026-09-16T19:45  3850e783    1 node   (test.regular_crates)
2026-09-17T07:30  158a89f6    1 node   (test.regular_crates)   <- newest run
```

16 → 3 → 1 → 1 as #3026 landed. Real nodes; I checked they are not the
`classification.*` fixture lanes.

## The survivor, and its cause is not the filed one

`test.regular_crates` in run 1838:

```
Summary [10.087s] 507 tests run: 507 passed, 0 skipped
expected 505 tests to execute, saw 507; refusing because the selected set changed
run-nextest-counted: cannot derive typed test results
```

**Every one of 507 tests passed and the node reads as a product failure.** The
cause is **not** stale prepared executables — that was #3026. It is a **stale
expected test count**: two tests were added and `NEXTEST_EXPECTED_EXECUTED` still
said 505.

The guard is *right* to refuse; the population genuinely changed and silently
accepting it is what the ratchet exists to prevent. The defect is in the
**consequence** of refusing.

## Repaired — measured, not assumed

Main now carries `NEXTEST_EXPECTED_EXECUTED = 508`. Whether that matched the tree
was unknowable from the ledger, and the validate that would answer it is blocked.
So I counted directly, in the pinned image, with the node's own selection:

```
cargo nextest list --locked --workspace --exclude hermit-detcore \
  --exclude hermit --exclude hermetic_infra_hermit_flaky-tests
  → 508 non-ignored tests.  DAG expects 508.  MATCH.
```

No live instance today.

## What generalises

**It recurs every time a test is added without updating the expected count.**
Structural, not a one-off; 1838 caught it mid-drift. And **3066 cannot reach it**
— the refusal arrives with a nonzero exit and 3066 requires exit 0. So the change
named after this defect neither fixes nor could have fixed its only live
instance. That is worth saying plainly before it lands.

The ruling to get, now with a real trigger rather than a hypothetical:

> When a node refuses to write typed results because its declared test population
> changed, and every test it ran passed, is that a product failure?

If no, the repair is narrow and belongs elsewhere: a count-mismatch refusal
should classify `no_result`. I did not make that change — it turns on the same
policy question, and my general argument (a nonzero exit is a real failed
condition) still holds. What is different in *this* shape is that the node's own
summary says 507 of 507 passed, so the evidence to overrule the exit code is
right there in the record.

---

# The cause dies at a boundary I had not located — found by checking my own published claim

I wrote in hermit#3073's body that the enriched refusal message reaches the
ledger via `test_results_error`. I had inferred that from reading the producer.
Then I read a record.

Canonical ledger entry for `test.regular_crates` in run 1838 — the node where
507 of 507 passed:

```
exit_code           2
reason              "exit 2"
failure_detail      "exit 2"
test_results_error  "required structured test results were not written to …"
```

Probing every field for the producer's text: `'expected 505'`, `'saw 507'`,
`'of which'`, `'selected set changed'`, `'cannot derive'` — **all absent**.

So 3073 improves the **node log**, not the ledger. Corrected on the PR before
review. It should still land on that narrower basis — the log is the first place
anyone debugging opens, and today it says only "expected 505, saw 507" with no
outcome — but a consumer reading the typed record gains nothing from it.

## What it locates

`test_results_error` carries **dagrun's** message, not the producer's. Dagrun
observes only that the declared file is absent and cannot know why. Nothing
carries the node's own account across that boundary, so every distinct cause
collapses to the same two fields:

| cause at the producer | what the ledger shows |
|---|---|
| count drift, every test passed | `"not written"` + `exit 2` |
| stale prepared executables | `"not written"` + `exit 2` |
| a malformed report | `"not written"` + `exit 2` |
| an unsupported event in the stream | `"not written"` + `exit 2` |

**That is this row's title condition with an exact location: the node-command →
dagrun boundary.** Not the classifier, which #3066 addresses. Not the producer's
diagnostics, which #3073 improves. Both were real; neither was this.

Closing it means carrying something typed across that boundary — a bounded tail
of the node's stderr, or a refusal cause publishable without publishing an
unsanctioned report. Both are **dagrun** changes, so agent-utils rather than
hermit, with their own owners. I am not designing it from here. The requirement
is stateable though: the record must distinguish *no report because the
population drifted and everything passed* from *no report because the harness
broke*.

## The method note, sixth time today

I published the claim, then checked it, and it was wrong. The check was one query
against data already on disk. The pattern is not that my inferences are unusually
bad — most were fair readings of real code. It is that **an inference about what
a system records is cheap to verify, and I kept not verifying before speaking.**
The remedy is not more caution in the reasoning. It is reading one record before
making a claim about what records contain.

---

# The boundary fix, fully specified and filed rather than guessed at

`dagrun_s_missing_report` (P2), wired to block the structured-results row.

I had written "someone should carry something typed across that boundary", which
is the kind of handoff that reads as helpful and costs the next person a day.
Every precondition is now verified.

**Site:** agent-utils `rs/dagrun/src/scheduler.rs`, `resolved_test_counts`, the
`TestResultsMode::Structured` arm (~2650-2662).

**Everything the repair needs is already in scope, which is the whole point:**

1. The function's third parameter **is** the node's output — `captured: &[u8]`.
   The `Structured` arm never reads it; the `LegacyStdout` arm does.
2. It is **both streams.** The caller (~3668) says so in its own comment: *"The
   step's captured output — both pipes, in arrival order — for the summary +
   failure detail. ONE tail, because there is one ring."* **This had to be
   checked, not assumed:** the same file documents per-stream rings elsewhere,
   and if that were what arrived the whole proposal would fail, because the
   message I care about goes to stderr.
3. A summariser already exists — `last_line()` at ~2442.
4. The caller **already calls it**, one line above the refusal.

So the refusal is constructed with the node's complete output in hand, beside a
helper that reads it, and mentions only the path that is missing. Same shape as
the hermit-side discard in #3073 — the evidence is right there.

## The trap, measured on the real case

**`last_line` alone does not help.** `test.regular_crates`'s final captured line
was the generic wrapper:

```
run-nextest-counted: cannot derive typed test results from /tmp/tmp.GnIJuuSDYR
```

The informative one is *second to last*. A one-line summary reproduces the
uselessness it is meant to cure. The repair needs a **bounded tail of several
lines** — bounded because the capture is ring-limited for a reason recorded in
that file: a runaway step once OOM-killed the runner.

Anyone implementing this from the existing helper would get it subtly wrong and
it would look right. That is why the trap is in the task description rather than
only here.

## Why I filed it instead of landing it

Scope, not difficulty. agent-utils is **main-only** so the push is the whole
review; its main needs a serialized writer and I cannot establish single-writer
from a worker lane; hermit consumes it by **gitlink**, so the effect is deferred
behind a separate reviewed pin bump regardless; and **every** dagrun consumer
inherits the change, not just hermit. A diagnostic improvement does not justify
taking that on unilaterally.

## Where the three pieces sit

| change | reaches | status |
|---|---|---|
| #3066 classifier — never-written + exit 0 → `no_result` | the verdict | open, population measured at zero, cannot reach the exit-2 shape |
| #3073 producer diagnostic — "of which N passed" | the **node log** only | open; I corrected an overstatement that it reached the ledger |
| `dagrun_s_missing_report` — carry a bounded tail | the **typed record** | filed, specified, not started |

None subsumes another, and only the third closes the row's title condition.
