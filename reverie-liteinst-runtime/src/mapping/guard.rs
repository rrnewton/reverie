use super::*;

const CAPACITY: usize = 4096;
const WORK_LIMIT: usize = 4 * CAPACITY;
const ROWS: usize = 64;
const PAGE_IS_GUARD: u64 = 1 << 8;
const PAGEMAP_SCAN: u64 = (3 << 30) | (96 << 16) | (b'f' as u64) << 8 | 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Error {
    pub reason: &'static str,
    pub syscall_result: Option<i64>,
    pub close_result: Option<i64>,
}
impl Error {
    pub fn new(reason: &'static str) -> Self {
        Self {
            reason,
            syscall_result: None,
            close_result: None,
        }
    }
    fn syscall(reason: &'static str, result: i64) -> Self {
        Self {
            reason,
            syscall_result: Some(result),
            close_result: None,
        }
    }
}

pub(super) enum State {
    FreshExecUnqueried,
    Observed(Vec<Range<u64>>),
}
impl State {
    pub fn active(&self) -> bool {
        matches!(self, Self::Observed(_))
    }
    pub fn ranges(&self) -> &[Range<u64>] {
        match self {
            Self::FreshExecUnqueried => &[],
            Self::Observed(ranges) => ranges,
        }
    }
}

#[repr(C)]
#[derive(Default)]
struct Query {
    size: u64,
    flags: u64,
    start: u64,
    end: u64,
    walk_end: u64,
    vec: u64,
    vec_len: u64,
    max_pages: u64,
    category_inverted: u64,
    category_mask: u64,
    category_anyof_mask: u64,
    return_mask: u64,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Region {
    start: u64,
    end: u64,
    categories: u64,
}
const _: () = assert!(std::mem::size_of::<Query>() == 96);
const _: () = assert!(std::mem::size_of::<Region>() == 24);

pub(super) struct Workspace {
    qualified: bool,
    fragments: Vec<Range<u64>>,
    guards: Vec<Range<u64>>,
    rows: [Region; ROWS],
}
impl Workspace {
    pub fn new() -> Self {
        Self {
            qualified: false,
            fragments: Vec::with_capacity(CAPACITY),
            guards: Vec::with_capacity(CAPACITY),
            rows: [Region::default(); ROWS],
        }
    }
    pub fn qualified() -> Self {
        Self {
            qualified: true,
            ..Self::new()
        }
    }
    fn prepare(&mut self, maps: &[Map], guest: &[Range<u64>]) -> Result<(), Error> {
        if self.guards.capacity() < CAPACITY {
            return Err(Error::new("guard workspace already consumed"));
        }
        self.fragments.clear();
        self.guards.clear();
        if maps.len() > CAPACITY || guest.len() > CAPACITY {
            return Err(Error::new("guard input limit"));
        }
        validate_ranges(maps.iter().map(|map| &map.range))?;
        validate_ranges(guest.iter())?;
        for map in maps {
            for owned in guest {
                let part = map.range.start.max(owned.start)..map.range.end.min(owned.end);
                if part.start < part.end {
                    if part.end >= LIMIT {
                        return Err(Error::new(
                            "guard inventory outside supported address space",
                        ));
                    }
                    append(&mut self.fragments, part)?;
                }
            }
        }
        Ok(())
    }
}
fn validate_ranges<'a>(ranges: impl Iterator<Item = &'a Range<u64>>) -> Result<(), Error> {
    let mut previous = 0;
    for range in ranges {
        if range.start < previous
            || range.start >= range.end
            || !range.start.is_multiple_of(PAGE)
            || !range.end.is_multiple_of(PAGE)
        {
            return Err(Error::new("invalid guard inventory range"));
        }
        previous = range.end;
    }
    Ok(())
}
fn append(ranges: &mut Vec<Range<u64>>, part: Range<u64>) -> Result<(), Error> {
    if let Some(last) = ranges.last_mut() {
        if last.end > part.start {
            return Err(Error::new("overlapping guard rows"));
        }
        if last.end == part.start {
            last.end = part.end;
            return Ok(());
        }
    }
    if ranges.len() == CAPACITY {
        return Err(Error::new("guard range capacity"));
    }
    ranges.push(part);
    Ok(())
}

trait Kernel {
    fn tid(&mut self) -> i64;
    fn open(&mut self) -> i64;
    fn query(&mut self, fd: i64, query: &mut Query, rows: &mut [Region]) -> i64;
    fn close(&mut self, fd: i64) -> i64;
}
struct Raw;
impl Kernel for Raw {
    fn tid(&mut self) -> i64 {
        inventory::raw(libc::SYS_gettid, [0; 6])
    }
    fn open(&mut self) -> i64 {
        inventory::raw(
            libc::SYS_openat,
            [
                libc::AT_FDCWD as u64,
                c"/proc/self/pagemap".as_ptr() as u64,
                (libc::O_RDONLY | libc::O_CLOEXEC) as u64,
                0,
                0,
                0,
            ],
        )
    }
    fn query(&mut self, fd: i64, query: &mut Query, _: &mut [Region]) -> i64 {
        inventory::raw(
            libc::SYS_ioctl,
            [fd as u64, PAGEMAP_SCAN, query as *mut Query as u64, 0, 0, 0],
        )
    }
    fn close(&mut self, fd: i64) -> i64 {
        inventory::raw(libc::SYS_close, [fd as u64, 0, 0, 0, 0, 0])
    }
}
struct Descriptor<'a, K: Kernel> {
    kernel: &'a mut K,
    fd: Option<i64>,
}
impl<K: Kernel> Descriptor<'_, K> {
    fn finish(mut self) -> i64 {
        self.kernel.close(self.fd.take().unwrap())
    }
}
impl<K: Kernel> Drop for Descriptor<'_, K> {
    fn drop(&mut self) {
        if let Some(fd) = self.fd.take() {
            self.kernel.close(fd);
        }
    }
}

pub(super) fn observe(
    tid: i64,
    maps: &[Map],
    guest: &[Range<u64>],
    workspace: &mut Workspace,
) -> Result<Vec<Range<u64>>, Error> {
    observe_with(&mut Raw, tid, maps, guest, workspace)
}
fn observe_with(
    kernel: &mut impl Kernel,
    tid: i64,
    maps: &[Map],
    guest: &[Range<u64>],
    workspace: &mut Workspace,
) -> Result<Vec<Range<u64>>, Error> {
    workspace.prepare(maps, guest)?;
    let current = kernel.tid();
    if tid <= 0 || current != tid {
        return Err(Error::syscall("guard observer wrong owner", current));
    }
    if workspace.fragments.is_empty() {
        return if workspace.qualified && kernel.tid() == tid {
            Ok(std::mem::take(&mut workspace.guards))
        } else {
            Err(Error::new(
                "empty guard baseline cannot qualify observation",
            ))
        };
    }
    let fd = kernel.open();
    if fd < 0 {
        return Err(Error::syscall("guard pagemap open failed", fd));
    }
    let mut descriptor = Descriptor {
        kernel,
        fd: Some(fd),
    };
    let result = walk(&mut descriptor, workspace);
    let closed = descriptor.finish();
    let current = kernel.tid();
    let result = result.and_then(|()| {
        if current == tid {
            Ok(())
        } else {
            Err(Error::syscall("guard observer owner changed", current))
        }
    });
    match result {
        Err(mut error) => {
            if closed != 0 {
                error.close_result = Some(closed);
            }
            Err(error)
        }
        Ok(()) if closed != 0 => Err(Error {
            reason: "guard pagemap close failed",
            syscall_result: None,
            close_result: Some(closed),
        }),
        Ok(()) => Ok(std::mem::take(&mut workspace.guards)),
    }
}
fn walk(
    descriptor: &mut Descriptor<'_, impl Kernel>,
    workspace: &mut Workspace,
) -> Result<(), Error> {
    let mut calls = 0_usize;
    let mut total_rows = 0_usize;
    for fragment in &workspace.fragments {
        let mut cursor = fragment.start;
        while cursor < fragment.end {
            calls += 1;
            if calls > WORK_LIMIT {
                return Err(Error::new("guard query work limit"));
            }
            workspace.rows.fill(Region::default());
            let mut query = Query {
                size: 96,
                start: cursor,
                end: fragment.end,
                vec: workspace.rows.as_mut_ptr() as u64,
                vec_len: ROWS as u64,
                return_mask: PAGE_IS_GUARD,
                ..Query::default()
            };
            let count =
                descriptor
                    .kernel
                    .query(descriptor.fd.unwrap(), &mut query, &mut workspace.rows);
            if count < 0 {
                return Err(Error::syscall("guard pagemap query failed", count));
            }
            if count == 0 || count as u64 > ROWS as u64 {
                return Err(Error::new("invalid guard query row count"));
            }
            total_rows += count as usize;
            if total_rows > WORK_LIMIT {
                return Err(Error::new("guard query row limit"));
            }
            if query.walk_end <= cursor
                || query.walk_end > fragment.end
                || !query.walk_end.is_multiple_of(PAGE)
            {
                return Err(Error::new("invalid guard query cursor"));
            }
            for row in &workspace.rows[..count as usize] {
                if row.start != cursor
                    || row.end <= row.start
                    || row.end > query.walk_end
                    || !row.start.is_multiple_of(PAGE)
                    || !row.end.is_multiple_of(PAGE)
                    || !matches!(row.categories, 0 | PAGE_IS_GUARD)
                {
                    return Err(Error::new("invalid guard query region"));
                }
                if row.categories == PAGE_IS_GUARD {
                    append(&mut workspace.guards, row.start..row.end)?;
                }
                cursor = row.end;
            }
            if cursor != query.walk_end {
                return Err(Error::new("incomplete guard query prefix"));
            }
        }
    }
    Ok(())
}

pub(super) fn unchanged_outside(
    before: &[Range<u64>],
    after: &[Range<u64>],
    affected: &[Range<u64>],
) -> bool {
    let outside = |ranges: &[Range<u64>]| {
        let mut ranges = ranges.to_vec();
        for part in affected {
            ranges = subtract(&ranges, part);
        }
        ranges
    };
    outside(before) == outside(after)
}

#[cfg(test)]
pub(super) mod tests;
