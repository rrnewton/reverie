use super::*;

pub(super) type Identity = ((u64, u64), u64);
#[derive(Debug)]
pub(super) struct Resolved {
    pub identity: Identity,
    pub length: u64,
}
fn unavailable() -> io::Error {
    io::Error::other("descriptor kernel mapping identity unavailable")
}

pub(super) fn resolve(fd: i32, bytes: &mut [u8]) -> io::Result<Option<Resolved>> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    let result = inventory::raw(
        libc::SYS_fstat,
        [fd as u64, (&raw mut stat) as u64, 0, 0, 0, 0],
    );
    if result == -i64::from(libc::EBADF) {
        return Ok(None);
    }
    if result != 0 || stat.st_mode & libc::S_IFMT != libc::S_IFREG || stat.st_size < 0 {
        return Err(unavailable());
    }
    let mut path = [0u8; 40];
    let prefix = b"/proc/self/fdinfo/";
    path[..prefix.len()].copy_from_slice(prefix);
    let mut digits = [0u8; 10];
    let mut value = fd as u32;
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    let end = prefix.len() + digits.len() - start;
    path[prefix.len()..end].copy_from_slice(&digits[start..]);
    let path = std::ffi::CStr::from_bytes_with_nul(&path[..=end]).map_err(|_| unavailable())?;
    let length = inventory::read(path, bytes)?;
    let (mount, inode) = fdinfo(std::str::from_utf8(&bytes[..length]).map_err(|_| unavailable())?)?;
    let length = inventory::read(c"/proc/self/mountinfo", bytes)?;
    if let Some(device) = mount_device(
        std::str::from_utf8(&bytes[..length]).map_err(|_| unavailable())?,
        mount,
    )? {
        return Ok(Some(Resolved {
            identity: (device, inode),
            length: stat.st_size as u64,
        }));
    }
    let mut filesystem: libc::statfs = unsafe { std::mem::zeroed() };
    if inventory::raw(
        libc::SYS_fstatfs,
        [fd as u64, (&raw mut filesystem) as u64, 0, 0, 0, 0],
    ) != 0
        || filesystem.f_type != libc::TMPFS_MAGIC
        || stat.st_ino != inode
    {
        return Err(unavailable());
    }
    Ok(Some(Resolved {
        identity: (
            (
                u64::from(libc::major(stat.st_dev)),
                u64::from(libc::minor(stat.st_dev)),
            ),
            inode,
        ),
        length: stat.st_size as u64,
    }))
}

fn fdinfo(text: &str) -> io::Result<(u64, u64)> {
    let mut mount = None;
    let mut inode = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            return Err(unavailable());
        };
        let slot = match key {
            "mnt_id" => &mut mount,
            "ino" => &mut inode,
            _ => continue,
        };
        if slot
            .replace(value.trim().parse::<u64>().map_err(|_| unavailable())?)
            .is_some()
        {
            return Err(unavailable());
        }
    }
    Ok((
        mount.ok_or_else(unavailable)?,
        inode.ok_or_else(unavailable)?,
    ))
}
fn mount_device(text: &str, selected: u64) -> io::Result<Option<(u64, u64)>> {
    let mut device = None;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let id = fields
            .next()
            .ok_or_else(unavailable)?
            .parse::<u64>()
            .map_err(|_| unavailable())?;
        if id != selected {
            continue;
        }
        fields.next().ok_or_else(unavailable)?;
        let (major, minor) = fields
            .next()
            .ok_or_else(unavailable)?
            .split_once(':')
            .ok_or_else(unavailable)?;
        let current = (
            major.parse().map_err(|_| unavailable())?,
            minor.parse().map_err(|_| unavailable())?,
        );
        if device.replace(current).is_some() {
            return Err(unavailable());
        }
    }
    Ok(device)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kernel_metadata_parsing_has_no_stat_device_substitution() {
        assert_eq!(
            fdinfo("pos:\t0\nflags:\t0100000\nmnt_id:\t42\nino:\t123\n").unwrap(),
            (42, 123)
        );
        assert_eq!(
            mount_device("42 7 0:47 /subvolume /home rw - btrfs /dev/test rw\n", 42).unwrap(),
            Some((0, 47))
        );
        assert_eq!(
            mount_device("42 7 0:47 /subvolume /home rw - btrfs /dev/test rw\n", 43).unwrap(),
            None
        );
        assert!(fdinfo("mnt_id: 42\nino: 123\nino: 124\n").is_err());
        assert!(fdinfo("ino: 123\n").is_err());
        assert!(mount_device("42 7 0:47 / / rw\n42 7 0:49 / / rw\n", 42).is_err());
        assert!(mount_device("42 7 invalid / / rw\n", 42).is_err());
    }
}
