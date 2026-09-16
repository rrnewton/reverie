# Derivative clarification: selected FP-image bytes, not only CPU-state bytes

This supplements, without overwriting, OWNED-CONTEXT-POSITIVE-RESULT.md and its
frozen source/binary/logs/archive. No assertion, source, binary or run changed.

Correct wording: **2,228 selected FP-image bytes compared, including reserved
bytes.** Defined fields covered include x87 values/control/status, MXCSR, vector
and opmask payloads, and PKRU. The comparison includes the eight-byte PKRU
component, comprising a32-bit register and four padding bytes; therefore the
old “defined FP state”/“semantic bytes” wording is not exact. Do not infer that
2,224 is an exhaustive defined-state byte count: other selected fields may also
include reserved representation bytes. Preserve the stronger byte comparisons.
The raw output key `fp_semantic_bytes` and original result.json key remain
historical labels, interpreted by this clarification rather than rewritten.

Independent review:
`/home/newton/work/dev-hermit/scratch/owned-positive-review-KbscQJ/INDEPENDENT_REVIEW.md`.
The reviewer approved the normal one-shot positive result with this nonblocking
wording correction and independently replayed the same binary with exit0. That
reviewer execution is distinct from the author's run02; this clarification did
not rerun the fixture or add it to an author-run success count.

Original report SHA256:
`12fc6a7eb3efb79231b792754af9f0aa032b95a52ba2eade6c76ba488377641d`.
Original frozen archive SHA256:
`43f991cb3f78345f85d2a4d5960a1bd15f81e0ffb82cddc3f3ff90a2c8673c75`.
Executed binary SHA256:
`15875caa8ccf711d9910eb704ee819be72a9f58698f3e6e25882928c75564256`.
Original result.json SHA256:
`c3e3107babedd0d943c60637a54b3492ded55100a44c680854b6b62d9e819711`.
Shared parser/gate dependency remains
`8bb9f79350955d791f6f08f315ad0e30bb74afc3`.

Scope unchanged: no SUD/Tool/RPC/Detcore/clock/reuse/general-return approval.
Prior preflight refusal and all unimplemented full-design requirements remain.
Sibling `OWNED-CONTEXT-POSITIVE-CLARIFICATION.json` supplies the corrected
machine-readable interpretation without altering original inputs.
