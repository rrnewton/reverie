//! Main-thread native fixture for mapped runtime adoption controls.

use std::path::Path;

#[path = "rpc_tool_guest/mapped_fork.rs"]
mod mapped_fork;

core::arch::global_asm!(
    r#"
    .text
    .p2align 4
    .global reverie_liteinst_rpc_raw_fork
    .hidden reverie_liteinst_rpc_raw_fork
    .type reverie_liteinst_rpc_raw_fork,@function
reverie_liteinst_rpc_raw_fork:
    mov eax, 57
    syscall
    nop
    nop
    nop
    ret
    .size reverie_liteinst_rpc_raw_fork, .-reverie_liteinst_rpc_raw_fork
"#
);

unsafe extern "C" {
    fn reverie_liteinst_rpc_raw_fork() -> i64;
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let mode = args.next().expect("mode");
    let directory = args.next().expect("evidence directory");
    let case = args.next().expect("case");
    let case = case.to_str().unwrap();
    match mode.to_str().unwrap() {
        "mapped-fork-host" => mapped_fork::host(Path::new(&directory), case),
        "mapped-fork-guest" => mapped_fork::guest(Path::new(&directory), case),
        "mapped-fork-control" => mapped_fork::control(Path::new(&directory), case),
        _ => panic!("unknown mapped fixture mode"),
    }
}
