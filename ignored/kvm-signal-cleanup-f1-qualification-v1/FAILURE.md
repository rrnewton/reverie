This first receipt attempt is intentionally retained as failed evidence.

The runner applied a 64 MiB `RLIMIT_FSIZE` to the Cargo child. That limit also
applied to KVM guest-memory backing files, not only captured stdout/stderr. The
focused test process therefore terminated with signal 25 (`SIGXFSZ`) before any
test result. Source identity remained unchanged. This is an evidence-harness
failure, not a source verdict.

`../kvm-signal-cleanup-f1-qualification-v2/` removes that child file-size
limit while retaining command timeouts and separate captured streams. Its
source-bound receipt completed all seven phases successfully.
