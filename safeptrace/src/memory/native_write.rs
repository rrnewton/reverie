/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Separately admitted native store under an original-task reservation. The
//! reader's predicates and transports remain unchanged. This module supplies
//! no MM membership, external-writer exclusion, or syscall-result authority.

use reverie_memory::NativeUserStoreOutcome as Outcome;
use reverie_memory::NativeUserStoreRefusal as StoreRefusal;

use super::*;

fn evidence(error: Error) -> StoreRefusal {
    match error {
        Error::Refused(reason) => StoreRefusal::Evidence(reason),
        // Shared mechanics below never classify read access. Refuse if that
        // changes; a read fault cannot establish a write or syscall errno.
        Error::Fault(_) => StoreRefusal::Evidence(Refusal::UnsupportedMapping),
    }
}
fn state(error: Errno) -> StoreRefusal {
    StoreRefusal::Evidence(Refusal::TargetState(error))
}

impl Mapping<'_> {
    fn write_access(&self, pkru: u32) -> Result<(), StoreRefusal> {
        self.complete().map_err(evidence)?;
        if self.kernel_page != Some(PAGE) || self.mmu_page != Some(PAGE) {
            return Err(StoreRefusal::Evidence(Refusal::UnsupportedMapping));
        }
        for flag in self.flags.as_ref().unwrap() {
            if !matches!(
                *flag,
                "rd" | "wr"
                    | "ex"
                    | "sh"
                    | "mr"
                    | "mw"
                    | "me"
                    | "ms"
                    | "gd"
                    | "lo"
                    | "lf"
                    | "sr"
                    | "rr"
                    | "dc"
                    | "de"
                    | "ac"
                    | "nr"
                    | "wf"
                    | "dd"
                    | "sd"
                    | "hg"
                    | "nh"
                    | "mg"
                    | "sl"
            ) {
                return Err(StoreRefusal::Evidence(Refusal::UnsupportedMapping));
            }
        }
        self.private_backing().map_err(evidence)?;
        if self.permissions[1] != b'w' {
            return Err(StoreRefusal::WriteDenied);
        }
        let key = self.key.unwrap();
        // Linux __pkru_allows_write: both AD and WD deny writing.
        if pkru & (3u32 << (2 * key)) != 0 {
            return Err(StoreRefusal::ProtectionKey(key));
        }
        Ok(())
    }
}

fn qualify(
    permit: &crate::NativeStorePermit<'_>,
    address: usize,
    length: usize,
) -> Result<(), StoreRefusal> {
    let end = range_end(address, length).map_err(evidence)?;
    permit.validate().map_err(state)?;
    if unsafe { libc::sysconf(libc::_SC_PAGESIZE) } != PAGE as libc::c_long {
        return Err(StoreRefusal::Evidence(Refusal::UnsupportedPlatform));
    }
    let control = permit.control();
    let tid = control.expected_tid();
    if tid <= 0 || permit.stopped().pid().as_raw() != tid {
        return Err(StoreRefusal::Evidence(Refusal::WrongTask));
    }
    validate_native_mode(permit.stopped()).map_err(evidence)?;
    let layout = native_pkru_layout()
        .map_err(|_| StoreRefusal::Evidence(Refusal::UnsupportedPlatform))?
        .ok_or(StoreRefusal::Evidence(Refusal::UnsupportedPlatform))?;
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open("/proc")
        .map_err(proc_error)
        .map_err(evidence)?;
    let mount = verify_proc(&root, None).map_err(evidence)?;
    let caller_tid =
        Errno::result(unsafe { libc::syscall(libc::SYS_gettid) }).map_err(state)? as usize;
    proc_view(
        &proc_file(&root, mount, "thread-self/status", MAX_STATUS).map_err(evidence)?,
        caller_tid,
    )
    .map_err(evidence)?;
    let original = control.task_directory();
    mm_bound::directory_identity(original, control.task_directory_identity()).map_err(evidence)?;
    verify_proc_fd(original, Some(mount)).map_err(evidence)?;
    // Duplicate the already retained original directory; never reopen by TID.
    let original = File::from(
        original
            .try_clone_to_owned()
            .map_err(proc_error)
            .map_err(evidence)?,
    );
    mm_bound::target_proc_view(
        &proc_file(&original, mount, "status", MAX_STATUS).map_err(evidence)?,
        tid as usize,
    )
    .map_err(evidence)?;
    let smaps = proc_file(&original, mount, "smaps", MAX_SMAPS).map_err(evidence)?;
    let selected = mapping(&smaps, address, end).map_err(evidence)?;
    let xstate = permit
        .stopped()
        .getxstate()
        .map_err(target_error)
        .map_err(evidence)?;
    let pkru = decode_native_pkru_xstate(&xstate.0, layout)
        .map_err(|_| StoreRefusal::Evidence(Refusal::UnsupportedPlatform))?;
    selected.write_access(pkru)?;
    permit.validate().map_err(state)
}

pub(crate) fn write(
    permit: &crate::NativeStorePermit<'_>,
    address: usize,
    bytes: &[u8],
) -> Outcome {
    if let Err(error) = qualify(permit, address, bytes.len()) {
        return Outcome::Refused(error);
    }
    let local = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    let remote = libc::iovec {
        iov_base: address as *mut libc::c_void,
        iov_len: bytes.len(),
    };
    // AUTONOMOUS-BOT-IMPLEMENTED; TODO-HUMAN-REVIEW
    // https://github.com/rrnewton/reverie/pull/897
    // The distinct original-task ticket prevents notifier reap/PID reuse across
    // this one numeric transfer. No Rust reference to the remote bytes exists.
    // It does not prevent externally authorized fatal kernel teardown.
    #[cfg(test)]
    TRANSFERS.with(|count| count.set(count.get() + 1));
    let raw = unsafe {
        syscalls::syscall!(
            syscalls::Sysno::process_vm_writev,
            permit.control().expected_tid(),
            &local as *const _,
            1usize,
            &remote as *const _,
            1usize,
            0usize
        )
    };
    // Retain the actual result BEFORE any check that can fail after the effect.
    #[cfg(test)]
    AFTER_STORE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().as_mut() {
            hook();
        }
    });
    let postcheck = permit.validate();
    Outcome::Attempted { raw, postcheck }
}

#[cfg(test)]
thread_local! {
    static AFTER_STORE: std::cell::RefCell<Option<Box<dyn FnMut()>>> = const { std::cell::RefCell::new(None) };
    static TRANSFERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
#[cfg(test)]
impl crate::NativeStorePermit<'_> {
    pub(crate) fn store_hook_for_test(hook: Option<Box<dyn FnMut()>>) {
        AFTER_STORE.with(|slot| *slot.borrow_mut() = hook);
    }
    pub(crate) fn store_count_for_test() -> usize {
        TRANSFERS.with(|count| count.replace(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn smaps(key: u8) -> String {
        format!(
            "1000-2000 rw-p 00000000 00:00 0\nKernelPageSize: 4 kB\nMMUPageSize: 4 kB\nLazyFree: 0 kB\nProtectionKey: {key}\nVmFlags: rd wr mr mw me ac sd\n"
        )
    }
    #[test]
    fn write_permission_uses_ad_and_wd_for_every_actual_mapping_key() {
        for key in 0..16 {
            let text = smaps(key);
            let map = mapping(text.as_bytes(), 0x1000, 0x1200).unwrap();
            for bits in 0u32..4 {
                assert_eq!(
                    map.write_access(bits << (2 * key)),
                    if bits == 0 {
                        Ok(())
                    } else {
                        Err(StoreRefusal::ProtectionKey(key))
                    }
                );
            }
            assert_eq!(map.write_access(3 << (2 * ((key + 1) % 16))), Ok(()));
        }
    }
    #[test]
    fn write_refuses_readonly_special_shared_file_and_discardable_mappings() {
        let base = smaps(0);
        let cases = [
            (
                base.replace("rw-p", "r--p").replace("rd wr", "rd"),
                StoreRefusal::WriteDenied,
            ),
            (
                base.replace("rw-p", "rw-s").replace("rd wr", "rd wr sh ms"),
                StoreRefusal::Evidence(Refusal::UnsupportedBacking),
            ),
            (
                base.replace("00:00 0", "08:01 12"),
                StoreRefusal::Evidence(Refusal::UnsupportedBacking),
            ),
            (
                base.replace("LazyFree: 0", "LazyFree: 4"),
                StoreRefusal::Evidence(Refusal::UnsupportedBacking),
            ),
            (
                base.replace("ac sd", "ac sd um"),
                StoreRefusal::Evidence(Refusal::UnsupportedMapping),
            ),
            (
                base.replace("ac sd", "ac sd de"),
                StoreRefusal::Evidence(Refusal::UnsupportedBacking),
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(
                mapping(text.as_bytes(), 0x1000, 0x1004)
                    .unwrap()
                    .write_access(0),
                Err(expected)
            );
        }
        let stack = base.replace("ac sd", "ac sd gd");
        assert_eq!(
            mapping(stack.as_bytes(), 0x1000, 0x1004)
                .unwrap()
                .write_access(0),
            Ok(())
        );
        assert!(matches!(
            mapping(stack.as_bytes(), 0xfff, 0x1003),
            Err(Error::Refused(Refusal::MappingMissing))
        ));
    }
}
