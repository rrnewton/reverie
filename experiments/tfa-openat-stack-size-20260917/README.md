# openat stack-size reproducer — promoted out of /tmp

Preservation only. Created by hermit-906 on 2026-09-17; promoted here by hermit-908
on 2026-09-17 because the only copies were host-local files under `/tmp`, which ages
out. The experiment was **not** re-run, the diagnosis was **not** re-derived, and the
probe was **not** edited. Everything here is byte-identical to the `/tmp` original.

## What this reproduces

A two-directional reproducer for a Hermit defect. A synthetic guest issues one raw
`openat` with a caller-supplied stack size:

| writable bytes below the stack pointer | strict Hermit | native |
|---|---|---|
| 192 | `errno 14` (EFAULT), exit 1 | exit 0 |
| 512 | exit 0 | exit 0 |

The observed stdout, from the files in `evidence/`:

- `hermit-906-open-stack-512.stdout` — `openat raw_rc=3 errno=0`
- `hermit-906-open-stack-192.stdout` — `openat raw_rc=-14 errno=14`
- `hermit-906-open-stack-192-info.stdout` — `openat raw_rc=-14 errno=14`

`tfa_open_stack_probe.c` is the item that matters most: it is the only thing that can
rebuild that bracket.

## How the probe works

`tfa_open_stack_probe.c` maps two pages `PROT_NONE`, `mprotect`s the second page
`PROT_READ|PROT_WRITE`, and points the new stack at `region + page_size + offset`, so
`argv[1]` is exactly the number of writable bytes below the stack pointer.
`tfa_open_stack_probe.S` provides `raw_openat_on_stack`, which swaps `%rsp` to that
address and issues syscall 257 (`openat`) with `AT_FDCWD`, flags 0, mode 0, then
restores `%rsp`. Usage is `tfa_open_stack_probe <offset> <path>`.

## Files

Sources (the rebuildable core, and the whole of what is preserved as code):

- `tfa_open_stack_probe.c` — 1,036 B
- `tfa_open_stack_probe.S` — 403 B

`evidence/` — the report / stdout / stderr triples, all `safehermit` run records:

- `hermit-906-open-stack-512.{report,stdout,stderr}`
- `hermit-906-open-stack-192.{report,stdout,stderr}`
- `hermit-906-open-stack-192-info.{report,stdout,stderr}` — the `.stderr` here is the
  28,813-byte one, the only large log in the set
- `hermit-906-open-loop.{stdout,stderr}` plus `hermit-906-open-loop-safehermit.report`

Three `.stderr` files are zero bytes (512, 192, open-loop). That is their original state, not a copy failure.

## Two deviations from the file list I was given, both deliberate

**1. `/tmp/hermit-906-open-loop.report` does not exist.** It was named in the task and
is genuinely absent from `/tmp`; it was not deleted here. What does exist is
`/tmp/hermit-906-open-loop-safehermit.report` (1,561 B, same 21:59 timestamp as
`hermit-906-open-loop.stdout`), which appears to be the same record under a different
name. It is preserved in `evidence/` under its real name. The named path is recorded
as missing rather than quietly satisfied by the near-miss.

**2. The compiled probe binary is NOT preserved here, and will be lost.**
`/tmp/tfa_open_stack_probe` (25,432 B) is the exact binary that produced the bracket
above. I copied it in, then backed it out, for two reasons: it was not on the list I
was given, and this repository has an explicit rule against binaries in this very
directory tree — `.gitignore:59`, `/experiments/**/*.bin`. Forcing it past a
deliberate rule to preserve an unrequested file is the owner's call, not mine.

It still sits at `/tmp/tfa_open_stack_probe` and **`/tmp` ages out**, so if it is
wanted it has to be rescued deliberately. What is recorded instead, so it stays
identifiable and rebuildable:

- sha256 `7ceb7db65afca8a8ab9d0e3c3c201d81b41080672d98d7a2f3df36f007ecfb4a`
- `BuildID[sha1]=6b5e6435f9f910ae38a3bfb09a912edb0b844dd4`
- GCC 11.5.0 20240719 (Red Hat 11.5.0-14/-15), dynamically linked, built with
  debug_info, not stripped

**No build command survived** — not in the reports, and not in any Claude transcript
on this host (hermit-906 ran under a different harness). One has since been re-derived
and verified; see "Rebuilding" below. That recipe is what replaces the binary.

Note for anyone promoting artifacts into `experiments/` in future: that `*.bin` rule
skips files during `git add` while every per-file existence check still passes. The
copied-count against tracked-count comparison is what catches it.

## The Hermit binary under test

From the `safehermit` reports, every run used the same binary:

```
binary=.../ignored/tfa-hermit-binary-readiness/79da523137dbac400faca6130593361b58a01468/hermit
binary_sha256=000eeb5e59345b6c9c14bf194d6d5b392294a52d25d40209e553376f3a84764a
```

That path is under `ignored/` and is not preserved by this commit. The sha256 is
recorded here so the binary can be identified if it is found elsewhere.

## Rebuilding

hermit-906 left no build command. This one was re-derived on 2026-09-17 from the two
preserved sources and then checked against the preserved evidence, because a recipe
that only reproduces the failing arm is indistinguishable from a probe broken for an
unrelated reason.

```bash
gcc -O2 -g -o tfa_open_stack_probe tfa_open_stack_probe.c tfa_open_stack_probe.S
```

That is the whole of it. The `.S` is assembled by the same driver invocation. **The
preserved sources are sufficient** for both probes — no missing header, no further
source file, no extra link dependency. Both builds are clean with no warnings.

### The open-loop control arm

`tfa_open_loop_probe.c` is the second probe in this experiment and it is **not a side
experiment**. It opens and closes one file 5,000 times and prints `5000 opens
completed`. It is what excluded path, symlink and open-count as causes before the
stack hypothesis was formed, and it is therefore what makes the EFAULT attributable to
stack space rather than to the open pattern. If this arm ever refuses, the 192/512
bracket means much less than it appears to.

Its source was still only in `/tmp` after the first promotion — the evidence was
tracked while the thing that regenerates it was not — and it is promoted here for that
reason. Single source, no `.S`:

```bash
gcc -O2 -g -o tfa_open_loop_probe tfa_open_loop_probe.c
```

Run it as `tfa_open_loop_probe <path>`, with any existing openable file. Under the
frozen hermit binary it is one `bin/safehermit` invocation:

```bash
bin/safehermit "$HERMIT_BIN" run -- ./tfa_open_loop_probe <path>
```

Expected: exit 0 and `5000 opens completed`, in about a second. Re-measured 2026-09-17:
native and hermit both produce exactly that, matching the preserved
`hermit-906-open-loop.stdout` and its report's `exit_code=0` and `elapsed_secs=1`.

### Verifying everything at once

`build.sh` builds both probes, and `./build.sh --verify` also runs all six arms and
checks them:

| arm | expected |
|---|---|
| native 192 | exit 0, `openat raw_rc=3 errno=0` |
| native 512 | exit 0, `openat raw_rc=3 errno=0` |
| native open-loop | exit 0, `5000 opens completed` |
| hermit 192 | exit 1, `openat raw_rc=-14 errno=14` |
| hermit 512 | exit 0, `openat raw_rc=3 errno=0` |
| hermit open-loop | exit 0, `5000 opens completed` |

Measured 2026-09-17 on devbig030: all six reproduce, and the three hermit values match
the preserved `hermit-906-open-stack-192.stdout`, `-512.stdout` and
`hermit-906-open-loop.stdout` byte for byte.

Exit codes: `0` all four reproduced, `1` an arm disagreed, `2` the hermit arms could not
run so the verification is **incomplete** — deliberately not the same as a pass. The
script refuses to run the hermit arms against anything whose sha256 is not the frozen
`000eeb5e59345b6c9c14bf194d6d5b392294a52d25d40209e553376f3a84764a`, since a different
hermit build is a different experiment. All three outcomes were mutation-tested rather
than assumed.

Hermit is invoked through `bin/safehermit`, as the parent `AGENTS.md` requires.

### The second argument

The probe takes `<writable-bytes-below-the-new-stack-pointer> <path>`. The path only
needs to be an existing, openable file; `build.sh` creates its own so the check is
self-contained.

**Path length is not what sets the bracket**, which is worth stating because it is a
natural guess and it is wrong. The original run used a 220-character path, long enough
that one might assume the 221 bytes with its NUL is what overflows 192. Re-tested with
a 67-character path: the 192 arm still returns EFAULT and the 512 arm still succeeds.
Both path lengths reproduce both arms.

For provenance, the original second argument, recovered from
`hermit-906-open-stack-192-info.stderr`, was:

```
/home/newton/fbsource/buck-out/v2/art/fbcode/bfa56033afdaaeb5/logdevice/logservice/server/log_store/test/__log_store_test__/__log_store_test__shared_libs_symlink_tree/libwarm_storage_common_client_prod_exception_utils.so
```

It is a `buck-out` path and will not survive a clean, which is why `build.sh` does not
depend on it.

The open-loop arm's original path argument is **not recoverable**: that run's `.stderr`
is empty and its report records no arguments. Since the probe only needs an existing
openable file, and the arm reproduces with both the long path above and a short local
one, nothing appears to turn on it — but it is unknown rather than known.

### Rebuilt binary is not byte-identical to hermit-906's

The rebuild is 26,424 bytes against the original's 25,432, so `-O2 -g` is not
necessarily the exact original flag set. The criterion here is behavioural: the rebuilt
probe reproduces all four arms with the recorded values. Anyone needing the original
bytes should note they are gone — see above.
