# Changes-requested gate false-clear audit, 2026-08-27

These five files preserve the evidence used for TaskGraph task
`how-many-other-pull-requests-did-the-broken-gate-falsely-clear`.

The replay covered the interval from dev-hermit commit
`35505ca11a7ba387588cf9f37d9cbe8a4532f2ea` at
`2026-08-25T19:26:57Z` through the corrected production default in
`75dc69b67590d59acf870543fa3344ac22a7c3a8` at
`2026-08-27T10:28:52Z`. Merged pull-request records were cut off at their merge
time; open records were cut off at the latter timestamp.

The measured result was 33 pull requests changing from CLEAR to BLOCKED: 30
merged and 3 open. The content review found 26 merged pull requests whose
objections had been addressed and 4 with substantive findings still visible on
the then-current remote main.

Files:

- `hermit-121-false-clear-results.json`: replay results.
- `hermit-121-false-clear-detailed.json`: replay results with parsed refusal
  records and comment bodies.
- `hermit-121-false-clear-summary.txt`: compact per-pull-request summary.
- `hermit-121-objection-excerpts.txt`: extracted objection text used for the
  content review.
- `hermit-121-merged-content-classification.tsv`: the final merged-pull-request
  classification.

The original files were copied byte-for-byte from `/tmp` and verified by the
checksums in `CHECKSUMS.sha256` before the temporary copies were relied on again.

