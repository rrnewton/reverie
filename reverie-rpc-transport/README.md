# reverie-rpc-transport

Cross-process transport for [Reverie](https://docs.rs/reverie-core)
`GlobalTool` RPC, using Unix-domain sockets and bincode framing. A backend can
use it to keep tool-global state in a coordinator process while guest-side
callbacks run elsewhere.

This is support for backends that explicitly select it. Hermit's default
ptrace and KVM installation does not require this transport.

```toml
[dependencies]
reverie-rpc-transport = "0.5"
```

See the [API documentation](https://docs.rs/reverie-rpc-transport) for the
server, readiness, connection, and shutdown contracts. For the project and
CLI, visit [Hermit](https://hermetic-infra.org) and
[`hermit-run`](https://crates.io/crates/hermit-run).

## Initial-root declaration

A config-first launcher may opt in with
`InitialRootReadyMonitor::for_child(&mut child)` and
`server.require_initial_root_ready(monitor.clone())`. The owned or borrowed
`BlockingRpcClient::declare_initial_root_ready()` exchanges a fixed control on
its existing stream. After waiting for its real child and draining connections,
the launcher checks `monitor.check_declared()` alongside existing errors and
exit status. Config delivery and application requests do not declare readiness;
the control never invokes `GlobalTool::receive_rpc`.

This is one transport declaration intended for the initial root image, **not
proof of Tool installation, activation or image identity**. Exec/reconnection
does not reset it. No launcher or installer adopts it in this change. Private
InitialImage/successor admission cannot be combined with this mode. A later
installer must report from its actual installed owner after activation, within
its installation transaction, without recursively intercepting control I/O.

The caller supplies its actual never-reaped Child, excludes other waiters and
retains cleanup ownership. Binding observes status without reaping. Server and
client share a PID namespace. Both the retained root pidfd and the socket's
actual `SO_PEERPIDFD` must be live when a declaration begins; cached numeric PID
comparison alone is insufficient. This authenticates the original connecting
process, not an inherited-FD writer, physical calling thread or installed Tool.
The encoded sender must equal the root PID. Peer capture errors do not affect
ordinary config/application/EOF traffic; they refuse only a readiness control.

The outer u32 big-endian frame is unchanged. The control is exactly twelve bytes:
`ff ff ff ff 52 49 01 00` followed by a positive little-endian i32 sender. The
ACK changes byte 7 to `01`. Reserved-family malformed frames, duplicates and
incomplete ACKs refuse; a later duplicate can invalidate an ACK already seen by
a client. Legacy positive-TID RPC decoding remains unchanged. Proper reserved
prefixes of lengths 1–5 now refuse before application decode: lengths 4 and 5
intentionally tighten formerly decodable invalid negative-TID unit/u8 messages.
No numeric-peer fallback is used when the kernel cannot provide `SO_PEERPIDFD`.
