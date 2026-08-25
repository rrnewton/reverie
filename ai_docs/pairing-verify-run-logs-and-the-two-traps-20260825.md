# Pairing `--verify` run logs, and the two traps that corrupt cell-level analysis

Written 2026-08-25 by `agent(hermit-006)`. Tool: `scripts/pair_verify_runs.py`,
tests `scripts/test_pair_verify_runs.py`. Landed on dev-hermit main `55bd3697`.

This exists because three agents are doing cell-level divergence analysis at once and
both traps below produce **plausible numbers rather than errors**. Neither is caught by
running the analysis and reading the output. I hit both, published wrong figures from
each, and retracted them.

## Why pairing is the whole problem

A `hermit run --verify` invocation writes two logs. Every question of the form "do these
cells share a mechanism", "is this failure intermittent", "did boxing change anything"
is a comparison **between the two logs of one invocation**. Get the pairing wrong and
every downstream number is a statement about whichever logs happened to line up.

## Trap 1 — pairing by modification time

Measured over 886 run-log pairs: pairing by mtime **skipped 364 of them (41%)** and
**cross-paired different guests** — it matched a `bash` log to a `/bin/true` log. It
fails silently and the surviving subset still yields a tidy-looking histogram.

The key that works:

| component | rule |
| --- | --- |
| cell identity | the guest's first `execve` line — program and argv exactly as logged |
| invocation | nearest mtime **within that cell group only** |
| quality gate | the pair must share *N* identical leading `DETLOG` lines (default 50) |

Pairs failing the gate are reported **UNPAIRED with a reason**, never dropped silently,
so a caller can see what fraction of the population a result actually covers.

> **Report the unpaired count alongside any pattern.** If a large fraction failed the
> gate, the pattern is over whatever survived, and that belongs in the same sentence as
> the claim rather than in a footnote.

## Trap 2 — the worktree root shatters per-cell tallies

⚠️ **This one specifically breaks per-cell analysis, which is what several agents are
doing right now.**

The `execve` argv embeds the worktree root, so the *same logical cell* run from
`worktrees/003` and from `worktrees/010` produces **two different cell keys**. A census
of one corpus found the same scripts under at least nine distinct roots — `depth2163`,
`cloudlint`, `003`, `010`, `011`, `cand-rebase`, `measure`, and several
`validate-fresh-*`.

This is harmless for *pairing*, because both runs of one invocation share a root. It is
fatal for *aggregation*: one cell failing nine times looks like nine cells failing once,
which is precisely the shape of "35 rare failures versus one mechanism".

Use `--strip-prefix` when aggregating by cell. Leave it off when pairing.

## The three divergence classes — counts alone misclassify

| class | definition |
| --- | --- |
| `controlflow` | record counts differ, **or** the syscall name or return value differs |
| `clock` | every difference is confined to a `tv_sec` / `tv_nsec` / `tv_usec` payload |
| `observation` | same syscall, same return, same buffer length — **differing bytes** |

The `observation` class was established by `agent(hermit-003)` on netlink cells whose
differing bytes were interface counters: twelve of twelve 64-bit fields increased, zero
decreased. **Identical `DETLOG` counts does not mean pure-clock** — that was the framing
error which this taxonomy replaced.

### Trap 2b — the `[iobuf]` record shape

Detcore emits buffer *content hashes* as

```
[iobuf][dtid N] <op> <dir> 0xADDR+LEN-><sha256>
```

A matcher that only handles `finish syscall #N: name(...) = ret` lets these fall through,
`shape()` returns `(None, None, None)`, and **every observation divergence is filed as
control-flow**. Fixing this moved **128 of 711 pairs** and changed a figure I had already
published from 290/324 to **418/201**.

Shape-match `[iobuf]` on `(dtid, op, dir, addr, len)` and compare the hash separately.

## Two worked examples of `observation`, with sources traced

Both were resolved by walking *back* from the divergent record to the syscall that
opened the fd — the same move each time, and worth copying:

1. **AF_NETLINK.** `socket(16, 524291, 0) = Ok(11)` upstream of a `recvmsg(11) = Ok(1468)`
   whose `+1468` buffer hash differed. Host-live interface data.
2. **A live host file.** `openat(-100, "/var/facebook/configerator-client-system-configs/
   falcon/core_entities/0/…") = Ok(35)` then `read(35, …) = Ok(246)` with a differing
   `+246` hash. The file is rewritten every two minutes by the `falcon` daemon — measured,
   two hashes 120 s apart. Per hermit's `AGENTS.md`, "Hermit does not make a changing
   filesystem or external network deterministic", so that one is a container-isolation
   defect, **not** a detcore defect.

The lesson generalises: an `observation` divergence is usually explained by finding what
the fd is, not by studying the bytes.

## Running it

```
python3 scripts/pair_verify_runs.py --dir <log-dir> \
    [--run1-glob run1_log_*] [--run2-glob run2_log_*] \
    [--min-prefix 50] [--strip-prefix]
```

Prints the paired/unpaired counts, unpaired reasons, the four-way classification, and a
per-cell breakdown.

## ⚠️ Provenance of the numbers quoted above

Every figure here was measured on a corpus of 886 run-log pairs in `/tmp` on devbig014.
**That corpus was deleted before this was written** — it went from 886 pairs to one, with
1.4 TB free, so it was cleaned rather than evicted. The numbers are therefore **honest but
no longer reproducible from that data**. Treat them as prior observations that motivate
the tool, not as a baseline to diff against. The tool and its tests are durable; regenerate
your own population.
