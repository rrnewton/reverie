# Kernel 7.1 TFA AsyncSSL factor-2 scheduler continuation

This prospective schema-v2 continuation contains only
`fbcode//folly/io/async/test:async_ssl_socket_test`. Prior kernel-7.1 roots and
their terminal records remain immutable; no evidence is imported. The same
canonical target is counted once across every root, so this continuation cannot
increase the distinct-target count.

The materially separate cell sweeps only `--sched-seed` values 0 through 15.
Seed, RNG seed, and fuzz seed remain zero. It removes
`--chaos-target-races`, enables deterministic per-thread slowdown with maximum
factor 2.0 and zero-length epochs, leaves `--target-timeslice` absent, retains
the 200,000,000-ns maximum timeslice, and records normal continuous clock
progression with multiplier 1.0.

Validate with:

```sh
python3 experiments/tfa-flaky-tests_20260917/tools/validate.py \
  --root experiments/tfa-kernel71-asyncssl-factor2-continuation_20260921
```

Material execution requires this prospectively committed PLANNED record, this
root's authenticated helpers, `bin/safehermit`, and the unchanged
`asyncssl-selected-gtest-v2` oracle. Plan creation and validation run no Hermit.
