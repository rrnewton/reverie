# Unretirable refusal census, 2026-08-27

This directory preserves the evidence for TaskGraph task
`three-ways-a-refusal-becomes-unretirable-do-they-share-a-root`.

## Current count

`current-census.json` was refreshed at `2026-08-27T12:13:20Z` from all 48 open
pull requests in the five project repositories:

- `rrnewton/hermit`: 30
- `rrnewton/reverie`: 10
- `rrnewton/dev-hermit`: 5
- `rrnewton/agent-utils`: 2
- `rrnewton/liteinst2`: 1

The query read issue comments, reviews, review threads, and review comments. No
connection was truncated, every refusal record had a numeric GitHub comment id,
and no review or review-comment container carried a marker. The current liveness
input came from the repository's independent `/proc` reader: full host PID
namespace, 746 own-uid processes, zero unreadable agent-capable processes, and
30 distinct live agent names.

The readers find 23 unanswered refusal records across 11 pull requests:

- 13 records across 6 pull requests name a live issuer and remain retirable by
  that issuer.
- 4 records across 4 pull requests name no issuer. All four retain their GitHub
  comment id and remain retirable through exact `RETIRES #<comment-id>`.
- 6 records across 5 pull requests name no live issuer and cannot be retired by
  any currently running agent.
- 0 records lack a GitHub comment id. Had any done so, the collector would have
  reported `INDETERMINATE_MISSING_COMMENT_ID` and the overall census would have
  been `INDETERMINATE`, rather than assigning a position or guessing.

The six records with no live issuer are:

- https://github.com/rrnewton/hermit/pull/2176#issuecomment-5274807361 —
  `hermit-903`.
- https://github.com/rrnewton/hermit/pull/2176#issuecomment-5274823796 —
  `hermit-903`; this is the machine-readable correction of the same review, so
  the two records are one underlying objection. `approval_binding` retains both
  records; `changes-requested-gate` suppresses the later identical verdict as a
  citation.
- https://github.com/rrnewton/hermit/pull/2223#issuecomment-5276923975 —
  `hermit-908`.
- https://github.com/rrnewton/hermit/pull/2304#issuecomment-5419595129 —
  `hermit-015`.
- https://github.com/rrnewton/hermit/pull/2680#issuecomment-5420278252 —
  `hermit-016`.
- https://github.com/rrnewton/hermit/pull/2699#issuecomment-5430409607 — the
  parser reads `devbig014`, a host name, as the issuer. This is mechanically the
  same result — no live agent can match it — but it is not a departed-agent
  record.

Thus the current count is six machine records across five pull requests, or
five underlying objections. Restricting the count to actual departed-agent
issuers gives five machine records across four pull requests, or four underlying
objections. That is 4 of 48 open pull requests, 8.3 percent. Including the
host-valued issuer gives 5 of 48, 10.4 percent.

## The three reported causes

They do not have one implementation cause.

1. A departed named issuer is an authority problem after the identity was read
   correctly. `retirement_allowed` accepts a retirement of a named refusal only
   from that same issuer. Preserving the comment id does not change that, which
   is why https://github.com/rrnewton/hermit/pull/2680 remained blocked after
   the CLI fix.

   The production `approval_binding.py --repo rrnewton/hermit --pr 2680 --json`
   output can appear to contradict this: its `REFUSED_STALE` row carries
   `refusal_issuers=[]`. That field includes only unanswered refusals whose
   refused SHA equals the current head. PR2680's refusal names `642a2990502a`
   while the current head is `020b8d754369`, so the empty list says nothing
   about the stale refusal's issuer. The live landing gate and both underlying
   parsers read the real comment as `hermit-016`.

2. The CLI defect was loss of the GitHub comment id between the API response and
   `evaluate`. The evaluator and its policy were already correct. Commit
   `a82eba216c4b232fd71f7c0f9f83926aac7c3af7` preserves the id and refuses an
   object record whose id is absent.

3. The old Codex refusal on
   https://github.com/rrnewton/reverie/pull/503#issuecomment-5437464601 has no
   disclosure prefix. That makes its issuer unreadable, but current policy
   deliberately gives an unsigned refusal the exact-comment-id route. It is not
   unretirable while its id is present. The earlier probe tried an ordinary
   withdrawal and an approval, neither of which is allowed to claim an unsigned
   refusal, but did not try `RETIRES #5437464601`.

`reproduce_pr503.py` demonstrates the distinction against both readers. The
ordinary signed withdrawal leaves the real unsigned refusal outstanding. Adding
the exact `RETIRES #5437464601` clears it in `approval_binding`; the landing gate
returns 0 when ids are supplied and returns 1 with the original OUTRUN message
when they are deliberately omitted.

The second and third cases therefore interact: an unsigned refusal relies on
the comment id, so a reader that discards the id removes its only retirement
route. They still originate at different points — one discarded an existing id,
the other comment never recorded an issuer. The first case is independent of
both: it has both identities and lacks a live authorized issuer.

## Reproduction

Run the local two-direction case without GitHub:

```text
python3 ai_docs/evidence/unretirable-refusals-20260827/reproduce_pr503.py
```

Settle the PR2680 current-head filtering question and exercise both permitted
and refused exact retirements:

```text
python3 ai_docs/evidence/unretirable-refusals-20260827/reproduce_pr2680.py
```

Verify that a missing comment id produces an indeterminate result rather than
being assigned a retirement route:

```text
python3 ai_docs/evidence/unretirable-refusals-20260827/collect.py --self-test
```

Run a fresh five-repository census when GitHub API capacity is available:

```text
python3 ai_docs/evidence/unretirable-refusals-20260827/collect.py \
  --output ai_docs/evidence/unretirable-refusals-20260827/current-census.json
```

`hermit-census-cached-input-20260827T0240.json` is the earlier complete
23-open-pull-request Hermit API response. Replay it without GitHub using:

```text
python3 ai_docs/evidence/unretirable-refusals-20260827/collect.py \
  --cached-hermit \
  ai_docs/evidence/unretirable-refusals-20260827/hermit-census-cached-input-20260827T0240.json \
  --live-agents-from \
  ai_docs/evidence/unretirable-refusals-20260827/hermit-census-cached-result-20260827T0250.json
```

`hermit-census-cached-result-20260827T0250.json` preserves the result of that
replay. It exactly reproduces the prior 21-row split: 11 no-live issuers, 3
exact-comment-id exits, and 7 live issuers. The later merge of
https://github.com/rrnewton/hermit/pull/2674 removed five of those mechanically
unretirable records from the open population.
