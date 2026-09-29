# Invalid gate attempt — do not use as evidence

The arc-f, arc-lint, tfa_phase2_test and containment_test logs and .rc files in
this directory came from a single multi-command shell invocation. The owner
flagged it as malformed in the process table: one line, with no `&&` after `cd`
and no separators between commands. None of those results are gate evidence.
They are kept here unmodified.

The pyre-pre-fix.* and pyre.* files came from separate invocations before this
attempt; they are superseded by the re-run under ../relocation-20260925-r2/.

The gates were re-run one command per invocation under ../relocation-20260925-r2/.
