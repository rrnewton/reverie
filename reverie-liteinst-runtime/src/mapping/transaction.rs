use super::*;

pub(super) enum Request {
    Guard {
        range: Option<Range<u64>>,
        install: bool,
    },
    Map {
        fixed: Option<Range<u64>>,
        length: u64,
        protection: i32,
        fd: Option<i32>,
        offset: u64,
        shared: bool,
    },
    Protect {
        range: Range<u64>,
        protection: i32,
    },
    Unmap(Range<u64>),
    Remap {
        source: Range<u64>,
        length: u64,
        destination: Option<Range<u64>>,
        may_move: bool,
    },
    Break {
        previous: u64,
        requested: u64,
        original: u64,
        affected: Option<Range<u64>>,
    },
}

fn protection(value: u64) -> Result<i32, Failure> {
    if value & !7 != 0 || value & 6 == 6 {
        return Err(refuse("unsupported mapping protection"));
    }
    Ok(value as i32)
}
fn pages(value: u64) -> Result<u64, Failure> {
    value
        .checked_add(PAGE - 1)
        .map(|value| value & !(PAGE - 1))
        .filter(|value| *value > 0 && *value < LIMIT)
        .ok_or_else(|| refuse("unsupported mapping length"))
}
fn break_range(left: u64, right: u64) -> Option<Range<u64>> {
    let start = (left.min(right) + PAGE - 1) & !(PAGE - 1);
    let end = (left.max(right) + PAGE - 1) & !(PAGE - 1);
    (start < end).then_some(start..end)
}

impl Request {
    pub fn decode(
        number: i64,
        args: [u64; 6],
        current_brk: u64,
        original_brk: u64,
    ) -> Result<Self, Failure> {
        match number {
            libc::SYS_madvise => {
                let install = match args[2] as i32 {
                    102 => true,
                    103 => false,
                    _ => return Err(refuse("unsupported madvise advice")),
                };
                let range = if args[1] == 0 {
                    None
                } else {
                    let end = args[0]
                        .checked_add(args[1])
                        .and_then(|end| end.checked_add(PAGE - 1))
                        .map(|end| end & !(PAGE - 1))
                        .filter(|end| *end < LIMIT)
                        .ok_or_else(|| refuse("unsupported guard range"))?;
                    if !args[0].is_multiple_of(PAGE) || args[0] >= end {
                        return Err(refuse("unsupported guard alignment"));
                    }
                    Some(args[0]..end)
                };
                Ok(Self::Guard { range, install })
            }
            libc::SYS_mmap => {
                let flags = args[3];
                let kind = flags & libc::MAP_TYPE as u64;
                let allowed = (libc::MAP_PRIVATE
                    | libc::MAP_SHARED
                    | libc::MAP_DROPPABLE
                    | libc::MAP_ANONYMOUS
                    | libc::MAP_STACK
                    | libc::MAP_FIXED
                    | libc::MAP_FIXED_NOREPLACE
                    | libc::MAP_DENYWRITE
                    | libc::MAP_EXECUTABLE) as u64;
                let supported_type = kind == libc::MAP_PRIVATE as u64
                    || kind == libc::MAP_SHARED as u64
                    || kind == libc::MAP_DROPPABLE as u64
                        && flags & libc::MAP_ANONYMOUS as u64 != 0;
                if flags & !allowed != 0 || !supported_type {
                    return Err(refuse("unsupported mmap flags"));
                }
                let length = pages(args[1])?;
                let fixed = if flags & (libc::MAP_FIXED | libc::MAP_FIXED_NOREPLACE) as u64 != 0 {
                    Some(rounded(args[0], length)?)
                } else {
                    None
                };
                if !args[5].is_multiple_of(PAGE) || args[5].checked_add(length).is_none() {
                    return Err(refuse("unsupported file offset"));
                }
                Ok(Self::Map {
                    shared: kind == libc::MAP_SHARED as u64,
                    offset: args[5],
                    fixed,
                    length,
                    protection: protection(args[2])?,
                    fd: (flags & libc::MAP_ANONYMOUS as u64 == 0).then_some(args[4] as i32),
                })
            }
            libc::SYS_mprotect => Ok(Self::Protect {
                range: rounded(args[0], args[1])?,
                protection: protection(args[2])?,
            }),
            libc::SYS_munmap => Ok(Self::Unmap(rounded(args[0], args[1])?)),
            libc::SYS_mremap => {
                if args[3] & !(libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED) as u64 != 0 {
                    return Err(refuse("unsupported mremap flags"));
                }
                let source = rounded(args[0], args[1])?;
                let length = pages(args[2])?;
                let may_move = args[3] & libc::MREMAP_MAYMOVE as u64 != 0;
                let destination = if args[3] & libc::MREMAP_FIXED as u64 != 0 {
                    if !may_move {
                        return Err(refuse("fixed mremap without MAYMOVE"));
                    }
                    Some(rounded(args[4], length)?)
                } else {
                    None
                };
                if destination
                    .as_ref()
                    .is_some_and(|range| overlap(range, &source))
                {
                    return Err(refuse("overlapping mremap source and destination"));
                }
                Ok(Self::Remap {
                    source,
                    length,
                    destination,
                    may_move,
                })
            }
            libc::SYS_brk => {
                if args[0] >= LIMIT || current_brk >= LIMIT || original_brk >= LIMIT {
                    return Err(refuse("unsupported brk address"));
                }
                let affected = (args[0] >= original_brk)
                    .then(|| break_range(current_brk, args[0]))
                    .flatten();
                Ok(Self::Break {
                    previous: current_brk,
                    requested: args[0],
                    original: original_brk,
                    affected,
                })
            }
            _ => Err(refuse("not a mapping operation")),
        }
    }

    pub fn destructive(&self) -> [Option<Range<u64>>; 2] {
        match self {
            Self::Guard { range, .. } => [range.clone(), None],
            Self::Map { fixed, .. } => [fixed.clone(), None],
            Self::Protect { range, .. } | Self::Unmap(range) => [Some(range.clone()), None],
            Self::Remap {
                source,
                destination,
                ..
            } => [Some(source.clone()), destination.clone()],
            Self::Break { affected, .. } => [affected.clone(), None],
        }
    }
    pub fn moving_source(&self) -> Option<&Range<u64>> {
        match self {
            Self::Remap { source, .. } => Some(source),
            _ => None,
        }
    }
    pub fn guard_effects(&self, result: i64, current_brk: u64) -> Result<Vec<Range<u64>>, Failure> {
        let mut ranges: Vec<_> = self.destructive().into_iter().flatten().collect();
        match self {
            Self::Map { length, .. } | Self::Remap { length, .. } if result >= 0 => {
                ranges.push(rounded(result as u64, *length)?);
            }
            Self::Break { previous, .. } => {
                if let Some(range) = break_range(*previous, current_brk) {
                    ranges.push(range);
                }
            }
            _ => {}
        }
        Ok(normalize(ranges))
    }
    pub fn validate_guards(&self, result: i64, guards: &[Range<u64>]) -> Result<(), &'static str> {
        if result == 0
            && let Self::Guard {
                range: Some(range),
                install,
            } = self
        {
            if *install && !owned(guards, range) {
                return Err("successful guard install not observed");
            }
            if !*install && guards.iter().any(|guard| overlap(guard, range)) {
                return Err("successful guard removal not observed");
            }
        }
        Ok(())
    }
    pub fn file(&self) -> Option<i32> {
        match self {
            Self::Map { fd, .. } => *fd,
            _ => None,
        }
    }
    pub fn file_extent(&self) -> Option<Range<u64>> {
        match self {
            Self::Map {
                offset,
                length,
                fd: Some(_),
                ..
            } => Some(*offset..*offset + *length),
            _ => None,
        }
    }
    pub fn growing_source(&self) -> Option<&Range<u64>> {
        match self {
            Self::Remap { source, length, .. } if *length > source.end - source.start => {
                Some(source)
            }
            _ => None,
        }
    }
    pub fn validate_aliases(
        &self,
        maps: &[Map],
        file: Option<fd_identity::Identity>,
    ) -> Result<(), Failure> {
        match self {
            Self::Map {
                offset,
                length,
                protection,
                shared,
                fixed,
                ..
            } => {
                if let Some(identity) = file {
                    check_aliases(
                        maps,
                        FileAccess {
                            identity,
                            bytes: *offset..*offset + *length,
                            protection: *protection,
                            shared: *shared,
                        },
                        fixed.as_ref(),
                    )?;
                }
            }
            Self::Protect { range, protection } => {
                for map in maps
                    .iter()
                    .filter(|map| map.inode != 0 && overlap(&map.range, range))
                {
                    let part = map.range.start.max(range.start)..map.range.end.min(range.end);
                    check_aliases(
                        maps,
                        FileAccess {
                            identity: (map.device, map.inode),
                            bytes: file_bytes(map, &part)?,
                            protection: *protection,
                            shared: map.shared,
                        },
                        Some(&part),
                    )?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn commit(
        &self,
        result: i64,
        guest: &[Range<u64>],
        before: &Snapshot,
        after: &Snapshot,
    ) -> Result<Vec<Range<u64>>, &'static str> {
        if matches!(result, -512 | -513 | -514 | -516) {
            return Err("internal restart result requires lifecycle handling");
        }
        if result < -4095 {
            return Err("unrecognized kernel result encoding");
        }
        let mut affected: Vec<Range<u64>> = self.destructive().into_iter().flatten().collect();
        let mut candidates = guest.to_vec();
        if result < 0 && !matches!(self, Self::Protect { .. } | Self::Break { .. }) {
            let retained: Vec<_> = after.maps.iter().map(|map| map.range.clone()).collect();
            if !unchanged(&before.maps, &after.maps, &retained) {
                return Err("failed effect changed retained backing or permissions");
            }
        }
        match self {
            Self::Guard { .. } => {
                if result > 0 {
                    return Err("unexpected guard result");
                }
                let all: Vec<_> = before
                    .maps
                    .iter()
                    .chain(&after.maps)
                    .map(|map| map.range.clone())
                    .collect();
                if !unchanged(&before.maps, &after.maps, &all) {
                    return Err("guard effect changed VMA or backing");
                }
            }
            Self::Map {
                fixed,
                length,
                protection,
                ..
            } if result >= 0 => {
                let range =
                    rounded(result as u64, *length).map_err(|_| "invalid mmap kernel result")?;
                if fixed.as_ref().is_some_and(|fixed| *fixed != range)
                    || !has_protection(&after.maps, &range, *protection)
                {
                    return Err("mmap result not present with requested protection");
                }
                if fixed.is_none() && before.maps.iter().any(|map| overlap(&map.range, &range)) {
                    return Err("nonfixed mmap replaced a prior mapping");
                }
                affected.push(range.clone());
                candidates.push(range);
            }
            Self::Protect { range, protection } => {
                if result == 0 && !has_protection(&after.maps, range, *protection) {
                    return Err("mprotect success not observed");
                }
                if result > 0 {
                    return Err("unexpected mprotect result");
                }
                if !same_backing(&before.maps, &after.maps, range) {
                    return Err("mprotect changed backing or mapping extent");
                }
                for map in after.maps.iter().filter(|map| overlap(&map.range, range)) {
                    let part = map.range.start.max(range.start)..map.range.end.min(range.end);
                    if map.protection != *protection
                        && !unchanged(&before.maps, &after.maps, &[part])
                    {
                        return Err("unexpected partial mprotect permissions");
                    }
                }
            }
            Self::Unmap(range) => {
                if result > 0
                    || result == 0 && after.maps.iter().any(|map| overlap(&map.range, range))
                {
                    return Err("munmap result disagrees with inventory");
                }
            }
            Self::Remap {
                source,
                length,
                destination,
                may_move,
            } if result >= 0 => {
                let target =
                    rounded(result as u64, *length).map_err(|_| "invalid mremap kernel result")?;
                if destination
                    .as_ref()
                    .is_some_and(|expected| *expected != target)
                    || !may_move && target.start != source.start
                {
                    return Err("unexpected mremap destination");
                }
                if target.start != source.start && overlap(&target, source) {
                    return Err("overlapping mremap result");
                }
                if destination.is_none()
                    && before.maps.iter().any(|map| {
                        overlap(&map.range, &target)
                            && !owned(
                                std::slice::from_ref(source),
                                &(map.range.start.max(target.start)..map.range.end.min(target.end)),
                            )
                    })
                {
                    return Err("mremap replaced unrelated mapping");
                }
                if !moved_backing(&before.maps, &after.maps, source, &target) {
                    return Err("mremap backing mismatch");
                }
                if target.start != source.start
                    && after.maps.iter().any(|map| overlap(&map.range, source))
                {
                    return Err("mremap source remains mapped");
                }
                if target.start == source.start
                    && target.end < source.end
                    && after
                        .maps
                        .iter()
                        .any(|map| overlap(&map.range, &(target.end..source.end)))
                {
                    return Err("mremap shrink tail remains mapped");
                }
                affected.push(target.clone());
                candidates.push(target);
            }
            Self::Break {
                previous,
                requested,
                original,
                affected: _,
            } => {
                if result <= 0
                    || result as u64 != after.brk
                    || after.brk < *original
                    || after.brk != *previous && after.brk != *requested
                {
                    return Err("brk result disagrees with retained ownership");
                }
                if let Some(range) = break_range(*previous, after.brk) {
                    if after.brk > *previous {
                        if !has_protection(&after.maps, &range, libc::PROT_READ | libc::PROT_WRITE)
                        {
                            return Err("brk growth not readable writable");
                        }
                        candidates.push(range);
                    } else if after.maps.iter().any(|map| overlap(&map.range, &range)) {
                        return Err("brk shrink remains mapped");
                    }
                }
            }
            _ => {}
        }
        if !matches!(self, Self::Break { .. }) && before.brk != after.brk {
            return Err("non-brk effect changed break");
        }
        let mut outside: Vec<_> = before
            .maps
            .iter()
            .chain(&after.maps)
            .map(|map| map.range.clone())
            .collect();
        for range in affected {
            outside = subtract(&outside, &range);
        }
        if !unchanged(&before.maps, &after.maps, &outside) {
            return Err("mapping changed outside effect scope");
        }
        let mut committed = Vec::new();
        for candidate in candidates {
            for map in after
                .maps
                .iter()
                .filter(|map| overlap(&map.range, &candidate))
            {
                if map.protection & 6 == 6 {
                    return Err("writable executable guest mapping");
                }
                let part = map.range.start.max(candidate.start)..map.range.end.min(candidate.end);
                if map.inode != 0 {
                    check_aliases(
                        &after.maps,
                        FileAccess {
                            identity: (map.device, map.inode),
                            bytes: file_bytes(map, &part).map_err(|failure| failure.reason)?,
                            protection: map.protection,
                            shared: map.shared,
                        },
                        None,
                    )
                    .map_err(|failure| failure.reason)?;
                }
                committed.push(part);
            }
        }
        Ok(normalize(committed))
    }
}

fn at(maps: &[Map], address: u64) -> Option<&Map> {
    maps.iter().find(|map| map.range.contains(&address))
}

struct FileAccess {
    identity: fd_identity::Identity,
    bytes: Range<u64>,
    protection: i32,
    shared: bool,
}
fn file_bytes(map: &Map, part: &Range<u64>) -> Result<Range<u64>, Failure> {
    let start = map
        .offset
        .checked_add(part.start - map.range.start)
        .ok_or_else(|| refuse("file offset overflow"))?;
    Ok(start
        ..start
            .checked_add(part.end - part.start)
            .ok_or_else(|| refuse("file offset overflow"))?)
}
fn check_aliases(
    maps: &[Map],
    new: FileAccess,
    removed: Option<&Range<u64>>,
) -> Result<(), Failure> {
    for map in maps
        .iter()
        .filter(|map| (map.device, map.inode) == new.identity)
    {
        let conflict = new.protection & libc::PROT_EXEC != 0
            && map.shared
            && map.protection & libc::PROT_WRITE != 0
            || new.shared
                && new.protection & libc::PROT_WRITE != 0
                && map.protection & libc::PROT_EXEC != 0;
        if !conflict {
            continue;
        }
        let parts = if let Some(removed) = removed {
            [
                map.range.start..map.range.end.min(removed.start).max(map.range.start),
                map.range.start.max(removed.end).min(map.range.end)..map.range.end,
            ]
        } else {
            [map.range.clone(), 0..0]
        };
        for part in parts {
            if part.start < part.end && overlap(&new.bytes, &file_bytes(map, &part)?) {
                return Err(refuse("writable shared alias of executable guest storage"));
            }
        }
    }
    Ok(())
}
fn backing(left: &Map, left_address: u64, right: &Map, right_address: u64) -> bool {
    left.device == right.device
        && left.inode == right.inode
        && left.shared == right.shared
        && (left.inode == 0
            || left
                .offset
                .checked_add(left_address - left.range.start)
                .zip(right.offset.checked_add(right_address - right.range.start))
                .is_some_and(|(left, right)| left == right))
}
fn compare(left: &[Map], right: &[Map], range: &Range<u64>, permissions: bool) -> bool {
    let mut cursor = range.start;
    while cursor < range.end {
        match (at(left, cursor), at(right, cursor)) {
            (Some(old), Some(new)) => {
                if !backing(old, cursor, new, cursor)
                    || permissions && old.protection != new.protection
                {
                    return false;
                }
                cursor = old.range.end.min(new.range.end).min(range.end);
            }
            (None, None) => {
                cursor = left
                    .iter()
                    .chain(right)
                    .filter(|map| map.range.start > cursor)
                    .map(|map| map.range.start)
                    .min()
                    .unwrap_or(range.end)
                    .min(range.end);
            }
            _ => return false,
        }
    }
    true
}
pub(super) fn unchanged(left: &[Map], right: &[Map], ranges: &[Range<u64>]) -> bool {
    ranges.iter().all(|range| compare(left, right, range, true))
}
fn same_backing(left: &[Map], right: &[Map], range: &Range<u64>) -> bool {
    compare(left, right, range, false)
}
fn has_protection(maps: &[Map], range: &Range<u64>, protection: i32) -> bool {
    let mut cursor = range.start;
    while cursor < range.end {
        let Some(map) = at(maps, cursor) else {
            return false;
        };
        if map.protection != protection {
            return false;
        }
        cursor = map.range.end.min(range.end);
    }
    true
}
fn moved_backing(before: &[Map], after: &[Map], source: &Range<u64>, target: &Range<u64>) -> bool {
    let mut offset = 0;
    let length = target.end - target.start;
    while offset < length {
        let source_address = source.start + offset.min(source.end - source.start - 1);
        let Some(old) = at(before, source_address) else {
            return false;
        };
        let Some(new) = at(after, target.start + offset) else {
            return false;
        };
        if old.protection != new.protection
            || !backing(old, source.start + offset, new, target.start + offset)
        {
            return false;
        }
        let old_end = if source.start + offset < source.end {
            old.range.end - source.start
        } else {
            length
        };
        let next = old_end.min(new.range.end - target.start).min(length);
        if next <= offset {
            return false;
        }
        offset = next;
    }
    true
}
