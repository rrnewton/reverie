use super::*;

const FRAME: usize = 8;
const FLOATING: usize = 768;
const STANDARD: Format = Format::StandardXsave {
    xfeatures: 7,
    xstate_size: 832,
};

#[repr(align(64))]
struct Storage([u8; 2048]);

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn fixture(frame: usize, floating: usize, format: Format) -> Box<Storage> {
    let mut source = Box::new(Storage([0x5a; 2048]));
    populate(&mut source.0, frame, floating, format);
    source
}

fn populate(source: &mut [u8], frame: usize, floating: usize, format: Format) {
    let address = source.as_ptr() as usize;
    put_u64(source, frame + FP_POINTER, (address + floating) as u64);
    match format {
        Format::Legacy => {
            put_u64(source, frame + UC_FLAGS, 6);
            put_u32(source, floating + 464, 0);
        }
        Format::StandardXsave {
            xfeatures,
            xstate_size,
        } => {
            put_u64(source, frame + UC_FLAGS, 7);
            put_u32(source, floating + 464, MAGIC1);
            put_u32(source, floating + 468, xstate_size + 4);
            put_u64(source, floating + 472, xfeatures);
            put_u32(source, floating + 480, xstate_size);
            source[floating + 484..floating + 512].fill(0);
            put_u64(source, floating + 512, xfeatures);
            source[floating + 520..floating + XSAVE_HEADER_END].fill(0);
            put_u32(source, floating + xstate_size as usize, MAGIC2);
        }
    }
}

fn reject(source: &[u8], frame: usize, restorer: usize, format: Format, expected: Error) {
    let mut destination = Storage([0xa7; 2048]);
    assert_eq!(
        relocate(source, frame, restorer, format, &mut destination.0).unwrap_err(),
        expected
    );
    assert_eq!(destination.0, [0xa7; 2048]);
}

#[test]
fn standard_copy_preserves_every_byte_except_relocated_pointer() {
    let source = fixture(FRAME, FLOATING, STANDARD);
    let original = source.0;
    let address = source.0.as_ptr() as usize + FRAME;
    let mut destination = Storage([0xa7; 2048]);
    let image = relocate(
        &source.0,
        address,
        address + 8,
        STANDARD,
        &mut destination.0,
    )
    .unwrap();
    let mut expected = original[FRAME..FRAME + PREFIX_BYTES].to_vec();
    put_u64(&mut expected, FP_POINTER, image.fp_bytes().as_ptr() as u64);
    assert_eq!(image.frame_bytes(), expected);
    assert_eq!(image.fp_bytes(), &original[FLOATING..FLOATING + 836]);
    assert_eq!(image.frame_bytes().as_ptr() as usize % 16, 8);
    assert_eq!(image.fp_bytes().as_ptr() as usize % 64, 0);
    assert_eq!(source.0, original);
    assert_eq!(&destination.0[..DEST_FRAME], &[0; DEST_FRAME]);
    assert_eq!(
        &destination.0[DEST_FP + 836..],
        &[0xa7; 2048 - DEST_FP - 836]
    );
}

#[test]
fn output_does_not_borrow_retired_source() {
    let mut source = fixture(FRAME, FLOATING, STANDARD);
    let address = source.0.as_ptr() as usize + FRAME;
    let expected = source.0[FLOATING..FLOATING + 836].to_vec();
    let mut destination = Storage([0xa7; 2048]);
    let image = relocate(
        &source.0,
        address,
        address + 8,
        STANDARD,
        &mut destination.0,
    )
    .unwrap();
    source.0.fill(0xcc);
    drop(source);
    assert_eq!(image.fp_bytes(), expected);
    assert_eq!(
        read_u64(image.frame_bytes(), FP_POINTER).unwrap(),
        image.fp_bytes().as_ptr() as u64
    );
}

#[test]
fn legacy_reserved_bytes_survive_and_only_sixteen_byte_fp_alignment_is_required() {
    let floating = FLOATING + 16;
    let source = fixture(FRAME, floating, Format::Legacy);
    let address = source.0.as_ptr() as usize + FRAME;
    let mut destination = Storage([0xa7; 2048]);
    let image = relocate(
        &source.0,
        address,
        address + 8,
        Format::Legacy,
        &mut destination.0,
    )
    .unwrap();
    assert_eq!(
        image.fp_bytes(),
        &source.0[floating..floating + LEGACY_BYTES]
    );
    assert_eq!(&image.fp_bytes()[468..512], &[0x5a; 44]);
}

#[test]
fn floating_span_can_precede_prefix_without_copying_the_gap() {
    let frame = 1032;
    let floating = 64;
    let source = fixture(frame, floating, STANDARD);
    let address = source.0.as_ptr() as usize + frame;
    let mut destination = Storage([0xa7; 2048]);
    let image = relocate(
        &source.0,
        address,
        address + 8,
        STANDARD,
        &mut destination.0,
    )
    .unwrap();
    assert_eq!(image.fp_bytes(), &source.0[floating..floating + 836]);
    assert_eq!(image.frame_bytes().len(), PREFIX_BYTES);
}

#[test]
fn every_truncated_prefix_or_floating_span_refuses_without_writes() {
    let source = fixture(FRAME, FLOATING, STANDARD);
    let address = source.0.as_ptr() as usize + FRAME;
    for length in 0..FRAME + PREFIX_BYTES {
        reject(
            &source.0[..length],
            address,
            address + 8,
            STANDARD,
            Error::SourceBounds,
        );
    }
    for length in FLOATING..FLOATING + 836 {
        reject(
            &source.0[..length],
            address,
            address + 8,
            STANDARD,
            Error::SourceBounds,
        );
    }
}

#[test]
fn pointed_span_cannot_escape_or_overlap_source_prefix() {
    for (offset, expected) in [
        (0, Error::Overlap),
        (64, Error::Overlap),
        (2048, Error::SourceBounds),
        (2048 - LEGACY_BYTES + 1, Error::SourceBounds),
    ] {
        let mut source = fixture(FRAME, FLOATING, STANDARD);
        let base = source.0.as_ptr() as usize;
        put_u64(&mut source.0, FRAME + FP_POINTER, (base + offset) as u64);
        reject(
            &source.0,
            base + FRAME,
            base + FRAME + 8,
            STANDARD,
            expected,
        );
    }
    let mut source = fixture(FRAME, FLOATING, STANDARD);
    let base = source.0.as_ptr() as usize;
    for (pointer, expected) in [
        ((base - 64) as u64, Error::SourceBounds),
        (u64::MAX - 255, Error::Overflow),
        (0, Error::NullFpUnsupported),
    ] {
        put_u64(&mut source.0, FRAME + FP_POINTER, pointer);
        reject(
            &source.0,
            base + FRAME,
            base + FRAME + 8,
            STANDARD,
            expected,
        );
    }
}

#[test]
fn late_floating_overlap_is_rejected() {
    let frame = 1032;
    let mut source = fixture(frame, 512, STANDARD);
    put_u64(&mut source.0, frame + UC_FLAGS, 7);
    let base = source.0.as_ptr() as usize;
    put_u64(&mut source.0, frame + FP_POINTER, (base + 512) as u64);
    reject(
        &source.0,
        base + frame,
        base + frame + 8,
        STANDARD,
        Error::Overlap,
    );
}

#[test]
fn frame_and_restorer_relation_and_alignment_are_checked() {
    let source = fixture(FRAME, FLOATING, STANDARD);
    let base = source.0.as_ptr() as usize;
    reject(
        &source.0,
        base + FRAME,
        base + FRAME,
        STANDARD,
        Error::RestorerPosition,
    );
    reject(&source.0, base, base + 8, STANDARD, Error::Alignment);
    reject(&source.0, base - 1, base + 7, STANDARD, Error::SourceBounds);
    reject(&source.0, usize::MAX - 7, 0, STANDARD, Error::Overflow);
    let source = fixture(FRAME, FLOATING + 16, STANDARD);
    let address = source.0.as_ptr() as usize + FRAME;
    reject(&source.0, address, address + 8, STANDARD, Error::Alignment);
}

#[test]
fn every_short_or_misaligned_destination_refuses_without_writes() {
    let source = fixture(FRAME, FLOATING, STANDARD);
    let address = source.0.as_ptr() as usize + FRAME;
    for length in 0..DEST_FP + 836 {
        let mut destination = Storage([0xa7; 2048]);
        assert_eq!(
            relocate(
                &source.0,
                address,
                address + 8,
                STANDARD,
                &mut destination.0[..length]
            )
            .unwrap_err(),
            Error::DestinationBounds
        );
        assert_eq!(destination.0, [0xa7; 2048]);
    }
    for offset in 1..64 {
        let mut destination = Storage([0xa7; 2048]);
        assert_eq!(
            relocate(
                &source.0,
                address,
                address + 8,
                STANDARD,
                &mut destination.0[offset..]
            )
            .unwrap_err(),
            Error::Alignment
        );
        assert_eq!(destination.0, [0xa7; 2048]);
    }
}

#[test]
fn metadata_trailer_and_reserved_header_corruption_refuse() {
    for offset in [
        468, 469, 470, 471, 472, 479, 480, 483, 484, 511, 519, 520, 527, 528, 575, 832, 835,
    ] {
        let mut source = fixture(FRAME, FLOATING, STANDARD);
        source.0[FLOATING + offset] ^= 0x80;
        let address = source.0.as_ptr() as usize + FRAME;
        reject(&source.0, address, address + 8, STANDARD, Error::Metadata);
    }
}

#[test]
fn format_flags_features_and_compacted_layout_are_not_silently_repaired() {
    for flags in [0, 1, 2, 3, 4, 5, 6, 15, u64::MAX] {
        let mut source = fixture(FRAME, FLOATING, STANDARD);
        put_u64(&mut source.0, FRAME + UC_FLAGS, flags);
        let address = source.0.as_ptr() as usize + FRAME;
        reject(
            &source.0,
            address,
            address + 8,
            STANDARD,
            Error::UnsupportedFormat,
        );
    }
    let source = fixture(FRAME, FLOATING, STANDARD);
    let address = source.0.as_ptr() as usize + FRAME;
    for format in [
        Format::Legacy,
        Format::StandardXsave {
            xfeatures: 0,
            xstate_size: 832,
        },
        Format::StandardXsave {
            xfeatures: 7 | (1 << 8),
            xstate_size: 832,
        },
        Format::StandardXsave {
            xfeatures: 7 | (1 << 63),
            xstate_size: 832,
        },
        Format::StandardXsave {
            xfeatures: 7,
            xstate_size: 575,
        },
    ] {
        reject(
            &source.0,
            address,
            address + 8,
            format,
            Error::UnsupportedFormat,
        );
    }
    reject(
        &source.0,
        address,
        address + 8,
        Format::StandardXsave {
            xfeatures: 3,
            xstate_size: 832,
        },
        Error::Metadata,
    );
    reject(
        &source.0,
        address,
        address + 8,
        Format::StandardXsave {
            xfeatures: 7,
            xstate_size: u32::MAX,
        },
        Error::Overflow,
    );
    let mut source = source;
    put_u64(&mut source.0, FLOATING + 520, 1 << 63);
    reject(&source.0, address, address + 8, STANDARD, Error::Metadata);
    put_u32(&mut source.0, FLOATING + 464, 0);
    reject(
        &source.0,
        address,
        address + 8,
        STANDARD,
        Error::UnsupportedFormat,
    );
}

#[test]
fn absent_active_components_do_not_shrink_the_copied_payload() {
    let mut source = fixture(FRAME, FLOATING, STANDARD);
    put_u64(&mut source.0, FLOATING + 512, 0);
    let address = source.0.as_ptr() as usize + FRAME;
    let mut destination = Storage([0xa7; 2048]);
    let image = relocate(
        &source.0,
        address,
        address + 8,
        STANDARD,
        &mut destination.0,
    )
    .unwrap();
    assert_eq!(image.fp_bytes().len(), 836);
    assert_eq!(image.fp_bytes(), &source.0[FLOATING..FLOATING + 836]);
}

#[test]
fn checked_range_arithmetic_and_allocation_overlap_do_not_need_aliased_references() {
    assert_eq!(allocation(usize::MAX - 7, 8), Err(Error::Overflow));
    assert_eq!(span(&(64..128), usize::MAX - 1, 4), Err(Error::Overflow));
    assert_eq!(span(&(64..128), 63, 1), Err(Error::SourceBounds));
    assert_eq!(span(&(64..128), 127, 2), Err(Error::SourceBounds));
    assert_eq!(span(&(64..128), 64, 64), Ok(0..64));
    assert!(overlaps(&(64..128), &(64..128)));
    assert!(overlaps(&(64..128), &(127..256)));
    assert!(overlaps(&(64..128), &(0..65)));
    assert!(!overlaps(&(64..128), &(128..256)));
    assert!(!overlaps(&(64..128), &(96..96)));
    assert!(!overlaps(&(96..96), &(64..128)));
}

#[test]
fn supplied_host_sized_synthetic_payload_is_not_capped_to_a_smaller_default() {
    #[repr(align(64))]
    struct LargeStorage([u8; 4096]);
    let format = Format::StandardXsave {
        xfeatures: 0x2e7,
        xstate_size: 2440,
    };
    let mut source = Box::new(LargeStorage([0x5a; 4096]));
    populate(&mut source.0, FRAME, FLOATING, format);
    let address = source.0.as_ptr() as usize + FRAME;
    let mut destination = LargeStorage([0xa7; 4096]);
    let image = relocate(&source.0, address, address + 8, format, &mut destination.0).unwrap();
    assert_eq!(image.fp_bytes().len(), 2444);
    assert_eq!(image.fp_bytes(), &source.0[FLOATING..FLOATING + 2444]);
}
