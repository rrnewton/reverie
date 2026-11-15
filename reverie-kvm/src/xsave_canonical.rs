/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Canonicalization of fixed-component (x87/SSE/YMM) XSAVE images to the
//! always-set + INIT form, for images Hermit passes to `KVM_SET_XSAVE`.
//! See `ai_docs/plans/2026-11-15-intel-v3-support.md` (dev-hermit).
//!
//! Provenance rule (agreed with the KVM-v3 lane): a clear source
//! `XSTATE_BV` bit means the payload bytes are not evidence — the kernel
//! may leave them stale (the measured stale-MXCSR case) — so that
//! component canonicalizes to exact INIT, MXCSR included. A set bit
//! makes the payload authoritative: exact-INIT payloads are rewritten
//! to the canonical INIT bytes, non-INIT payloads are preserved.

use kvm_bindings::kvm_xsave;

const XSAVE_SIZE: usize = 832;
const XMM_OFFSET: usize = 160;
const XMM_END: usize = 416;
const MXCSR_OFFSET: usize = 24;
const ST_SPACE_OFFSET: usize = 32;
const HEADER_OFFSET: usize = 512;
const YMM_OFFSET: usize = 576;
const MXCSR_INIT: u32 = 0x1f80;
const FCW_INIT: u16 = 0x037f;

/// The canonical INIT bytes of the x87 component's non-MXCSR legacy
/// fields: FCW, zero FSW/FOP/FIP/FDP, abridged FTW all-empty, zero
/// registers. Byte 5 and MXCSR/MXCSR_MASK (24..32) are not x87-owned.
fn x87_is_init(bytes: &[u8; XSAVE_SIZE]) -> bool {
    // Abridged FTW (byte 4) is 0 when every x87 register is empty.
    bytes[0..2] == FCW_INIT.to_le_bytes()
        && bytes[2..4] == [0, 0]
        && bytes[4] == 0
        && bytes[5] == 0
        && bytes[6..24].iter().all(|byte| *byte == 0)
        && bytes[ST_SPACE_OFFSET..XMM_OFFSET]
            .iter()
            .all(|byte| *byte == 0)
}

fn write_x87_init(bytes: &mut [u8; XSAVE_SIZE]) {
    bytes[0..24].fill(0);
    bytes[0..2].copy_from_slice(&FCW_INIT.to_le_bytes());
    bytes[ST_SPACE_OFFSET..XMM_OFFSET].fill(0);
}

fn sse_is_init(bytes: &[u8; XSAVE_SIZE]) -> bool {
    u32::from_le_bytes(bytes[MXCSR_OFFSET..MXCSR_OFFSET + 4].try_into().unwrap()) == MXCSR_INIT
        && bytes[XMM_OFFSET..XMM_END].iter().all(|byte| *byte == 0)
}

fn write_sse_init(bytes: &mut [u8; XSAVE_SIZE]) {
    bytes[MXCSR_OFFSET..MXCSR_OFFSET + 4].copy_from_slice(&MXCSR_INIT.to_le_bytes());
    bytes[XMM_OFFSET..XMM_END].fill(0);
}

fn ymm_is_init(bytes: &[u8; XSAVE_SIZE]) -> bool {
    bytes[YMM_OFFSET..XSAVE_SIZE].iter().all(|byte| *byte == 0)
}

/// Rewrite the 832-byte standard-format prefix of `xsave` to the
/// always-set + INIT canonical form for the fixed x87/SSE/YMM guest.
/// Returns whether any byte changed. Idempotent byte-for-byte.
/// `MXCSR_MASK` (not state) and bytes beyond 832 are untouched.
pub(crate) fn canonicalize_kvm_xsave(xsave: &mut kvm_xsave) -> bool {
    let mut bytes = [0u8; XSAVE_SIZE];
    for (index, word) in xsave.region[..XSAVE_SIZE / 4].iter().enumerate() {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    let before = bytes;
    let source_bv = u64::from_le_bytes(bytes[HEADER_OFFSET..HEADER_OFFSET + 8].try_into().unwrap());
    if source_bv & 0b001 == 0 || x87_is_init(&bytes) {
        write_x87_init(&mut bytes);
    }
    if source_bv & 0b010 == 0 || sse_is_init(&bytes) {
        write_sse_init(&mut bytes);
    }
    if source_bv & 0b100 == 0 || ymm_is_init(&bytes) {
        bytes[YMM_OFFSET..XSAVE_SIZE].fill(0);
    }
    // Legacy padding and software-reserved bytes are not state in a
    // KVM_SET_XSAVE image; canonical form carries none.
    bytes[XMM_END..HEADER_OFFSET].fill(0);
    bytes[HEADER_OFFSET..HEADER_OFFSET + 8].copy_from_slice(&0x7u64.to_le_bytes());
    bytes[HEADER_OFFSET + 8..YMM_OFFSET].fill(0);
    if bytes == before {
        return false;
    }
    for (index, chunk) in bytes.as_chunks::<4>().0.iter().enumerate() {
        xsave.region[index] = u32::from_le_bytes(*chunk);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::canonicalize_kvm_xsave;
    use kvm_bindings::kvm_xsave;

    const MXCSR: usize = 24;
    const MXCSR_MASK: usize = 28;
    const XMM: usize = 160;
    const BV: usize = 512;
    const YMM: usize = 576;

    fn bytes(xsave: &kvm_xsave) -> Vec<u8> {
        xsave
            .region
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect()
    }

    fn put32(xsave: &mut kvm_xsave, offset: usize, value: u32) {
        xsave.region[offset / 4] = value;
    }

    fn put64(xsave: &mut kvm_xsave, offset: usize, value: u64) {
        xsave.region[offset / 4] = value as u32;
        xsave.region[offset / 4 + 1] = (value >> 32) as u32;
    }

    fn init_image() -> kvm_xsave {
        let mut xsave = kvm_xsave::default();
        put32(&mut xsave, 0, 0x037f);
        xsave.region[1] = 0; // FTW abridged tag byte (offset 4): 0 = all empty.
        put32(&mut xsave, MXCSR, 0x1f80);
        put32(&mut xsave, MXCSR_MASK, 0xffbf);
        xsave
    }

    fn bv(xsave: &kvm_xsave) -> u64 {
        u64::from(xsave.region[128]) | u64::from(xsave.region[129]) << 32
    }

    #[test]
    fn clear_bit_payload_is_not_evidence_and_becomes_exact_init() {
        // GET-derived stale MXCSR: SSE bit clear, stale MXCSR/XMM bytes.
        let mut xsave = init_image();
        put64(&mut xsave, BV, 0x5); // x87 + YMM set, SSE clear
        put32(&mut xsave, MXCSR, 0x3f80);
        put32(&mut xsave, XMM, 0xdead_beef);
        assert!(canonicalize_kvm_xsave(&mut xsave));
        let got = bytes(&xsave);
        assert_eq!(bv(&xsave), 0x7);
        assert_eq!(u32::from_le_bytes(got[MXCSR..MXCSR + 4].try_into().unwrap()), 0x1f80);
        assert!(got[XMM..XMM + 256].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn set_bit_noninit_mxcsr_and_payloads_are_preserved() {
        let mut xsave = init_image();
        put64(&mut xsave, BV, 0x7);
        put32(&mut xsave, 0, 0x027f); // non-INIT x87 control word
        put32(&mut xsave, MXCSR, 0x3f80);
        put32(&mut xsave, YMM, 0x3333_4444);
        let before = bytes(&xsave);
        assert!(!canonicalize_kvm_xsave(&mut xsave));
        assert_eq!(bytes(&xsave), before);
    }

    #[test]
    fn exact_init_with_clear_bits_becomes_always_set_and_idempotent() {
        let mut xsave = init_image();
        put64(&mut xsave, BV, 0);
        put64(&mut xsave, BV + 8, 1 << 63); // compacted marker must go
        assert!(canonicalize_kvm_xsave(&mut xsave));
        assert_eq!(bv(&xsave), 0x7);
        let once = bytes(&xsave);
        assert!(!canonicalize_kvm_xsave(&mut xsave));
        assert_eq!(bytes(&xsave), once);
    }

    #[test]
    fn beyond_832_bytes_and_mxcsr_mask_are_untouched() {
        let mut xsave = init_image();
        put64(&mut xsave, BV, 0);
        xsave.region[300] = 0x5a5a_5a5a;
        canonicalize_kvm_xsave(&mut xsave);
        assert_eq!(xsave.region[300], 0x5a5a_5a5a);
        assert_eq!(xsave.region[MXCSR_MASK / 4], 0xffbf);
    }
}
