# Rejected wrkslots ownerless nested-submodule recovery

Status: **MUST NOT LAND**. TaskGraph review blockers rejected this uncommitted change.

- Source worktree: `/home/newton/work/dev-hermit/worktrees/slots/wrkslots-ownerless-nested-submodule-recovery`
- Exact base: `bf09769daafcfecf83f80dc2cb762099c0be532e`
- Preserved native patch: `/home/newton/work/dev-hermit/ai_docs/2026-09-01-rejected-wrkslots-ownerless-nested-submodule-recovery.patch`
- Patch SHA-256: `2aca94b9786711ad38dd7922c18d0cab5c5ad796204145de158a542db26ae2bb`
- Preserved path: `py/wrkslots/cli.py`

The artifact matched the live `git diff --binary --no-ext-diff --no-textconv` byte-for-byte. `git apply --check` passed in the clean source worktree at the exact base above. Only `py/wrkslots/cli.py` was restored from `HEAD`; afterward the worktree was clean at that base.

No registry operation, commit, push, or directory deletion was performed.
