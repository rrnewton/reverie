/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `Guest::initial_stack_pointer` under ptrace: the post-exec callback runs at
//! the exec stop, before the new image's first instruction, and the backend
//! reports the stack pointer the image starts with. It is the address of
//! argc, followed by the argument pointers and their NULL, as the kernel laid
//! them out.

#![cfg(target_arch = "x86_64")]

use std::sync::Mutex;

use reverie::Errno;
use reverie::Guest;
use reverie::Tool;
use reverie::syscalls::Addr;
use reverie::syscalls::MemoryAccess;

/// What the post-exec callback saw: the reported initial stack pointer, the
/// stack pointer in the registers, and the argument strings read from the
/// stack at the reported address.
static SEEN: Mutex<Option<Seen>> = Mutex::new(None);

/// The reported initial stack pointer, the registers' stack pointer, and the
/// argument strings.
type Seen = (Option<u64>, u64, Vec<String>);

#[derive(Debug, Default, Clone)]
struct StackTool;

/// The NUL-terminated string at `address`.
fn read_string<T: Guest<StackTool>>(guest: &T, address: u64) -> Result<String, Errno> {
    let mut bytes = Vec::new();
    loop {
        let at = Addr::<u8>::from_raw(address as usize + bytes.len()).ok_or(Errno::EFAULT)?;
        let byte: u8 = guest.memory().read_value(at)?;
        if byte == 0 {
            return Ok(String::from_utf8_lossy(&bytes).into_owned());
        }
        bytes.push(byte);
    }
}

#[reverie::tool]
impl Tool for StackTool {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_post_exec<T: Guest<Self>>(&self, guest: &mut T) -> Result<(), Errno> {
        let reported = guest.initial_stack_pointer();
        let rsp = guest.regs().await.rsp;
        let mut arguments = Vec::new();
        if let Some(stack) = reported {
            let word = |at: u64| -> Result<u64, Errno> {
                guest
                    .memory()
                    .read_value(Addr::<u64>::from_raw(at as usize).ok_or(Errno::EFAULT)?)
            };
            let argc = word(stack)?;
            for index in 0..argc {
                arguments.push(read_string(guest, word(stack + 8 * (1 + index))?)?);
            }
            assert_eq!(word(stack + 8 * (1 + argc))?, 0, "argv ends with NULL");
        }
        *SEEN.lock().unwrap() = Some((reported, rsp, arguments));
        Ok(())
    }
}

#[test]
fn post_exec_reports_the_initial_stack_pointer_at_argc() {
    let (output, ()) =
        reverie_ptrace::testing::test_cmd::<StackTool>("/bin/true", &["first", "second"]).unwrap();
    assert!(output.status.success(), "{output:?}");
    let (reported, rsp, arguments) = SEEN.lock().unwrap().take().expect("post-exec ran");
    // At the exec stop no instruction of the image has run, so the registers
    // hold the same stack pointer.
    assert_eq!(reported, Some(rsp));
    assert_eq!(&arguments[1..], ["first", "second"]);
    assert!(arguments[0].ends_with("true"), "{arguments:?}");
}
