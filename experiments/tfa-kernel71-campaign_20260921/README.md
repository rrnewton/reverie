# Kernel 7.1 TFA campaign

This is a separate schema-v2 instantiation of the fixed TFA cohort and
acceptance contract. It exists so kernel-7.1 cells can be planned without
altering or replacing the earlier kernel-6.13 experiment records in
`../tfa-flaky-tests_20260917`.

The completed campaign result and its evidence audit are recorded in
[`RESULTS.md`](RESULTS.md).

The acceptance authority remains dev-hermit commit
`9d7f603c43618715b9245c74acb3e164c17d2ae8`, path
`ai_docs/tfa-acceptance-bracket-2026-09-17.md`, blob
`4a53f0c25623ad6371ee86b4556adada1218745d`. The 30-target registry remains
the byte-identical `cohort.json` published by commit
`ee6562142e3796bb56a1dd0e28b77c1c16a78cc2`.

Validate this campaign from the repository root with:

```sh
python3 experiments/tfa-flaky-tests_20260917/tools/validate.py \
  --root experiments/tfa-kernel71-campaign_20260921
```

No result from the ignored fixed-binary diagnostic sweep is copied into this
campaign. Each material record must point to its own strict-ancestor PLANNED
record and to evidence committed under this campaign root.
