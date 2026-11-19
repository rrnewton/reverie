# reverie-signal

A pure, backend-neutral model of Linux signal semantics for Reverie tools
and backends: pending queues with siginfo, blocked masks, dispositions,
alternate stacks, Linux's receiver-selection rule, syscall-restart mapping
and handler frame-entry policy.

The crate decides *what* Linux does; callers decide *when*. It performs no
I/O, names no backend and no consumer, and its state is a plain value that
callers may clone, snapshot or serialize. Delivery mechanics (kernel
delivery, frame bytes) stay with backends.
