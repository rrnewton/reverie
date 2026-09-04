use std::ops::Range;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

struct Execution {
    owner: AtomicI64,
    start: AtomicU64,
    end: AtomicU64,
}

impl Execution {
    const fn new() -> Self {
        Self {
            owner: AtomicI64::new(0),
            start: AtomicU64::new(0),
            end: AtomicU64::new(0),
        }
    }

    fn revoke(
        &self,
        tid: impl FnOnce() -> i64,
        mut protect: impl FnMut(&Range<u64>, i32) -> i64,
    ) -> Result<bool, i64> {
        let owner = self.owner.load(Ordering::Acquire);
        if owner == 0 {
            return Ok(false);
        }
        if owner != tid() {
            return Err(-i64::from(libc::EXDEV));
        }
        let range = self.start.load(Ordering::Relaxed)..self.end.load(Ordering::Relaxed);
        let result = protect(&range, libc::PROT_READ);
        if result != 0 {
            return Err(result);
        }
        self.owner.store(0, Ordering::Release);
        Ok(true)
    }

    fn enable(
        &self,
        range: &Range<u64>,
        owner: i64,
        mut protect: impl FnMut(&Range<u64>, i32) -> i64,
    ) -> Result<(), (i64, Option<i64>)> {
        if range.start == 0
            || range.start >= range.end
            || range.end >= 1 << 47
            || range.start % 4096 != 0
            || range.end % 4096 != 0
        {
            return Err((-i64::from(libc::EINVAL), None));
        }
        if owner <= 0 {
            return Err((owner, None));
        }
        if self
            .owner
            .compare_exchange(0, owner, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err((-i64::from(libc::EBUSY), None));
        }
        self.start.store(range.start, Ordering::Relaxed);
        self.end.store(range.end, Ordering::Relaxed);
        let result = protect(range, libc::PROT_READ | libc::PROT_EXEC);
        if result != 0 {
            return Err((result, self.revoke(|| owner, protect).err()));
        }
        Ok(())
    }
}

static EXECUTION: Execution = Execution::new();

fn protect(range: &Range<u64>, mode: i32) -> i64 {
    unsafe {
        reverie_preload::trap::raw_syscall6(
            libc::SYS_mprotect,
            [range.start, range.end - range.start, mode as u64, 0, 0, 0],
        )
    }
}

pub(crate) fn revoke_execution() -> Result<bool, i64> {
    EXECUTION.revoke(
        || unsafe { reverie_preload::trap::raw_syscall6(libc::SYS_gettid, [0; 6]) },
        protect,
    )
}

pub(super) fn enable_execution(range: &Range<u64>) -> Result<(), i64> {
    let owner = unsafe { reverie_preload::trap::raw_syscall6(libc::SYS_gettid, [0; 6]) };
    EXECUTION
        .enable(range, owner, protect)
        .map_err(|(result, rollback)| {
            if let Some(rollback) = rollback {
                reverie_preload::trap::report_terminal126(
                    "vdso/enable-rollback",
                    "raw-result",
                    Some(rollback),
                );
            }
            result
        })
}

#[cfg(test)]
mod execution_tests {
    use super::*;

    #[test]
    fn grant_revoke_reenter_and_owner_checks_use_exact_retained_range() {
        let execution = Execution::new();
        let mut operations = Vec::new();
        let range = 0x4000..0x6000;
        assert_eq!(
            execution.revoke(
                || panic!("inactive owner query"),
                |_, _| panic!("inactive protection")
            ),
            Ok(false)
        );
        for _ in 0..2 {
            assert_eq!(
                execution.enable(&range, 7, |range, mode| {
                    operations.push((range.clone(), mode));
                    0
                }),
                Ok(())
            );
            assert_eq!(
                execution.enable(&range, 7, |_, _| panic!("nested grant")),
                Err((-i64::from(libc::EBUSY), None))
            );
            assert_eq!(
                execution.revoke(|| 8, |_, _| panic!("foreign revoke")),
                Err(-i64::from(libc::EXDEV))
            );
            assert_eq!(
                execution.revoke(
                    || 7,
                    |range, mode| {
                        operations.push((range.clone(), mode));
                        0
                    }
                ),
                Ok(true)
            );
        }
        assert_eq!(
            operations,
            [
                (range.clone(), 5),
                (range.clone(), 1),
                (range.clone(), 5),
                (range, 1)
            ]
        );
    }

    #[test]
    fn grant_and_revoke_errors_retain_ownership_until_checked_cleanup() {
        let execution = Execution::new();
        let range = 0x4000..0x5000;
        let mut modes = Vec::new();
        assert_eq!(
            execution.enable(&range, 7, |_, mode| {
                modes.push(mode);
                if mode == 5 { -22 } else { -13 }
            }),
            Err((-22, Some(-13)))
        );
        assert_eq!(modes, [5, 1]);
        assert_eq!(execution.owner.load(Ordering::Acquire), 7);
        assert_eq!(execution.revoke(|| 7, |_, _| -5), Err(-5));
        assert_eq!(execution.owner.load(Ordering::Acquire), 7);
        assert_eq!(
            execution.revoke(
                || 7,
                |observed, mode| {
                    assert_eq!(observed, &range);
                    assert_eq!(mode, 1);
                    0
                }
            ),
            Ok(true)
        );
        assert_eq!(execution.owner.load(Ordering::Acquire), 0);
        assert_eq!(
            execution.enable(&range, 7, |_, mode| if mode == 5 { -12 } else { 0 }),
            Err((-12, None))
        );
        assert_eq!(execution.owner.load(Ordering::Acquire), 0);
    }

    #[test]
    fn malformed_range_and_failed_owner_never_change_permissions() {
        let execution = Execution::new();
        for range in [0..4096, 1..4096, 4096..4097, 8192..4096, 4096..1 << 47] {
            assert_eq!(
                execution.enable(&range, 7, |_, _| panic!("invalid range")),
                Err((-22, None))
            );
        }
        assert_eq!(
            execution.enable(&(4096..8192), -5, |_, _| panic!("failed owner")),
            Err((-5, None))
        );
        assert_eq!(execution.owner.load(Ordering::Acquire), 0);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Change {
    pub range: Range<u64>,
    pub before: i32,
    pub after: i32,
}

#[derive(Debug)]
pub(super) struct Failure {
    pub cause: i64,
    pub applied: usize,
    pub rollback: Vec<(Range<u64>, i64)>,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "vDSO protection failure {} after {} changes; rollback {:?}",
            self.cause, self.applied, self.rollback
        )
    }
}
impl std::error::Error for Failure {}

pub(super) fn apply(
    changes: &[Change],
    mut protect: impl FnMut(&Range<u64>, i32) -> i64,
) -> Result<(), Failure> {
    if changes.is_empty()
        || changes.len() > 3
        || changes.iter().enumerate().any(|(index, change)| {
            change.range.start == 0
                || change.range.start % 4096 != 0
                || change.range.end % 4096 != 0
                || change.range.start >= change.range.end
                || change.range.end >= 1 << 47
                || !matches!(change.before, libc::PROT_READ | 5)
                || !matches!(change.after, libc::PROT_NONE | libc::PROT_READ)
                || changes[..index].iter().any(|prior| {
                    prior.range.start < change.range.end && change.range.start < prior.range.end
                })
        })
    {
        return Err(Failure {
            cause: -i64::from(libc::EINVAL),
            applied: 0,
            rollback: Vec::new(),
        });
    }
    let mut rollback = Vec::with_capacity(changes.len());
    for (index, change) in changes.iter().enumerate() {
        let result = protect(&change.range, change.after);
        if result != 0 {
            for prior in changes[..=index].iter().rev() {
                rollback.push((prior.range.clone(), protect(&prior.range, prior.before)));
            }
            return Err(Failure {
                cause: result,
                applied: index,
                rollback,
            });
        }
    }
    Ok(())
}
