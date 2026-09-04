//! Guest mapping effects, not guest address selection or determinization.

use std::cell::Cell;
use std::io;
use std::ops::Range;
use std::sync::Mutex;
use std::sync::OnceLock;

use inventory::Map;
use inventory::Snapshot;

mod fd_identity;
mod guard;
mod initial;
mod inventory;
#[cfg(test)]
mod tests;
mod transaction;
pub use initial::prepare_private;
use transaction::Request;

const PAGE: u64 = 4096;
const LIMIT: u64 = 1 << 47;
static OWNER: OnceLock<Mutex<Owner>> = OnceLock::new();
thread_local! { static CONTINUATION: Cell<Option<(u64, u64)>> = const { Cell::new(None) }; }

pub(crate) struct Dispatch;
impl Drop for Dispatch {
    fn drop(&mut self) {
        CONTINUATION.set(None);
    }
}
pub(crate) fn enter(pc: u64, sp: u64) -> Result<Dispatch, Failure> {
    if CONTINUATION.replace(Some((pc, sp))).is_some() {
        return Err(refuse("nested mapping dispatch"));
    }
    Ok(Dispatch)
}
pub(crate) fn update_continuation(pc: u64, sp: u64) -> Result<(), Failure> {
    if CONTINUATION.get().is_none() {
        return Err(refuse("no mapping dispatch"));
    }
    CONTINUATION.set(Some((pc, sp)));
    Ok(())
}

#[derive(Debug)]
pub(crate) struct Failure {
    pub(crate) reason: &'static str,
    pub(crate) result: Option<i64>,
    observation: Option<guard::Error>,
}
impl Failure {
    pub(crate) fn ordinary(reason: &'static str, result: Option<i64>) -> Self {
        Self {
            reason,
            result,
            observation: None,
        }
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            output,
            "guest mapping {}; kernel-result={:?}",
            self.reason, self.result
        )?;
        if let Some(error) = self.observation {
            write!(output, "; observation={error:?}")?;
        }
        Ok(())
    }
}
impl std::error::Error for Failure {}
fn refuse(reason: &'static str) -> Failure {
    Failure::ordinary(reason, None)
}
fn observation_failure(error: guard::Error, result: Option<i64>) -> Failure {
    Failure {
        reason: "guard observation failed",
        result,
        observation: Some(error),
    }
}
fn overlap(left: &Range<u64>, right: &Range<u64>) -> bool {
    left.start < right.end && right.start < left.end
}
fn rounded(address: u64, length: u64) -> Result<Range<u64>, Failure> {
    let end = address
        .checked_add(length)
        .and_then(|end| end.checked_add(PAGE - 1))
        .map(|end| end & !(PAGE - 1))
        .filter(|end| *end < LIMIT)
        .ok_or_else(|| refuse("unsupported range"))?;
    if address == 0 || address >= end || !address.is_multiple_of(PAGE) {
        return Err(refuse("unsupported alignment or empty range"));
    }
    Ok(address..end)
}
fn normalize(mut ranges: Vec<Range<u64>>) -> Vec<Range<u64>> {
    ranges.sort_by_key(|range| range.start);
    let mut output: Vec<Range<u64>> = Vec::new();
    for range in ranges {
        if let Some(last) = output.last_mut()
            && range.start <= last.end
        {
            last.end = last.end.max(range.end);
        } else {
            output.push(range);
        }
    }
    output
}
fn subtract(ranges: &[Range<u64>], removed: &Range<u64>) -> Vec<Range<u64>> {
    let mut output = Vec::new();
    for range in ranges {
        if !overlap(range, removed) {
            output.push(range.clone());
            continue;
        }
        if range.start < removed.start {
            output.push(range.start..removed.start);
        }
        if range.end > removed.end {
            output.push(removed.end..range.end);
        }
    }
    output
}
fn owned(ranges: &[Range<u64>], range: &Range<u64>) -> bool {
    crate::vdso::covered(ranges, range.start, range.end - range.start)
}

#[derive(Clone, Debug)]
pub(crate) struct View {
    pub(crate) readable: Vec<Range<u64>>,
    pub(crate) writable: Vec<Range<u64>>,
    pub(crate) executable: Vec<Range<u64>>,
}
trait Provider {
    fn snapshot(&mut self, output: &mut Snapshot) -> io::Result<()>;
    fn file(&mut self, fd: i32, bytes: &mut [u8]) -> io::Result<Option<fd_identity::Resolved>>;
    fn execute(&mut self, number: i64, args: [u64; 6]) -> i64;
    fn guards(
        &mut self,
        _: i64,
        _: &[Map],
        _: &[Range<u64>],
        _: &mut guard::Workspace,
    ) -> Result<Vec<Range<u64>>, guard::Error> {
        Err(guard::Error::new("guard observation unavailable"))
    }
}
struct Linux;
impl Provider for Linux {
    fn guards(
        &mut self,
        tid: i64,
        maps: &[Map],
        guest: &[Range<u64>],
        workspace: &mut guard::Workspace,
    ) -> Result<Vec<Range<u64>>, guard::Error> {
        guard::observe(tid, maps, guest, workspace)
    }
    fn snapshot(&mut self, output: &mut Snapshot) -> io::Result<()> {
        output.capture()
    }
    fn file(&mut self, fd: i32, bytes: &mut [u8]) -> io::Result<Option<fd_identity::Resolved>> {
        fd_identity::resolve(fd, bytes)
    }
    fn execute(&mut self, number: i64, args: [u64; 6]) -> i64 {
        unsafe { reverie_preload::user_dispatch::forward_syscall(number, args) }
    }
}
struct Owner {
    tid: i64,
    guest: Vec<Range<u64>>,
    snapshot: Snapshot,
    original_brk: u64,
    generation: u64,
    poisoned: bool,
    guards: guard::State,
}
impl Owner {
    fn output_with<T>(
        &mut self,
        provider: &mut impl Provider,
        tid: i64,
        address: u64,
        length: u64,
        effect: impl FnOnce() -> T,
    ) -> Result<T, Failure> {
        if tid != self.tid {
            return Err(refuse("wrong kernel thread"));
        }
        if self.poisoned {
            return Err(refuse("owner poisoned"));
        }
        let mut workspace = self.guards.active().then(guard::Workspace::qualified);
        let mut current = Snapshot::empty();
        provider.snapshot(&mut current).map_err(|_| {
            self.poisoned = true;
            refuse("output inventory unavailable")
        })?;
        if current.brk != self.snapshot.brk
            || !transaction::unchanged(&self.snapshot.maps, &current.maps, &self.guest)
        {
            self.poisoned = true;
            return Err(refuse("guest mappings or brk changed outside transaction"));
        }
        if let Some(workspace) = &mut workspace {
            self.poisoned = true;
            let observed = provider
                .guards(self.tid, &current.maps, &self.guest, workspace)
                .map_err(|error| observation_failure(error, None))?;
            if observed != self.guards.ranges() {
                return Err(refuse("guard state changed outside transaction"));
            }
        }
        let end = u128::from(address) + u128::from(length);
        for map in &current.maps {
            let start = address.max(map.range.start);
            let end = end.min(u128::from(map.range.end));
            if u128::from(start) < end && !owned(&self.guest, &(start..end as u64)) {
                return Err(refuse("private output overlap"));
            }
        }
        let result = effect();
        self.poisoned = false;
        Ok(result)
    }

    fn view(&self) -> Result<View, Failure> {
        if self.poisoned {
            return Err(refuse("owner poisoned"));
        }
        let mut view = View {
            readable: Vec::new(),
            writable: Vec::new(),
            executable: Vec::new(),
        };
        for map in &self.snapshot.maps {
            for guest in &self.guest {
                if !overlap(&map.range, guest) {
                    continue;
                }
                let part = map.range.start.max(guest.start)..map.range.end.min(guest.end);
                let mut parts = vec![part];
                for guard in self.guards.ranges() {
                    parts = subtract(&parts, guard);
                }
                for part in parts {
                    if map.protection & libc::PROT_READ != 0 {
                        view.readable.push(part.clone());
                    }
                    if map.protection & (libc::PROT_READ | libc::PROT_WRITE) == 3 {
                        view.writable.push(part.clone());
                    }
                    if map.protection & (libc::PROT_READ | libc::PROT_EXEC) == 5 {
                        view.executable.push(part);
                    }
                }
            }
        }
        view.readable = normalize(view.readable);
        view.writable = normalize(view.writable);
        view.executable = normalize(view.executable);
        Ok(view)
    }
    fn execute(
        &mut self,
        provider: &mut impl Provider,
        number: i64,
        args: [u64; 6],
        pinned: &[Range<u64>],
    ) -> Result<i64, Failure> {
        if self.poisoned {
            return Err(refuse("owner poisoned"));
        }
        let next_generation = self.generation.checked_add(1).ok_or_else(|| {
            self.poisoned = true;
            refuse("generation exhausted")
        })?;
        let observing = self.guards.active() || number == libc::SYS_madvise;
        let mut guard_before = observing.then(|| {
            if self.guards.active() {
                guard::Workspace::qualified()
            } else {
                guard::Workspace::new()
            }
        });
        let mut guard_after = observing.then(guard::Workspace::qualified);
        let mut before = Snapshot::empty();
        let mut after = Snapshot::empty();
        provider.snapshot(&mut before).map_err(|_| {
            self.poisoned = true;
            refuse("pre-effect inventory unavailable")
        })?;
        if before.brk != self.snapshot.brk
            || !transaction::unchanged(&self.snapshot.maps, &before.maps, &self.guest)
        {
            self.poisoned = true;
            return Err(refuse("guest mappings or brk changed outside transaction"));
        }
        let request = Request::decode(number, args, before.brk, self.original_brk)?;
        for affected in request.destructive().into_iter().flatten() {
            if pinned.iter().any(|range| overlap(range, &affected))
                || before.maps.iter().any(|map| {
                    overlap(&map.range, &affected)
                        && !owned(
                            &self.guest,
                            &(map.range.start.max(affected.start)..map.range.end.min(affected.end)),
                        )
                })
            {
                return Err(refuse("private or active continuation overlap"));
            }
        }
        if let Some(source) = request.moving_source()
            && !owned(&self.guest, source)
        {
            return Err(refuse("mremap source not entirely guest owned"));
        }
        if let Some(fd) = request.file() {
            let identity = provider
                .file(fd, &mut before.bytes)
                .map_err(|_| refuse("mapping backing identity unavailable"))?;
            if let Some(backing) = identity {
                let (device, inode) = backing.identity;
                request.validate_aliases(&before.maps, Some(backing.identity))?;
                if before.maps.iter().any(|map| {
                    map.inode == inode && map.device == device && !owned(&self.guest, &map.range)
                }) {
                    return Err(refuse("private mapping backing descriptor"));
                }
                let end = backing
                    .length
                    .checked_add(PAGE - 1)
                    .map(|value| value & !(PAGE - 1));
                if request
                    .file_extent()
                    .is_none_or(|range| end.is_none_or(|end| range.end > end))
                {
                    return Err(refuse("file mapping extends beyond observed backing pages"));
                }
            }
        }
        if let Some(source) = request.growing_source()
            && before
                .maps
                .iter()
                .any(|map| map.inode != 0 && overlap(&map.range, source))
        {
            return Err(refuse(
                "file mapping growth requires retained backing-size ownership",
            ));
        }
        request.validate_aliases(&before.maps, None)?;
        if let Some(workspace) = &mut guard_before {
            self.poisoned = true;
            let observed = provider
                .guards(self.tid, &before.maps, &self.guest, workspace)
                .map_err(|error| observation_failure(error, None))?;
            if observed != self.guards.ranges() {
                return Err(refuse("guard state changed outside transaction"));
            }
        }
        self.poisoned = true;
        let result = provider.execute(number, args);
        provider.snapshot(&mut after).map_err(|_| Failure {
            reason: "post-effect inventory unavailable",
            result: Some(result),
            observation: None,
        })?;
        let mut private = Vec::new();
        for map in &before.maps {
            let mut parts = vec![map.range.clone()];
            for guest in &self.guest {
                parts = subtract(&parts, guest);
            }
            private.extend(parts);
        }
        if !transaction::unchanged(&before.maps, &after.maps, &private) {
            return Err(Failure {
                reason: "private mappings changed",
                result: Some(result),
                observation: None,
            });
        }
        let next = request
            .commit(result, &self.guest, &before, &after)
            .map_err(|reason| Failure {
                reason,
                result: Some(result),
                observation: None,
            })?;
        let guards = if let Some(workspace) = &mut guard_after {
            let observed = provider
                .guards(self.tid, &after.maps, &next, workspace)
                .map_err(|error| observation_failure(error, Some(result)))?;
            let affected = request
                .guard_effects(result, after.brk)
                .map_err(|mut error| {
                    error.result = Some(result);
                    error
                })?;
            if !guard::unchanged_outside(self.guards.ranges(), &observed, &affected) {
                return Err(Failure {
                    reason: "guards changed outside effect scope",
                    result: Some(result),
                    observation: None,
                });
            }
            request
                .validate_guards(result, &observed)
                .map_err(|reason| Failure {
                    reason,
                    result: Some(result),
                    observation: None,
                })?;
            Some(observed)
        } else {
            None
        };
        self.guest = next;
        self.snapshot = after;
        if let Some(guards) = guards {
            self.guards = guard::State::Observed(guards);
        }
        self.generation = next_generation;
        self.poisoned = false;
        Ok(result)
    }
}

pub(crate) fn operation(number: i64) -> bool {
    matches!(
        number,
        libc::SYS_mmap
            | libc::SYS_mprotect
            | libc::SYS_munmap
            | libc::SYS_mremap
            | libc::SYS_brk
            | libc::SYS_madvise
    )
}
pub(crate) fn ready() -> bool {
    OWNER.get().is_some()
}
#[cfg(test)]
pub(crate) fn with_guard_route_model(assert_readiness: impl Fn(bool)) {
    guard::tests::registered_guard_route(assert_readiness);
}
pub(crate) fn with_guest_output<T>(
    address: u64,
    length: u64,
    effect: impl FnOnce() -> T,
) -> Result<T, Failure> {
    let mut owner = OWNER
        .get()
        .ok_or_else(|| refuse("owner not prepared"))?
        .lock()
        .map_err(|_| refuse("owner lock poisoned"))?;
    let tid = unsafe { reverie_preload::trap::raw_syscall6(libc::SYS_gettid, [0; 6]) };
    owner.output_with(&mut Linux, tid, address, length, effect)
}
pub(crate) fn view() -> Result<Option<View>, Failure> {
    OWNER
        .get()
        .map(|owner| {
            owner
                .lock()
                .map_err(|_| refuse("owner lock poisoned"))?
                .view()
        })
        .transpose()
}
pub(crate) fn execute(number: i64, args: [u64; 6], pinned: &[Range<u64>]) -> Result<i64, Failure> {
    let mut owner = OWNER
        .get()
        .ok_or_else(|| refuse("owner not prepared"))?
        .lock()
        .map_err(|_| refuse("owner lock poisoned"))?;
    let tid = unsafe { reverie_preload::trap::raw_syscall6(libc::SYS_gettid, [0; 6]) };
    if tid != owner.tid {
        return Err(refuse("wrong kernel thread"));
    }
    owner.execute(&mut Linux, number, args, pinned)
}

pub(crate) fn inject(number: i64, args: [u64; 6]) -> Result<i64, Failure> {
    let (pc, sp) = CONTINUATION
        .get()
        .ok_or_else(|| refuse("no retained guest continuation"))?;
    let stack_begin = sp
        .checked_sub(128)
        .ok_or_else(|| refuse("invalid retained guest stack"))?
        & !(PAGE - 1);
    let pc_begin = pc
        .checked_sub(2)
        .ok_or_else(|| refuse("invalid retained guest pc"))?
        & !(PAGE - 1);
    let pinned = [
        rounded(pc_begin, pc - pc_begin + 16)?,
        rounded(
            stack_begin,
            sp.checked_add(8)
                .ok_or_else(|| refuse("invalid retained guest stack"))?
                - stack_begin,
        )?,
    ];
    if crate::runtime::protected_injected_syscall(number, args).is_some() {
        return Err(refuse("protected runtime descriptor mapping"));
    }
    execute(number, args, &pinned)
}
