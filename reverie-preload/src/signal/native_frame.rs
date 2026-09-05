//! Checked relocation of native Linux x86-64 signal-frame byte images.
//!
//! This is not kernel provenance authentication or permission to call sigreturn.
//! The caller supplies stable readable source bytes and an independently obtained
//! expected FP format. Kernel restore semantics, segment/flag validity, altstack
//! policy, dynamic xstate permissions and runtime ownership are separate gates.
//! No signal handler, allocation, native control or return path is installed here.

use std::ops::Range;

/// Native `rt_sigframe`: restorer word, kernel ucontext and 128-byte siginfo.
/// This is the Linux x86-64 ABI, not `size_of::<libc::ucontext_t>()`.
const PREFIX_BYTES: usize = 440;
const FP_POINTER: usize = 232;
const UC_FLAGS: usize = 8;
const LEGACY_BYTES: usize = 512;
const XSAVE_HEADER_END: usize = 576;
const MAGIC1: u32 = 0x4650_5853;
const MAGIC2: u32 = 0x4650_5845;
const USER_FEATURES: u64 = 0xe02ff;
const DEST_FRAME: usize = 8;
const DEST_FP: usize = 448;

/// Exact independently supplied format expectation, not discovered from input.
///
/// Standard XSAVE payloads are copied opaquely, without feature loss. The caller
/// must qualify the feature/size pair independently; matching it here does not
/// prove that a CPU or kernel accepts the layout. Unknown user features,
/// supervisor state and compacted XSAVE are unsupported.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    Legacy,
    StandardXsave { xfeatures: u64, xstate_size: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Overflow,
    SourceBounds,
    DestinationBounds,
    Overlap,
    Alignment,
    RestorerPosition,
    NullFpUnsupported,
    UnsupportedFormat,
    Metadata,
}

/// Immutable relocated bytes borrowing only the destination, never the source.
///
/// This is not a sealed runtime continuation or a consumed-generation witness.
/// There is no execution method or pool-reuse protocol. The destination cannot
/// be safely mutated while this view is in use; dropping it ends only a Rust
/// borrow, not a kernel-frame lifetime.
#[derive(Debug)]
pub struct RelocatedFrame<'destination> {
    bytes: &'destination [u8],
}

impl RelocatedFrame<'_> {
    pub fn frame_bytes(&self) -> &[u8] {
        &self.bytes[DEST_FRAME..DEST_FRAME + PREFIX_BYTES]
    }

    pub fn fp_bytes(&self) -> &[u8] {
        &self.bytes[DEST_FP..]
    }
}

/// Validate both source spans and relocate them into disjoint aligned storage.
///
/// `frame_address` and `restorer_sp` are asserted input addresses, NOT proof of
/// a kernel-origin event. They must describe the native frame/restorer relation
/// inside `source`; the FP pointer is checked against that same source allocation
/// before being followed as a slice offset. No raw pointer is dereferenced.
///
/// Destination starts at a 64-byte boundary; its frame begins eight bytes later,
/// so the restorer position is 16-byte aligned. FP data is separately 64-byte
/// aligned. Prefix/FP reserved data is preserved, except unsupported XSAVE header
/// fields reject. Null FP is refused until its semantics are qualified.
/// Every error leaves destination unchanged. Successful relocation changes only
/// the FP pointer and destination-only padding; it never repairs source data.
///
/// Safe borrowing prevents a stale view from observing destination reuse:
/// ```compile_fail
/// use reverie_preload::signal::native_frame::{relocate, Format};
/// let source = [0u8; 2048];
/// let mut destination = [0u8; 2048];
/// let view = relocate(&source, 0, 8, Format::Legacy, &mut destination).unwrap();
/// destination.fill(0);
/// assert!(!view.frame_bytes().is_empty());
/// ```
pub fn relocate<'destination>(
    source: &[u8],
    frame_address: usize,
    restorer_sp: usize,
    format: Format,
    destination: &'destination mut [u8],
) -> Result<RelocatedFrame<'destination>, Error> {
    let source_allocation = allocation(source.as_ptr() as usize, source.len())?;
    let destination_allocation = allocation(destination.as_ptr() as usize, destination.len())?;
    if overlaps(&source_allocation, &destination_allocation) {
        return Err(Error::Overlap);
    }
    let prefix = span(&source_allocation, frame_address, PREFIX_BYTES)?;
    if frame_address.checked_add(8).ok_or(Error::Overflow)? != restorer_sp {
        return Err(Error::RestorerPosition);
    }
    if frame_address % 16 != 8 || destination_allocation.start % 64 != 0 {
        return Err(Error::Alignment);
    }
    let prefix_bytes = &source[prefix.clone()];
    let fp_address =
        usize::try_from(read_u64(prefix_bytes, FP_POINTER)?).map_err(|_| Error::Overflow)?;
    if fp_address == 0 {
        return Err(Error::NullFpUnsupported);
    }
    let initial_fp = span(&source_allocation, fp_address, LEGACY_BYTES)?;
    if overlaps(&prefix, &initial_fp) {
        return Err(Error::Overlap);
    }
    let fp_length = validate_format(
        &source[initial_fp],
        read_u64(prefix_bytes, UC_FLAGS)?,
        format,
    )?;
    let fp = span(&source_allocation, fp_address, fp_length)?;
    let alignment = match format {
        Format::Legacy => 16,
        Format::StandardXsave { .. } => 64,
    };
    if !fp_address.is_multiple_of(alignment) {
        return Err(Error::Alignment);
    }
    if overlaps(&prefix, &fp) {
        return Err(Error::Overlap);
    }
    if let Format::StandardXsave { xfeatures, .. } = format {
        validate_xsave(&source[fp.clone()], xfeatures)?;
    }
    let required = DEST_FP.checked_add(fp_length).ok_or(Error::Overflow)?;
    if required > destination.len() {
        return Err(Error::DestinationBounds);
    }
    let relocated_fp = destination_allocation
        .start
        .checked_add(DEST_FP)
        .ok_or(Error::Overflow)?;
    destination[..DEST_FRAME].fill(0);
    destination[DEST_FRAME..DEST_FRAME + PREFIX_BYTES].copy_from_slice(prefix_bytes);
    destination[DEST_FRAME + PREFIX_BYTES..DEST_FP].fill(0);
    destination[DEST_FP..required].copy_from_slice(&source[fp]);
    destination[DEST_FRAME + FP_POINTER..DEST_FRAME + FP_POINTER + 8]
        .copy_from_slice(&(relocated_fp as u64).to_le_bytes());
    Ok(RelocatedFrame {
        bytes: &destination[..required],
    })
}

fn validate_format(fp: &[u8], flags: u64, format: Format) -> Result<usize, Error> {
    match format {
        Format::Legacy => {
            if flags != 6 || read_u32(fp, 464)? != 0 {
                return Err(Error::UnsupportedFormat);
            }
            Ok(LEGACY_BYTES)
        }
        Format::StandardXsave {
            xfeatures,
            xstate_size,
        } => {
            if flags != 7
                || xfeatures & 3 != 3
                || xfeatures & !USER_FEATURES != 0
                || xstate_size < XSAVE_HEADER_END as u32
                || read_u32(fp, 464)? != MAGIC1
            {
                return Err(Error::UnsupportedFormat);
            }
            let extended_size = xstate_size.checked_add(4).ok_or(Error::Overflow)?;
            if read_u32(fp, 468)? != extended_size
                || read_u64(fp, 472)? != xfeatures
                || read_u32(fp, 480)? != xstate_size
                || fp[484..512].iter().any(|byte| *byte != 0)
            {
                return Err(Error::Metadata);
            }
            Ok(extended_size as usize)
        }
    }
}

fn validate_xsave(fp: &[u8], xfeatures: u64) -> Result<(), Error> {
    if read_u64(fp, 512)? & !xfeatures != 0
        || fp[520..XSAVE_HEADER_END].iter().any(|byte| *byte != 0)
        || read_u32(fp, fp.len() - 4)? != MAGIC2
    {
        return Err(Error::Metadata);
    }
    Ok(())
}

fn allocation(address: usize, length: usize) -> Result<Range<usize>, Error> {
    Ok(address..address.checked_add(length).ok_or(Error::Overflow)?)
}

fn span(allocation: &Range<usize>, address: usize, length: usize) -> Result<Range<usize>, Error> {
    let end = address.checked_add(length).ok_or(Error::Overflow)?;
    if address < allocation.start || end > allocation.end {
        return Err(Error::SourceBounds);
    }
    Ok(address - allocation.start..end - allocation.start)
}

fn overlaps(first: &Range<usize>, second: &Range<usize>) -> bool {
    first.start < first.end
        && second.start < second.end
        && first.start < second.end
        && second.start < first.end
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    let end = offset.checked_add(4).ok_or(Error::Overflow)?;
    let value = bytes.get(offset..end).ok_or(Error::SourceBounds)?;
    Ok(u32::from_le_bytes(
        value.try_into().map_err(|_| Error::SourceBounds)?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, Error> {
    let end = offset.checked_add(8).ok_or(Error::Overflow)?;
    let value = bytes.get(offset..end).ok_or(Error::SourceBounds)?;
    Ok(u64::from_le_bytes(
        value.try_into().map_err(|_| Error::SourceBounds)?,
    ))
}

#[cfg(test)]
mod tests;
