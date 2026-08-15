# Results table — cells this change enables

**Derived, not transcribed.** Produced by running the author's own
`extract-cells.py` against three refs and diffing the enabled-cell sets. Anyone
can reproduce it:

    python3 extract-cells.py <ref>     # -> JSON of "test|mode|backend" -> {ci, assert}

| ref | enabled cells |
|---|---|
| `61b9ba7aa` base (hermit main) | 627 |
| `f40babbbb` reviewed tip | 638 |
| `b8bfa040d` current tip | 644 |

## The 11 cells the review covers — base → reviewed tip

    backend-parity-c/aio-refusal|verify|liteinst
    backend-parity-c/event-delivery-ordering|verify|liteinst
    backend-parity-c/eventfd-semantics|verify|liteinst
    backend-parity-c/fcntl-owner|verify|liteinst
    backend-parity-c/file-io-roundtrip|verify|liteinst
    backend-parity-c/pipe-capacity|verify|liteinst
    backend-parity-c/sendfile-copy|verify|liteinst
    backend-parity-c/signal-delivery-sequence|verify|liteinst
    backend-parity-c/vectored-io|verify|liteinst
    c-programs/ioctl-fioclex|verify|liteinst
    c-programs/pause-alarm-interrupt|verify|liteinst

## The 6 cells added AFTER the reviewed tip — NOT covered

    backend-parity-c/mkdir-rmdir|verify|liteinst
    backend-parity-c/o-tmpfile-anon|verify|liteinst
    backend-parity-c/record-lock|verify|liteinst
    backend-parity-c/set-tid-address|verify|liteinst
    backend-parity-c/symlink-ops|verify|liteinst
    backend-parity-c/umask-mode|verify|liteinst

## A UNIT NOTE, RAISED NOT RESOLVED

The review is described as **"31/31 verified under both manifests"**. This table
finds **11 cells** at the reviewed tip. Those are different quantities and should
not be read as the same one.

**31 is exactly the entry count of each checksum manifest** — `CHECKSUMS.relative.sha256`
and `CHECKSUMS.absolute.sha256` carry 31 lines each, and "under both manifests"
matches that reading precisely. On that reading 31/31 is a **file-integrity**
result, not 31 mutation brackets.

I have **not** resolved which the author meant and am not asserting it. The
integrity claim is independently confirmed here (31/31 OK at source, at the
tracked path, and from `git archive origin/main`). **The mutation verdicts
themselves are not present in this directory** — no results, verdict, or pass/fail
data file exists in it — so this table cannot corroborate them and does not try to.
