use core::arch::global_asm;
use std::ffi::CStr;

const CALLS: u64 = 32;

global_asm!(
    r#"
    .text
    .p2align 4
    .global reverie_liteinst_fixed_getpid
    .hidden reverie_liteinst_fixed_getpid
    .type reverie_liteinst_fixed_getpid,@function
reverie_liteinst_fixed_getpid:
    mov eax, 39
    .global reverie_liteinst_fixed_getpid_site
    .hidden reverie_liteinst_fixed_getpid_site
reverie_liteinst_fixed_getpid_site:
    syscall
    nop
    nop
    nop
    ret
    .size reverie_liteinst_fixed_getpid, .-reverie_liteinst_fixed_getpid

    .p2align 6
    .global reverie_liteinst_self_lea
    .hidden reverie_liteinst_self_lea
    .type reverie_liteinst_self_lea,@function
reverie_liteinst_self_lea:
    mov eax, 39
    .global reverie_liteinst_self_lea_syscall
    .hidden reverie_liteinst_self_lea_syscall
reverie_liteinst_self_lea_syscall:
    syscall
    .global reverie_liteinst_self_lea_site
    .hidden reverie_liteinst_self_lea_site
reverie_liteinst_self_lea_site:
    lea rax, [rip + reverie_liteinst_self_lea_site]
    ret
    .size reverie_liteinst_self_lea, .-reverie_liteinst_self_lea
"#
);

unsafe extern "C" {
    fn reverie_liteinst_fixed_getpid() -> i64;
    static reverie_liteinst_fixed_getpid_site: u8;
    fn reverie_liteinst_self_lea() -> u64;
    static reverie_liteinst_self_lea_syscall: u8;
    static reverie_liteinst_self_lea_site: u8;
}

type CountFn = unsafe extern "C" fn(u64) -> u64;

unsafe fn count_function(name: &CStr) -> CountFn {
    // SAFETY: RTLD_DEFAULT searches already loaded DSOs and name is terminated.
    let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    assert!(!symbol.is_null(), "missing preload counter export");
    // SAFETY: both exported counter symbols have this exact C ABI.
    unsafe { core::mem::transmute(symbol) }
}

fn main() {
    if let Some(mode) = std::env::args().nth(1) {
        match mode.as_str() {
            "pc-relative-native" => self_lea(false),
            "pc-relative-hooked" => self_lea(true),
            _ => panic!("unknown trap-count mode: {mode}"),
        }
        return;
    }

    let mut expected = None;
    for _ in 0..CALLS {
        // SAFETY: the assembly function preserves the C ABI and returns getpid.
        let observed = unsafe { reverie_liteinst_fixed_getpid() };
        assert_eq!(*expected.get_or_insert(observed), observed);
    }

    let address = core::ptr::addr_of!(reverie_liteinst_fixed_getpid_site) as usize as u64;
    // SAFETY: names and exported function signatures are fixed by the runtime.
    let traps = unsafe { count_function(c"reverie_liteinst_site_trap_count")(address) };
    // SAFETY: names and exported function signatures are fixed by the runtime.
    let hooks = unsafe { count_function(c"reverie_liteinst_site_hook_count")(address) };
    println!("calls={CALLS} traps={traps} hooks={hooks}");
    assert_eq!(traps, 1);
    assert_eq!(hooks, CALLS);
}

fn self_lea(hooked: bool) {
    // The two-byte syscall and seven-byte LEA share the displaced prefix.
    // The LEA names its own retained instruction, so a relocation that treats
    // the address as an internal encoder label returns a trampoline address.
    let expected = core::ptr::addr_of!(reverie_liteinst_self_lea_site) as usize as u64;
    let mut addresses = 0;
    for call in 0..CALLS {
        // SAFETY: the function preserves the C ABI and computes an address.
        let observed = unsafe { reverie_liteinst_self_lea() };
        assert_eq!(
            observed, expected,
            "self-relative LEA changed at call {call}"
        );
        addresses += 1;
    }
    if hooked {
        let address = core::ptr::addr_of!(reverie_liteinst_self_lea_syscall) as usize as u64;
        // SAFETY: the runtime exports both functions with this exact C ABI.
        let traps = unsafe { count_function(c"reverie_liteinst_site_trap_count")(address) };
        let hooks = unsafe { count_function(c"reverie_liteinst_site_hook_count")(address) };
        assert_eq!(traps, 1);
        assert_eq!(hooks, CALLS);
        println!(
            "pc-relative hooked: calls={CALLS} addresses={addresses} traps={traps} hooks={hooks}"
        );
    } else {
        // SAFETY: dlsym only searches already loaded DSOs for this fixed name.
        let counter = unsafe {
            libc::dlsym(
                libc::RTLD_DEFAULT,
                c"reverie_liteinst_site_trap_count".as_ptr(),
            )
        };
        assert!(
            counter.is_null(),
            "native oracle loaded the LiteInst runtime"
        );
        println!("pc-relative native: calls={CALLS} addresses={addresses}");
    }
}
