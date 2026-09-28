# Standalone classification authority refusal

**The retained refusal is explained by the invocation context, before the new preservation predicate runs.** The standalone AU worktree has no Git superproject. The existing nested d750 checkout has both superprojects, but its enclosing Hermit commit still pins AU2781, so changing only the entrypoint would encounter the next authentication refusal. No currently coherent candidate invocation was established, and none was executed here.

Source: Agent Utils `d750224cbf10c5a3b35eec40fa4d57d0e98493cb`, tree `8a41f1130199bc2dde1f60e6d053f1817375c94a`; `py/wrkslots/cli.py` SHA256 `384963f661eb758e9cb9bd167ada6f2b393aca6f2d49467566c1b0003f88dcd6`. Both actual standalone and nested `__main__.py`/`cli.py` bytes match that commit. The four authority functions below are byte-identical at AU2781, base872 and d750; this is not newly introduced authority behavior.

## Exact failing path

The retained attempt called `/tmp/kvm-frozen-cleanup-au-landing-20260917-d750224/py/wrkslots/__main__.py` with canonical `--project-root /home/newton/work/dev-hermit`. The entrypoint inserts its own package parent before importing `wrkslots.cli`. Authority discovery uses the resolved loaded `cli.__file__`, not the caller's cwd or `--project-root` as a substitute parser authority.

At d750 `cli.py:16748`, `_frozen_validation_authority_chain` resolves the loaded AU checkout and calls `_trusted_git_superproject(agent_checkout, label="agent-utils")`. Lines16706–16718 ask Git for `rev-parse --path-format=absolute --show-superproject-working-tree`. `_trusted_git_text` at16665–16675 strips ASCII output and refuses an empty value with the exact retained message.

A bounded read-only Git query against the actual standalone worktree returned exit0, **empty stdout**, and empty stderr for precisely that superproject operation. Its HEAD is d750. Its Git common directory is shared with the nested AU repository, but that does not make this separate worktree a checked-out submodule. Thus the observed message has a concrete source and filesystem explanation; no need to infer a preservation-algorithm failure from it.

The classification caller first obtains its initial process census (`cli.py:24398–24428`), then enters `_frozen_no_proof_disposition` at24430. The current-schema branch calls `_terminal_validation_record` at17815 before `_current_incomplete_frozen_result` at17822 or `_current_incomplete_frozen_retention` at17823. The terminal reader calls the canonical parser at17169; parser authentication reaches the failing chain above. The exception becomes `could-not-classify`/`blocks_entry: true` at24483–24490. CLI exit0 reports completed JSON classification, not permission to enter, an eligibility pass, or a cleanup result.

## The existing nested checkout is not ready for a retry

Read-only Git observations during this diagnosis:

| Actual checkout | Actual HEAD | Actual committed child pin |
| --- | --- | --- |
| cleanup slot parent | `fba5d50c96756889bc5c2d3e6caffb9adc8bcb7a` | Hermit `98d58b9bd6ea722c6d7087d45d5fd81792abfa16` |
| cleanup slot `hermit` | `98d58b9bd6ea722c6d7087d45d5fd81792abfa16` | AU `2781b1054efc3a9c561dbed35584a6fca1ed8676` |
| cleanup slot `hermit/agent-utils` | `d750224cbf10c5a3b35eec40fa4d57d0e98493cb` | — |

The nested AU superproject query correctly names the nested Hermit checkout; Hermit's query names the outer cleanup slot. However, `_frozen_validation_authority_commit` at16788–16803 compares the actual AU HEAD with the committed Hermit Gitlink. They differ, so the next source-derived refusal would be `loaded agent-utils HEAD differs from the enclosing checkout's pinned Gitlink`. This is a prediction from exact current facts and code, **not an executed second classification**. The outer parent→Hermit edge currently agrees. The authority functions then also require the actual outer pin and common Git repository, or the existing sealed separate-clone authority.

The appropriate next route is normal reviewed pin integration establishing both committed links, followed by fresh source/controller binding. After those prerequisites actually hold, the same bounded component command may use the nested `hermit/agent-utils/py/wrkslots/__main__.py` with the original state-root, checkout, record and repository arguments. [PROPOSED-NORMAL-INVOCATION.json](PROPOSED-NORMAL-INVOCATION.json) retains that concrete argv and its explicit **not executable now** status. No pin change, synthetic authority, override or retry is proposed as a shortcut.

The maintained parent `ci-hub/bin/wrkslots` at fba5 selects that nested package and preserves the shared project root; it does not bypass the two committed Gitlink checks. The existing real-Git control `test_frozen_parser_authority_accepts_real_nested_linked_worktree` at `test_lifecycle.py:4159` constructs and commits both nested pins before invoking the authority helper. Linked outer worktrees are supported; a standalone AU worktree is a different context. This control was read, not run.

## Preserved evidence and limits

Original attempt: actual exit0,25.550259351730347 wall seconds,25.393031 CPU seconds, one process census; JSON schema1/requested1/`blocks_entry:true`/`could-not-classify`. Original stdout356 bytes SHA256 `68b5c76949f7f3b2870f86502fcacfaf1b47c0e899d4003688cf2ad2d028f5b9`; receipt SHA256 `426bdc7f0b1aae3def0c7c052f0b7a36c7c3f5185e83bf80569d5b4aad0a08dd`. BEFORE and AFTER records are byte-identical SHA256 `f6ff9bd817cde9d4f323d3ac2f3519e689fcad2748303eaaf53e76a140d0f361`. Original bytes are copied under `attempt-1/` without modification.

[READBACK.json](READBACK.json), SHA256 `d53e93414edafd3c6793629d39024d20176fd9bc6e9c81d10f38f25497f8f048`, records complete Git query argv/results, source hashes and equal authority-function hashes across2781/872/d750. This diagnosis performed only artifact/source reads and bounded read-only Git metadata queries. It did not run classification, a new process census, recovery, cleanup, admission, tests or builds, and changed no product source, index, refs, registry or existing evidence.

This explains the specific authority refusal. It does not establish that the real1839 clone will satisfy later parser, shape, liveness, pristine-tree or repeated-binding checks after coherent integration. The measured failure remains a refusal. No assertion, timeout, authority comparison, unknown-live preservation rule or deletion-proof requirement was weakened or relabelled.
