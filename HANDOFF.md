Owned task: vision-ci-signal-is-trustworthy-end-to-end, coordinator dev-hermit.

Reverie slot/branch: dev-hermit-sabre-random-bootstrap-20260917 /
dev-hermit/sabre-random-bootstrap-20260917. Base24cd5bb518b027eddb62a226805210d74d31c3d8.

Implementing the root-authorized optional loader bootstrap boundary, real early
getrandom ingress, vDSO syscall coverage, and adapter initialization seam.
No Hermit Cargo pin may reference this branch: upstream must land first.
No Hermit guest run, publication, or merge is released at this checkpoint.

Original diagnostic040cb is frozen elsewhere: five tests passed, one failed.
Evidence: parent ignored/ci-hub/run1828-fix-forward-20260916/sabre-random-bootstrap-1
and this slot's ignored/sabre-random-bootstrap-20260917.

Do not move/reclaim this slot. Ordinary thread clocks, chaos RNG, and metadata
are not bootstrap payload. The eventual consumer transfers random/auxv state
only, using shared Detcore semantics and one-time image-bound ownership.
