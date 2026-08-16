---
name: good-hermit-binary-for-tests
description: "Prevent stale-binary test attribution. Use when a Hermit workload unexpectedly hangs or regresses: rebuild in the assigned slot at the exact reported SHA before diagnosing product behavior."
---

# Bind tests to the source under review

Never select a Hermit binary by timestamp or by a remembered path in a primary
checkout. Build in the assigned worktree, record `git rev-parse HEAD`, and run
the test with that worktree's binary. If behavior is surprising, repeat the
focused command against a clean current-main control before attributing it to
the change.

Report the source SHA, build command, binary path, test command, and outcome.
An old July 2026 observation about one debug binary and `make` is historical
incident context, not a durable binary-selection rule.
