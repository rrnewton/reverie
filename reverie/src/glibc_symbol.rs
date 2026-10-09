/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Finds a versioned function in the process's `libc.so.6` without the `dl*`
//! API (<https://github.com/rrnewton/reverie/issues/980>).
//!
//! Guest preloads that link gcc 15's unwinder statically need glibc's real
//! `_dl_find_object@GLIBC_2.35` and must find it at any time: while another
//! thread runs a library constructor under `dlopen`, before their own
//! initializer, and without changing the caller's `dlerror` state.
//! `dlsym`/`dlvsym` take the loader's `dl_load_lock`, which `dlopen` holds
//! while it runs constructors, and they set or clear `dlerror`. This module
//! uses only `dl_iterate_phdr`, which takes the loader's recursive
//! `dl_load_write_lock` and never touches `dlerror`. glibc holds that lock
//! only while it appends to or removes from its list of loaded objects
//! (`_dl_add_to_namespace_list`, and `dlclose` after destructors have run) and
//! while `dl_iterate_phdr` calls its callback. No constructor or destructor
//! runs under it, so a lookup cannot wait on a thread that is running one.
//!
//! It finds the object whose `DT_SONAME` is `libc.so.6` in the caller's loader
//! namespace and reads its dynamic section: the string and symbol tables, the
//! GNU hash table (or the SysV one, which glibc 2.36 and later may omit), and
//! the version tables. Every address it reads is checked to lie in one of the
//! object's readable loaded segments. A name is found only as a defined
//! `STT_FUNC` symbol (not an IFUNC, whose address is its resolver) of global or
//! weak binding whose version index names the requested version. Anything
//! unexpected gives `None`. It allocates nothing and cannot panic, so it may run
//! inside a `dl_iterate_phdr` callback or an unwinder.
//!
//! Its exposure is the lock's: the same thread may re-enter (the lock is
//! recursive), but another thread's `dl_iterate_phdr` callback that waits for
//! this lookup would wait forever, and a signal handler that interrupted the
//! loader's own list update on this thread would see that update half done.
//! That is the exposure libgcc's unwinder had on every unwind before glibc
//! 2.35, when it found unwind tables with `dl_iterate_phdr`.

use std::ffi::c_int;
use std::ffi::c_void;
use std::mem::size_of;

/// The address of `name` at `version` (for example `_dl_find_object` at
/// `GLIBC_2.35`) in this process's `libc.so.6`, or `None` when that libc does
/// not define it, defines it only as an IFUNC, or cannot be read as expected.
pub fn glibc_versioned_function(name: &[u8], version: &[u8]) -> Option<usize> {
    find(name, version, &[HashTable::Gnu, HashTable::SysV])
}

/// Which symbol hash table a lookup may use, in order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HashTable {
    Gnu,
    SysV,
}

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PF_R: u32 = 4;

const DT_NULL: i64 = 0;
const DT_HASH: i64 = 4;
const DT_STRTAB: i64 = 5;
const DT_SYMTAB: i64 = 6;
const DT_STRSZ: i64 = 10;
const DT_SONAME: i64 = 14;
const DT_GNU_HASH: i64 = 0x6fff_fef5;
const DT_VERSYM: i64 = 0x6fff_fff0;
const DT_VERDEF: i64 = 0x6fff_fffc;
const DT_VERDEFNUM: i64 = 0x6fff_fffd;

const STT_FUNC: u8 = 2;
const STB_GLOBAL: u8 = 1;
const STB_WEAK: u8 = 2;
const SHN_UNDEF: u16 = 0;
const VERSYM_INDEX: u16 = 0x7fff;

/// More loaded segments than any libc has; extra ones are ignored, which can
/// only make a read fail.
const MAX_SEGMENTS: usize = 16;
/// Bounds on table walks, so corrupt data cannot loop forever.
const MAX_STEPS: usize = 1 << 20;
const MAX_NAME: usize = 256;

fn find(name: &[u8], version: &[u8], tables: &[HashTable]) -> Option<usize> {
    struct Search<'a> {
        name: &'a [u8],
        version: &'a [u8],
        tables: &'a [HashTable],
        found: Option<usize>,
    }

    unsafe extern "C" fn visit(
        info: *mut libc::dl_phdr_info,
        _size: libc::size_t,
        data: *mut c_void,
    ) -> c_int {
        // SAFETY: `data` is the `Search` passed below, and glibc passes a valid
        // `info` whose program headers stay mapped during the callback.
        let (search, info) = unsafe { (&mut *data.cast::<Search>(), &*info) };
        // SAFETY: as above.
        let Some(object) = (unsafe { Object::new(info) }) else {
            return 0;
        };
        if !object.soname_is(b"libc.so.6") {
            return 0;
        }
        search.found = search
            .tables
            .iter()
            .find_map(|table| object.lookup(search.name, search.version, *table));
        1
    }

    let mut search = Search {
        name,
        version,
        tables,
        found: None,
    };
    // SAFETY: `visit` matches dl_iterate_phdr's callback contract and `search`
    // outlives the call.
    unsafe { libc::dl_iterate_phdr(Some(visit), (&raw mut search).cast()) };
    search.found
}

/// One loaded object's readable segments and dynamic-section entries.
struct Object {
    base: usize,
    segments: [(usize, usize); MAX_SEGMENTS],
    segment_count: usize,
    strtab: Option<usize>,
    strsz: Option<usize>,
    symtab: Option<usize>,
    soname: Option<usize>,
    gnu_hash: Option<usize>,
    sysv_hash: Option<usize>,
    versym: Option<usize>,
    verdef: Option<usize>,
    verdefnum: Option<usize>,
}

impl Object {
    /// # Safety
    ///
    /// `info` must describe a loaded object whose program headers are mapped.
    unsafe fn new(info: &libc::dl_phdr_info) -> Option<Self> {
        let mut object = Object {
            base: info.dlpi_addr as usize,
            segments: [(0, 0); MAX_SEGMENTS],
            segment_count: 0,
            strtab: None,
            strsz: None,
            symtab: None,
            soname: None,
            gnu_hash: None,
            sysv_hash: None,
            versym: None,
            verdef: None,
            verdefnum: None,
        };
        if info.dlpi_phdr.is_null() {
            return None;
        }
        // SAFETY: glibc passes `dlpi_phnum` mapped program headers.
        let headers = unsafe { std::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum.into()) };
        let mut dynamic = None;
        for header in headers {
            let start = object.base.checked_add(header.p_vaddr as usize)?;
            let end = start.checked_add(header.p_memsz as usize)?;
            match header.p_type {
                PT_LOAD if header.p_flags & PF_R != 0 && object.segment_count < MAX_SEGMENTS => {
                    object.segments[object.segment_count] = (start, end);
                    object.segment_count += 1;
                }
                PT_DYNAMIC => dynamic = Some((start, end)),
                _ => {}
            }
        }
        let (mut entry, end) = dynamic?;
        while entry.checked_add(16)? <= end {
            let tag = object.read::<i64>(entry)?;
            let value = object.read::<u64>(entry.wrapping_add(8))? as usize;
            match tag {
                DT_NULL => break,
                DT_STRTAB => object.strtab = Some(object.relocate(value)),
                DT_STRSZ => object.strsz = Some(value),
                DT_SYMTAB => object.symtab = Some(object.relocate(value)),
                DT_SONAME => object.soname = Some(value),
                DT_GNU_HASH => object.gnu_hash = Some(object.relocate(value)),
                DT_HASH => object.sysv_hash = Some(object.relocate(value)),
                DT_VERSYM => object.versym = Some(object.relocate(value)),
                DT_VERDEF => object.verdef = Some(object.relocate(value)),
                DT_VERDEFNUM => object.verdefnum = Some(value),
                _ => {}
            }
            entry = entry.wrapping_add(16);
        }
        Some(object)
    }

    /// An address-valued dynamic entry as an address: glibc has already
    /// added the load base to most of them in place on x86-64, but a value
    /// below the base is still an offset from it.
    fn relocate(&self, value: usize) -> usize {
        if value < self.base {
            self.base.wrapping_add(value)
        } else {
            value
        }
    }

    /// Reads a `T` at `address` if all of it lies in one readable segment.
    fn read<T: Copy>(&self, address: usize) -> Option<T> {
        let end = address.checked_add(size_of::<T>())?;
        self.segments[..self.segment_count]
            .iter()
            .any(|(start, stop)| *start <= address && end <= *stop)
            // SAFETY: the bytes lie in a readable segment of a loaded object.
            .then(|| unsafe { std::ptr::read_unaligned(address as *const T) })
    }

    /// Whether the string at `offset` in the string table is exactly `text`.
    fn string_is(&self, offset: usize, text: &[u8]) -> bool {
        let (Some(strtab), Some(strsz)) = (self.strtab, self.strsz) else {
            return false;
        };
        if text.len() >= MAX_NAME
            || offset
                .checked_add(text.len())
                .is_none_or(|end| end >= strsz)
        {
            return false;
        }
        let start = strtab.wrapping_add(offset);
        text.iter()
            .chain(std::iter::once(&0u8))
            .enumerate()
            .all(|(index, byte)| self.read::<u8>(start.wrapping_add(index)) == Some(*byte))
    }

    fn soname_is(&self, text: &[u8]) -> bool {
        self.soname
            .is_some_and(|offset| self.string_is(offset, text))
    }

    fn lookup(&self, name: &[u8], version: &[u8], table: HashTable) -> Option<usize> {
        let index = self.version_index(version)?;
        match table {
            HashTable::Gnu => self.gnu_lookup(name, index),
            HashTable::SysV => self.sysv_lookup(name, index),
        }
    }

    /// The `vd_ndx` of the version definition named `version`.
    fn version_index(&self, version: &[u8]) -> Option<u16> {
        let mut definition = self.verdef?;
        for _ in 0..self.verdefnum?.min(MAX_STEPS) {
            // Elf64_Verdef: vd_version, vd_flags, vd_ndx, vd_cnt (u16 each),
            // vd_hash, vd_aux, vd_next (u32 each).
            let ndx = self.read::<u16>(definition.wrapping_add(4))?;
            let aux = self.read::<u32>(definition.wrapping_add(12))? as usize;
            let next = self.read::<u32>(definition.wrapping_add(16))? as usize;
            // Elf64_Verdaux: vda_name, vda_next (u32 each).
            let name = self.read::<u32>(definition.checked_add(aux)?)? as usize;
            if self.string_is(name, version) {
                return Some(ndx);
            }
            if next == 0 {
                return None;
            }
            definition = definition.checked_add(next)?;
        }
        None
    }

    /// The address of symbol `index` if it is `name`, a defined function of
    /// global or weak binding, at version index `version`.
    fn symbol_matches(&self, index: usize, name: &[u8], version: u16) -> Option<usize> {
        // Elf64_Sym: st_name (u32), st_info, st_other (u8), st_shndx (u16),
        // st_value, st_size (u64): 24 bytes.
        let symbol = self.symtab?.checked_add(index.checked_mul(24)?)?;
        let st_name = self.read::<u32>(symbol)? as usize;
        let st_info = self.read::<u8>(symbol.wrapping_add(4))?;
        let st_shndx = self.read::<u16>(symbol.wrapping_add(6))?;
        let st_value = self.read::<u64>(symbol.wrapping_add(8))? as usize;
        if st_info & 0xf != STT_FUNC
            || !matches!(st_info >> 4, STB_GLOBAL | STB_WEAK)
            || st_shndx == SHN_UNDEF
            || st_value == 0
            || !self.string_is(st_name, name)
        {
            return None;
        }
        let versym = self.read::<u16>(self.versym?.checked_add(index.checked_mul(2)?)?)?;
        (versym & VERSYM_INDEX == version).then(|| self.base.wrapping_add(st_value))
    }

    fn gnu_lookup(&self, name: &[u8], version: u16) -> Option<usize> {
        let table = self.gnu_hash?;
        let nbuckets = self.read::<u32>(table)? as usize;
        let symoffset = self.read::<u32>(table.wrapping_add(4))? as usize;
        let bloom_size = self.read::<u32>(table.wrapping_add(8))? as usize;
        let bloom_shift = self.read::<u32>(table.wrapping_add(12))?;
        if nbuckets == 0 || bloom_size == 0 {
            return None;
        }
        let hash = gnu_hash(name);
        let bloom = table.wrapping_add(16);
        let word = self.read::<u64>(bloom.checked_add(((hash as usize / 64) % bloom_size) * 8)?)?;
        let mask = (1u64 << (hash % 64)) | (1u64 << ((hash >> (bloom_shift % 32)) % 64));
        if word & mask != mask {
            return None;
        }
        let buckets = bloom.checked_add(bloom_size.checked_mul(8)?)?;
        let chains = buckets.checked_add(nbuckets.checked_mul(4)?)?;
        let mut index =
            self.read::<u32>(buckets.wrapping_add((hash as usize % nbuckets) * 4))? as usize;
        if index < symoffset {
            return None;
        }
        for _ in 0..MAX_STEPS {
            let chain =
                self.read::<u32>(chains.checked_add((index - symoffset).checked_mul(4)?)?)?;
            if (chain | 1) == (hash | 1)
                && let Some(address) = self.symbol_matches(index, name, version)
            {
                return Some(address);
            }
            if chain & 1 != 0 {
                return None;
            }
            index = index.checked_add(1)?;
        }
        None
    }

    fn sysv_lookup(&self, name: &[u8], version: u16) -> Option<usize> {
        let table = self.sysv_hash?;
        let nbucket = self.read::<u32>(table)? as usize;
        let nchain = self.read::<u32>(table.wrapping_add(4))? as usize;
        if nbucket == 0 {
            return None;
        }
        let buckets = table.wrapping_add(8);
        let chains = buckets.checked_add(nbucket.checked_mul(4)?)?;
        let mut index = self
            .read::<u32>(buckets.wrapping_add((sysv_hash(name) as usize % nbucket) * 4))?
            as usize;
        for _ in 0..nchain.min(MAX_STEPS) {
            if index == 0 || index >= nchain {
                return None;
            }
            if let Some(address) = self.symbol_matches(index, name, version) {
                return Some(address);
            }
            index = self.read::<u32>(chains.checked_add(index.checked_mul(4)?)?)? as usize;
        }
        None
    }
}

fn gnu_hash(name: &[u8]) -> u32 {
    name.iter().fold(5381u32, |hash, byte| {
        hash.wrapping_mul(33).wrapping_add(u32::from(*byte))
    })
}

fn sysv_hash(name: &[u8]) -> u32 {
    name.iter().fold(0u32, |hash, byte| {
        let hash = (hash << 4).wrapping_add(u32::from(*byte));
        let high = hash & 0xf000_0000;
        (hash ^ (high >> 24)) & !high
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::CStr;
    use std::ffi::CString;

    use super::*;

    /// What `dlvsym` reports for `name` at `version`, as the reference.
    fn dlvsym(name: &str, version: &str) -> Option<usize> {
        let (name, version) = (CString::new(name).unwrap(), CString::new(version).unwrap());
        // SAFETY: both names are NUL-terminated.
        let symbol = unsafe { libc::dlvsym(libc::RTLD_DEFAULT, name.as_ptr(), version.as_ptr()) };
        (!symbol.is_null()).then_some(symbol as usize)
    }

    /// The lookup through both tables, the GNU table alone, and the SysV table
    /// alone (as on a libc with only one of them).
    fn each_table(name: &str, version: &str) -> [Option<usize>; 3] {
        let (name, version) = (name.as_bytes(), version.as_bytes());
        [
            glibc_versioned_function(name, version),
            find(name, version, &[HashTable::Gnu]),
            find(name, version, &[HashTable::SysV]),
        ]
    }

    #[test]
    fn dl_find_object_is_found_where_dlvsym_finds_it() {
        let expected = dlvsym("_dl_find_object", "GLIBC_2.35");
        assert_eq!(each_table("_dl_find_object", "GLIBC_2.35"), [expected; 3]);
    }

    /// Plain functions at their versions, found through each hash table as
    /// `dlvsym` finds them.
    #[test]
    fn versioned_functions_are_found_where_dlvsym_finds_them() {
        for (name, version) in [
            ("dl_iterate_phdr", "GLIBC_2.2.5"),
            ("dlvsym", "GLIBC_2.34"),
            ("pthread_create", "GLIBC_2.34"),
            ("realpath", "GLIBC_2.3"),
            ("realpath", "GLIBC_2.2.5"),
        ] {
            let expected = dlvsym(name, version);
            assert!(expected.is_some(), "dlvsym finds no {name}@{version}");
            assert_eq!(each_table(name, version), [expected; 3], "{name}@{version}");
        }
    }

    /// `realpath` has two versions at two addresses; each version finds its
    /// own, including the older, non-default one (its versym hidden bit is
    /// set).
    #[test]
    fn each_version_of_a_function_is_its_own() {
        let new = glibc_versioned_function(b"realpath", b"GLIBC_2.3");
        let old = glibc_versioned_function(b"realpath", b"GLIBC_2.2.5");
        assert!(new.is_some() && old.is_some());
        assert_ne!(new, old);
    }

    #[test]
    fn an_absent_name_or_version_or_a_prefix_is_not_found() {
        assert_eq!(each_table("_dl_find_object", "GLIBC_2.99"), [None; 3]);
        assert_eq!(
            each_table("no_such_function_anywhere", "GLIBC_2.2.5"),
            [None; 3]
        );
        assert_eq!(each_table("realpat", "GLIBC_2.3"), [None; 3]);
        assert_eq!(each_table("realpath", "GLIBC_2.3.4"), [None; 3]);
    }

    /// An IFUNC's symbol value is its resolver, not the function, so it is
    /// refused (memcpy is an IFUNC in x86-64 glibc).
    #[test]
    fn an_ifunc_is_not_found() {
        assert!(dlvsym("memcpy", "GLIBC_2.14").is_some());
        assert_eq!(each_table("memcpy", "GLIBC_2.14"), [None; 3]);
    }

    /// The lookup changes no `dlerror` state: a caller's pending message is
    /// still there after it.
    #[test]
    fn a_lookup_keeps_a_pending_dlerror() {
        // SAFETY: the name is NUL-terminated; it names no library.
        let missing = unsafe {
            libc::dlopen(
                c"/nonexistent/reverie-glibc-symbol-missing.so".as_ptr(),
                libc::RTLD_NOW,
            )
        };
        assert!(missing.is_null());
        assert!(glibc_versioned_function(b"dl_iterate_phdr", b"GLIBC_2.2.5").is_some());
        assert!(glibc_versioned_function(b"_dl_find_object", b"GLIBC_2.99").is_none());
        // SAFETY: dlerror has no preconditions; a non-null result is a
        // NUL-terminated message.
        let error = unsafe { libc::dlerror() };
        assert!(!error.is_null(), "the pending dlerror message is gone");
        // SAFETY: as above.
        let message = unsafe { CStr::from_ptr(error) }.to_string_lossy();
        assert!(
            message.contains("reverie-glibc-symbol-missing.so"),
            "{message}"
        );
    }

    /// libc's soname matches exactly "libc.so.6", not a prefix or extension
    /// of it.
    #[test]
    fn a_string_matches_only_itself() {
        unsafe extern "C" fn visit(
            info: *mut libc::dl_phdr_info,
            _size: libc::size_t,
            data: *mut c_void,
        ) -> c_int {
            // SAFETY: as in `find`.
            let (checks, info) = unsafe { (&mut *data.cast::<[bool; 3]>(), &*info) };
            // SAFETY: as in `find`.
            let Some(object) = (unsafe { Object::new(info) }) else {
                return 0;
            };
            if !object.soname_is(b"libc.so.6") {
                return 0;
            }
            *checks = [
                true,
                object.soname_is(b"libc.so"),
                object.soname_is(b"libc.so.6x"),
            ];
            1
        }
        let mut checks = [false; 3];
        // SAFETY: `visit` matches the callback contract; `checks` outlives it.
        unsafe { libc::dl_iterate_phdr(Some(visit), (&raw mut checks).cast()) };
        assert_eq!(
            checks,
            [true, false, false],
            "[found libc, prefix matched, extension matched]"
        );
    }

    #[test]
    fn the_hashes_are_the_elf_ones() {
        assert_eq!(gnu_hash(b""), 5381);
        assert_eq!(gnu_hash(b"printf"), 0x156b_2bb8);
        assert_eq!(sysv_hash(b""), 0);
        assert_eq!(sysv_hash(b"printf"), 0x0779_05a6);
    }
}
