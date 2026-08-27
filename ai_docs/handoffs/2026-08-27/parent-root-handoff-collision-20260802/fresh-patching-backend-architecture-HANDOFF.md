# Coordinator restart handoff

- Current task: `fresh_patching_backend_architecture`
- Branch: `research/fresh-patching-backend-architecture`
- Remote branch: https://github.com/rrnewton/dev-hermit/tree/research/fresh-patching-backend-architecture
- PR: N/A (research-only parent ai_doc; no product PR applies)
- Exact SHA: `81a923a26f03503ef18e96c3d0c37e501c14f403`

## Done

- Fresh source trace of SaBRe, LiteInst, and e9patch at Hermit
  `c7531a837a65d388707e3b14642a6ba36b660267`, Hermit's Reverie pin
  `b9a7fa777e902ef937766e2db7c1a333b1e15e31`, and Reverie main
  `ef5ffebccb7e4bfd0e82a2bc7bc703c2ed6212a4`.
- Confirmed production placement: SaBRe runs Detcore in the guest; Hermit
  LiteInst and e9patch run the sole Detcore in the ptrace host.
- Corrected the e9patch detail: Hermit's internal path applies e9tool
  `before empty` and then ordinary ptrace; Reverie's separate generic e9patch
  backend uses a validated injected SIGTRAP but still dispatches Tool in ptrace.
- Mapped current sharing and backend-private code.
- Coordinated with `audit_cross_backend_detlog` and reviewed open Hermit PR
  #1448's SaBRe DETLOG sink evidence.
- Delivered and pushed
  `ai_docs/fresh-patching-backend-architecture_20260802.md`.

## Next

- After the tg sync restart, attach the durable artifact path and exact SHA to
  the task note.
- Preserve existing task tags and add `implemented`; leave status
  `in_progress` for coordinator closure under the research-task exception.
- Coordinator can integrate commit
  `81a923a26f03503ef18e96c3d0c37e501c14f403` into parent main.

## Blocker

None. Wind-down was requested before the task-note/tag closeout.

## Validation

- Markdown reference definitions: no duplicates, undefined references, or
  unused definitions.
- ASCII-only artifact.
- `git diff --check`: clean.
- Exact-SHA runtime smoke limitations are documented in the ai_doc: SaBRe was
  not built into that binary, LiteInst failed activation in this environment,
  and `/bin/true` had zero e9patch candidate sites. Architecture conclusions
  come from literal production dispatch paths, not that smoke run.
- Reverie LiteInst manifest/lock drift is documented; it does not alter the
  placement conclusion.
