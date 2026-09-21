# HTTPCoro ResetRateLimit midpoint plan

The exact selected case is frozen to strict ptrace chaos with explicit epoch
`2026-09-21T21:01:52Z`, clock multiplier 0.5, deterministic per-thread
slowdown (`10.0`, epoch length zero), random scheduling, and scheduler seeds 0
through 15 in ascending order. Seed, RNG seed, and fuzz seed are zero. The
fixed dyadic 0.5 midpoint is prospectively distinct, while seed-derived
slowdown factors span effective progression around it. Target-timeslice is
intentionally unset because the selected phase has one guest thread; maximum
timeslice is 200,000,000 ns, and `--chaos-target-races` is absent.

PASS and the sole line-1685/SIGABRT target-owned FAIL class are byte-for-byte
unchanged from `httpcoro-reset-rate-limit-exact-v2`. The live launcher retains
stdout through a constant-memory reader with a lethal 67,108,864-byte cap and
authenticates its create/run/admit helper blobs, cgroup bounds, safehermit
report, and both qgroups. No older run is imported.
