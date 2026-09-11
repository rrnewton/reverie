/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Provides APIs to disable VDSOs at runtime.
use nix::sys::mman::ProtFlags;
use reverie::Errno;
use reverie::Error;
use reverie::Guest;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::AddrMut;
use reverie::syscalls::MemoryAccess;
use reverie::syscalls::Mprotect;
pub use reverie::vdso::VdsoSyscallSite;
pub use reverie::vdso::is_patch_required;
#[cfg(target_arch = "x86_64")]
pub use reverie::vdso::patch_current_vdso;
use reverie::vdso::subscribed_vdso_patches;
use tracing::debug;

/// patch VDSOs when enabled
///
/// `guest` must be in one of ptrace's stopped states.
pub async fn vdso_patch<G, T>(guest: &mut G, subscriptions: &Subscription) -> Result<(), Error>
where
    G: Guest<T>,
    T: Tool,
{
    if let Some(vdso) = procfs::process::Process::new(guest.pid().as_raw())
        .map_or_else(
            |_| Vec::new(),
            |p| match p.maps() {
                Ok(maps) => maps.0,
                Err(_) => Vec::new(),
            },
        )
        .iter()
        .find(|e| e.pathname == procfs::process::MMapPath::Vdso)
    {
        let mut memory = guest.memory();

        // Allow write access to the vdso memory page.
        guest
            .inject_with_retry(
                Mprotect::new()
                    .with_addr(AddrMut::from_raw(vdso.address.0 as usize))
                    .with_len((vdso.address.1 - vdso.address.0) as usize)
                    .with_protection(
                        ProtFlags::PROT_READ | ProtFlags::PROT_WRITE | ProtFlags::PROT_EXEC,
                    ),
            )
            .await?;

        for (name, (offset, size, bytes, _sysno)) in subscribed_vdso_patches(subscriptions) {
            let start = vdso.address.0 + offset;
            assert!(bytes.len() <= *size);
            let rptr = AddrMut::from_raw(start as usize).ok_or(Errno::EFAULT)?;
            memory.write_exact(rptr, bytes)?;
            assert!(*size >= bytes.len());
            if *size > bytes.len() {
                let fill: Vec<u8> = std::iter::repeat_n(0x90u8, size - bytes.len()).collect();
                memory.write_exact(unsafe { rptr.add(bytes.len()) }, &fill)?;
            }
            debug!("{} patched {}@{:x}", guest.pid(), name, start);
        }

        guest
            .inject_with_retry(
                Mprotect::new()
                    .with_addr(AddrMut::from_raw(vdso.address.0 as usize))
                    .with_len((vdso.address.1 - vdso.address.0) as usize)
                    .with_protection(ProtFlags::PROT_READ | ProtFlags::PROT_EXEC),
            )
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn old_site_path_is_the_shared_type() {
        let site = reverie::vdso::VdsoSyscallSite {
            address: 17,
            number: 23,
            mapping_start: 16,
            mapping_len: 4096,
        };
        let old: crate::VdsoSyscallSite = site;
        let shared: reverie::vdso::VdsoSyscallSite = old;
        assert_eq!(shared.address, 17);
        assert_eq!(shared.number, 23);
        assert_eq!(shared.mapping_start, 16);
        assert_eq!(shared.mapping_len, 4096);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn old_patch_path_is_the_shared_function() {
        type Patch =
            fn(&reverie::Subscription) -> Result<Vec<crate::VdsoSyscallSite>, reverie::Error>;
        let old: Patch = crate::patch_current_vdso;
        let shared: Patch = reverie::vdso::patch_current_vdso;
        assert!(std::ptr::fn_addr_eq(old, shared));
    }
}
