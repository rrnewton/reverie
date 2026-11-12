/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Compile the production capture-refusal helpers with identity and EXIT stubs.
//! This checks the native callback latch and terminal leaf, not DR scheduling or
//! controller Pending ownership; the live mapping-capture tests exercise those.

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::path::Path;
use std::process::Command;
use std::process::Output;

const CLIENT: &str = include_str!("../native/client.c");
const CAPTURE: &str = include_str!("../native/mapping_capture.h");

fn production_helpers() -> String {
    let end = CLIENT
        .find("} mapping_capture_refusal_t;")
        .expect("actual refusal type")
        + "} mapping_capture_refusal_t;".len();
    let start = CLIENT[..end]
        .rfind("typedef struct {")
        .expect("actual refusal type start");
    let helpers = CAPTURE
        .split_once("static bool mapping_capture_active(void) {")
        .expect("actual capture-active helper")
        .1
        .split_once("static void mapping_capture_withdraw(void) {")
        .expect("end of actual fatal helpers")
        .0;
    format!(
        "{PRELUDE}\n{}\n{ENVIRONMENT}\nstatic bool mapping_capture_active(void) {{{helpers}\n{CASES}",
        &CLIENT[start..end],
    )
}

fn bounded(command: &mut Command) -> Output {
    Command::new("timeout")
        .args(["--signal=TERM", "--kill-after=2s", "10s"])
        .arg(command.get_program())
        .args(command.get_args())
        .output()
        .unwrap_or_else(|error| panic!("start {command:?}: {error}"))
}

fn retain(directory: &Path, name: &str, output: &Output) {
    std::fs::write(directory.join(format!("{name}.stdout")), &output.stdout)
        .expect("retain actual stdout");
    std::fs::write(directory.join(format!("{name}.stderr")), &output.stderr)
        .expect("retain actual stderr");
    std::fs::write(
        directory.join(format!("{name}.status")),
        format!("{:?}\n", output.status),
    )
    .expect("retain actual status");
}

fn compile(directory: &Path, name: &str, source: &str) -> std::path::PathBuf {
    let source_path = directory.join(format!("{name}.c"));
    let executable = directory.join(name);
    std::fs::write(&source_path, source).expect("retain actual helper translation unit");
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let output = bounded(
        Command::new(compiler)
            .args(["-std=c11", "-O2", "-Wall", "-Wextra", "-Werror"])
            .arg(&source_path)
            .arg("-o")
            .arg(&executable),
    );
    retain(directory, &format!("{name}-compile"), &output);
    assert!(
        output.status.success(),
        "compile {source_path:?}: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    executable
}

fn run(directory: &Path, executable: &Path, case: &str) -> Output {
    let output = bounded(Command::new(executable).arg(case));
    retain(
        directory,
        &format!(
            "{}-{case}",
            executable.file_name().unwrap().to_str().unwrap()
        ),
        &output,
    );
    output
}

fn check(output: &Output, status: i32, stdout: &[u8]) {
    assert_eq!(
        (
            output.status.code(),
            output.stdout.as_slice(),
            output.stderr.as_slice()
        ),
        (Some(status), stdout, b"".as_slice()),
        "actual native helper output: {output:?}",
    );
}

fn artifact_directory() -> std::path::PathBuf {
    let parent = option_env!("CARGO_TARGET_TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    for attempt in 0..128 {
        let directory = parent.join(format!(
            "mapping-capture-fatal-{}-{attempt}",
            std::process::id()
        ));
        match std::fs::create_dir(&directory) {
            Ok(()) => return directory,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("create {directory:?}: {error}"),
        }
    }
    panic!("no unused native capture helper directory under {parent:?}");
}

#[test]
fn withdrawn_capture_refusal_is_bound_to_the_returned_fatal_callback() {
    let directory = artifact_directory();
    eprintln!("mapping capture fatal artifacts: {}", directory.display());
    let source = production_helpers();
    let executable = compile(&directory, "intact", &source);
    for case in ["fatal-idle", "fatal-restored"] {
        check(
            &run(&directory, &executable, case),
            101,
            b"terminal status=101 state=6 calls=1\n",
        );
    }
    for case in ["record-refusals", "take-refusals", "clear"] {
        check(
            &run(&directory, &executable, case),
            0,
            b"controls completed\n",
        );
    }
    check(
        &run(&directory, &executable, "active-terminal"),
        125,
        b"terminal status=125 state=6 calls=1\n",
    );

    // Recreate the old missing-latch behavior in the actual helper definition.
    // A compile error, timeout, or arbitrary crash is not this negative result.
    let signature = "static void mapping_capture_record_withdrawn_refusal(";
    let start = source.find(signature).expect("actual record helper");
    let end = source[start..]
        .find("static bool mapping_capture_take_fatal_refusal(")
        .map(|offset| start + offset)
        .expect("actual following take helper");
    let mut mutant = source.clone();
    mutant.replace_range(
        start..end,
        "static void mapping_capture_record_withdrawn_refusal(\n\
         uintptr_t context, uint32_t operation, mapping_capture_arguments_t* args) {\n\
         (void)context; (void)operation; (void)args;\n}\n\n",
    );
    let mutant = compile(&directory, "missing-record", &mutant);
    let mutant_output = run(&directory, &mutant, "fatal-idle");
    check(&mutant_output, 91, b"fatal callback continued\n");
    assert!(
        std::panic::catch_unwind(|| {
            check(
                &mutant_output,
                101,
                b"terminal status=101 state=6 calls=1\n",
            );
        })
        .is_err(),
        "the intact fatal oracle must reject the compiled missing-record mutation",
    );
    std::fs::remove_dir_all(directory).expect("remove successful helper fixtures");
}

const PRELUDE: &str = r#"
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdatomic.h>
#include <string.h>

enum { MAPPING_IDLE, MAPPING_REQUESTED, MAPPING_ENTERING, MAPPING_PARKED,
       MAPPING_RELEASE_REQUESTED, MAPPING_RESTORED, MAPPING_FAILED };
enum { DR_MAPPING_CAPTURE_EXIT = 1 };
"#;

const ENVIRONMENT: &str = r#"
/* Only the environment and leaf are stubbed. The latch/terminal code below
 * is extracted verbatim from the two actual native production files. */
typedef struct {
  bool mapping_capture_callback_active;
  mapping_capture_refusal_t mapping_capture_refusal;
} prototype_counters_t;
typedef struct { int32_t root_pid; uint64_t nonce; } mapping_capture_arguments_t;
typedef struct {
  void* application_context;
  uint64_t image, last_nonce;
  _Atomic bool eligible, transaction_active;
  _Atomic uint32_t state;
} mapping_capture_slot_t;

static prototype_counters_t counters, other_counters;
static prototype_counters_t* tls_counters;
static mapping_capture_slot_t slot;
static mapping_capture_slot_t* mapping_capture_slot;
static int context_object, foreign_context_object;
static void* current_context;
static int32_t physical_pid, physical_tid, runtime_process_id;
static bool runtime_is_root;
static uint64_t runtime_process_epoch;
static _Atomic int runtime_background_state;
static unsigned native_runtime_entries, terminal_calls;
static const int thread_state_index = 3;

static void* dr_get_current_drcontext(void) { return current_context; }
static int32_t dr_get_process_id(void) { return physical_pid; }
static int32_t dr_get_thread_id(void* context) {
  return context == current_context ? physical_tid : -1;
}
static void* drmgr_get_tls_field(void* context, int index) {
  assert(index == thread_state_index);
  return context == current_context ? tls_counters : NULL;
}
static intptr_t dr_mapping_capture_control(int operation, uintptr_t a,
                                           uintptr_t b, uintptr_t c) {
  assert(operation == DR_MAPPING_CAPTURE_EXIT && b == 0 && c == 0);
  assert(++terminal_calls == 1);
  assert(mapping_capture_slot == &slot);
  assert(atomic_load(&slot.state) == MAPPING_FAILED);
  printf("terminal status=%lu state=%u calls=%u\n", (unsigned long)a,
         atomic_load(&slot.state), terminal_calls);
  assert(fflush(stdout) == 0);
  _Exit((int)a);
}
"#;

const CASES: &str = r#"
static uintptr_t context(void) { return (uintptr_t)&context_object; }
static uintptr_t foreign_context(void) { return (uintptr_t)&foreign_context_object; }
static mapping_capture_arguments_t arguments(void) {
  return (mapping_capture_arguments_t){ .root_pid = 41, .nonce = 8 };
}
static void reset(void) {
  memset(&counters, 0, sizeof(counters));
  memset(&other_counters, 0, sizeof(other_counters));
  memset(&slot, 0, sizeof(slot));
  tls_counters = &counters;
  mapping_capture_slot = &slot;
  current_context = &context_object;
  physical_pid = physical_tid = runtime_process_id = 41;
  runtime_is_root = true;
  runtime_process_epoch = slot.image = 19;
  slot.application_context = &context_object;
  slot.last_nonce = 7;
  atomic_init(&slot.eligible, false);
  atomic_init(&slot.transaction_active, false);
  atomic_init(&slot.state, MAPPING_IDLE);
  atomic_store(&runtime_background_state, 2);
  native_runtime_entries = terminal_calls = 0;
  mapping_capture_begin_callback(&counters);
}
static void assert_cleared(void) {
  mapping_capture_refusal_t* refusal = &counters.mapping_capture_refusal;
  assert(!counters.mapping_capture_callback_active);
  assert(refusal->context == 0 && refusal->pid == 0 && refusal->tid == 0);
  assert(refusal->image == 0 && refusal->nonce == 0);
  assert(terminal_calls == 0);
}
static void record_refusals(void) {
  for (unsigned row = 0; row != 19; ++row) {
    reset();
    mapping_capture_arguments_t args = arguments();
    mapping_capture_arguments_t* input = &args;
    uintptr_t callback_context = context();
    uint32_t operation = 0;
    switch (row) {
      case 0: operation = 1; break;
      case 1: mapping_capture_slot = NULL; break;
      case 2: input = NULL; break;
      case 3: callback_context = foreign_context(); break;
      case 4: current_context = &foreign_context_object; break;
      case 5: runtime_is_root = false; break;
      case 6: ++physical_pid; break;
      case 7: ++physical_tid; break;
      case 8: ++runtime_process_epoch; break;
      case 9: ++args.root_pid; break;
      case 10: args.nonce = 0; break;
      case 11: args.nonce = slot.last_nonce; break;
      case 12: atomic_store(&slot.eligible, true); break;
      case 13: atomic_store(&slot.transaction_active, true); break;
      case 14: atomic_store(&runtime_background_state, 1); break;
      case 15: atomic_store(&slot.state, MAPPING_PARKED); break;
      case 16: counters.mapping_capture_callback_active = false; break;
      case 17: tls_counters = NULL; break;
      case 18: slot.application_context = &foreign_context_object; break;
      default: abort();
    }
    mapping_capture_record_withdrawn_refusal(callback_context, operation, input);
    assert(counters.mapping_capture_refusal.nonce == 0);
    assert(!mapping_capture_take_fatal_refusal(&counters, context(), -1));
    assert_cleared();
  }
}
static void take_refusals(void) {
  for (unsigned row = 0; row != 14; ++row) {
    reset();
    mapping_capture_arguments_t args = arguments();
    mapping_capture_record_withdrawn_refusal(context(), 0, &args);
    assert(counters.mapping_capture_refusal.nonce == args.nonce);
    uintptr_t callback_context = context();
    switch (row) {
      case 0: callback_context = foreign_context(); break;
      case 1: current_context = &foreign_context_object; break;
      case 2: ++physical_pid; break;
      case 3: ++physical_tid; break;
      case 4: ++runtime_process_id; break;
      case 5: runtime_is_root = false; break;
      case 6: ++runtime_process_epoch; break;
      case 7: ++slot.image; break;
      case 8: mapping_capture_slot = NULL; break;
      case 9: atomic_store(&slot.transaction_active, true); break;
      case 10: native_runtime_entries = 1; break;
      case 11: tls_counters = &other_counters; break;
      case 12: slot.application_context = &foreign_context_object; break;
      case 13: counters.mapping_capture_refusal.nonce = 0; break;
      default: abort();
    }
    assert(!mapping_capture_take_fatal_refusal(&counters, callback_context, -1));
    assert_cleared();
  }
}
static void clear_controls(void) {
  const int32_t actions[] = { 0, 1, 2, -2, 3 };
  for (unsigned i = 0; i != sizeof(actions) / sizeof(actions[0]); ++i) {
    reset();
    mapping_capture_arguments_t args = arguments();
    mapping_capture_record_withdrawn_refusal(context(), 0, &args);
    assert(counters.mapping_capture_refusal.nonce == args.nonce);
    assert(!mapping_capture_take_fatal_refusal(&counters, context(), actions[i]));
    assert_cleared();
    mapping_capture_begin_callback(&counters);
    assert(!mapping_capture_take_fatal_refusal(&counters, context(), -1));
    assert_cleared();
  }
  reset();
  mapping_capture_arguments_t args = arguments();
  mapping_capture_record_withdrawn_refusal(context(), 0, &args);
  mapping_capture_begin_callback(&counters);
  assert(!mapping_capture_take_fatal_refusal(&counters, context(), -1));
  assert_cleared();
}
int main(int argc, char** argv) {
  assert(argc == 2);
  reset();
  if (strcmp(argv[1], "fatal-idle") == 0 || strcmp(argv[1], "fatal-restored") == 0) {
    if (strcmp(argv[1], "fatal-restored") == 0)
      atomic_store(&slot.state, MAPPING_RESTORED);
    mapping_capture_arguments_t args = arguments();
    mapping_capture_record_withdrawn_refusal(context(), 0, &args);
    if (mapping_capture_take_fatal_refusal(&counters, context(), -1)) {
      assert_cleared();
      mapping_capture_terminal_with_status(101);
    }
    puts("fatal callback continued");
    return 91;
  }
  if (strcmp(argv[1], "active-terminal") == 0) {
    atomic_store(&slot.transaction_active, true);
    atomic_store(&slot.state, MAPPING_PARKED);
    mapping_capture_terminal();
    return 92;
  }
  if (strcmp(argv[1], "record-refusals") == 0)
    record_refusals();
  else if (strcmp(argv[1], "take-refusals") == 0)
    take_refusals();
  else if (strcmp(argv[1], "clear") == 0)
    clear_controls();
  else
    return 93;
  puts("controls completed");
  return 0;
}
"#;
