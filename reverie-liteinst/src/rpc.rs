//! Coordinator RPC handle for in-guest Reverie tools, now in
//! [`reverie_inguest::guest::rpc`].

pub use reverie_inguest::guest::rpc::CoordinatorRpc;
pub(crate) use reverie_inguest::guest::rpc::note_fork_in_child;
// The async-signal-safe spinlock is shared across the in-guest tool hosts.
pub(crate) use reverie_inguest::sync::SpinMutex;
