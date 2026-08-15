# Provenance — adversarial review of LiteInst cell enablement

Promoted 2026-08-15 by hermit-coord (devbig030) out of `ignored/`, which is
gitignored: the evidence survived agent context loss but not host loss, and
nothing in the system would ever have promoted it.

**Author: hermit-teardown.** It produced this review and explicitly asked whether
the evidence should be promoted, correctly declining to do so itself mid-fix.
Promotion is on that request; the content is its work, unmodified.

## WHAT THIS EVIDENCE COVERS — AND WHAT IT NO LONGER COVERS

Reviewed at a pinned pair, recorded by the author:

    hermit main   61b9ba7aa   (still origin/main at promotion time)
    branch tip    f40babbbb   pinned at the author's 12:38 PDT fetch

**THE BRANCH TIP HAS SINCE MOVED AND THE AUTHOR DID NOT RE-FETCH.**
`origin/feature/liteinst-enable-two-verified-cells` is now **`b8bfa040d`**, one
commit ahead: *"Enable LiteInst verify on six exit-gated backend-parity cells"*.

**That commit is NOT covered by this review.** The 31/31 result is true of
`f40babbbb` and says nothing about the six cells added after it. Quote this
evidence as **"31/31 at `f40babbbb`"**, never as "the branch is reviewed" — a
verification is bound to a head exactly as an approval is, and the bare phrase is
the one people trust instead of checking.

## INTEGRITY

The author shipped its own `CHECKSUMS.relative.sha256` and
`CHECKSUMS.absolute.sha256`, 31 entries each. **Verified 31/31 OK at the source
before copying, and 31/31 OK again at this tracked path after copying.** The two
uncovered files are the checksum manifests themselves, which cannot list their own
hashes.

That manifest is the author's own finalization step, which is why this was treated
as a declared stopping point rather than a guess from file mtimes — the directory
had been quiet only 8 minutes and **hermit-teardown was alive (rc 1, 5 pids) at
promotion time**. A live author plus a short quiet window would normally refuse
promotion; a complete, self-verifying checksum manifest is the author's own signal
that the capture is closed.

## WHY THE 27 `.c` FILES ARE HERE

They are the baseline/mutant pairs — the reviewer's constructed mutants and their
unmutated counterparts. They are **not reconstructible** from `fixture-diffs.patch`,
which is the pull request's own diff rather than the mutations. They are the proof
of the refuse arm: without them the 31/31 claim cannot be re-verified. 3318 lines
of C, 130 KB total, all text, no binaries and no build output.

`commits.txt` records the eleven commits in scope. `extract-cells.py` is the
15-line helper that derived the cell list. `manifest-diffs.patch` and
`fixture-diffs.patch` are the reviewed diffs.
