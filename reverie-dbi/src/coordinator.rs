/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Cross-process coordination for the DynamoRIO (DBI) multi-process vehicle.
//!
//! A DynamoRIO client is *strictly per-process*: every process DR injects into
//! (the initial target plus, under the default `-follow_children`, every
//! forked/execed child) loads its own copy of the client `.so` into its own
//! address space with its own private globals. Nothing in the Rust runtime's
//! process-global `static`s or the native client's file-scope state is shared
//! across a `fork`/`execve` boundary. So "coordinator IPC" cannot be a shared
//! `static`; it has to go through an OS channel.
//!
//! This module implements the coordination channel using the canonical, robust
//! DynamoRIO multi-process pattern (the one drcachesim's `-offline` mode and
//! Dr. Memory use): **per-process record files aggregated offline**. Each
//! instrumented process writes one small record — its pid, its parent pid, its
//! observed syscall count, and its program name — into a shared *coordinator
//! directory* named by [`COORD_DIR_ENV`]. Because the directory path travels in
//! the environment, it survives both `fork` (inherited) and `execve` (env is
//! preserved across exec), so every followed child in the process tree finds
//! the same coordinator.
//!
//! The native client writes those records with DynamoRIO-safe file I/O
//! (`dr_open_file`/`dr_write_file`); this module owns the *format* (so the two
//! sides agree) and the *offline aggregation*: reading every record back,
//! reconstructing the process tree from the parent links, and assigning
//! **deterministic in-tree ids** that do not depend on the (nondeterministic)
//! OS pid values. That deterministic id assignment is the "process tree
//! determinism" primitive: the same tree shape yields the same ids run to run.
//!
//! What this does *not* yet provide is *live* shared global state during
//! execution (a cross-process [`reverie::GlobalTool`] instance) or a scheduler
//! that spans processes — those need an online coordinator plus the cooperative
//! executor tracked by the M3 roadmap and are deliberately out of scope here.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

/// Environment variable naming the shared coordinator directory. The launcher
/// sets it; every followed child inherits it across `fork` and `execve`.
pub const COORD_DIR_ENV: &str = "REVERIE_DBI_COORD_DIR";

/// One process's contribution to the coordinator, written once at process exit.
///
/// The wire format is a single line: `pid ppid syscalls comm\n`. `comm` is the
/// program's base name with any whitespace replaced by `_`, so the record is
/// always exactly four space-separated fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessRecord {
    /// This process's pid (`dr_get_process_id`). Stable across `execve`.
    pub pid: i32,
    /// The parent pid (`getppid`), used to reconstruct the tree.
    pub ppid: i32,
    /// Syscalls observed by the client in this process image.
    pub syscalls: u64,
    /// Program base name (`dr_get_application_name`), whitespace-sanitized.
    pub comm: String,
}

impl ProcessRecord {
    /// The per-process file name inside the coordinator directory.
    pub fn file_name(pid: i32) -> String {
        format!("proc-{pid}")
    }

    /// Encodes the record as the single-line wire format (with trailing `\n`).
    pub fn encode(&self) -> String {
        format!(
            "{} {} {} {}\n",
            self.pid,
            self.ppid,
            self.syscalls,
            sanitize_comm(&self.comm)
        )
    }

    /// Parses one wire-format line. Returns `None` for malformed input so a
    /// partially written or garbage file cannot poison aggregation.
    pub fn parse(line: &str) -> Option<ProcessRecord> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        // Split into at most four fields; comm is the remainder (already
        // whitespace-free by construction, but be lenient on read).
        let mut fields = line.splitn(4, char::is_whitespace);
        let pid = fields.next()?.parse().ok()?;
        let ppid = fields.next()?.parse().ok()?;
        let syscalls = fields.next()?.parse().ok()?;
        let comm = fields.next().unwrap_or("").to_string();
        Some(ProcessRecord {
            pid,
            ppid,
            syscalls,
            comm,
        })
    }
}

fn sanitize_comm(comm: &str) -> String {
    let trimmed = comm.trim();
    if trimmed.is_empty() {
        return "?".to_string();
    }
    trimmed
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect()
}

/// Returns the coordinator directory from the environment, if enabled.
pub fn coord_dir_from_env() -> Option<PathBuf> {
    coord_dir_from(std::env::var_os(COORD_DIR_ENV))
}

fn coord_dir_from(value: Option<OsString>) -> Option<PathBuf> {
    let value = value?;
    if value.is_empty() {
        None
    } else {
        Some(PathBuf::from(value))
    }
}

/// Writes a record into the coordinator directory (one file per pid).
///
/// This mirrors exactly what the native client does with DynamoRIO-safe I/O;
/// it exists for tests and for any non-DR caller. Writing a distinct file per
/// pid means concurrent processes never contend on the same file.
pub fn write_record(dir: &Path, record: &ProcessRecord) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join(ProcessRecord::file_name(record.pid));
    fs::write(path, record.encode())
}

/// Reads and parses every `proc-*` record in the coordinator directory.
///
/// Malformed or unrelated files are skipped rather than erroring, so a stray
/// file in the directory cannot break aggregation. Duplicate pids (e.g. a
/// fork-then-exec that recorded twice) are collapsed to the last writer, which
/// for the exec case is the post-exec image — the one whose counts we want.
pub fn collect_records(dir: &Path) -> io::Result<Vec<ProcessRecord>> {
    let mut by_pid: BTreeMap<i32, ProcessRecord> = BTreeMap::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("proc-") {
            continue;
        }
        let Ok(contents) = fs::read_to_string(entry.path()) else {
            continue;
        };
        if let Some(record) = ProcessRecord::parse(&contents) {
            by_pid.insert(record.pid, record);
        }
    }
    Ok(by_pid.into_values().collect())
}

/// One node in the reconstructed process tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeNode {
    /// Deterministic in-tree id (preorder from the root), independent of pids.
    pub det_id: u64,
    /// Deterministic id of the in-tree parent, or `None` for the root.
    pub parent_det_id: Option<u64>,
    /// The underlying per-process record.
    pub record: ProcessRecord,
}

/// A process tree assembled from coordinator records, with deterministic ids.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ProcessTree {
    /// Nodes in deterministic preorder (index == position, not the det_id of a
    /// specific pid — use [`TreeNode::det_id`]). `nodes[0]` is the root when the
    /// tree is non-empty.
    pub nodes: Vec<TreeNode>,
}

impl ProcessTree {
    /// Builds the tree from records, assigning deterministic in-tree ids.
    ///
    /// The root is the process whose parent pid is not itself a recorded pid
    /// (the initial `drrun` target, whose parent is the untraced launcher).
    /// Ids are assigned by a preorder walk in which siblings are ordered by a
    /// pid-independent key `(comm, syscalls, pid)` — the trailing pid is only a
    /// last-resort tie-break between siblings that are otherwise identical
    /// (same program, same syscall count); distinguishing those perfectly would
    /// require capturing the parent's fork order, a live-coordination follow-up.
    pub fn build(records: Vec<ProcessRecord>) -> ProcessTree {
        if records.is_empty() {
            return ProcessTree::default();
        }
        let pids: std::collections::HashSet<i32> = records.iter().map(|r| r.pid).collect();

        // children_of[ppid] -> child records; roots collected separately.
        let mut children_of: HashMap<i32, Vec<ProcessRecord>> = HashMap::new();
        let mut roots: Vec<ProcessRecord> = Vec::new();
        for record in records {
            if pids.contains(&record.ppid) && record.ppid != record.pid {
                children_of.entry(record.ppid).or_default().push(record);
            } else {
                roots.push(record);
            }
        }

        let sort_key = |r: &ProcessRecord| (r.comm.clone(), r.syscalls, r.pid);
        roots.sort_by_key(&sort_key);
        for kids in children_of.values_mut() {
            kids.sort_by_key(&sort_key);
        }

        let mut nodes = Vec::new();
        let mut next_id: u64 = 0;
        // Explicit stack of (record, parent_det_id) for deterministic preorder.
        // Push roots in reverse so the first root is processed first.
        let mut stack: Vec<(ProcessRecord, Option<u64>)> = Vec::new();
        for root in roots.into_iter().rev() {
            stack.push((root, None));
        }
        while let Some((record, parent_det_id)) = stack.pop() {
            let det_id = next_id;
            next_id += 1;
            let pid = record.pid;
            nodes.push(TreeNode {
                det_id,
                parent_det_id,
                record,
            });
            if let Some(kids) = children_of.remove(&pid) {
                for kid in kids.into_iter().rev() {
                    stack.push((kid, Some(det_id)));
                }
            }
        }

        ProcessTree { nodes }
    }

    /// Number of processes in the tree.
    pub fn process_count(&self) -> usize {
        self.nodes.len()
    }

    /// Tree-wide syscall total, aggregated across every process.
    pub fn total_syscalls(&self) -> u64 {
        self.nodes.iter().map(|n| n.record.syscalls).sum()
    }

    /// A fully deterministic rendering: it deliberately omits raw OS pids, so
    /// two runs of the same workload produce byte-identical output. This is the
    /// artifact determinism tests compare.
    pub fn render_deterministic(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "reverie-dbi coordinator: {} process(es), {} syscalls total\n",
            self.process_count(),
            self.total_syscalls()
        ));
        for node in &self.nodes {
            let parent = match node.parent_det_id {
                Some(p) => p.to_string(),
                None => "-".to_string(),
            };
            out.push_str(&format!(
                "  proc {} parent {} comm {} syscalls {}\n",
                node.det_id, parent, node.record.comm, node.record.syscalls
            ));
        }
        out
    }
}

/// Convenience: read the coordinator directory and build the tree in one step.
pub fn summarize_dir(dir: &Path) -> io::Result<ProcessTree> {
    Ok(ProcessTree::build(collect_records(dir)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(pid: i32, ppid: i32, syscalls: u64, comm: &str) -> ProcessRecord {
        ProcessRecord {
            pid,
            ppid,
            syscalls,
            comm: comm.to_string(),
        }
    }

    #[test]
    fn record_round_trips_through_wire_format() {
        let r = rec(42, 7, 113, "echo");
        let encoded = r.encode();
        assert_eq!(encoded, "42 7 113 echo\n");
        assert_eq!(ProcessRecord::parse(&encoded), Some(r));
    }

    #[test]
    fn record_comm_whitespace_is_sanitized_and_parse_is_lenient() {
        let r = rec(1, 0, 5, "my prog");
        assert_eq!(r.encode(), "1 0 5 my_prog\n");
        // Missing comm parses to empty; blank/garbage lines reject.
        assert_eq!(ProcessRecord::parse("3 2 9"), Some(rec(3, 2, 9, "")));
        assert_eq!(ProcessRecord::parse("   "), None);
        assert_eq!(ProcessRecord::parse("not a record"), None);
        assert_eq!(ProcessRecord::parse("1 x 3 c"), None);
    }

    #[test]
    fn coord_dir_env_parsing_treats_empty_as_disabled() {
        assert_eq!(coord_dir_from(None), None);
        assert_eq!(coord_dir_from(Some(OsString::from(""))), None);
        assert_eq!(
            coord_dir_from(Some(OsString::from("/tmp/x"))),
            Some(PathBuf::from("/tmp/x"))
        );
    }

    #[test]
    fn write_then_collect_reads_all_records_and_skips_noise() {
        let dir = tempfile::tempdir().unwrap();
        write_record(dir.path(), &rec(100, 50, 10, "bash")).unwrap();
        write_record(dir.path(), &rec(101, 100, 20, "echo")).unwrap();
        // An unrelated file must be ignored.
        fs::write(dir.path().join("README"), b"ignore me").unwrap();
        // A malformed proc file must be skipped, not fatal.
        fs::write(dir.path().join("proc-bad"), b"garbage").unwrap();

        let mut records = collect_records(dir.path()).unwrap();
        records.sort_by_key(|r| r.pid);
        assert_eq!(
            records,
            vec![rec(100, 50, 10, "bash"), rec(101, 100, 20, "echo")]
        );
    }

    #[test]
    fn duplicate_pid_keeps_last_writer_for_exec_case() {
        let dir = tempfile::tempdir().unwrap();
        // Simulate fork (pre-exec) then execve re-recording under the same pid.
        write_record(dir.path(), &rec(200, 100, 3, "bash")).unwrap();
        write_record(dir.path(), &rec(200, 100, 158, "cat")).unwrap();
        let records = collect_records(dir.path()).unwrap();
        assert_eq!(records, vec![rec(200, 100, 158, "cat")]);
    }

    #[test]
    fn tree_reconstructs_pipeline_with_deterministic_ids() {
        // bash (root; parent is the untraced launcher pid 9) forks echo and cat.
        let records = vec![
            rec(1002, 1000, 40, "cat"),
            rec(1000, 9, 120, "bash"),
            rec(1001, 1000, 30, "echo"),
        ];
        let tree = ProcessTree::build(records);
        assert_eq!(tree.process_count(), 3);
        assert_eq!(tree.total_syscalls(), 190);

        // Root is bash (its ppid 9 is not a recorded pid).
        assert_eq!(tree.nodes[0].record.comm, "bash");
        assert_eq!(tree.nodes[0].det_id, 0);
        assert_eq!(tree.nodes[0].parent_det_id, None);

        // Children ordered by (comm, syscalls, pid): "cat" before "echo".
        assert_eq!(tree.nodes[1].record.comm, "cat");
        assert_eq!(tree.nodes[1].parent_det_id, Some(0));
        assert_eq!(tree.nodes[2].record.comm, "echo");
        assert_eq!(tree.nodes[2].parent_det_id, Some(0));
    }

    #[test]
    fn deterministic_render_is_independent_of_pid_values() {
        // Two runs of the same workload with entirely different OS pids must
        // render identically.
        let run_a = vec![
            rec(500, 9, 120, "bash"),
            rec(501, 500, 30, "echo"),
            rec(502, 500, 40, "cat"),
        ];
        let run_b = vec![
            rec(99001, 42, 120, "bash"),
            rec(99050, 99001, 30, "echo"),
            rec(99070, 99001, 40, "cat"),
        ];
        let a = ProcessTree::build(run_a).render_deterministic();
        let b = ProcessTree::build(run_b).render_deterministic();
        assert_eq!(a, b);
        assert!(a.contains("3 process(es), 190 syscalls total"));
        assert!(a.contains("proc 0 parent - comm bash"));
    }

    #[test]
    fn empty_directory_yields_empty_tree() {
        let dir = tempfile::tempdir().unwrap();
        let tree = summarize_dir(dir.path()).unwrap();
        assert_eq!(tree.process_count(), 0);
        assert_eq!(tree.total_syscalls(), 0);
        assert_eq!(
            tree.render_deterministic(),
            "reverie-dbi coordinator: 0 process(es), 0 syscalls total\n"
        );
    }

    #[test]
    fn nested_tree_assigns_preorder_ids() {
        // root -> a -> grandchild ; root -> b
        let records = vec![
            rec(1, 9, 1, "root"),
            rec(2, 1, 2, "a_child"),
            rec(3, 1, 3, "b_child"),
            rec(4, 2, 4, "grandchild"),
        ];
        let tree = ProcessTree::build(records);
        // Preorder with siblings by comm: root(0) -> a_child(1) -> grandchild(2) -> b_child(3)
        let order: Vec<&str> = tree.nodes.iter().map(|n| n.record.comm.as_str()).collect();
        assert_eq!(order, vec!["root", "a_child", "grandchild", "b_child"]);
        assert_eq!(tree.nodes[2].parent_det_id, Some(1)); // grandchild under a_child
        assert_eq!(tree.nodes[3].parent_det_id, Some(0)); // b_child under root
    }
}
