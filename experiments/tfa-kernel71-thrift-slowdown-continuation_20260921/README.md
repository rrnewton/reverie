# Kernel 7.1 TFA Thrift slowdown/timeslice continuation

This independent schema-v2 continuation prospectively tests scheduler seeds
for the Thrift Instrumentation target under a frozen per-thread slowdown and
1,000,000 ns target-timeslice cell. The canonical target remains the counting
key; no prior evidence is imported or double-counted.

Validate from the repository root with:

```sh
python3 experiments/tfa-flaky-tests_20260917/tools/validate.py \
  --root experiments/tfa-kernel71-thrift-slowdown-continuation_20260921
```

Every material run must use this root's committed PLANNED record, helper blobs,
`bin/safehermit`, and exact predeclared oracle.
