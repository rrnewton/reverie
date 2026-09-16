# KVM inotify and aggregate vectored I/O integration

## Local result

- Worktree: `/home/newton/work/dev-hermit/worktrees/slots/kvm-inotify-vectored`
- Branch: `codex/kvm-inotify-vectored`
- Local commit: `5b4fff2092155a2b1e13b50ae2b25d3fd863808d`
- Tree: `d252c5e57c8ba5acfdb8622f117044d93c15bf05`
- Parents, in order:
  - `611cb6b018fc29a7a0421eb264970c768cd2bda0`
  - `90ad5b98fa897f03e817d74b1fa66e68f1b758fb`
- Nothing was pushed or merged upstream.

The merge conflict in `reverie-kvm/tests/static_elf.rs` was only the two
branches inserting independent test blocks at the same location. Both blocks
are retained. The aggregate vectored-I/O parent is the repaired commit
`90ad5b98fa897f03e817d74b1fa66e68f1b758fb`, not its earlier head.

## Implemented behavior

All six aggregate vectored syscalls enter `vectored_io`: `readv`, `writev`,
`preadv`, `pwritev`, `preadv2`, and `pwritev2`.

- Current-position calls are `readv`, `writev`, `preadv2` with offset `-1`, and
  `pwritev2` with offset `-1`.
- Positioned `preadv`, `pwritev`, `preadv2`, and `pwritev2` use the existing
  seekability preflight before importing guest vectors. Inotify and sockets
  therefore return the host `ESPIPE` result without changing their queues or
  tracked descriptor state.
- Virtual signalfd reads and captured stdout/stderr writes retain their existing
  dedicated paths after vector decoding. Other descriptors use the staged
  aggregate host call.

For tracked inotify descriptors, only `readv` and current-position
`preadv2(-1)` use the new path. A positive host result is gathered across the
staged vectors for exactly the returned byte count, canonicalized once, then
scattered before guest copyout. A nonempty attempt calls
`retire_inotify_cookies_if_drained` exactly once after the host call, including
host and canonicalization errors. A zero-total call leaves the inotify state
unchanged. Negative results never parse prefilled staging bytes, and an
`EFAULT` never copies raw host cookie bytes into guest memory.

For tracked sockets, current-position aggregate writes hold the existing
per-description `send_lock`, publish one plain message with the aggregate
payload length and `ConnectedPeer`, make one nonblocking `sendmsg`, then finish
or roll back that message once. Current-position aggregate reads hold
`SOCKET_MESSAGE_IO_LOCK`, make one nonblocking `recvmsg`, and discard the
corresponding plain message once after success. A datagram consumed on
`EFAULT` also discards its message; a stream fault leaves its message and bytes
queued. Zero-total calls use the original vector syscall and do not change
message state. Socket `preadv2`/`pwritev2` flags that native Linux rejects with
`EOPNOTSUPP` are rejected before publication.

Existing `InotifyDescriptionState` and `SocketDescriptionState` sharing across
duplication, fork, exec, and `SCM_RIGHTS` is unchanged; the aggregate paths
clone the same `Arc` values from the descriptor maps.

## Added regression coverage

- Real KVM test
  `dynamic_inotify_vectored_move_cookies_cover_both_current_position_calls`
  covers both `readv` and `preadv2(-1)`. A move pair spans the 32-byte iovec
  boundary and reconstructs equal canonical cookies. A separate one-sided move
  is also read for each syscall.
- Unit test `inotify_vectored_reads_transform_one_logical_event_stream` checks
  both current-position calls, a zero-length middle iovec, a move pair, a
  one-sided move, a host error, zero-total state preservation, and positioned
  `ESPIPE`. It explicitly checks an empty host queue and empty cookie map after
  the reads that drain the queue.
- Unit test `inotify_vectored_fault_does_not_copy_host_cookie_bytes` checks both
  current-position calls and verifies that a protected destination reports
  `EFAULT` without exposing a raw cookie prefix.
- Unit test `aggregate_vectored_socket_io_preserves_message_order_and_rights`
  covers both write/read pairs. It sends a typed inotify right A, a plain
  aggregate datagram, and typed inotify right B; receives A, the aggregate
  payload, and B in order; verifies shared descriptor state for both rights;
  and checks the message queue is empty. It also covers zero-total calls and
  unsupported socket flags.
- Unit test `socket_vectored_fault_updates_only_consumed_message_state` checks
  datagram and stream `EFAULT` behavior for both syscall pairs.

## Validation

The following passed from this exact tree before the local commit; the final
source was restored byte-for-byte after each mutation, and `cargo fmt --check`
and `git show --check` passed afterward.

- `cargo test -p reverie-kvm vectored -- --test-threads=1`: 18 unit tests and 4
  real KVM tests passed.
- Focused inotify tests: 13 unit tests and 3 real KVM tests passed.
- `cargo test -p reverie-kvm -- --test-threads=1`: 327 tests passed (266 unit,
  3 counter, 2 erestartsys, 47 static KVM, 3 strace, 6 vmcall).
- `cargo test --workspace --all-features -- --test-threads=1`: passed with the
  repository's existing ignored tests unchanged.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`:
  passed.
- `cargo fmt --all -- --check`, `git diff --check`, and `git show --check`:
  passed.

Native probes established the endpoint details used by the implementation:
raw socket `readv`/`writev` with zero vectors does not consume or send a
datagram; socket `preadv2`/`pwritev2` accepts flags 0, 1, 2, 4, 8, 16, 32, and
256 on this host, rejects 64 (`RWF_ATOMIC`) and 128 (`RWF_DONTCACHE`) with
`EOPNOTSUPP`, and retains `EINVAL` for conflicting combinations.

Nine focused mutations were each rejected by a named regression:

1. Omitting aggregate inotify canonicalization failed the real KVM cookie test.
2. Omitting inotify cleanup after a one-sided move left the cookie map nonempty.
3. Allowing inotify `EFAULT` prefix copyback exposed raw host event bytes.
4. Omitting aggregate socket publication shifted the pending message count.
5. Omitting successful aggregate receive discard broke the next typed receive.
6. Omitting datagram `EFAULT` discard left a stale pending message.
7. Treating zero-total socket calls as messages consumed an existing marker.
8. Forwarding unsupported socket vector flags returned success instead of
   `EOPNOTSUPP`.
9. Canonicalizing only the first 32-byte vector made the move pair crossing the
   iovec boundary fail in the real KVM test.

Mutation logs are retained under `/tmp/mutation-*.log` and
`/tmp/kvm-inotify-vectored-mutation9.log` for this session.

## Remaining review boundary

This is a stable local integration commit, not an upstream proposal. It has not
received an independent review and was deliberately not pushed. No test,
assertion, comparator, or existing ignore annotation was weakened.
