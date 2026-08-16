# Provenance — liteinst+verify rows 1-20 qualification evidence

**Produced by `hermit-kvm2`.** Promoted by `hermit-coord`; not authored, not re-scored. Source
`ignored/kvm2-q0120/` — **gitignored, so it survived context loss and not host loss, and devbig014
could not read it.**

## Scoping — and this slice carries a stronger caveat than the others

> **Qualification is EXIT-STATUS REFUSABILITY UNDER THE STRIPPED COMPARATOR — `parity=false` on
> every accept arm — NOT PARITY AND NOT L2.**

Invocation: `hermit run --strict --verify`, **ptrace backend**, portable lane. Not a receipt.

**EVERY VERDICT HERE IS ON THE PTRACE REFERENCE ARM. THE LITEINST ARM WAS MEASURED 0 OF 20** — the
LiteInst preload runtime is unavailable on that agent's binary and refuses at launch. **These are
liteinst rows, so none of these verdicts is a qualified liteinst cell**, and the producing agent said
so rather than writing a bare `QUALIFIED`.

**Of its 14 ptrace-qualified rows, 2 are structurally barred for liteinst by `-524`**
(`fork-exec-pipeline`, `short-io-split-identity`), leaving **12 eligible — and eligible is not
qualified.** That was a falling count and the honest direction.

## IS THE SOURCE STILL BEING WRITTEN?

**This directory: stable** across a 40-second interval at promotion time. **The producing agent:
still working** — `hermit-kvm2` has moved on to kvm+verify 71. The directory holds completed
liteinst 1-20 work.

| | |
|---|---|
| content digest at promotion | `a4bb1bc5f53c007baf65b800086f783b1ac1d42dcde01c175640b9f7fd27014c` |
| text files promoted | 12 |
| bytes | 37,226 |

Recompute over the source with the same exclusion; a mismatch means **re-promote wholesale**.

## What was excluded

- **`__pycache__/qual.cpython-312.pyc`** — build output, regenerated from `qual.py` which is here.

## What is here

`qual.py` and `recheck.py` (the operators), `slice.json` (the population), `out.json`,
`recheck.json`, `recheck2.json` (results), `table.txt` (the per-cell table), `run.log`, `run2.log`,
`recheck.log`, `recheck2.log`, and `set_tid_address_main.c` — **the origin/main copy of the fixture
whose staleness flipped row 18 from NOT-EXIT-GATED to QUALIFIED.**
