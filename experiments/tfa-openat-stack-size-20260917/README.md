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

**No build command survives anywhere** — not in the reports, and not in any Claude
transcript on this host (hermit-906 ran under a different harness). The two sources
plus the compiler version above are what a rebuild has to work from.

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
