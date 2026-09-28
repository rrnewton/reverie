use std::fs::File;
use std::os::unix::fs::FileExt;

use super::ordered::Buffer;
use super::ordered::Limits;
use super::ordered::Role;
use super::*;

const REQUIRED_SEALS: i32 = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW;

fn limits() -> Limits {
    Limits {
        producers: 4,
        slots: 8,
        max_record_bytes: PAYLOAD * 2,
        host_pending_bytes: PAYLOAD * 4,
        guest_pending_bytes: PAYLOAD * 4,
        pending_records: 8,
    }
}

fn descriptor_identity(fd: i32) -> Option<(u64, u64)> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    (unsafe { libc::fstat(fd, &mut stat) } == 0).then_some((stat.st_dev, stat.st_ino))
}

fn assert_no_backing_descriptor(identity: (u64, u64)) {
    // Compare backing identity rather than a descriptor count or number: other
    // parallel tests may open unrelated files and reuse a just-closed number.
    for entry in std::fs::read_dir("/proc/self/fd").unwrap() {
        let entry = entry.unwrap();
        let number: i32 = entry.file_name().to_str().unwrap().parse().unwrap();
        assert_ne!(
            descriptor_identity(number),
            Some(identity),
            "mapping retained backing descriptor {number}"
        );
    }
}

fn no_wait(_: &SharedBuffer, _: u32) -> Result<(), PublishError> {
    panic!("unexpected capacity wait")
}

fn valid_image() -> Vec<u8> {
    // No protocol object survives into the malformed fixture construction.
    let (mapping, descriptor) = unsafe { Buffer::create(limits()) }.unwrap();
    drop(mapping);
    let file = File::from(descriptor);
    let mut image = vec![0; file.metadata().unwrap().len() as usize];
    file.read_exact_at(&mut image, 0).unwrap();
    image
}

fn backing(image: &[u8], length: usize, seals: i32) -> OwnedFd {
    let raw = unsafe {
        libc::memfd_create(
            c"guest-log-import-fixture".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    assert!(raw >= 0, "{}", io::Error::last_os_error());
    let file = unsafe { File::from_raw_fd(raw) };
    file.set_len(length as u64).unwrap();
    file.write_all_at(&image[..image.len().min(length)], 0)
        .unwrap();
    assert_eq!(unsafe { libc::fcntl(raw, libc::F_ADD_SEALS, seals) }, 0);
    file.into()
}

fn rejected_and_closed(descriptor: OwnedFd) -> io::Error {
    let number = descriptor.as_raw_fd();
    let identity = descriptor_identity(number).expect("live fixture descriptor");
    // This fixture has no concurrent writer, protocol object or other alias.
    let error = unsafe { Buffer::import(descriptor) }.err().unwrap();
    assert_ne!(
        descriptor_identity(number),
        Some(identity),
        "failed import retained its input descriptor"
    );
    assert_no_backing_descriptor(identity);
    error
}

#[test]
fn direct_transfer_closes_all_backing_descriptors_and_preserves_ordered_records() {
    // These two mappings cooperate through one writer per incarnation and one
    // collector. All backing descriptors close before any writer is activated.
    let (host, descriptor) = unsafe { Buffer::create(limits()) }.unwrap();
    let number = descriptor.as_raw_fd();
    let identity = descriptor_identity(number).unwrap();
    assert!(number > 2);
    let flags = unsafe { libc::fcntl(number, libc::F_GETFD) };
    assert!(flags >= 0);
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
    let seals = unsafe { libc::fcntl(number, libc::F_GET_SEALS) };
    assert!(seals >= 0);
    assert_eq!(seals & REQUIRED_SEALS, REQUIRED_SEALS);
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::fstat(number, &mut stat) }, 0);
    for size in [stat.st_size - 1, stat.st_size + 1] {
        assert_eq!(unsafe { libc::ftruncate(number, size) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));
    }
    let guest = unsafe { Buffer::import(descriptor) }.unwrap();
    assert_ne!(descriptor_identity(number), Some(identity));
    assert_no_backing_descriptor(identity);

    let mut host_writer = unsafe { host.activate(0, 11) }.unwrap();
    let mut guest_writer = unsafe { guest.activate(1, 22) }.unwrap();
    let mut collector = host.collector().unwrap();
    assert!(
        guest.collector().is_err(),
        "import admitted a second collector"
    );
    let guest_bytes: Vec<u8> = (0..PAYLOAD + 19).map(|index| (index * 31) as u8).collect();
    assert_eq!(
        guest_writer
            .write_record(&guest_bytes, no_wait)
            .unwrap()
            .order,
        1
    );
    assert_eq!(
        host_writer
            .write_record(b"host\0reply\xff", no_wait)
            .unwrap()
            .order,
        2
    );
    guest_writer.finish(no_wait).unwrap();
    host_writer.finish(no_wait).unwrap();
    for (order, producer, expected) in [(1, 1, guest_bytes.as_slice()), (2, 0, b"host\0reply\xff")]
    {
        let record = collector.poll().unwrap().unwrap();
        assert_eq!((record.order(), record.producer()), (order, producer));
        assert_eq!(record.bytes(), expected);
        record.release().unwrap();
    }
    assert!(collector.poll().unwrap().is_none());
    assert!(collector.guest_complete());
    assert_eq!(host.used_bytes(Role::Host), 0);
    assert_eq!(guest.used_bytes(Role::Guest), 0);
    host.close(Role::Host);
    guest.close(Role::Guest);
    assert_eq!(collector.commits_observed(), Ok(true));
}

#[test]
fn direct_creation_rejects_invalid_limits() {
    for settings in [
        Limits {
            producers: 1,
            ..limits()
        },
        Limits {
            slots: 0,
            ..limits()
        },
        Limits {
            pending_records: 1,
            ..limits()
        },
        Limits {
            host_pending_bytes: usize::MAX,
            ..limits()
        },
    ] {
        assert!(unsafe { Buffer::create(settings) }.is_err());
    }
}

#[test]
fn direct_import_rejects_each_missing_size_seal_and_closes_descriptor() {
    let image = valid_image();
    for missing in [libc::F_SEAL_SEAL, libc::F_SEAL_SHRINK, libc::F_SEAL_GROW] {
        let descriptor = backing(&image, image.len(), REQUIRED_SEALS & !missing);
        assert_eq!(rejected_and_closed(descriptor).kind(), io::ErrorKind::Other);
    }
}

#[test]
fn direct_import_rejects_short_extended_and_oversized_layouts_and_closes_descriptor() {
    let image = valid_image();
    for length in [
        0,
        std::mem::size_of::<Header>() - 1,
        std::mem::size_of::<Header>(),
        image.len() - 1,
        image.len() + 1,
        32 * 1024 * 1024 + 1,
    ] {
        let descriptor = backing(&image, length, REQUIRED_SEALS);
        assert_eq!(rejected_and_closed(descriptor).kind(), io::ErrorKind::Other);
    }
}

#[test]
fn direct_import_rejects_versions_and_inconsistent_headers_and_closes_descriptor() {
    let image = valid_image();
    let base = size(Options {
        producers: limits().producers,
        slots: limits().slots,
        byte_limit: limits().host_pending_bytes + limits().guest_pending_bytes,
    })
    .unwrap();
    for (offset, value) in [
        (std::mem::offset_of!(Header, magic), MAGIC),
        (std::mem::offset_of!(Header, slots), 0),
        (std::mem::offset_of!(Header, producers), 0),
        (std::mem::offset_of!(Header, byte_limit), 1),
        (base, 0), // V4 max_record_bytes must be nonzero.
    ] {
        let mut malformed = image.clone();
        malformed[offset..offset + 8].copy_from_slice(&value.to_ne_bytes());
        let descriptor = backing(&malformed, malformed.len(), REQUIRED_SEALS);
        assert_eq!(rejected_and_closed(descriptor).kind(), io::ErrorKind::Other);
    }
    let descriptor = create_descriptor(
        Options {
            byte_limit: 64,
            producers: 1,
            slots: 2,
        },
        None,
    )
    .unwrap();
    assert_eq!(rejected_and_closed(descriptor).kind(), io::ErrorKind::Other);
}

#[test]
fn direct_import_closes_non_mappable_and_read_only_descriptors() {
    let (endpoint, peer) = UnixStream::pair().unwrap();
    drop(peer);
    assert_eq!(
        rejected_and_closed(endpoint.into()).kind(),
        io::ErrorKind::Other
    );

    let (mapping, descriptor) = unsafe { Buffer::create(limits()) }.unwrap();
    drop(mapping);
    let read_only: OwnedFd = File::open(format!("/proc/self/fd/{}", descriptor.as_raw_fd()))
        .unwrap()
        .into();
    drop(descriptor);
    assert_eq!(
        rejected_and_closed(read_only).raw_os_error(),
        Some(libc::EACCES)
    );
}
