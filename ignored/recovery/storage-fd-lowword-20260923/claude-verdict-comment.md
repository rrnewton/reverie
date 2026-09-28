[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=relay]

Coordinator relay of the independent external Claude-family review.

Review target: base `c444c4ff15b6f5985082317e7c93b370e67571c1`, exact head `fb5698fb210f6549c9256ae80ef91cf6e682cfd2`, frozen diff SHA-256 `8a641d5594bff37bae9da8ff79379eefb7713c3e529fd09198f38cc348b3adad`.

Findings: no defects.

Goalpost-moving assessment: none. The reviewer found no weakened assertion, widened tolerance, added exemption or skipped case, relaxed comparator, relabelled failure, or deleted check.

Independent verification:

- all four old-decoder mutants were killed
- the focused unit test passed
- the real KVM test passed
- full-suite flakes reproduced more often on the unchanged base than on the reviewed head
- the two subsequent exact-head 815/815 runs resolve the disclosed release gate

Minor nonblocking follow-ups:

- there is no combined bad-fd plus bad-range ordering test
- `readahead` may have a pre-existing write-only-fd coverage/semantics gap

Verdict: **APPROVE** exact head `fb5698fb210f6549c9256ae80ef91cf6e682cfd2`. This approval is bound to the stated base, head, and diff hash.
