/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A signal handler starts with Linux's initial FPU state,
//! `XsaveImage::initialized_kvm`, which must cover every user state component
//! the guest enables.

use super::*;

#[test]
fn the_initial_fpu_image_covers_every_component_the_guest_enables() {
    let mut backend = match KvmBackend::new(16 * 1024 * 1024) {
        Ok(backend) => backend,
        Err(error) => {
            assert!(
                std::env::var_os("REVERIE_REQUIRE_KVM").is_none(),
                "KVM is required: {error}"
            );
            eprintln!("skipping the initial FPU image KVM test: {error}");
            return;
        }
    };
    backend
        .install_static_elf(&minimal_test_elf(&[0xf4]), "/initial-fpu-state")
        .unwrap();
    // The guest enables x87, SSE and YMM state, the components signal frames
    // carry, and its CPUID table offers no other.
    let xcrs = backend.vcpu.get_xcrs().unwrap();
    let xcr0 = xcrs.xcrs[..xcrs.nr_xcrs as usize]
        .iter()
        .find(|xcr| xcr.xcr == 0)
        .unwrap()
        .value;
    assert_eq!(xcr0, crate::signal::FIXED_XFEATURES);
    let cpuid = backend.vcpu.get_cpuid2(KVM_MAX_CPUID_ENTRIES).unwrap();
    let extended = cpuid
        .as_slice()
        .iter()
        .find(|entry| entry.function == 0xd && entry.index == 0)
        .unwrap();
    assert_eq!(
        u64::from(extended.eax) | u64::from(extended.edx) << 32,
        crate::signal::FIXED_XFEATURES
    );
    // CR4.PKE is clear and XCR0 has no PKRU bit: the guest has no PKRU, so
    // nothing stands for Linux's pkru_write_default.
    assert_eq!(
        kvm_ioctls::VcpuFd::get_sregs(&backend.vcpu).unwrap().cr4 & (1 << 22),
        0
    );
    // The image lists each enabled component in XSTATE_BV, so KVM_SET_XSAVE
    // loads each one from it, and is in the standard format KVM_SET_XSAVE
    // requires (XCOMP_BV zero).
    let image = XsaveImage::initialized_kvm();
    assert_eq!(
        u64::from(image.region[128]) | u64::from(image.region[129]) << 32,
        xcr0
    );
    assert_eq!((image.region[130], image.region[131]), (0, 0));
    // SAFETY: the image holds only the components the guest enables.
    unsafe { backend.vcpu.set_xsave(&image).unwrap() };
}
