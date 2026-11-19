# reverie-signal

A pure, backend-neutral model of Linux signal semantics for Reverie tools
and backends: pending queues with siginfo, blocked masks, dispositions,
alternate stacks, Linux's receiver-selection rule, syscall-restart mapping
and handler frame-entry policy.

The crate decides *what* Linux does; callers decide *when*. It performs no
I/O, names no backend and no consumer, and its state is a plain value that
callers may clone, snapshot or serialize. Delivery mechanics (kernel
delivery, frame bytes) stay with backends.

## Build-system note (internal builds)

The frame tests compile two small C ABI probes at test time
(`tests/fixtures/signal_abi_*.c`, needs `cc`) and the neutrality test
walks the workspace manifests. Internal (Buck) targets for this crate
must declare those fixtures as resources and keep the test wired into
the layering tree; the Cargo DAG already selects this package in its
core test node.
