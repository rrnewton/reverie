/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// TODO-HUMAN-REVIEW(PR-98): Cross-process `fork()` aggregation proof for the
// reverie-rpc-transport coordinator model, authored by an autonomous bot as
// evidence for the DBI global-state fix (task impl-dbi-global-state-fix). This
// is a test-only artifact; it changes no guest-visible syscall behavior.

//! Real-`fork()` aggregation proof for the coordinator model.
//!
//! # What this proves that `round_trip.rs` does not
//!
//! [`round_trip::aggregates_across_many_connections`] opens several client
//! connections from a *single* process, so it stands in for a `fork` tree but
//! never actually crosses a process boundary. That is exactly the case the DBI
//! backend already handles today (one process, one in-address-space
//! `GlobalState`).
//!
//! The P0 defect this transport exists to fix is different: DynamoRIO follows
//! `fork`, so each guest **process** gets an independent, copy-on-write copy of
//! `GlobalState`, and effects (syscall counts, scheduling, virtual time) never
//! aggregate across the tree. See the crate docs and the task
//! `impl-dbi-global-state-fix`.
//!
//! This test reproduces that exact scenario with a real `libc::fork()`:
//!
//! * the **parent** process is the coordinator — it owns the one and only
//!   `Arc<Counter>` [`GlobalTool`] behind an [`RpcServer`];
//! * each **child** is a genuinely separate process with its own COW copy of
//!   the address space (so its *own* copy of the `Counter` struct is useless to
//!   the parent — precisely the fragmentation that breaks DBI today);
//! * every child connects an [`RpcClient`] back to the parent's Unix-domain
//!   socket and issues increments;
//! * after reaping the children, the parent asserts the coordinator-side
//!   `Counter` holds the sum over the whole tree.
//!
//! If the children were incrementing their own forked copies of the global
//! state (the current DBI bug), the parent's total would be `0`, not
//! `children * per_child`.

use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;

use async_trait::async_trait;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Tid;
use reverie_rpc_transport::RpcClient;
use reverie_rpc_transport::RpcServer;

/// A minimal aggregating global tool, mirroring a syscall counter that must sum
/// across a whole process tree. Deliberately identical in spirit to the
/// `Counter` in `round_trip.rs` so the only new variable here is the real
/// process boundary.
#[derive(Default)]
struct Counter {
    total: Mutex<u64>,
    /// Distinct originating tids observed, so we can prove every child's traffic
    /// actually reached the one coordinator (not just that the sum happens to
    /// match).
    froms: Mutex<Vec<i32>>,
}

#[async_trait]
impl GlobalTool for Counter {
    type Request = u64;
    type Response = u64;
    type Config = String;

    async fn receive_rpc(&self, from: Tid, increment: u64) -> u64 {
        self.froms.lock().unwrap().push(from.as_raw());
        let mut total = self.total.lock().unwrap();
        *total += increment;
        *total
    }
}

/// Allocate a unique, short-lived socket path under the temp dir.
fn unique_sock_path(tag: &str) -> std::path::PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("reverie-rpc-{tag}-{}-{n}.sock", std::process::id()))
}

/// Body run inside a freshly `fork()`ed child process.
///
/// A child is a separate process: only the calling thread survives the fork, so
/// the parent's (multi-threaded) Tokio runtime does not exist here. The child
/// therefore builds its **own** current-thread runtime and touches nothing but
/// the socket — modelling a re-initialised DynamoRIO client in a followed fork
/// child, which likewise reconnects fresh to the coordinator.
///
/// Returns the process exit code; the caller must `std::process::exit` with it
/// so the test harness does not run in the child.
fn child_body(server_path: &Path, tid: i32, increments: u64) -> i32 {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => return 11,
    };
    runtime.block_on(async move {
        let client = match RpcClient::<Counter>::connect(server_path, Tid::from_raw(tid)).await {
            Ok(c) => c,
            Err(_) => return 12,
        };
        // The handshake must have delivered the coordinator's config into this
        // separate address space (the child never constructed it locally).
        if client.config() != "coordinator-config" {
            return 13;
        }
        for _ in 0..increments {
            // Each round-trip blocks this child until the coordinator replies,
            // exactly as a guest thread will block awaiting its scheduler turn.
            let _running_total = client.send_rpc(1).await;
        }
        0
    })
}

/// The core proof: real forked children all aggregate into one coordinator
/// `GlobalState`.
#[test]
fn aggregates_across_a_real_fork_tree() {
    let children: i32 = 4;
    let per_child: u64 = 50;

    let global = Arc::new(Counter::default());
    let path = unique_sock_path("fork-agg");

    // The parent hosts the coordinator on a multi-threaded runtime. We bind the
    // socket and start the accept loop BEFORE forking, so the listener is live
    // and the kernel will queue the children's connect() calls immediately.
    let runtime = tokio::runtime::Runtime::new().expect("build parent runtime");
    let server = runtime.block_on(async {
        RpcServer::bind(&path, global.clone(), "coordinator-config".to_string())
            .expect("bind coordinator socket")
    });
    let server_path = server.path().to_path_buf();
    let serve_handle = runtime.spawn(async move { server.serve().await });

    // Fork the children. We fork from the main thread (not a Tokio worker); the
    // workers are parked in epoll_wait on accept at this point, so this is a
    // safe place to fork.
    let mut child_pids = Vec::new();
    for i in 0..children {
        // SAFETY: `fork` is inherently unsafe. The child does not touch the
        // parent's runtime state; it builds its own runtime and exits via
        // `process::exit`, never returning into the test harness.
        match unsafe { libc::fork() } {
            -1 => panic!("fork failed: {}", std::io::Error::last_os_error()),
            0 => {
                let code = child_body(&server_path, 1000 + i, per_child);
                std::process::exit(code);
            }
            pid => child_pids.push(pid),
        }
    }

    // Parent: reap every child and require a clean exit. A nonzero code is a
    // child-side connect/handshake/RPC failure encoded by `child_body`.
    for pid in child_pids {
        let mut status: libc::c_int = 0;
        let waited = unsafe { libc::waitpid(pid, &mut status, 0) };
        assert_eq!(waited, pid, "waitpid must reap the exact child");
        let exited = libc::WIFEXITED(status);
        let code = libc::WEXITSTATUS(status);
        assert!(
            exited && code == 0,
            "child {pid} failed (exited={exited}, code={code})"
        );
    }

    // The whole point: the coordinator's single Counter holds the sum over the
    // entire fork tree. With the old per-process GlobalState this would be 0
    // (each child incremented its own COW copy, now discarded with the child).
    let total = *global.total.lock().unwrap();
    assert_eq!(
        total,
        children as u64 * per_child,
        "forked children must aggregate into ONE coordinator GlobalState, not \
         per-process copies"
    );

    // And every child's traffic really reached the coordinator: one distinct
    // originating tid per child, each with exactly `per_child` requests.
    let froms = global.froms.lock().unwrap();
    assert_eq!(froms.len(), (children as u64 * per_child) as usize);
    for i in 0..children {
        let tid = 1000 + i;
        assert_eq!(
            froms.iter().filter(|&&f| f == tid).count(),
            per_child as usize,
            "coordinator must have seen every RPC from child tid {tid}"
        );
    }

    serve_handle.abort();
}

/// Negative control: reproduce the *pre-fix* DBI architecture and show why it
/// fragments. Here the "global state" lives in the process address space (as it
/// does today, injected into each guest), and children mutate it directly with
/// **no** coordinator RPC. Because `fork()` gives each child its own COW copy,
/// the parent's copy is untouched: its total stays `0`.
///
/// This is what makes `aggregates_across_a_real_fork_tree` meaningful — the same
/// fork tree, differing only in whether increments go through the coordinator,
/// produces `children * per_child` (fixed) versus `0` (broken).
#[test]
fn control_in_process_state_fragments_across_fork() {
    let children: i32 = 4;
    let per_child: u64 = 50;

    // Stand-in for the injected, in-guest GlobalState of today's DBI backend.
    let in_process_total = Arc::new(Mutex::new(0u64));

    let mut child_pids = Vec::new();
    for _ in 0..children {
        // SAFETY: as above; the child only mutates its own COW copy and exits.
        match unsafe { libc::fork() } {
            -1 => panic!("fork failed: {}", std::io::Error::last_os_error()),
            0 => {
                // Child mutates ITS copy (the fork COW copy), never the parent's.
                let mut total = in_process_total.lock().unwrap();
                *total += per_child;
                std::process::exit(0);
            }
            pid => child_pids.push(pid),
        }
    }
    for pid in child_pids {
        let mut status: libc::c_int = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
    }

    // The defect, made explicit: without a coordinator the parent sees nothing
    // the children did. This is exactly why per-process GlobalState is broken.
    assert_eq!(
        *in_process_total.lock().unwrap(),
        0,
        "in-process state does NOT aggregate across fork() — the P0 defect"
    );
}

/// A late-joining fork child (one that connects after earlier children have
/// already finished and exited) still reaches the same coordinator state. This
/// mirrors a fork that happens deep into a run: the child must observe the
/// accumulated global state, not a fresh one.
#[test]
fn late_fork_child_observes_accumulated_state() {
    let global = Arc::new(Counter::default());
    let path = unique_sock_path("fork-late");

    let runtime = tokio::runtime::Runtime::new().expect("build parent runtime");
    let server = runtime.block_on(async {
        RpcServer::bind(&path, global.clone(), "coordinator-config".to_string())
            .expect("bind coordinator socket")
    });
    let server_path = server.path().to_path_buf();
    let serve_handle = runtime.spawn(async move { server.serve().await });

    // First wave: one child contributes, then fully exits.
    let first = match unsafe { libc::fork() } {
        -1 => panic!("fork failed: {}", std::io::Error::last_os_error()),
        0 => std::process::exit(child_body(&server_path, 2001, 30)),
        pid => pid,
    };
    let mut status: libc::c_int = 0;
    assert_eq!(unsafe { libc::waitpid(first, &mut status, 0) }, first);
    assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
    assert_eq!(*global.total.lock().unwrap(), 30);

    // Second wave: a NEW child forked after the first has gone. It must see the
    // running total continue from 30, proving state is coordinator-owned and
    // persists across the come-and-go of individual processes.
    let second = match unsafe { libc::fork() } {
        -1 => panic!("fork failed: {}", std::io::Error::last_os_error()),
        0 => std::process::exit(child_body(&server_path, 2002, 20)),
        pid => pid,
    };
    let mut status2: libc::c_int = 0;
    assert_eq!(unsafe { libc::waitpid(second, &mut status2, 0) }, second);
    assert!(libc::WIFEXITED(status2) && libc::WEXITSTATUS(status2) == 0);

    assert_eq!(
        *global.total.lock().unwrap(),
        50,
        "state must accumulate across the lifetime of the whole tree"
    );

    serve_handle.abort();
}
