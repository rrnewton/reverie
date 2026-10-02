/* Copyright (c) Meta Platforms, Inc. and affiliates.
 * Licensed under the BSD-style license in the root LICENSE file. */

use std::env;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

const SOURCES: [&str; 6] = [
    "tests/source-observation-equivalence.c",
    "tests/fixtures/clone3_join.c",
    "tests/private_preentry_signal.c",
    "tests/private-replay-transport.c",
    "tests/private-continuation.c",
    "tests/private-timer-wrapper.c",
];

fn run(command: &mut Command) {
    let status = command
        .status()
        .unwrap_or_else(|error| panic!("native fixture command {command:?}: {error}"));
    assert!(
        status.success(),
        "native fixture command {command:?}: {status}"
    );
}

fn compile(tool: &cc::Tool, source: &str, output: &Path, renamed_main: bool) {
    let mut command = tool.to_command();
    // Keep the native fixture's assertions and instruction-level test premises
    // even when the surrounding Cargo profile or CFLAGS requests optimization.
    command.args([
        "-std=gnu11",
        "-O0",
        "-g",
        "-Wall",
        "-Wextra",
        "-Werror",
        "-UNDEBUG",
    ]);
    if renamed_main {
        command.arg("-Dmain=continuation_original_main");
    }
    command.arg("-c").arg(source).arg("-o").arg(output);
    run(&mut command);
}

fn link(tool: &cc::Tool, objects: &[PathBuf], output: &Path) {
    let mut command = tool.to_command();
    command.args(objects).arg("-o").arg(output);
    run(&mut command);
}

fn main() {
    for source in SOURCES {
        println!("cargo:rerun-if-changed={source}");
    }
    // These fixture programs use Linux x86-64 signal frames and instructions.
    // Build scripts run on HOST, including during non-test builds; use TARGET.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux")
        || env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("x86_64")
    {
        return;
    }

    println!("cargo:rerun-if-env-changed=CC_FORCE_DISABLE");
    let disabled = env::var_os("CC_FORCE_DISABLE").is_some_and(|value| {
        !value.is_empty() && value != "0" && value != "false" && value != "no"
    });
    assert!(
        !disabled,
        "native fixture compilation was disabled by CC_FORCE_DISABLE"
    );

    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must set OUT_DIR"));
    // get_compiler selects the target tool and emits its compiler-environment
    // change inputs. We build executables, not a library linked into Reverie.
    let tool = cc::Build::new()
        .opt_level(0)
        .debug(true)
        .try_get_compiler()
        .expect("a target C compiler is required for native ptrace fixtures");
    assert!(
        tool.is_like_gnu() || tool.is_like_clang(),
        "fixtures require a GNU-compatible target C compiler"
    );

    for (name, source) in [
        ("source-observation", SOURCES[0]),
        ("clone3-join", SOURCES[1]),
        ("private-signal", SOURCES[2]),
        ("private-replay", SOURCES[3]),
        ("private-continuation", SOURCES[4]),
    ] {
        let object = output.join(format!("{name}.o"));
        compile(&tool, source, &object, false);
        link(&tool, &[object], &output.join(name));
    }
    let original = output.join("private-timer-original.o");
    let wrapper = output.join("private-timer-wrapper.o");
    compile(&tool, SOURCES[4], &original, true);
    compile(&tool, SOURCES[5], &wrapper, false);
    link(&tool, &[original, wrapper], &output.join("private-timer"));
}
