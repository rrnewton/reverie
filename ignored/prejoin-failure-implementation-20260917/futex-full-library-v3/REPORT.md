The full serial library passed all 454 methods on the first execution of committed 12d4ce8c: 454 passed, 0 failed, 0 ignored, 0 filtered. All prior 453 names and original 427 are present; the only added method is the eight-case cleanup control. The three methods that failed in the prior 450/3 run now pass with their direct error and cleanup assertions intact.

Actual API 12 admission and REVERIE_REQUIRE_KVM=1 were required. The service used 9.178808 CPU seconds and 19.010510442 observed wall seconds, below the unchanged 30 CPU / 60 wall / 16 GiB / zero swap / 1 MiB limits. Payload and admission exit 0, complete accounting, and two fresh inactive/empty service readbacks are retained. Raw output is bounded and untruncated.

The full run includes all 44 previously selected native methods and all four earlier VM methods, so they are not repeated separately. The original 22 static integration methods remain next. Full source, all 199 inputs, actual ELF and admission checks pass after execution.

The original 426/1 futex enrollment timing failure remains unexplained, and the prior 450/3 result remains retained. No unchanged-run retry was used here. This is Reverie component evidence, not a Hermit strict INFO, repeat-determinism or canonical parity result.
