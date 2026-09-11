use super::*;

const MAX_MAPS: usize = 4096;
const MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Map {
    pub range: Range<u64>,
    pub protection: i32,
    pub offset: u64,
    pub device: (u64, u64),
    pub inode: u64,
    pub stack: bool,
    pub shared: bool,
}

pub(super) struct Snapshot {
    pub maps: Vec<Map>,
    pub brk: u64,
    pub bytes: Vec<u8>,
}

impl Snapshot {
    pub fn empty() -> Self {
        Self {
            maps: Vec::with_capacity(MAX_MAPS),
            brk: 0,
            bytes: vec![0; MAX_BYTES],
        }
    }

    pub fn capture(&mut self) -> io::Result<()> {
        self.maps.clear();
        let used = read(c"/proc/self/maps", &mut self.bytes)?;
        let bytes = std::str::from_utf8(&self.bytes[..used])
            .map_err(|_| io::Error::other("non-UTF8 mapping inventory"))?;
        parse_into(bytes, &mut self.maps)?;
        let brk = raw(libc::SYS_brk, [0; 6]);
        if brk <= 0 || brk as u64 >= LIMIT {
            return Err(io::Error::other("unavailable current brk"));
        }
        self.brk = brk as u64;
        Ok(())
    }
}

pub(super) fn read(path: &std::ffi::CStr, bytes: &mut [u8]) -> io::Result<usize> {
    let fd = raw(
        libc::SYS_openat,
        [
            libc::AT_FDCWD as u64,
            path.as_ptr() as u64,
            (libc::O_RDONLY | libc::O_CLOEXEC) as u64,
            0,
            0,
            0,
        ],
    );
    if fd < 0 {
        return Err(io::Error::other("mapping inventory open failed"));
    }
    let mut used = 0;
    let mut failures = 0;
    let status = loop {
        if used == bytes.len() {
            break Err(io::Error::other("mapping inventory byte limit"));
        }
        let result = raw(
            libc::SYS_read,
            [
                fd as u64,
                unsafe { bytes.as_mut_ptr().add(used) } as u64,
                (bytes.len() - used) as u64,
                0,
                0,
                0,
            ],
        );
        if result == -i64::from(libc::EINTR) && failures < 16 {
            failures += 1;
            continue;
        }
        if result < 0 {
            break Err(io::Error::other("mapping inventory read failed"));
        }
        if result == 0 {
            break Ok(());
        }
        used += result as usize;
    };
    let close = raw(libc::SYS_close, [fd as u64, 0, 0, 0, 0, 0]);
    status?;
    if close != 0 {
        return Err(io::Error::other("mapping inventory close failed"));
    }
    Ok(used)
}

pub(super) fn raw(number: i64, args: [u64; 6]) -> i64 {
    unsafe { reverie_preload::trap::raw_syscall6(number, args) }
}

fn parse_into(text: &str, output: &mut Vec<Map>) -> io::Result<()> {
    let invalid = || io::Error::other("unsupported mapping inventory");
    for line in text.lines() {
        if output.len() == MAX_MAPS || output.len() == output.capacity() {
            return Err(invalid());
        }
        let mut fields = line.split_whitespace();
        let mut field = || fields.next().ok_or_else(invalid);
        let (start, end) = field()?.split_once('-').ok_or_else(invalid)?;
        let start = u64::from_str_radix(start, 16).map_err(|_| invalid())?;
        let end = u64::from_str_radix(end, 16).map_err(|_| invalid())?;
        let permissions = field()?.as_bytes();
        let offset = u64::from_str_radix(field()?, 16).map_err(|_| invalid())?;
        let (major, minor) = field()?.split_once(':').ok_or_else(invalid)?;
        let device = (
            u64::from_str_radix(major, 16).map_err(|_| invalid())?,
            u64::from_str_radix(minor, 16).map_err(|_| invalid())?,
        );
        let inode = field()?.parse::<u64>().map_err(|_| invalid())?;
        if start >= end
            || start % PAGE != 0
            || end % PAGE != 0
            || permissions.len() != 4
            || !matches!(permissions[0], b'r' | b'-')
            || !matches!(permissions[1], b'w' | b'-')
            || !matches!(permissions[2], b'x' | b'-')
            || !matches!(permissions[3], b'p' | b's')
            || output
                .last()
                .is_some_and(|previous| previous.range.end > start)
        {
            return Err(invalid());
        }
        output.push(Map {
            range: start..end,
            offset,
            device,
            inode,
            protection: i32::from(permissions[0] == b'r')
                | (i32::from(permissions[1] == b'w') << 1)
                | (i32::from(permissions[2] == b'x') << 2),
            stack: fields.next() == Some("[stack]"),
            shared: permissions[3] == b's',
        });
    }
    Ok(())
}
