pub(crate) fn compare_native_xsave(before: &[u8], after: &[u8]) -> Result<bool, &'static str> {
    if before.len() != 2440 || after.len() != 2440 {
        return Err("unsupported XSAVE image size");
    }
    if before[520..528] != [0; 8] || after[520..528] != [0; 8] {
        return Err("unsupported compacted XSAVE image");
    }
    if before[..512] != after[..512] || before[520..] != after[520..] {
        return Err("XSAVE payload or other header field changed");
    }
    let before_bv = u64::from_le_bytes(before[512..520].try_into().unwrap());
    let after_bv = u64::from_le_bytes(after[512..520].try_into().unwrap());
    if before_bv & !0x2e7 != 0 {
        return Err("unsupported XSAVE state component");
    }
    if before_bv == after_bv {
        return Ok(false);
    }
    if before_bv & 0x80 != 0
        && after_bv == before_bv & !0x80
        && before[1408..2432].iter().all(|byte| *byte == 0)
        && after[1408..2432].iter().all(|byte| *byte == 0)
    {
        return Ok(true);
    }
    Err("impermissible XSTATE_BV transition")
}

#[cfg(test)]
mod tests {
    use super::compare_native_xsave;

    fn initialized_hi16() -> [u8; 2440] {
        let mut image = [0; 2440];
        image[512..520].copy_from_slice(&0x2a7u64.to_le_bytes());
        image
    }

    #[test]
    fn identical_images_have_no_header_transition() {
        let before = initialized_hi16();
        assert_eq!(compare_native_xsave(&before, &before), Ok(false));
    }

    #[test]
    fn initialized_hi16_clear_is_explicit_not_full_byte_equality() {
        let before = initialized_hi16();
        let mut after = before;
        after[512] &= !0x80;
        assert_ne!(before, after);
        assert_eq!(compare_native_xsave(&before, &after), Ok(true));
    }

    #[test]
    fn every_other_image_bit_remains_exact() {
        let before = initialized_hi16();
        let mut accepted = before;
        accepted[512] &= !0x80;
        for offset in 0..2440 {
            for bit in 0..8 {
                let mut changed = accepted;
                changed[offset] ^= 1 << bit;
                if offset == 512 && bit == 7 {
                    assert_eq!(compare_native_xsave(&before, &changed), Ok(false));
                } else {
                    assert!(
                        compare_native_xsave(&before, &changed).is_err(),
                        "offset={offset} bit={bit}"
                    );
                }
            }
        }
    }

    #[test]
    fn nonzero_hi16_rejects_a_falsely_permitted_mask() {
        for offset in 1408..2432 {
            let mut before = initialized_hi16();
            before[offset] = 1;
            let mut after = before;
            after[512] &= !0x80;
            assert_eq!(before[512] & !0x80, after[512]);
            assert_eq!(&before[1408..2432], &after[1408..2432]);
            assert!(
                compare_native_xsave(&before, &after).is_err(),
                "offset={offset}"
            );
        }
    }

    #[test]
    fn setting_the_bit_is_not_the_permitted_clear() {
        let mut before = initialized_hi16();
        before[512] &= !0x80;
        let mut after = before;
        after[512] |= 0x80;
        assert!(compare_native_xsave(&before, &after).is_err());
    }

    #[test]
    fn sizes_and_compacted_format_are_not_accepted() {
        let before = initialized_hi16();
        assert!(compare_native_xsave(&before[..2439], &before).is_err());
        assert!(compare_native_xsave(&before, &before[..2439]).is_err());
        let mut compacted = before;
        compacted[527] = 0x80;
        assert!(compare_native_xsave(&compacted, &compacted).is_err());
    }
}
