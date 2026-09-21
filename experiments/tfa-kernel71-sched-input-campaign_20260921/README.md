# Kernel 7.1 TFA scheduler-input continuation

This independent schema-v2 continuation prospectively tests scheduler seeds
for the Thrift Instrumentation target after the separate target-races cell was
exhausted. The canonical target remains the counting key; no prior evidence is
imported or double-counted.

Validate from the repository root with:

```sh
python3 experiments/tfa-flaky-tests_20260917/tools/validate.py \
  --root experiments/tfa-kernel71-sched-input-campaign_20260921
```

Every material run must use this root's committed PLANNED record, helper blobs,
`bin/safehermit`, and exact predeclared oracle.
