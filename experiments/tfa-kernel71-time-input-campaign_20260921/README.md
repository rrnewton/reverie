# Kernel 7.1 TFA time-input continuation

This is a prospective schema-v2 continuation for targets whose original
kernel-7.1 plan cannot be extended without invalidating its strict-ancestor
receipt.  The canonical target remains the counting key; the older campaign is
retained as history and must not be double-counted.

The acceptance authority remains dev-hermit commit
`9d7f603c43618715b9245c74acb3e164c17d2ae8`, path
`ai_docs/tfa-acceptance-bracket-2026-09-17.md`, blob
`4a53f0c25623ad6371ee86b4556adada1218745d`.  `cohort.json` and
`experiment.schema.json` are byte-identical to the kernel-7.1 campaign copies.

Validate from the repository root with:

```sh
python3 experiments/tfa-flaky-tests_20260917/tools/validate.py \
  --root experiments/tfa-kernel71-time-input-campaign_20260921
```

Every material run must use this root's prospectively committed plan, the
parent repository's `bin/safehermit`, and the exact target oracle declared in
the record.  No evidence from the older fuzz-seed cell is imported.
