use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::fd::BorrowedFd;
use std::os::unix::fs::FileExt;
use std::os::unix::fs::MetadataExt;

use super::*;
use crate::startup::AuxvSnapshot;
use crate::startup::InterpreterImage;
use crate::startup::original_interpreter::OriginalInterpreter;

fn invalid() -> io::Error {
    io::Error::other("initial guest mapping binding unavailable or unsupported")
}
fn word<const SIZE: usize>(bytes: &[u8], offset: usize) -> io::Result<[u8; SIZE]> {
    bytes
        .get(offset..offset.checked_add(SIZE).ok_or_else(invalid)?)
        .ok_or_else(invalid)?
        .try_into()
        .map_err(|_| invalid())
}
fn half(bytes: &[u8], offset: usize) -> io::Result<u16> {
    Ok(u16::from_le_bytes(word(bytes, offset)?))
}
fn small(bytes: &[u8], offset: usize) -> io::Result<u32> {
    Ok(u32::from_le_bytes(word(bytes, offset)?))
}
fn wide(bytes: &[u8], offset: usize) -> io::Result<u64> {
    Ok(u64::from_le_bytes(word(bytes, offset)?))
}
fn permissions(flags: u32) -> io::Result<i32> {
    if flags & !7 != 0 || flags & 3 == 3 {
        return Err(invalid());
    }
    Ok(((flags & 4) >> 2 | (flags & 2) | ((flags & 1) << 2)) as i32)
}

struct Segment {
    pages: Range<u64>,
    file: Range<u64>,
    offset: u64,
    protection: i32,
}
struct Image {
    segments: Vec<Segment>,
    identity: fd_identity::Identity,
    phdr: Range<u64>,
}

fn main_image(initial: &AuxvSnapshot) -> io::Result<Image> {
    let file = File::open("/proc/self/exe")?;
    let identity = file.metadata()?;
    let mut header = [0; 64];
    file.read_exact_at(&mut header, 0)?;
    let kind = half(&header, 16)?;
    if &header[..7] != b"\x7fELF\x02\x01\x01"
        || !matches!(kind, 2 | 3)
        || half(&header, 18)? != 62
        || small(&header, 20)? != 1
        || half(&header, 52)? != 64
        || half(&header, 54)? != 56
    {
        return Err(invalid());
    }
    let count = usize::from(half(&header, 56)?);
    if count == 0 || count > 4096 {
        return Err(invalid());
    }
    let offset = wide(&header, 32)?;
    let length = count * 56;
    if offset
        .checked_add(length as u64)
        .is_none_or(|end| end > identity.len())
        || initial.program_headers().end - initial.program_headers().start != length as u64
    {
        return Err(invalid());
    }
    let mut headers = vec![0; length];
    file.read_exact_at(&mut headers, offset)?;
    let mut mapped_headers = vec![0u8; length];
    let local = libc::iovec {
        iov_base: mapped_headers.as_mut_ptr().cast(),
        iov_len: length,
    };
    let remote = libc::iovec {
        iov_base: initial.program_headers().start as *mut libc::c_void,
        iov_len: length,
    };
    let pid = inventory::raw(libc::SYS_getpid, [0; 6]);
    let observed = inventory::raw(
        libc::SYS_process_vm_readv,
        [
            pid as u64,
            (&raw const local) as u64,
            1,
            (&raw const remote) as u64,
            1,
            0,
        ],
    );
    if pid <= 0 || observed != length as i64 || mapped_headers != headers {
        return Err(io::Error::other(
            "kernel-mapped guest PHDR bytes unavailable or changed",
        ));
    }
    let after = file.metadata()?;
    if (
        identity.dev(),
        identity.ino(),
        identity.len(),
        identity.mtime(),
        identity.mtime_nsec(),
        identity.ctime(),
        identity.ctime_nsec(),
    ) != (
        after.dev(),
        after.ino(),
        after.len(),
        after.mtime(),
        after.mtime_nsec(),
        after.ctime(),
        after.ctime_nsec(),
    ) {
        return Err(io::Error::other("guest executable changed during binding"));
    }
    parse_main(
        &header,
        &headers,
        offset,
        fd_identity::resolve(file.as_raw_fd(), &mut vec![0; 1024 * 1024])?
            .ok_or_else(invalid)?
            .identity,
        identity.len(),
        initial,
    )
}

fn parse_main(
    header: &[u8],
    headers: &[u8],
    phoff: u64,
    identity: fd_identity::Identity,
    file_size: u64,
    initial: &AuxvSnapshot,
) -> io::Result<Image> {
    let mut phdr_virtual = None;
    let mut dynamic = false;
    for item in headers.as_chunks::<56>().0 {
        let kind = small(item, 0)?;
        if kind == 3 {
            dynamic = true;
        }
        if kind != 1 {
            continue;
        }
        let offset = wide(item, 8)?;
        let file_length = wide(item, 32)?;
        if offset <= phoff
            && phoff.checked_add(headers.len() as u64).is_some_and(|end| {
                offset
                    .checked_add(file_length)
                    .is_some_and(|file_end| end <= file_end)
            })
        {
            let address = wide(item, 16)?
                .checked_add(phoff - offset)
                .ok_or_else(invalid)?;
            if phdr_virtual.replace(address).is_some() {
                return Err(invalid());
            }
        }
    }
    if !dynamic {
        return Err(invalid());
    }
    let bias = initial
        .program_headers()
        .start
        .checked_sub(phdr_virtual.ok_or_else(invalid)?)
        .ok_or_else(invalid)?;
    if bias % PAGE != 0
        || half(header, 16)? == 2 && bias != 0
        || bias.checked_add(wide(header, 24)?).ok_or_else(invalid)? != initial.guest_entry()
    {
        return Err(invalid());
    }
    let mut segments: Vec<Segment> = Vec::new();
    for item in headers.as_chunks::<56>().0 {
        if small(item, 0)? != 1 {
            continue;
        }
        let offset = wide(item, 8)?;
        let address = bias.checked_add(wide(item, 16)?).ok_or_else(invalid)?;
        let file_length = wide(item, 32)?;
        let memory_length = wide(item, 40)?;
        let alignment = wide(item, 48)?;
        if file_length > memory_length
            || offset % PAGE != address % PAGE
            || alignment > 1
                && (!alignment.is_power_of_two()
                    || wide(item, 16)? % alignment != offset % alignment)
            || offset
                .checked_add(file_length)
                .is_none_or(|end| end > file_size)
        {
            return Err(invalid());
        }
        if memory_length == 0 {
            continue;
        }
        let end = address.checked_add(memory_length).ok_or_else(invalid)?;
        let file_end = address.checked_add(file_length).ok_or_else(invalid)?;
        let pages = rounded(address & !(PAGE - 1), end - (address & !(PAGE - 1)))
            .map_err(io::Error::other)?;
        let file_end = file_end.checked_add(PAGE - 1).ok_or_else(invalid)? & !(PAGE - 1);
        if segments
            .iter()
            .any(|segment| overlap(&segment.pages, &pages))
        {
            return Err(invalid());
        }
        segments.push(Segment {
            file: pages.start..file_end,
            offset: offset & !(PAGE - 1),
            pages,
            protection: permissions(small(item, 4)?)?,
        });
    }
    if segments.is_empty() {
        return Err(invalid());
    }
    Ok(Image {
        segments,
        identity,
        phdr: initial.program_headers().clone(),
    })
}

fn check_range(
    maps: &[Map],
    range: &Range<u64>,
    protection: i32,
    file: Option<(fd_identity::Identity, u64)>,
) -> io::Result<()> {
    let mut cursor = range.start;
    while cursor < range.end {
        let map = maps
            .iter()
            .find(|map| map.range.contains(&cursor))
            .ok_or_else(invalid)?;
        if map.protection != protection || map.shared {
            return Err(invalid());
        }
        if let Some(((device, inode), offset)) = file {
            if map.device != device
                || map.inode != inode
                || map.offset.checked_add(cursor - map.range.start)
                    != offset.checked_add(cursor - range.start)
            {
                return Err(invalid());
            }
        } else if map.inode != 0 {
            return Err(invalid());
        }
        cursor = map.range.end.min(range.end);
    }
    Ok(())
}

fn initial_guest_ranges(
    maps: &[Map],
    retained_stack: &Range<u64>,
    reservation: &Range<u64>,
    main: &Image,
) -> io::Result<Vec<Range<u64>>> {
    let mut stacks = maps.iter().filter(|map| map.stack);
    let stack = stacks
        .next()
        .ok_or_else(|| io::Error::other("initial guest stack VMA missing"))?;
    if stacks.next().is_some() {
        return Err(io::Error::other("initial guest stack VMA is ambiguous"));
    }
    if retained_stack.is_empty()
        || !stack.range.contains(&retained_stack.start)
        || stack.range.end != retained_stack.end
        || stack.protection != libc::PROT_READ | libc::PROT_WRITE
        || stack.shared
        || stack.inode != 0
        || stack.device != (0, 0)
    {
        return Err(io::Error::other(format!(
            "initial guest stack binding mismatch: retained={retained_stack:#x?}; VMA={:#x?}; protection={:#x}; shared={}; device={:?}; inode={}",
            stack.range, stack.protection, stack.shared, stack.device, stack.inode
        )));
    }
    let mut guest = vec![stack.range.clone(), reservation.clone()];
    guest.extend(main.segments.iter().map(|segment| segment.pages.clone()));
    for (index, range) in guest.iter().enumerate() {
        if let Some(previous) = guest[..index]
            .iter()
            .find(|previous| overlap(previous, range))
        {
            return Err(io::Error::other(format!(
                "initial guest mapping ranges overlap: {previous:#x?} and {range:#x?}"
            )));
        }
    }
    Ok(normalize(guest))
}

/// Binds initial guest ownership before public Tool construction.
///
/// # Safety
/// The caller must be the sole controlled-startup kernel thread, after the real
/// original-loader mapping and before any guest or loader execution. `initial`
/// must retain actual exec auxv/stack/brk, and `original_fd` must name the held
/// sealed `original` used for that mapping. The private allocator must never use brk. Runtime
/// helpers and asynchronous handlers must not mutate guest VMAs or registrations.
pub unsafe fn prepare_private(
    initial: &AuxvSnapshot,
    original: &OriginalInterpreter,
    original_fd: BorrowedFd<'_>,
) -> io::Result<()> {
    if OWNER.get().is_some() {
        return Err(io::Error::other("mapping owner already prepared"));
    }
    if std::fs::read_dir("/proc/self/task")?.count() != 1 {
        return Err(io::Error::other(
            "initial mapping owner requires the sole kernel thread",
        ));
    }
    let personality = inventory::raw(libc::SYS_personality, [u32::MAX as u64, 0, 0, 0, 0, 0]);
    if personality < 0 || personality & 0x0400000 != 0 {
        return Err(io::Error::other(
            "unsupported READ_IMPLIES_EXEC personality",
        ));
    }
    let main = main_image(initial)?;
    let image = InterpreterImage::parse(original.bytes()).map_err(io::Error::other)?;
    let end = initial
        .base()
        .checked_add(image.required_span())
        .ok_or_else(invalid)?;
    let plan = original
        .plan(initial, initial.base()..end, &[])
        .map_err(io::Error::other)?;
    let mut snapshot = Snapshot::empty();
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if inventory::raw(
        libc::SYS_fstat,
        [
            original_fd.as_raw_fd() as u64,
            (&raw mut stat) as u64,
            0,
            0,
            0,
            0,
        ],
    ) != 0
        || (stat.st_dev, stat.st_ino) != original.file_identity()
    {
        return Err(invalid());
    }
    let original_identity = fd_identity::resolve(original_fd.as_raw_fd(), &mut snapshot.bytes)?
        .ok_or_else(invalid)?
        .identity;
    snapshot.capture()?;
    if snapshot.brk != initial.brk() {
        return Err(io::Error::other(
            "private initialization changed original exec brk",
        ));
    }
    let guest = initial_guest_ranges(&snapshot.maps, initial.stack(), plan.reservation(), &main)?;
    for segment in &main.segments {
        check_range(
            &snapshot.maps,
            &segment.file,
            segment.protection,
            Some((main.identity, segment.offset)),
        )?;
        check_range(
            &snapshot.maps,
            &(segment.file.end..segment.pages.end),
            segment.protection,
            None,
        )?;
    }
    if !owned(&guest, &main.phdr) {
        return Err(invalid());
    }
    for segment in plan.segments() {
        let protection = permissions(segment.flags())?;
        if let Some((offset, range)) = segment.file_pages() {
            check_range(
                &snapshot.maps,
                range,
                protection,
                Some((original_identity, *offset)),
            )?;
        }
        check_range(&snapshot.maps, segment.anonymous_pages(), protection, None)?;
    }
    for gap in plan.gaps() {
        check_range(&snapshot.maps, gap, libc::PROT_NONE, None)?;
    }
    let tid = inventory::raw(libc::SYS_gettid, [0; 6]);
    if tid <= 0 {
        return Err(invalid());
    }
    OWNER
        .set(Mutex::new(Owner {
            tid,
            guest,
            snapshot,
            original_brk: initial.brk(),
            generation: 0,
            poisoned: false,
            guards: guard::State::FreshExecUnqueried,
        }))
        .map_err(|_| io::Error::other("mapping owner publication conflict"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack_fixture() -> (Map, Image) {
        (
            Map {
                range: 0x7000..0xc000,
                protection: libc::PROT_READ | libc::PROT_WRITE,
                offset: 0,
                device: (0, 0),
                inode: 0,
                stack: true,
                shared: false,
            },
            Image {
                segments: vec![Segment {
                    pages: 0x1000..0x2000,
                    file: 0x1000..0x2000,
                    offset: 0,
                    protection: libc::PROT_READ,
                }],
                identity: ((8, 1), 7),
                phdr: 0x1040..0x1100,
            },
        )
    }

    #[test]
    fn retained_rsp_suffix_binds_full_kernel_stack_vma() {
        let (stack, main) = stack_fixture();
        let retained = 0xbb10..0xc000;
        assert_ne!(stack.range, retained);
        let guest = initial_guest_ranges(
            std::slice::from_ref(&stack),
            &retained,
            &(0x3000..0x4000),
            &main,
        )
        .unwrap();
        assert_eq!(guest, vec![0x1000..0x2000, 0x3000..0x4000, 0x7000..0xc000]);
        assert!(owned(&guest, &(0x7000..0x8000)));
        assert!(!owned(&guest, &(0x6000..0x7000)));
        assert_eq!(retained, 0xbb10..0xc000);
        assert_eq!(
            initial_guest_ranges(&[stack], &(0x7000..0xc000), &(0x3000..0x4000), &main).unwrap(),
            guest
        );
    }

    #[test]
    fn stack_binding_rejects_changed_extent_backing_permissions_and_ambiguity() {
        let (stack, main) = stack_fixture();
        for retained in [
            0x6000..0xc000,
            0xbb10..0xd000,
            0xbb10..0xbff0,
            0xc000..0xc000,
        ] {
            assert!(
                initial_guest_ranges(
                    std::slice::from_ref(&stack),
                    &retained,
                    &(0x3000..0x4000),
                    &main,
                )
                .is_err()
            );
        }
        for mutation in 0..5 {
            let mut changed = stack.clone();
            match mutation {
                0 => changed.protection |= libc::PROT_EXEC,
                1 => changed.protection = libc::PROT_READ,
                2 => changed.shared = true,
                3 => changed.inode = 1,
                4 => changed.device = (8, 1),
                _ => unreachable!(),
            }
            let error =
                initial_guest_ranges(&[changed], &(0xbb10..0xc000), &(0x3000..0x4000), &main)
                    .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("initial guest stack binding mismatch")
            );
            assert_eq!(error.raw_os_error(), None);
        }
        assert!(initial_guest_ranges(&[], &(0xbb10..0xc000), &(0x3000..0x4000), &main).is_err());
        assert!(
            initial_guest_ranges(
                &[stack.clone(), stack],
                &(0xbb10..0xc000),
                &(0x3000..0x4000),
                &main,
            )
            .is_err()
        );
    }

    #[test]
    fn full_stack_prefix_overlap_is_not_admitted_as_guest_ownership() {
        let (stack, mut main) = stack_fixture();
        let retained = 0xbb10..0xc000;
        let prefix = 0x7000..0x8000;
        assert!(!overlap(&retained, &prefix));
        assert!(
            initial_guest_ranges(std::slice::from_ref(&stack), &retained, &prefix, &main).is_err()
        );
        main.segments[0].pages = prefix;
        assert!(initial_guest_ranges(&[stack], &retained, &(0x3000..0x4000), &main).is_err());
    }

    #[test]
    fn ordinary_host_main_phdr_binding_not_owner_installation() {
        let mut observed = Snapshot::empty();
        observed.capture().unwrap();
        let stack = observed
            .maps
            .iter()
            .find(|map| map.stack)
            .unwrap()
            .range
            .clone();
        let bytes = std::fs::read("/proc/self/auxv").unwrap();
        let initial = AuxvSnapshot::parse(&bytes, stack, observed.brk).unwrap();
        let main = main_image(&initial).unwrap();
        assert!(!main.segments.is_empty());
        assert_eq!(main.phdr, *initial.program_headers());
        assert!(
            main.segments
                .iter()
                .any(|segment| segment.pages.contains(&initial.guest_entry())
                    && segment.protection & 4 != 0)
        );
        assert!(OWNER.get().is_none());
    }

    #[test]
    fn file_range_rejects_partial_private_backing_alias_and_permissions() {
        let mut first = Map {
            range: 0x1000..0x2000,
            protection: 1,
            offset: 0,
            device: (8, 1),
            inode: 7,
            stack: false,
            shared: false,
        };
        let mut second = first.clone();
        second.range = 0x2000..0x3000;
        second.offset = PAGE;
        let identity = ((8, 1), 7);
        assert!(
            check_range(
                &[first.clone(), second.clone()],
                &(0x1000..0x3000),
                1,
                Some((identity, 0))
            )
            .is_ok()
        );
        second.inode = 9;
        assert!(
            check_range(
                &[first.clone(), second.clone()],
                &(0x1000..0x3000),
                1,
                Some((identity, 0))
            )
            .is_err()
        );
        second.inode = 7;
        second.offset = 0;
        assert!(
            check_range(
                &[first.clone(), second],
                &(0x1000..0x3000),
                1,
                Some((identity, 0))
            )
            .is_err()
        );
        first.shared = true;
        assert!(check_range(&[first], &(0x1000..0x2000), 1, Some((identity, 0))).is_err());
        assert!(permissions(7).is_err());
        assert_eq!(permissions(5).unwrap(), 5);
    }
}
