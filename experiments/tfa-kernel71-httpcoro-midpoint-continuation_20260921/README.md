# Kernel 7.1 TFA HTTPCoro midpoint continuation

This prospective schema-v2 continuation contains only
`fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests`, exact case
`HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2`.
Prior roots remain immutable, no prior evidence is imported, and this same
canonical target counts once across all roots.

The prior default-clock repeatable strict line-1685 result and the 0.125-clock
all-PASS cell are planning context only. This new cell prospectively tests the
fixed dyadic midpoint multiplier 0.5; seed-derived slowdown factors span
effective progression around that midpoint. It sweeps only scheduler seeds 0 through 15, fixes
seed/RNG/fuzz roots at zero, enables deterministic per-thread slowdown, leaves
target-timeslice intentionally unset because the selected phase has one guest
thread, retains the 200,000,000-ns maximum timeslice, and does not enable
`--chaos-target-races`.

Validate with:

```sh
python3 experiments/tfa-flaky-tests_20260917/tools/validate.py \
  --root experiments/tfa-kernel71-httpcoro-midpoint-continuation_20260921
```

Material execution requires this committed PLANNED record, authenticated
helpers, `bin/safehermit`, and the unchanged
`httpcoro-reset-rate-limit-exact-v2` oracle. Plan validation runs no Hermit.
