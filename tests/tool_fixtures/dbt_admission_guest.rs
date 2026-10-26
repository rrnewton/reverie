// Copyright (c) Meta Platforms, Inc. and affiliates.
// Licensed under the BSD-style license in the repository LICENSE file.

fn main() {
    let operation = std::env::args().nth(1).expect("operation");
    // Each transition must be refused before it takes effect. If a regression
    // permits birth, the child exits immediately and the parent reaps it; the
    // test still fails because there was no named refusal.
    unsafe {
        let child = match operation.as_str() {
            "fork" => libc::syscall(libc::SYS_fork),
            "vfork" => raw_vfork(),
            "clone" => libc::syscall(libc::SYS_clone, libc::SIGCHLD, 0, 0, 0, 0),
            "clone3" => {
                let mut fields = [0_u64; 11];
                fields[4] = libc::SIGCHLD as u64;
                libc::syscall(
                    libc::SYS_clone3,
                    fields.as_ptr(),
                    std::mem::size_of_val(&fields),
                )
            }
            "execve" | "execveat" => {
                let path = c"/bin/true";
                let argv = [path.as_ptr(), std::ptr::null()];
                let env = [std::ptr::null::<libc::c_char>()];
                if operation == "execve" {
                    libc::syscall(libc::SYS_execve, path.as_ptr(), argv.as_ptr(), env.as_ptr())
                } else {
                    libc::syscall(
                        libc::SYS_execveat,
                        libc::AT_FDCWD,
                        path.as_ptr(),
                        argv.as_ptr(),
                        env.as_ptr(),
                        0,
                    )
                }
            }
            "sigusr1" | "sigkill" => {
                let pid = libc::syscall(libc::SYS_getpid);
                libc::syscall(
                    libc::SYS_kill,
                    pid,
                    if operation == "sigusr1" {
                        libc::SIGUSR1
                    } else {
                        libc::SIGKILL
                    },
                );
                libc::_exit(99)
            }
            "getpid" => {
                println!("{}", libc::syscall(libc::SYS_getpid));
                return;
            }
            "raw-close-range" => {
                let phase_file = std::env::args_os().nth(2).expect("phase file");
                std::fs::write(&phase_file, b"before-full-range\n").unwrap();
                println!("before-full-range");
                assert_eq!(libc::syscall(libc::SYS_close_range, 3_u32, u32::MAX, 0), 0);
                assert!(libc::syscall(libc::SYS_getpid) > 0);
                std::fs::write(&phase_file, b"after-full-range\n").unwrap();
                println!("after-full-range");
                return;
            }
            "protect-close-range" | "protect-primary-range" | "protect-dup2" => {
                let primary = libc::syscall(libc::SYS_getpid, 0x5246_4454, 1);
                assert!(primary >= 3);
                if operation == "protect-close-range" {
                    assert_eq!(libc::syscall(libc::SYS_close_range, 3_u32, u32::MAX, 0), 0);
                } else if operation == "protect-primary-range" {
                    assert_eq!(libc::syscall(libc::SYS_close_range, primary, primary, 0), 0);
                } else {
                    assert_eq!(libc::syscall(libc::SYS_dup2, 1, primary), -1);
                    assert_eq!(
                        std::io::Error::last_os_error().raw_os_error(),
                        Some(libc::EBADF)
                    );
                }
                // This subscribed call must reach the real coordinator and
                // receive its reply over the still-owned primary socket.
                assert_eq!(libc::syscall(libc::SYS_getpid, 0x5246_4454, 2), 0x517a);
                println!("primary-survived");
                return;
            }
            _ => panic!("unknown operation"),
        };
        if child == 0 {
            libc::_exit(99);
        }
        if child > 0 {
            libc::waitpid(child as i32, std::ptr::null_mut(), 0);
        }
        libc::_exit(99);
    }
}

// The negative oracle must remain safe even if a regression admits vfork.
// The child exits entirely inside this assembly and never returns onto or
// writes the suspended parent's Rust stack.
unsafe fn raw_vfork() -> libc::c_long {
    let result: i64;
    unsafe {
        std::arch::asm!(
            "syscall", "test rax, rax", "jnz 2f",
            "mov rax, {exit}", "mov rdi, 99", "syscall", "ud2", "2:",
            inlateout("rax") libc::SYS_vfork => result,
            exit = const libc::SYS_exit,
            lateout("rdi") _, lateout("rcx") _, lateout("r11") _,
            options(nostack),
        );
    }
    result
}
