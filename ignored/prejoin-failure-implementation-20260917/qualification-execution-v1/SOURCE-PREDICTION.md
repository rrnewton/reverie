# Source observation before actual qualification

The original panic integration control asserts that the final error contains `thread 3: guest thread panicked during teardown` at reverie-kvm/tests/support/leader_exit.rs:832. The current caught-panic producer retains an UnexpectedVcpuExit cause; its Display adds `unexpected vCPU exit: ` inside the WorkerFailure diagnostic. Source inspection therefore predicts that the existing assertion will reject `thread 3: unexpected vCPU exit: guest thread panicked during teardown`.

This is a predicted diagnostic compatibility failure, not a measured test result. Root directs the first unchanged 4 + 22 cohort to proceed once the caller is released, retaining this finding and all original assertions. Source v14 remains frozen. The separate live-sibling status 0 contract must also be measured without replacing it with 255. A first failure stops dependent stages; unexecuted cohort members remain unmeasured.
