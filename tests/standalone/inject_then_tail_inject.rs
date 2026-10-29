/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::alloc;
use std::env;

use nix::unistd;
use reverie::Error;
use reverie::syscalls::Sysno;

#[path = "../tool_fixtures/inject_then_tail.rs"]
mod inject_then_tail_tool;

use inject_then_tail_tool::NUM_REPS;
use inject_then_tail_tool::RAND_SIZE;
use inject_then_tail_tool::TestTool;

fn guest_mode() {
    println!("Running in guest mode (actual test).");
    let tid = unistd::gettid();
    let pid = unistd::getpid();
    let gid = unistd::getgid();
    let ppid = unistd::getppid();
    let pgid = unistd::getpgid(None).unwrap();
    let sid = unistd::getsid(None).unwrap();
    println!(
        "Read IDs: t {}, p {}, g {}, pp {}, pg {}, s{}",
        tid, pid, gid, ppid, pgid, sid,
    );
    // let r = syscalls::syscall!(0, 100, 0);
    let sz = RAND_SIZE;
    let rand_num: u64 = unsafe {
        let layout = alloc::Layout::from_size_align(sz, sz).unwrap();
        let buf = alloc::alloc(layout);
        let no = Sysno::getrandom as i64;
        let rand = libc::syscall(no, buf, sz, 0);
        if rand < 0 {
            panic!("getrandom returned error code {}\n", rand);
        } else if rand != sz as i64 {
            panic!(
                "getrandom did not generate all {} bytes (instead {}\n",
                sz, rand
            );
        }

        #[allow(clippy::cast_ptr_alignment)]
        let num: u64 = *(buf as *mut u64);
        alloc::dealloc(buf, layout);
        num
    };
    println!("Generated random number: {}", rand_num);
}

async fn host_mode(thisprog: &str) -> Result<i32, Error> {
    println!("Running in HOST mode (ReverieTool)");

    let mut command = reverie::process::Command::new(thisprog);
    command.arg("guest");

    let tracer = reverie_ptrace::TracerBuilder::<TestTool>::new(command)
        .config(NUM_REPS)
        .spawn()
        .await?;
    let (status, _) = tracer.wait().await?;

    Ok(status.code().unwrap_or(1))
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args: Vec<String> = env::args().collect();
    match &args[..] {
        [p] => std::process::exit(host_mode(p).await?),
        [_, s] if s == "guest" => guest_mode(),
        _ => panic!(
            "Expected 'guest' or no CLI argument. Got unexpected command line args ({}): {:?}",
            args.len(),
            args
        ),
    }

    Ok(())
}
