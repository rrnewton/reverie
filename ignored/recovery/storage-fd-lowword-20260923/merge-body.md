[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=impl]

Linux consumes only the declared 32-bit descriptor argument from each syscall register. Apply that ABI coercion to fallocate, fsync, fdatasync, readahead, and sync_file_range while preserving bit-31 EBADF behavior and existing validation order.

Add focused unit coverage plus native, direct-KVM, and Tool-KVM output/exit/status parity checks for accepted high-word aliases, post-decode errors, and invalid low words.
