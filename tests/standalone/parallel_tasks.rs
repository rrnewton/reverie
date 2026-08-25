/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::env;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::thread;

use reverie::Error;
use reverie::Tool;

#[derive(Debug, Default, Clone)]
struct TestTool {}

#[reverie::tool]
impl Tool for TestTool {
    type GlobalState = ();
    type ThreadState = ();
}

const NUM_ELEMENTS: usize = 1_000_000;

/// In guest mode two threads will try to fill up half of the data array with their thread id as
/// value. The threads grab indices through an atomic int. For sufficiently large arrays we expect
/// the thread ids to show up interleaved.
fn guest_mode() {
    let shared_data = Arc::new(Box::new([0; NUM_ELEMENTS]));
    let shared_idx = Arc::new(AtomicUsize::new(0));

    let handles: Vec<thread::JoinHandle<_>> = (0..2)
        .map(|rank| {
            let idx = shared_idx.clone();
            let data = shared_data.clone();
            thread::spawn(move || {
                // Distinct nonzero value per worker. This used to be the thread id, but
                // `ThreadId` has no stable numeric accessor and the switch-point count
                // only needs the two workers to write DIFFERENT values -- it compares
                // adjacent elements and never inspects the value itself.
                let tid = rank as u64 + 1;

                // SAFETY: the workers only ever write to indices handed out by the shared
                // atomic counter, so no two writes alias. This is the stable spelling of
                // `Arc::get_mut_unchecked`: it rebuilds the same mutable slice, so
                // indexing keeps its bounds check. Deliberately still a data race -- that
                // is what this test exercises -- so do NOT "fix" it with atomics.
                let data: &mut [u64] = unsafe {
                    std::slice::from_raw_parts_mut(data.as_ptr() as *mut u64, NUM_ELEMENTS)
                };

                // Give each thread half of the fetch_add attempts.
                for _ in 0..(NUM_ELEMENTS / 2) {
                    let idx = idx.fetch_add(1, Ordering::SeqCst);
                    data[idx] = tid;
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    // Calculate the number of switch points. E.g. the number of times we observed interleaved
    // writes between the threads.
    let mut switch_points = 0;
    let mut prev = shared_data[0];
    for i in 1..shared_data.len() {
        if prev != shared_data[i] {
            prev = shared_data[i];
            switch_points += 1;
        }
    }

    println!("Switch points: {}", switch_points);
    if switch_points <= 1 {
        eprintln!("Expected more than 1 switch point!");
        std::process::exit(1);
    }
}

async fn host_mode(thisprog: &str) -> Result<i32, Error> {
    println!("Running in HOST mode (ReverieTool)");

    let mut command = reverie::process::Command::new(thisprog);
    command.arg("guest");

    let tracer = reverie_ptrace::TracerBuilder::<TestTool>::new(command)
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
