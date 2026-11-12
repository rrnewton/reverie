#![deny(warnings)]
use std::error::Error;
use std::io;

use liteinst2::patcher::StalenessBudget;
use liteinst2::scanner::InstructionScanner;
use liteinst2::trampoline::HookContext;
use liteinst2::trampoline::HookSite;
use liteinst2::trampoline::InstalledHook;
use liteinst2::trampoline::TrampolineArena;

const PAGE_BYTES: usize = 4096;

unsafe extern "C" fn replace_zero(context: *mut HookContext) {
    // SAFETY: liteinst2 passes exclusive access to the saved register frame.
    unsafe {
        (*context).rax = 40;
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let name = b"liteinst2-example\0";
    // SAFETY: the name is NUL terminated and all arguments are scalar.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_memfd_create,
            name.as_ptr().cast::<libc::c_char>(),
            libc::MFD_CLOEXEC,
        ) as libc::c_int
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: fd is live and the length is one page.
    if unsafe { libc::ftruncate(fd, PAGE_BYTES as libc::off_t) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: both mappings cover the same complete memfd.
    let writable = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            PAGE_BYTES,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    // SAFETY: this is the RX alias of the same backing object.
    let executable = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            PAGE_BYTES,
            libc::PROT_READ | libc::PROT_EXEC,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    // SAFETY: mappings retain the backing object.
    unsafe { libc::close(fd) };
    if writable == libc::MAP_FAILED || executable == libc::MAP_FAILED {
        return Err(io::Error::last_os_error().into());
    }

    // xor eax,eax; inc eax; nop; ret; padding
    let code = [0x31, 0xC0, 0xFF, 0xC0, 0x90, 0xC3, 0x90, 0x90];
    // SAFETY: the writable mapping has a complete page.
    unsafe {
        std::ptr::copy_nonoverlapping(code.as_ptr(), writable.cast::<u8>(), code.len());
    }
    let address = executable as usize as u64;
    let scanner = InstructionScanner::default();
    let scan = scanner.scan(&code, address)?;
    let arena = TrampolineArena::allocate_near(address, 1)?;
    // SAFETY: both aliases and the arena remain live through every function call.
    let hook = unsafe {
        InstalledHook::install_replacing_first_in_arena(
            HookSite::new(&scanner, &scan, &code, address, address, writable.cast()),
            replace_zero,
            StalenessBudget::new(3_000).unwrap(),
            &arena,
        )?
    };
    // SAFETY: code is an extern C function returning u32.
    let function: unsafe extern "C" fn() -> u32 = unsafe { std::mem::transmute(executable) };

    assert_eq!(unsafe { function() }, 1);
    hook.activate()?;
    assert_eq!(unsafe { function() }, 41);
    hook.deactivate()?;
    assert_eq!(unsafe { function() }, 1);
    println!("inactive=1 active=41 restored=1");
    Ok(())
}
