# Predeclared default-parallel diagnostic matrix

Run the unchanged default Cargo test concurrency on five clean local-clone
executions of base `60f2d369b49e6ffbc2b2d9d0f0e55fead0ba6b09` and five clean local-clone
executions of head `ce442fce6b1ee366e0c3a9cff7fa981ff8893ae4`. The host reports 316 logical
CPUs, so the unqualified harness default is 316 test threads.

The runner must execute all ten runs regardless of individual status, retain
stdout/stderr and exit codes, and make no source changes. This is diagnostic,
not a retry-based qualification. The prior failed v4 receipt remains binding
evidence and is not superseded by this matrix.
