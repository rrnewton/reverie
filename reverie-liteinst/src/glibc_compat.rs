//! A library-private `_dl_find_object` (GLIBC_2.35), so the guest's dynamic
//! loader never has to find it in a glibc older than 2.35
//! (https://github.com/rrnewton/reverie/issues/980).
//!
//! build.rs links the unwinder from libgcc_eh.a. When libgcc was built against
//! glibc 2.35 or newer, as in a Nix build root with gcc 15, libgcc_eh.a looks
//! up a program counter's unwind tables with `_dl_find_object`, which would
//! otherwise be imported from the guest's libc.
//!
//! The assembly defines the name WEAK and hidden. Hidden: the static link
//! binds libgcc_eh.a's reference here, and the name stays out of the dynamic
//! symbol table, so the guest's own references still bind to its libc. Weak:
//! this crate is also an rlib, linked into other preloads that define the same
//! name for their own unwinder (Hermit's libdetcore_liteinst.so compiles
//! detcore-sabre/src/glibc_compat.rs). A strong definition there takes
//! precedence over this one instead of colliding with it.

use std::ffi::c_int;
use std::ffi::c_void;

std::arch::global_asm!(
    ".weak _dl_find_object",
    ".hidden _dl_find_object",
    ".set _dl_find_object, {find_object}",
    find_object = sym dl_find_object,
);

/// glibc's `struct dl_find_object` on x86_64.
#[repr(C)]
struct DlFindObject {
    dlfo_flags: u64,
    dlfo_map_start: *mut c_void,
    dlfo_map_end: *mut c_void,
    dlfo_link_map: *mut c_void,
    dlfo_eh_frame: *mut c_void,
    dlfo_reserved: [u64; 7],
}

/// The loaded object containing a program counter, as `_dl_find_object` reports it.
#[derive(Debug, PartialEq, Eq)]
struct LoadedObject {
    map_start: usize,
    map_end: usize,
    eh_frame: usize,
}

/// `_dl_find_object` over `dl_iterate_phdr`, the lookup libgcc used before
/// glibc 2.35. It returns 0 and fills `result` if a loaded object maps `pc`,
/// else -1. `dlfo_link_map` is null: libgcc reads only `dlfo_eh_frame`.
unsafe extern "C" fn dl_find_object(pc: *mut c_void, result: *mut DlFindObject) -> c_int {
    let Some(object) = loaded_object_containing(pc as usize) else {
        return -1;
    };
    // SAFETY: the caller passes a writable `struct dl_find_object`.
    unsafe {
        result.write(DlFindObject {
            dlfo_flags: 0,
            dlfo_map_start: object.map_start as *mut c_void,
            dlfo_map_end: object.map_end as *mut c_void,
            dlfo_link_map: std::ptr::null_mut(),
            dlfo_eh_frame: object.eh_frame as *mut c_void,
            dlfo_reserved: [0; 7],
        })
    };
    0
}

fn loaded_object_containing(pc: usize) -> Option<LoadedObject> {
    struct Search {
        pc: usize,
        found: Option<LoadedObject>,
    }

    unsafe extern "C" fn visit(
        info: *mut libc::dl_phdr_info,
        _size: libc::size_t,
        data: *mut c_void,
    ) -> c_int {
        // SAFETY: `data` is the `Search` passed below, and glibc passes a valid
        // `info` whose program headers stay mapped during the callback.
        let (search, info) = unsafe { (&mut *data.cast::<Search>(), &*info) };
        // SAFETY: as above; `dlpi_phnum` headers start at `dlpi_phdr`.
        let headers = unsafe { std::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum.into()) };
        let base = info.dlpi_addr as usize;
        let mut object = LoadedObject {
            map_start: usize::MAX,
            map_end: 0,
            eh_frame: 0,
        };
        let mut contains_pc = false;
        for header in headers {
            let start = base.wrapping_add(header.p_vaddr as usize);
            match header.p_type {
                libc::PT_LOAD => {
                    let end = start.wrapping_add(header.p_memsz as usize);
                    object.map_start = object.map_start.min(start);
                    object.map_end = object.map_end.max(end);
                    contains_pc |= (start..end).contains(&search.pc);
                }
                libc::PT_GNU_EH_FRAME => object.eh_frame = start,
                _ => {}
            }
        }
        if !contains_pc {
            return 0;
        }
        search.found = Some(object);
        1
    }

    let mut search = Search { pc, found: None };
    // SAFETY: `visit` matches dl_iterate_phdr's callback contract and `search`
    // outlives the call.
    unsafe { libc::dl_iterate_phdr(Some(visit), (&raw mut search).cast()) };
    search.found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dl_find_object_finds_this_code_and_its_unwind_tables() {
        let pc = dl_find_object as *const () as usize;
        let mut result = std::mem::MaybeUninit::<DlFindObject>::uninit();
        // SAFETY: `result` is writable storage for one `struct dl_find_object`.
        assert_eq!(
            unsafe { dl_find_object(pc as *mut c_void, result.as_mut_ptr()) },
            0
        );
        // SAFETY: the call returned 0, so it wrote `result`.
        let result = unsafe { result.assume_init() };
        let (start, end) = (result.dlfo_map_start as usize, result.dlfo_map_end as usize);
        assert!(
            (start..end).contains(&pc),
            "{start:#x}..{end:#x} misses {pc:#x}"
        );
        let eh_frame = result.dlfo_eh_frame as usize;
        assert!((start..end).contains(&eh_frame), "eh_frame {eh_frame:#x}");
        // SAFETY: `eh_frame` is inside a mapped object. The `.eh_frame_hdr`
        // section starts with version 1.
        assert_eq!(unsafe { *(eh_frame as *const u8) }, 1);
        let mut unused = std::mem::MaybeUninit::<DlFindObject>::uninit();
        // SAFETY: as above; a null program counter is in no loaded object.
        assert_eq!(
            unsafe { dl_find_object(std::ptr::null_mut(), unused.as_mut_ptr()) },
            -1
        );
    }
}
