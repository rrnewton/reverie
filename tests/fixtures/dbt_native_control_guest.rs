// Copyright (c) Meta Platforms, Inc. and affiliates.
// Licensed under the BSD-style license in the repository LICENSE file.

// Ordinary command guest: no Reverie dependency or Tool/runtime code.

use std::arch::x86_64::__cpuid_count;
use std::arch::x86_64::_mm_lfence;
use std::arch::x86_64::_rdtsc;
use std::io::Write;

fn getpid() -> i64 {
    let result: i64;
    // SAFETY: Linux x86_64 getpid takes no pointer or other argument.
    unsafe {
        std::arch::asm!("syscall", inlateout("rax") 39_i64 => result,
            lateout("rcx") _, lateout("r11") _, options(nostack));
    }
    result
}

fn ordered_tsc() -> u64 {
    // SAFETY: the project and this fixture target x86_64, which supplies SSE2.
    unsafe {
        _mm_lfence();
        let value = _rdtsc();
        _mm_lfence();
        value
    }
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("getpid") => println!("pid={}", getpid()),
        Some("exit42") => {
            assert!(getpid() > 0);
            std::process::exit(42);
        }
        Some("instructions") => {
            let physical = __cpuid_count(0, 0);
            let cpuid = __cpuid_count(0x4fff_fffe, 0x1357);
            let first = ordered_tsc();
            let second = ordered_tsc();
            println!(
                "physical={:08x},{:08x},{:08x},{:08x}",
                physical.eax, physical.ebx, physical.ecx, physical.edx
            );
            println!(
                "cpuid={:08x},{:08x},{:08x},{:08x}",
                cpuid.eax, cpuid.ebx, cpuid.ecx, cpuid.edx
            );
            println!("tsc={first},{second}");
        }
        Some("cpuid-error") => {
            println!("before-cpuid-error");
            std::io::stdout().flush().unwrap();
            std::hint::black_box(__cpuid_count(0x4fff_fffe, 0x1357));
            println!("after-cpuid-error");
        }
        Some("rdtsc-error") => {
            println!("before-rdtsc-error");
            std::io::stdout().flush().unwrap();
            // This explicit probe arms the Tool only after startup/loader work.
            std::hint::black_box(__cpuid_count(0x4fff_fffe, 0x1357));
            std::hint::black_box(ordered_tsc());
            println!("after-rdtsc-error");
        }
        _ => panic!("expected getpid, exit42, instructions, cpuid-error, or rdtsc-error"),
    }
}
