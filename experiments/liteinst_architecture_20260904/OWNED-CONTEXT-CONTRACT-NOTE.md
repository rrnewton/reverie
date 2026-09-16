OWNED-CONTEXT-REUSE-CONTRACT: bounded read-only checkpoint; no product changes, execution, network, delegation or activation.

Design: /home/newton/work/dev-hermit/experiments/liteinst_architecture_20260904/OWNED-CONTEXT-REUSE-CONTRACT.md
SHA256 d0b92fee048b0a75a1552b20aae28abc80f2c59adb2236c70c052aa0bbed18d8
Pinned runtime/parser/clock source8bb9f79350955d791f6f08f315ad0e30bb74afc3, inspected through git show, plus exact compiled-kernel return/error report and its independent review.

Conclusion: complete stable acceptance conditions can support a conditional faithful-return proof, but current runtime does not enforce them. Pending SIGSEGV/SI_KERNEL at the intended CPUID is ambiguous with partial failed rt_sigreturn; opcode, target PC/SP, zero AX, hashes or equal RCB cannot authorize Tool dispatch or reuse. The report enumerates acceptance conditions, enforcement sites versus unsafe caller assumptions, terminal outcomes, generation ownership and exact proposed write set through existing Tool/ThreadState/RPC. Current syscall pending-clear-before-return must not be copied as an owned-context consumption protocol.

Concrete optional execution witness analyzed: fixed one-thread store then indirect jump without scratch GPR/stack/vector use or guest-text changes, bound to the exclusive active generation. NOT qualified: RF/TF/debug semantics, general DSO TLS addressing and exact clock/signal windows remain blockers; no clear-flags workaround or clock calibration. It cannot substitute for faithful FP/altstack acceptance. Real consecutive CPUID events, reusable generations, failure containment, nesting and clock tests remain required, not waived. No SUD/Detcore/backend qualification.

POSITIVE-REPORT-CORRECTION, derivative only:
/home/newton/work/dev-hermit/experiments/liteinst_architecture_20260904/OWNED-CONTEXT-POSITIVE-CLARIFICATION.md
SHA256 27e31101a8756de601de448b901bb3788d64a817b0bfdba6213d4a8acaaf71c7
Machine-readable clarification: sibling OWNED-CONTEXT-POSITIVE-CLARIFICATION.json, SHA2566771b99be61e4f175f8342c278b76a8081ebf40b5ea78ce2fea37280a061377d.
Correct count:2228 selected FP-image bytes compared, including reserved bytes; PKRU contributes4 padding bytes. Preserve raw historical fp_semantic_bytes label and ALL assertions. Independent reviewer approved normal one-shot result and replayed the same binary exit0; author did not rerun it.

Rechecked original approved report/archive/result/source/parser/gate/binary hashes: all unchanged. Original archive43f991cb3f78345f85d2a4d5960a1bd15f81e0ffb82cddc3f3ff90a2c8673c75; binary15875caa8ccf711d9910eb704ee819be72a9f58698f3e6e25882928c75564256. Full document/review identities: experiments/liteinst_architecture_20260904/OWNED-CONTEXT-CONTRACT-AND-CLARIFICATION.sha256. No task closure or new completion claim.
