/* Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Live process-clone callback delivery regression coverage.
//!
//! The fixture's begin/end markers preserve source-call attribution. Parent and
//! child callbacks inside one separate-VM call may arrive in either order; the
//! parent raw result must instead identify the child callback's host PID. The
//! coordinated causal variant additionally proves that each child callback is
//! delivered once at its first pre-syscall safe point before that getuid reaches
//! normal Tool dispatch; callback rows are never globally sorted.

use std::path::Path;
use std::process::Command;

use reverie_dbt::DbtRunner;
use reverie_dbt::counter::SyscallCounterGlobal;

fn compile_fixture(output: &Path) {
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/process_clone_results.c");
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let status = Command::new(compiler)
        .args(["-O2", "-g", "-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(source)
        .arg("-o")
        .arg(output)
        .status()
        .expect("compile process-clone fixture");
    assert!(status.success(), "fixture compilation failed");
}

const CALL_PREFIX: &str = "reverie-dbt-test: process-clone-call ";
const RESULT_PREFIX: &str = "reverie-dbt-test: process-clone-result ";
const CAUSAL_PREFIX: &str = "reverie-dbt-test: process-clone-causal ";
const TOOL_ENTRY_PREFIX: &str = "reverie-dbt-test: process-clone-tool-entry ";

#[derive(Clone, Debug, Eq, PartialEq)]
struct CallbackResult {
    sequence: usize,
    pid: i64,
    sysnum: i64,
    result: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CallBlock {
    id: usize,
    operation: String,
    callbacks: Vec<CallbackResult>,
    causal: Vec<CausalResult>,
    tool_entries: Vec<ToolEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CausalResult {
    sequence: usize,
    pid: i64,
    sysnum: i64,
    result: i64,
    delivery: String,
    next_sysnum: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ToolEntry {
    sequence: usize,
    pid: i64,
    clone_sysnum: i64,
    sysnum: i64,
}

#[derive(Clone, Copy)]
enum ExpectedResults {
    None,
    Error(i64),
    ParentChild,
    ParentOnly,
}

#[derive(Clone, Copy)]
struct ExpectedCall {
    id: usize,
    operation: &'static str,
    sysnum: i64,
    results: ExpectedResults,
}

fn parse_number<T>(field: &str, prefix: &str, row: &str) -> Result<T, String>
where
    T: std::str::FromStr,
{
    field
        .strip_prefix(prefix)
        .ok_or_else(|| format!("field must use {prefix}<integer>: {row}"))?
        .parse()
        .map_err(|_| format!("field must contain an integer: {row}"))
}

fn parse_callback_stream(stderr: &[u8]) -> Result<Vec<CallBlock>, String> {
    let mut blocks = Vec::new();
    let mut current: Option<CallBlock> = None;

    for (sequence, line) in String::from_utf8_lossy(stderr).lines().enumerate() {
        if let Some(fields) = line.strip_prefix(CALL_PREFIX) {
            let fields: Vec<_> = fields.split_whitespace().collect();
            if fields.len() != 3 {
                return Err(format!(
                    "process-clone call marker must contain exactly three fields: {line}"
                ));
            }
            let phase = fields[0]
                .strip_prefix("phase=")
                .ok_or_else(|| format!("call marker must contain phase=<value>: {line}"))?;
            let id = parse_number(fields[1], "id=", line)?;
            let operation = fields[2]
                .strip_prefix("op=")
                .ok_or_else(|| format!("call marker must contain op=<value>: {line}"))?;

            match phase {
                "begin" => {
                    if let Some(open) = &current {
                        return Err(format!(
                            "call {id}/{operation} began inside open call {open:?}"
                        ));
                    }
                    current = Some(CallBlock {
                        id,
                        operation: operation.to_owned(),
                        callbacks: Vec::new(),
                        causal: Vec::new(),
                        tool_entries: Vec::new(),
                    });
                }
                "end" => {
                    let open = current.take().ok_or_else(|| {
                        format!("call {id}/{operation} ended without a begin marker")
                    })?;
                    if open.id != id || open.operation != operation {
                        return Err(format!(
                            "call end {id}/{operation} does not match open call {open:?}"
                        ));
                    }
                    blocks.push(open);
                }
                _ => return Err(format!("unknown call-marker phase {phase:?}: {line}")),
            }
        } else if let Some(fields) = line.strip_prefix(RESULT_PREFIX) {
            let fields: Vec<_> = fields.split_whitespace().collect();
            if fields.len() != 3 {
                return Err(format!(
                    "clone-result row must contain exactly three fields: {line}"
                ));
            }
            let callback = CallbackResult {
                sequence,
                pid: parse_number(fields[0], "pid=", line)?,
                sysnum: parse_number(fields[1], "sysnum=", line)?,
                result: parse_number(fields[2], "result=", line)?,
            };
            current
                .as_mut()
                .ok_or_else(|| format!("clone-result row appeared outside a call block: {line}"))?
                .callbacks
                .push(callback);
        } else if let Some(fields) = line.strip_prefix(CAUSAL_PREFIX) {
            let fields: Vec<_> = fields.split_whitespace().collect();
            if fields.len() != 5 {
                return Err(format!(
                    "clone-result causal row must contain exactly five fields: {line}"
                ));
            }
            let delivery = fields[3]
                .strip_prefix("delivery=")
                .ok_or_else(|| format!("causal row must contain delivery=<site>: {line}"))?;
            current
                .as_mut()
                .ok_or_else(|| format!("causal row appeared outside a call block: {line}"))?
                .causal
                .push(CausalResult {
                    sequence,
                    pid: parse_number(fields[0], "pid=", line)?,
                    sysnum: parse_number(fields[1], "sysnum=", line)?,
                    result: parse_number(fields[2], "result=", line)?,
                    delivery: delivery.to_owned(),
                    next_sysnum: parse_number(fields[4], "next_sysnum=", line)?,
                });
        } else if let Some(fields) = line.strip_prefix(TOOL_ENTRY_PREFIX) {
            let fields: Vec<_> = fields.split_whitespace().collect();
            if fields.len() != 3 {
                return Err(format!(
                    "clone-result Tool-entry row must contain exactly three fields: {line}"
                ));
            }
            current
                .as_mut()
                .ok_or_else(|| format!("Tool-entry row appeared outside a call block: {line}"))?
                .tool_entries
                .push(ToolEntry {
                    sequence,
                    pid: parse_number(fields[0], "pid=", line)?,
                    clone_sysnum: parse_number(fields[1], "clone_sysnum=", line)?,
                    sysnum: parse_number(fields[2], "sysnum=", line)?,
                });
        } else if line.starts_with("reverie-dbt-test: process-clone-") {
            return Err(format!("unrecognized process-clone probe row: {line}"));
        }
    }

    if let Some(open) = current {
        return Err(format!("call block did not end: {open:?}"));
    }
    Ok(blocks)
}

fn expected_calls() -> [ExpectedCall; 8] {
    [
        ExpectedCall {
            id: 1,
            operation: "invalid-clone-1",
            sysnum: libc::SYS_clone,
            results: ExpectedResults::Error(-libc::EINVAL as i64),
        },
        ExpectedCall {
            id: 2,
            operation: "malformed-clone3",
            sysnum: libc::SYS_clone3,
            results: ExpectedResults::None,
        },
        ExpectedCall {
            id: 3,
            operation: "invalid-clone-2",
            sysnum: libc::SYS_clone,
            results: ExpectedResults::Error(-libc::EINVAL as i64),
        },
        ExpectedCall {
            id: 4,
            operation: "fork",
            sysnum: libc::SYS_fork,
            results: ExpectedResults::ParentChild,
        },
        ExpectedCall {
            id: 5,
            operation: "clone",
            sysnum: libc::SYS_clone,
            results: ExpectedResults::ParentChild,
        },
        ExpectedCall {
            id: 6,
            operation: "clone-vm",
            sysnum: libc::SYS_clone,
            results: ExpectedResults::ParentOnly,
        },
        ExpectedCall {
            id: 7,
            operation: "clone3",
            sysnum: libc::SYS_clone3,
            results: ExpectedResults::ParentChild,
        },
        ExpectedCall {
            id: 8,
            operation: "vfork",
            sysnum: libc::SYS_vfork,
            results: ExpectedResults::ParentOnly,
        },
    ]
}

fn validate_callback_contract(blocks: &[CallBlock]) -> Result<(), String> {
    let expected = expected_calls();
    if blocks.len() != expected.len() {
        return Err(format!(
            "callback stream must contain exactly eight ordered call blocks: {blocks:?}"
        ));
    }
    let callback_count: usize = blocks.iter().map(|block| block.callbacks.len()).sum();
    if callback_count != 10 {
        return Err(format!(
            "callback stream must contain exactly ten callback rows, got {callback_count}: {blocks:?}"
        ));
    }

    for (block, expected) in blocks.iter().zip(expected) {
        if block.id != expected.id || block.operation != expected.operation {
            return Err(format!(
                "call block order/identity mismatch: expected {}/{}, got {block:?}",
                expected.id, expected.operation
            ));
        }
    }

    let first = blocks[0]
        .callbacks
        .first()
        .ok_or_else(|| "invalid-clone-1 emitted no callback".to_owned())?;
    if first.pid <= 0 {
        return Err(format!(
            "root callback emitter PID must be positive: {first:?}"
        ));
    }
    let root_pid = first.pid;

    for (block, expected) in blocks.iter().zip(expected) {
        if block
            .callbacks
            .iter()
            .any(|callback| callback.sysnum != expected.sysnum)
        {
            return Err(format!(
                "call {}/{} contains a callback for the wrong syscall {}: {:?}",
                block.id, expected.operation, expected.sysnum, block.callbacks
            ));
        }

        match expected.results {
            ExpectedResults::None => {
                if !block.callbacks.is_empty() {
                    return Err(format!(
                        "call {}/{} must emit no callbacks: {:?}",
                        block.id, expected.operation, block.callbacks
                    ));
                }
            }
            ExpectedResults::Error(error) => {
                if block.callbacks.len() != 1
                    || block.callbacks[0].pid != root_pid
                    || block.callbacks[0].result != error
                {
                    return Err(format!(
                        "call {}/{} must emit one root error {error}: {:?}",
                        block.id, expected.operation, block.callbacks
                    ));
                }
            }
            ExpectedResults::ParentOnly => {
                if block.callbacks.len() != 1
                    || block.callbacks[0].pid != root_pid
                    || block.callbacks[0].result <= 0
                {
                    return Err(format!(
                        "call {}/{} must emit one root parent-positive callback: {:?}",
                        block.id, expected.operation, block.callbacks
                    ));
                }
            }
            ExpectedResults::ParentChild => {
                if block.callbacks.len() != 2 {
                    return Err(format!(
                        "call {}/{} must emit exactly one parent and one child callback: {:?}",
                        block.id, expected.operation, block.callbacks
                    ));
                }
                let parents: Vec<_> = block
                    .callbacks
                    .iter()
                    .filter(|callback| callback.result > 0)
                    .collect();
                let children: Vec<_> = block
                    .callbacks
                    .iter()
                    .filter(|callback| callback.result == 0)
                    .collect();
                if parents.len() != 1 || children.len() != 1 {
                    return Err(format!(
                        "call {}/{} must emit one parent-positive and one child-zero callback: {:?}",
                        block.id, expected.operation, block.callbacks
                    ));
                }
                let parent = parents[0];
                let child = children[0];
                if parent.pid != root_pid {
                    return Err(format!(
                        "call {}/{} parent callback came from pid {}, expected root pid {root_pid}",
                        block.id, expected.operation, parent.pid
                    ));
                }
                if child.pid != parent.result {
                    return Err(format!(
                        "call {}/{} child emitter pid {} does not match raw parent result {}",
                        block.id, expected.operation, child.pid, parent.result
                    ));
                }
            }
        }
    }

    Ok(())
}

fn validate_causal_contract(blocks: &[CallBlock]) -> Result<(), String> {
    validate_callback_contract(blocks)?;
    let causal_count: usize = blocks.iter().map(|block| block.causal.len()).sum();
    if causal_count != 10 {
        return Err(format!(
            "causal stream must contain exactly ten callback-site rows, got {causal_count}: {blocks:?}"
        ));
    }

    for (block, expected) in blocks.iter().zip(expected_calls()) {
        if block.causal.len() != block.callbacks.len() {
            return Err(format!(
                "call {}/{} must pair every callback with one causal row: {block:?}",
                block.id, block.operation
            ));
        }
        for callback in &block.callbacks {
            let matches: Vec<_> = block
                .causal
                .iter()
                .filter(|causal| {
                    causal.pid == callback.pid
                        && causal.sysnum == callback.sysnum
                        && causal.result == callback.result
                })
                .collect();
            if matches.len() != 1 || matches[0].sequence <= callback.sequence {
                return Err(format!(
                    "call {}/{} did not causally pair callback {callback:?}: {block:?}",
                    block.id, block.operation
                ));
            }
            let expected_delivery = if callback.result == 0 {
                "child-pre"
            } else {
                "parent-post"
            };
            if matches[0].delivery != expected_delivery {
                return Err(format!(
                    "call {}/{} used the wrong delivery site for {callback:?}: {:?}",
                    block.id, block.operation, matches[0]
                ));
            }
        }

        match expected.results {
            ExpectedResults::ParentChild => {
                let child = block
                    .causal
                    .iter()
                    .find(|causal| causal.result == 0)
                    .ok_or_else(|| {
                        format!(
                            "call {}/{} has no child causal row",
                            block.id, block.operation
                        )
                    })?;
                if child.next_sysnum != libc::SYS_getuid {
                    return Err(format!(
                        "call {}/{} child callback was not delivered at its first getuid: {child:?}",
                        block.id, block.operation
                    ));
                }
                if block.tool_entries.len() != 1 {
                    return Err(format!(
                        "call {}/{} must have exactly one first-child Tool entry: {block:?}",
                        block.id, block.operation
                    ));
                }
                let entry = &block.tool_entries[0];
                if entry.pid != child.pid
                    || entry.clone_sysnum != expected.sysnum
                    || entry.sysnum != libc::SYS_getuid
                    || entry.sequence <= child.sequence
                {
                    return Err(format!(
                        "call {}/{} child callback must precede the matching first Tool syscall: child={child:?} entry={entry:?}",
                        block.id, block.operation
                    ));
                }
            }
            ExpectedResults::None | ExpectedResults::Error(_) | ExpectedResults::ParentOnly => {
                if !block.tool_entries.is_empty() {
                    return Err(format!(
                        "call {}/{} must not report a child Tool entry: {block:?}",
                        block.id, block.operation
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
fn callback_line(pid: i64, sysnum: i64, result: i64) -> String {
    format!("reverie-dbt-test: process-clone-result pid={pid} sysnum={sysnum} result={result}")
}

#[cfg(test)]
fn append_call(lines: &mut Vec<String>, id: usize, operation: &str, callbacks: Vec<String>) {
    lines.push(format!(
        "reverie-dbt-test: process-clone-call phase=begin id={id} op={operation}"
    ));
    lines.extend(callbacks);
    lines.push(format!(
        "reverie-dbt-test: process-clone-call phase=end id={id} op={operation}"
    ));
}

#[cfg(test)]
fn callback_pair(sysnum: i64, child_pid: i64, child_first: bool) -> Vec<String> {
    let parent = callback_line(1000, sysnum, child_pid);
    let child = callback_line(child_pid, sysnum, 0);
    if child_first {
        vec![child, parent]
    } else {
        vec![parent, child]
    }
}

#[cfg(test)]
fn synthetic_callback_stream(child_first: bool) -> String {
    let mut lines = Vec::new();

    append_call(
        &mut lines,
        1,
        "invalid-clone-1",
        vec![callback_line(1000, libc::SYS_clone, -libc::EINVAL as i64)],
    );
    append_call(&mut lines, 2, "malformed-clone3", Vec::new());
    append_call(
        &mut lines,
        3,
        "invalid-clone-2",
        vec![callback_line(1000, libc::SYS_clone, -libc::EINVAL as i64)],
    );
    append_call(
        &mut lines,
        4,
        "fork",
        callback_pair(libc::SYS_fork, 2001, child_first),
    );
    append_call(
        &mut lines,
        5,
        "clone",
        callback_pair(libc::SYS_clone, 2002, child_first),
    );
    append_call(
        &mut lines,
        6,
        "clone-vm",
        vec![callback_line(1000, libc::SYS_clone, 2003)],
    );
    append_call(
        &mut lines,
        7,
        "clone3",
        callback_pair(libc::SYS_clone3, 2004, child_first),
    );
    append_call(
        &mut lines,
        8,
        "vfork",
        vec![callback_line(1000, libc::SYS_vfork, 2005)],
    );
    lines.join("\n") + "\n"
}

fn validate_stream(stderr: &[u8]) -> Result<(), String> {
    let blocks = parse_callback_stream(stderr)?;
    validate_callback_contract(&blocks)
}

#[test]
fn malformed_callback_rows_fail_closed() {
    let stderr = b"reverie-dbt-test: process-clone-call phase=begin id=1 op=invalid-clone-1\n\
reverie-dbt-test: process-clone-result pid=1000 sysnum=56\n\
reverie-dbt-test: process-clone-call phase=end id=1 op=invalid-clone-1\n";
    assert!(parse_callback_stream(stderr).is_err());
}

#[test]
fn callback_rows_outside_call_blocks_fail_closed() {
    let stderr = b"reverie-dbt-test: process-clone-result pid=1000 sysnum=56 result=-22\n";
    assert!(parse_callback_stream(stderr).is_err());
}

#[test]
fn causal_rows_preserve_stream_order_without_sorting() {
    let stderr = format!(
        "{CALL_PREFIX}phase=begin id=4 op=fork\n\
         {RESULT_PREFIX}pid=2001 sysnum={} result=0\n\
         {CAUSAL_PREFIX}pid=2001 sysnum={} result=0 delivery=child-pre next_sysnum={}\n\
         {TOOL_ENTRY_PREFIX}pid=2001 clone_sysnum={} sysnum={}\n\
         {RESULT_PREFIX}pid=1000 sysnum={} result=2001\n\
         {CAUSAL_PREFIX}pid=1000 sysnum={} result=2001 delivery=parent-post next_sysnum={}\n\
         {CALL_PREFIX}phase=end id=4 op=fork\n",
        libc::SYS_fork,
        libc::SYS_fork,
        libc::SYS_getuid,
        libc::SYS_fork,
        libc::SYS_getuid,
        libc::SYS_fork,
        libc::SYS_fork,
        libc::SYS_wait4,
    );
    let blocks = parse_callback_stream(stderr.as_bytes()).expect("parse causal fixture");
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].callbacks.len(), 2);
    assert_eq!(blocks[0].causal.len(), 2);
    assert_eq!(blocks[0].tool_entries.len(), 1);
    assert!(blocks[0].callbacks[0].sequence < blocks[0].causal[0].sequence);
    assert!(blocks[0].causal[0].sequence < blocks[0].tool_entries[0].sequence);
    assert!(blocks[0].tool_entries[0].sequence < blocks[0].callbacks[1].sequence);
}

#[test]
fn ordered_call_envelopes_accept_either_local_parent_child_order() {
    assert!(validate_stream(synthetic_callback_stream(false).as_bytes()).is_ok());
    assert!(validate_stream(synthetic_callback_stream(true).as_bytes()).is_ok());
}

#[test]
fn missing_child_callback_fails_contract() {
    let child = callback_line(2001, libc::SYS_fork, 0) + "\n";
    let stream = synthetic_callback_stream(false).replace(&child, "");
    assert!(validate_stream(stream.as_bytes()).is_err());
}

#[test]
fn duplicate_parent_replacing_child_fails_contract() {
    let child = callback_line(2001, libc::SYS_fork, 0);
    let parent = callback_line(1000, libc::SYS_fork, 2001);
    let stream = synthetic_callback_stream(false).replace(&child, &parent);
    assert!(validate_stream(stream.as_bytes()).is_err());
}

#[test]
fn wrong_syscall_call_attribution_fails_contract() {
    let child = callback_line(2001, libc::SYS_fork, 0);
    let wrong = callback_line(2001, libc::SYS_clone, 0);
    let stream = synthetic_callback_stream(false).replace(&child, &wrong);
    assert!(validate_stream(stream.as_bytes()).is_err());
}

#[test]
fn virtualized_or_nonmatching_parent_result_fails_contract() {
    let parent = callback_line(1000, libc::SYS_fork, 2001);
    let virtualized = callback_line(1000, libc::SYS_fork, 1);
    let stream = synthetic_callback_stream(false).replace(&parent, &virtualized);
    assert!(validate_stream(stream.as_bytes()).is_err());
}

#[test]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
fn process_clone_result_delivery_matches_the_public_contract() {
    let directory = tempfile::tempdir().expect("fixture tempdir");
    let fixture = directory.path().join("process-clone-results");
    compile_fixture(&fixture);

    let runner = DbtRunner::from_env()
        .expect("DYNAMORIO_HOME (or DynamoRIO_DIR) and REVERIE_DBT_CLIENT must be set")
        .client_argument("-test-wait-for-background");
    let mut guest = Command::new(fixture);
    guest.env("REVERIE_DBT_TEST_PROCESS_CLONE_RESULTS", "1");
    let output = runner
        .output(&guest)
        .expect("process-clone matrix must run");
    assert!(output.status.success(), "guest failed: {output:?}");
    assert_eq!(output.stdout, b"process-clone-results-ok\n");
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("stale process-clone result state"),
        "clone-result state leaked past its post-event: {output:?}"
    );

    validate_stream(&output.stderr).unwrap_or_else(|error| {
        panic!("process-clone callback contract failed: {error}; {output:?}")
    });
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires a built DynamoRIO and the reverie-dbt native client; run explicitly with --ignored"]
async fn child_callback_precedes_the_first_child_tool_syscall() {
    let directory = tempfile::tempdir().expect("fixture tempdir");
    let fixture = directory.path().join("process-clone-results-causal");
    compile_fixture(&fixture);

    let runner = DbtRunner::from_env()
        .expect("DYNAMORIO_HOME (or DynamoRIO_DIR) and REVERIE_DBT_CLIENT must be set")
        .client_argument("-test-wait-for-background");
    let mut guest = Command::new(fixture);
    guest
        .env("REVERIE_DBT_TEST_PROCESS_CLONE_RESULTS", "1")
        .env("REVERIE_DBT_TEST_PROCESS_CLONE_CAUSAL", "1")
        .env("HERMIT_DBT_SYSCALL_HISTOGRAM", "1");
    let (output, _) = runner
        .output_with_global::<SyscallCounterGlobal>(&guest, ())
        .await
        .expect("coordinated process-clone causal matrix must run");
    assert!(output.status.success(), "guest failed: {output:?}");
    assert_eq!(output.stdout, b"process-clone-results-ok\n");
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("stale process-clone result state"),
        "clone-result state leaked into a later syscall: {output:?}"
    );

    let blocks = parse_callback_stream(&output.stderr)
        .unwrap_or_else(|error| panic!("causal stream parse failed: {error}; {output:?}"));
    validate_causal_contract(&blocks)
        .unwrap_or_else(|error| panic!("causal callback contract failed: {error}; {output:?}"));
}
