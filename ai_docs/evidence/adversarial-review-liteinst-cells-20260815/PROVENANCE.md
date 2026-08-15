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

**THE REMOTE BRANCH TIP IS NOT f40babbbb.** Measured here after
`git fetch origin --prune` at promotion time:
`origin/feature/liteinst-enable-two-verified-cells` is **`b8bfa040d`**, and
`f40babbbb` is its ancestor. The local branch of the same name is also
`b8bfa040d`, and no ref anywhere resolves to `f40babbbb`.

**A CONFLICT IS RECORDED HERE RATHER THAN ADJUDICATED.** The author reported
re-establishing the tip before editing and finding it *still* `f40babbbb`. That is
not reproducible by the method used here — a pruned fetch, then
`git branch -r --contains`, then `rev-parse` on both the remote and local refs.
Either the author read a different ref, or the tip moved between its check and
mine. **I am not asserting which.** What is established is that at promotion time
the tip is `b8bfa040d` and one commit sits above the reviewed point.

That commit — *"Enable LiteInst verify on six exit-gated backend-parity cells"* —
adds six cells the review did not examine. They are named in `RESULTS.md`.

Quote this evidence as **"reviewed at `f40babbbb`"**, never as "the branch is
reviewed": a verification binds to a head exactly as an approval does.

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

## WHY THE 27 `.c` FILES ARE **NOT** HERE

They were promoted in the first pass and have since been removed at the author's
direction: the tracked form is the two diffs, the commit list, the extraction
script and a results table — **the evidence and the means to reproduce it, not the
bulk.**

**A tension worth recording rather than smoothing over.** I had kept them on the
grounds that they are the reviewer's constructed mutants, are not reconstructible
from `fixture-diffs.patch` (which is the pull request's own diff, not the
mutations), and are therefore the only proof of the refuse arm. The author's
judgement about its own method governs, and it is the author's evidence. **The
consequence is explicit: with the mutants gone, the 31/31 claim cannot be
re-verified from this directory** — their exact bytes remain pinned by the
checksum manifests, which is provenance without the content.

`commits.txt` records the eleven commits in scope. `extract-cells.py` is the
15-line helper that derives the enabled-cell set from any ref, and `RESULTS.md` is
its output. `manifest-diffs.patch` and `fixture-diffs.patch` are the reviewed diffs.
