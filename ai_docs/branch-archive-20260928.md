# Note on ai_docs/branch-archive-20260928.tsv

The 150 heads listed in `branch-archive-20260928.tsv` were copied to
`refs/heads/archive/2026-09-28/<name>` by coord2 (devbig030). At the same time,
a concurrent cleanup by a devbig014 agent (`ai_docs/evidence/branch-cleanup-20260928/`)
preserved the same tips under the non-branch refs `refs/archive/branches/<name>`
and deleted the originals with expected-SHA leases.

All 150 `archive/2026-09-28/*` heads were verified to be byte-identical (same SHA)
to their `refs/archive/branches/*` copies and were then deleted with
expected-SHA leases on 2026-09-28, so that they do not add branch clutter. The
authoritative preserved copies are `refs/archive/branches/<original-name>`.
