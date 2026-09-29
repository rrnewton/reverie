# Gap found at 3eb9d118 (kept as evidence, not relabelled)

`negctl-real/swap-phases` fed the real thrift ledger with the unfixed and
patched phases swapped. The renderer accepted it (rc 0) and rendered
`NOT SUPPORTED` instead of refusing it, because nothing bound the unfixed phase
to the manifest's `artifact_sha256`. Fixed by c1f8817a ("Bind the follow-up
unfixed phase to the manifest test ELF"); see
`../followup-20260925-r7-c1f8817a5ebb/negctl-real/`.
