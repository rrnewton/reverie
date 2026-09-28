[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=impl]

Addressed the exact-head review finding from `5d65b9f50f124f615c5f0c862af7b94919037c3f` without merging or broadening the slice.

New exact head: `79f139bd7e1b8b789e1b770597c03b9f0bbd6050`

- full base-to-head diff SHA-256: `af4e2e7f20c4262b3e0cc4eca4ada9382c55a2660da555bcca985564ca538d9c`
- incremental correction SHA-256: `30777bf3ee93ac725b48e10f55e68f0110713e5ace560feb6efa45beca5a4b41`

The decoded output is now checked for modeled existence and writability (or the existing open-standard contract) before any input/output `ENOSYS` route. The new unit matrix covers 30 positive-alias rows including procfs; real KVM covers 24 rows for native once, direct twice, and Tool twice. Pipe/socket inputs carry a per-row byte that must remain unconsumed.

Exact-head gates are green: fmt, all-target clippy with `-D warnings`, three focused units, required real KVM, full library parallel 819/819, and full library serial 819/819. Late-validation and existence-only mutants are killed by both unit and KVM cells (KVM exits 46 and 58). Three content-identical earlier parallel flakes remain disclosed in the PR body; none was filtered or relabelled.

Preserved limits are explicit: non-null-offset precedence still does not match native `ESPIPE`; private synthetic fdinfo can preempt `sendfile`; valid writable pipe output retains `ENOSYS`; existing `lseek` limitations are unchanged.

Three independent Codex artifact reviews found no correctness defect or goalpost moving. This head remains frozen and must receive a fresh independent Claude-family exact-head review before merge.
