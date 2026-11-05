/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The KVM backend reports when a guest process's exit is published
//! (`BackendCapabilities::reports_child_exit_publication`), as ptrace does
//! (reverie-ptrace/tests/child_exit_publication.rs): once per process leader,
//! after the publication and before the Tool's exit hook for that process,
//! with that process's PID. A thread that ends while its process goes on is
//! never reported: neither the root's thread nor the child's.

#![cfg(target_arch = "x86_64")]

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use kvm_ioctls::Kvm;
use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Pid;
use reverie::Tid;
use reverie::Tool;
use reverie_kvm::KvmBackend;

/// A lifecycle event as the global state saw it.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize
)]
enum Lifecycle {
    /// `GlobalTool::on_backend_process_exited`.
    Published(i32),
    /// `Tool::on_exit_thread`, reported by RPC.
    ExitThread(i32),
    /// `Tool::on_exit_process`, reported by RPC.
    ExitProcess(i32),
}

#[derive(Default)]
struct PublicationLog {
    events: Mutex<Vec<Lifecycle>>,
}

#[reverie::global_tool]
impl GlobalTool for PublicationLog {
    type Request = Lifecycle;
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Pid, event: Lifecycle) {
        self.events.lock().unwrap().push(event);
    }

    fn on_backend_process_exited(&self, pid: i32) {
        self.events.lock().unwrap().push(Lifecycle::Published(pid));
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct PublicationTool;

#[reverie::tool]
impl Tool for PublicationTool {
    type GlobalState = PublicationLog;
    type ThreadState = ();

    async fn on_exit_thread<G: GlobalRPC<Self::GlobalState>>(
        &self,
        tid: Tid,
        global_state: &G,
        _thread_state: (),
        _exit_status: ExitStatus,
    ) -> Result<(), Error> {
        global_state
            .send_rpc(Lifecycle::ExitThread(tid.as_raw()))
            .await;
        Ok(())
    }

    async fn on_exit_process<G: GlobalRPC<Self::GlobalState>>(
        self,
        pid: Pid,
        global_state: &G,
        _exit_status: ExitStatus,
    ) -> Result<(), Error> {
        global_state
            .send_rpc(Lifecycle::ExitProcess(pid.as_raw()))
            .await;
        Ok(())
    }
}

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "reverie-kvm-child-exit-publication-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn kvm_reports_child_exit_publication_once_before_exit_hooks() {
    const { assert!(reverie::BackendCapabilities::KVM.reports_child_exit_publication) };
    Kvm::new().expect("child exit publication test requires usable /dev/kvm");
    let directory = Directory::new();
    let source = directory.0.join("child_exit_publication.c");
    let executable = directory.0.join("child_exit_publication");
    std::fs::write(&source, include_str!("fixtures/child_exit_publication.c")).unwrap();
    let compiled = std::process::Command::new("/usr/bin/gcc")
        .args(["-O2", "-std=c11", "-pthread", "-Wall", "-Wextra", "-Werror"])
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "gcc: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );

    let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
    backend
        .install_static_elf_file_with_context(
            std::fs::File::open(&executable).unwrap(),
            &[executable.to_str().unwrap()],
            &[],
            &directory.0,
        )
        .unwrap();
    let (log, code, stdout, stderr) =
        futures::executor::block_on(backend.run_static_elf_with_tool::<PublicationTool>((), true))
            .unwrap();
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
    assert_eq!(String::from_utf8_lossy(&stdout), "ok\n");
    let events = log.events.into_inner().unwrap();

    let processes: Vec<i32> = events
        .iter()
        .filter_map(|event| match event {
            Lifecycle::ExitProcess(pid) => Some(*pid),
            _ => None,
        })
        .collect();
    // The root's exit hook runs last; the child's ran before it.
    let (&root, children) = processes.split_last().expect("exit hooks ran");
    let [child] = children else {
        panic!("expected one child process, saw {processes:?} in {events:?}");
    };
    assert_ne!(*child, root, "{events:?}");

    // Each process leader is reported exactly once, before its exit hook.
    for pid in [*child, root] {
        let published: Vec<usize> = events
            .iter()
            .enumerate()
            .filter(|(_, event)| **event == Lifecycle::Published(pid))
            .map(|(index, _)| index)
            .collect();
        let hook = events
            .iter()
            .position(|event| *event == Lifecycle::ExitProcess(pid))
            .unwrap();
        assert_eq!(
            published.len(),
            1,
            "process {pid} reported {} times: {events:?}",
            published.len()
        );
        assert!(
            published[0] < hook,
            "process {pid} reported after its exit hook: {events:?}"
        );
    }

    // Nothing else is reported: in particular no thread that ended while its
    // process went on. Both the root and the child ran one such thread.
    let threads: Vec<i32> = events
        .iter()
        .filter_map(|event| match event {
            Lifecycle::ExitThread(tid) if !processes.contains(tid) => Some(*tid),
            _ => None,
        })
        .collect();
    assert_eq!(
        threads.len(),
        2,
        "expected two non-leader thread exits: {events:?}"
    );
    let reported: Vec<i32> = events
        .iter()
        .filter_map(|event| match event {
            Lifecycle::Published(pid) => Some(*pid),
            _ => None,
        })
        .collect();
    let mut expected = vec![*child, root];
    expected.sort_unstable();
    let mut reported_sorted = reported.clone();
    reported_sorted.sort_unstable();
    assert_eq!(reported_sorted, expected, "{events:?}");
    for tid in threads {
        assert!(
            !reported.contains(&tid),
            "thread {tid} reported: {events:?}"
        );
    }
}
