use std::collections::VecDeque;

use super::*;

struct Mock {
    responses: VecDeque<(i64, u64, Vec<Region>)>,
    opened: usize,
    closed: usize,
    calls: usize,
    owner: i64,
    changed_owner: bool,
    open_result: i64,
    close_result: i64,
    panic_query: bool,
    one_page: bool,
    order: Vec<&'static str>,
    requests: Vec<(u64, u64)>,
}
impl Default for Mock {
    fn default() -> Self {
        Self {
            responses: VecDeque::new(),
            opened: 0,
            closed: 0,
            calls: 0,
            owner: 1,
            changed_owner: false,
            open_result: 7,
            close_result: 0,
            panic_query: false,
            one_page: false,
            order: Vec::new(),
            requests: Vec::new(),
        }
    }
}
impl Kernel for Mock {
    fn tid(&mut self) -> i64 {
        self.order.push("tid");
        self.owner
    }
    fn open(&mut self) -> i64 {
        self.order.push("open");
        self.opened += 1;
        self.open_result
    }
    fn close(&mut self, fd: i64) -> i64 {
        self.order.push("close");
        assert_eq!(fd, 7);
        self.closed += 1;
        if self.changed_owner {
            self.owner = 2;
        }
        self.close_result
    }
    fn query(&mut self, fd: i64, query: &mut Query, rows: &mut [Region]) -> i64 {
        self.order.push("query");
        self.requests.push((query.start, query.end));
        assert_eq!(fd, 7);
        assert_eq!(query.size, 96);
        assert_eq!(query.flags, 0);
        assert_eq!(
            query.category_mask
                | query.category_anyof_mask
                | query.category_inverted
                | query.max_pages
                | query.walk_end,
            0
        );
        assert_eq!(query.return_mask, PAGE_IS_GUARD);
        assert_eq!(query.vec_len, ROWS as u64);
        assert_eq!(query.vec, rows.as_mut_ptr() as u64);
        assert!(
            rows.iter()
                .all(|row| row.start == 0 && row.end == 0 && row.categories == 0)
        );
        self.calls += 1;
        assert!(!self.panic_query, "query unwind");
        if let Some((count, end, response)) = self.responses.pop_front() {
            query.walk_end = end;
            rows[..response.len()].copy_from_slice(&response);
            count
        } else {
            let end = if self.one_page {
                query.start + PAGE
            } else {
                query.end
            };
            rows[0] = row(query.start, end, 0);
            query.walk_end = end;
            1
        }
    }
}
fn row(start: u64, end: u64, categories: u64) -> Region {
    Region {
        start,
        end,
        categories,
    }
}
fn mapped(range: Range<u64>) -> Map {
    Map {
        range,
        protection: 3,
        offset: 0,
        device: (0, 0),
        inode: 0,
        stack: false,
        shared: false,
    }
}
fn run(mock: &mut Mock) -> Result<Vec<Range<u64>>, Error> {
    observe_with(
        mock,
        1,
        &[mapped(PAGE..4 * PAGE)],
        &[PAGE..4 * PAGE],
        &mut Workspace::new(),
    )
}

#[test]
fn complete_guard_cursor_protocol_matches_host_control() {
    let mut mock = Mock::default();
    for (start, end, category) in [
        (PAGE, 2 * PAGE, 0),
        (2 * PAGE, 3 * PAGE, PAGE_IS_GUARD),
        (3 * PAGE, 4 * PAGE, 0),
    ] {
        mock.responses
            .push_back((1, end, vec![row(start, end, category)]));
    }
    assert_eq!(run(&mut mock).unwrap(), vec![2 * PAGE..3 * PAGE]);
    assert_eq!((mock.opened, mock.calls, mock.closed), (1, 3, 1));
}

#[test]
fn explicit_clear_rows_and_mapped_fragments_do_not_scan_holes() {
    let mut mock = Mock::default();
    let maps = [
        mapped(PAGE..2 * PAGE),
        mapped(3 * PAGE..4 * PAGE),
        mapped(6 * PAGE..7 * PAGE),
    ];
    assert!(
        observe_with(
            &mut mock,
            1,
            &maps,
            &[PAGE..5 * PAGE],
            &mut Workspace::new()
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!((mock.calls, mock.closed), (2, 1));
}

#[test]
fn malformed_guard_results_close_without_empty_success() {
    let cases = vec![
        (0, 4 * PAGE, vec![]),
        (65, 4 * PAGE, vec![]),
        (1, PAGE, vec![row(PAGE, 2 * PAGE, 0)]),
        (1, 5 * PAGE, vec![row(PAGE, 5 * PAGE, 0)]),
        (1, 4 * PAGE, vec![row(PAGE + 1, 4 * PAGE, 0)]),
        (1, 4 * PAGE, vec![row(PAGE, PAGE, 0)]),
        (1, 4 * PAGE, vec![row(PAGE, 4 * PAGE, 1)]),
        (1, 4 * PAGE, vec![row(PAGE, 3 * PAGE, 0)]),
        (
            2,
            4 * PAGE,
            vec![row(PAGE, 3 * PAGE, 0), row(2 * PAGE, 4 * PAGE, 0)],
        ),
        (1, 4 * PAGE + 1, vec![row(PAGE, 4 * PAGE + 1, 0)]),
    ];
    for response in cases {
        let mut mock = Mock::default();
        mock.responses.push_back(response);
        assert!(run(&mut mock).is_err());
        assert_eq!((mock.opened, mock.calls, mock.closed), (1, 1, 1));
    }
}

#[test]
fn query_errno_and_close_failure_both_retained_without_retry() {
    let mut mock = Mock {
        close_result: -i64::from(libc::EINTR),
        ..Mock::default()
    };
    mock.responses
        .push_back((-i64::from(libc::EINVAL), 0, vec![]));
    let error = run(&mut mock).unwrap_err();
    assert_eq!(error.syscall_result, Some(-i64::from(libc::EINVAL)));
    assert_eq!(error.close_result, Some(-i64::from(libc::EINTR)));
    assert_eq!((mock.calls, mock.closed), (1, 1));
    let mut mock = Mock {
        close_result: -i64::from(libc::EIO),
        ..Mock::default()
    };
    assert_eq!(
        run(&mut mock).unwrap_err().close_result,
        Some(-i64::from(libc::EIO))
    );
    assert_eq!(mock.closed, 1);
}

#[test]
fn open_and_owner_errors_do_not_leak_descriptors() {
    for error in [libc::EMFILE, libc::EACCES] {
        let mut mock = Mock {
            open_result: -i64::from(error),
            ..Mock::default()
        };
        assert_eq!(
            run(&mut mock).unwrap_err().syscall_result,
            Some(-i64::from(error))
        );
        assert_eq!((mock.calls, mock.closed), (0, 0));
    }
    let mut mock = Mock {
        owner: 2,
        ..Mock::default()
    };
    assert!(run(&mut mock).is_err());
    assert_eq!(mock.opened, 0);
    let mut mock = Mock {
        changed_owner: true,
        ..Mock::default()
    };
    assert!(run(&mut mock).is_err());
    assert_eq!(mock.closed, 1);
}

#[test]
fn query_unwind_closes_once() {
    let mut mock = Mock {
        panic_query: true,
        ..Mock::default()
    };
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&mut mock))).is_err());
    assert_eq!((mock.calls, mock.closed), (1, 1));
}

#[test]
fn empty_unaligned_and_overlapping_inventory_refuses_before_open() {
    for maps in [
        vec![],
        vec![mapped(PAGE + 1..3 * PAGE)],
        vec![mapped(PAGE..3 * PAGE), mapped(2 * PAGE..4 * PAGE)],
        vec![mapped(PAGE..2 * PAGE); CAPACITY + 1],
    ] {
        let mut mock = Mock::default();
        assert!(
            observe_with(
                &mut mock,
                1,
                &maps,
                &[PAGE..4 * PAGE],
                &mut Workspace::new()
            )
            .is_err()
        );
        assert_eq!(mock.opened, 0);
    }
}

#[test]
fn observation_work_and_storage_limits_refuse_without_dropping_rows() {
    let mut mock = Mock {
        one_page: true,
        ..Mock::default()
    };
    let range = PAGE..((WORK_LIMIT + 2) as u64 * PAGE);
    assert!(
        observe_with(
            &mut mock,
            1,
            &[mapped(range.clone())],
            &[range],
            &mut Workspace::new()
        )
        .is_err()
    );
    assert_eq!(mock.calls, WORK_LIMIT);
    assert_eq!(mock.closed, 1);
    let mut guards = Vec::with_capacity(CAPACITY);
    for index in 0..CAPACITY {
        append(
            &mut guards,
            index as u64 * 2 * PAGE..(index as u64 * 2 + 1) * PAGE,
        )
        .unwrap();
    }
    assert!(
        append(
            &mut guards,
            CAPACITY as u64 * 2 * PAGE..(CAPACITY as u64 * 2 + 1) * PAGE
        )
        .is_err()
    );
    assert_eq!(guards.len(), CAPACITY);
}

#[test]
fn consumed_workspace_cannot_allocate_during_observation() {
    let mut mock = Mock::default();
    let mut workspace = Workspace::new();
    let maps = [mapped(PAGE..4 * PAGE)];
    observe_with(&mut mock, 1, &maps, &[PAGE..4 * PAGE], &mut workspace).unwrap();
    assert!(observe_with(&mut mock, 1, &maps, &[PAGE..4 * PAGE], &mut workspace).is_err());
    assert_eq!(mock.opened, 1);
}

#[test]
fn qualified_empty_inventory_is_not_a_new_capability_probe() {
    let mut mock = Mock::default();
    assert!(
        observe_with(&mut mock, 1, &[], &[], &mut Workspace::qualified())
            .unwrap()
            .is_empty()
    );
    assert_eq!((mock.opened, mock.calls, mock.closed), (0, 0, 0));
    assert!(observe_with(&mut mock, 1, &[], &[], &mut Workspace::new()).is_err());
}

struct Connected {
    kernel: Mock,
    before: Vec<Map>,
    after: Vec<Map>,
    calls: Vec<(i64, [u64; 6])>,
    result: i64,
}
impl Connected {
    fn new(maps: Vec<Map>, result: i64) -> Self {
        Self {
            kernel: Mock::default(),
            before: maps.clone(),
            after: maps,
            calls: Vec::new(),
            result,
        }
    }
    fn owner(&self) -> Owner {
        let mut snapshot = Snapshot::empty();
        snapshot.maps.extend_from_slice(&self.before);
        snapshot.brk = 0x90000;
        Owner {
            tid: 1,
            guest: self.before.iter().map(|map| map.range.clone()).collect(),
            snapshot,
            original_brk: 0x90000,
            generation: 0,
            poisoned: false,
            guards: State::FreshExecUnqueried,
        }
    }
    fn answer(&mut self, rows: Vec<Region>) {
        let end = rows.last().unwrap().end;
        self.kernel
            .responses
            .push_back((rows.len() as i64, end, rows));
    }
}
impl Provider for Connected {
    fn snapshot(&mut self, output: &mut Snapshot) -> io::Result<()> {
        self.kernel.order.push("snapshot");
        output.maps.clear();
        output.maps.extend_from_slice(if self.calls.is_empty() {
            &self.before
        } else {
            &self.after
        });
        output.brk = 0x90000;
        Ok(())
    }
    fn file(&mut self, _: i32, _: &mut [u8]) -> io::Result<Option<fd_identity::Resolved>> {
        panic!("unexpected file lookup in guard control")
    }
    fn execute(&mut self, number: i64, args: [u64; 6]) -> i64 {
        self.kernel.order.push("native");
        self.calls.push((number, args));
        self.result
    }
    fn guards(
        &mut self,
        tid: i64,
        maps: &[Map],
        guest: &[Range<u64>],
        workspace: &mut Workspace,
    ) -> Result<Vec<Range<u64>>, Error> {
        observe_with(&mut self.kernel, tid, maps, guest, workspace)
    }
}
fn guard_args(advice: u64) -> [u64; 6] {
    [PAGE, PAGE, (0x1234_u64 << 32) | advice, 0xa1, 0xb2, 0xc3]
}
fn assert_completed(owner: &Owner, guards: &[Range<u64>], generation: u64) {
    assert!(matches!(owner.guards, State::Observed(_)));
    assert_eq!(owner.guards.ranges(), guards);
    assert_eq!(owner.generation, generation);
    assert!(!owner.poisoned);
    assert!(owner.view().is_ok());
}
fn assert_poisoned_without_retry(owner: &mut Owner, provider: &mut Connected, generation: u64) {
    assert!(owner.poisoned);
    assert_eq!(owner.generation, generation);
    assert!(owner.view().is_err());
    let order = provider.kernel.order.clone();
    let effects = provider.calls.clone();
    assert_eq!(
        owner
            .execute(provider, libc::SYS_madvise, guard_args(103), &[])
            .unwrap_err()
            .reason,
        "owner poisoned"
    );
    assert_eq!(provider.kernel.order, order);
    assert_eq!(provider.calls, effects);
}

#[test]
fn connected_first_remove_qualifies_all_owned_mapped_fragments() {
    let mut provider = Connected::new(
        vec![
            mapped(PAGE..3 * PAGE),
            mapped(5 * PAGE..6 * PAGE),
            mapped(8 * PAGE..9 * PAGE),
        ],
        0,
    );
    let mut owner = provider.owner();
    owner.guest = vec![PAGE..6 * PAGE];
    let args = guard_args(103);
    assert_eq!(
        owner
            .execute(&mut provider, libc::SYS_madvise, args, &[])
            .unwrap(),
        0
    );
    assert_completed(&owner, &[], 1);
    assert_eq!(provider.calls, [(libc::SYS_madvise, args)]);
    assert_eq!(
        provider.kernel.requests,
        [
            (PAGE, 3 * PAGE),
            (5 * PAGE, 6 * PAGE),
            (PAGE, 3 * PAGE),
            (5 * PAGE, 6 * PAGE)
        ]
    );
    assert_eq!(
        provider.kernel.order,
        [
            "snapshot", "tid", "open", "query", "query", "close", "tid", "native", "snapshot",
            "tid", "open", "query", "query", "close", "tid"
        ]
    );
    assert_eq!((provider.kernel.opened, provider.kernel.closed), (2, 2));
}

#[test]
fn connected_first_install_negative_partial_and_empty_publish_actual_state() {
    for partial in [false, true] {
        let mut provider = Connected::new(vec![mapped(PAGE..3 * PAGE)], -i64::from(libc::ENOMEM));
        provider.answer(vec![row(PAGE, 3 * PAGE, 0)]);
        provider.answer(vec![
            row(PAGE, 2 * PAGE, if partial { PAGE_IS_GUARD } else { 0 }),
            row(2 * PAGE, 3 * PAGE, 0),
        ]);
        let mut owner = provider.owner();
        let args = guard_args(102);
        assert_eq!(
            owner
                .execute(&mut provider, libc::SYS_madvise, args, &[])
                .unwrap(),
            -i64::from(libc::ENOMEM)
        );
        assert_completed(&owner, if partial { &[PAGE..2 * PAGE] } else { &[] }, 1);
        assert_eq!(provider.calls, [(libc::SYS_madvise, args)]);
        assert_eq!(
            provider.kernel.order,
            [
                "snapshot", "tid", "open", "query", "close", "tid", "native", "snapshot", "tid",
                "open", "query", "close", "tid"
            ]
        );
        assert_eq!(
            (
                provider.kernel.opened,
                provider.kernel.calls,
                provider.kernel.closed
            ),
            (2, 2, 2)
        );
        assert!(provider.kernel.responses.is_empty());
    }
}

#[test]
fn connected_first_empty_baseline_refuses_without_native_effect() {
    for advice in [102, 103] {
        let mut provider = Connected::new(vec![], 0);
        let mut owner = provider.owner();
        let error = owner
            .execute(&mut provider, libc::SYS_madvise, guard_args(advice), &[])
            .unwrap_err();
        assert_eq!(error.result, None);
        assert_eq!(
            error.observation.unwrap().reason,
            "empty guard baseline cannot qualify observation"
        );
        assert!(matches!(owner.guards, State::FreshExecUnqueried));
        assert_eq!(provider.kernel.order, ["snapshot", "tid"]);
        assert!(provider.calls.is_empty());
        assert_eq!(
            (
                provider.kernel.opened,
                provider.kernel.calls,
                provider.kernel.closed
            ),
            (0, 0, 0)
        );
        assert_poisoned_without_retry(&mut owner, &mut provider, 0);
    }
}

#[test]
fn connected_last_unmap_clears_previously_qualified_guard_state() {
    let mut provider = Connected::new(vec![mapped(PAGE..2 * PAGE)], 0);
    provider.after.clear();
    provider.answer(vec![row(PAGE, 2 * PAGE, PAGE_IS_GUARD)]);
    let mut owner = provider.owner();
    owner.guards = State::Observed(vec![PAGE..2 * PAGE]);
    let args = [PAGE, PAGE, 0, 0, 0, 0];
    assert_eq!(
        owner
            .execute(&mut provider, libc::SYS_munmap, args, &[])
            .unwrap(),
        0
    );
    assert_completed(&owner, &[], 1);
    assert!(owner.guest.is_empty());
    assert!(owner.snapshot.maps.is_empty());
    assert_eq!(provider.calls, [(libc::SYS_munmap, args)]);
    assert_eq!(
        provider.kernel.order,
        [
            "snapshot", "tid", "open", "query", "close", "tid", "native", "snapshot", "tid", "tid"
        ]
    );
    assert_eq!(
        (
            provider.kernel.opened,
            provider.kernel.calls,
            provider.kernel.closed
        ),
        (1, 1, 1)
    );
}

#[test]
fn connected_preexisting_and_disappeared_guards_refuse_before_effect() {
    for preexisting in [false, true] {
        let mut provider = Connected::new(vec![mapped(PAGE..2 * PAGE)], 0);
        provider.answer(vec![row(
            PAGE,
            2 * PAGE,
            if preexisting { PAGE_IS_GUARD } else { 0 },
        )]);
        let mut owner = provider.owner();
        if !preexisting {
            owner.guards = State::Observed(vec![PAGE..2 * PAGE]);
        }
        let error = owner
            .execute(&mut provider, libc::SYS_madvise, guard_args(103), &[])
            .unwrap_err();
        assert_eq!(error.reason, "guard state changed outside transaction");
        assert_eq!(error.result, None);
        assert_eq!(owner.guards.active(), !preexisting);
        assert_eq!(
            provider.kernel.order,
            ["snapshot", "tid", "open", "query", "close", "tid"]
        );
        assert!(provider.calls.is_empty());
        assert_poisoned_without_retry(&mut owner, &mut provider, 0);
    }
}

#[test]
fn connected_post_effect_query_failure_retains_result_and_poison() {
    let mut provider = Connected::new(vec![mapped(PAGE..2 * PAGE)], 0);
    provider.answer(vec![row(PAGE, 2 * PAGE, 0)]);
    provider
        .kernel
        .responses
        .push_back((-i64::from(libc::EIO), 0, vec![]));
    let mut owner = provider.owner();
    let args = guard_args(102);
    let error = owner
        .execute(&mut provider, libc::SYS_madvise, args, &[])
        .unwrap_err();
    assert_eq!(error.result, Some(0));
    assert_eq!(
        error.observation.unwrap().syscall_result,
        Some(-i64::from(libc::EIO))
    );
    assert!(matches!(owner.guards, State::FreshExecUnqueried));
    assert_eq!(provider.calls, [(libc::SYS_madvise, args)]);
    assert_eq!(
        provider.kernel.order,
        [
            "snapshot", "tid", "open", "query", "close", "tid", "native", "snapshot", "tid",
            "open", "query", "close", "tid"
        ]
    );
    assert_poisoned_without_retry(&mut owner, &mut provider, 0);
}

#[test]
fn connected_generation_exhaustion_precedes_snapshot_query_and_native() {
    let mut provider = Connected::new(vec![mapped(PAGE..2 * PAGE)], 0);
    let mut owner = provider.owner();
    owner.generation = u64::MAX;
    assert_eq!(
        owner
            .execute(&mut provider, libc::SYS_madvise, guard_args(102), &[])
            .unwrap_err()
            .reason,
        "generation exhausted"
    );
    assert!(provider.kernel.order.is_empty());
    assert!(provider.calls.is_empty());
    assert_poisoned_without_retry(&mut owner, &mut provider, u64::MAX);
}

#[test]
fn connected_active_output_validates_observation_and_unwind_before_publication() {
    for mode in 0..6 {
        let mut provider = Connected::new(vec![mapped(PAGE..3 * PAGE)], 0);
        let mut owner = provider.owner();
        owner.guards = State::Observed(vec![PAGE..2 * PAGE]);
        owner.generation = 7;
        provider.answer(vec![
            row(PAGE, 2 * PAGE, if mode == 1 { 0 } else { PAGE_IS_GUARD }),
            row(2 * PAGE, 3 * PAGE, 0),
        ]);
        if mode == 2 {
            provider
                .kernel
                .responses
                .push_front((-i64::from(libc::EIO), 0, vec![]));
        }
        provider.kernel.panic_query = mode == 3;
        if mode == 4 {
            provider.kernel.close_result = -i64::from(libc::EBADF);
        }
        let effect = Cell::new(false);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            owner.output_with(&mut provider, 1, 2 * PAGE, PAGE, || {
                effect.set(true);
                assert_ne!(mode, 5, "output unwind");
                -i64::from(libc::EFAULT)
            })
        }));
        assert_eq!(effect.get(), matches!(mode, 0 | 5));
        assert_eq!(owner.guards.ranges(), &[PAGE..2 * PAGE]);
        assert!(provider.calls.is_empty());
        assert_eq!((provider.kernel.opened, provider.kernel.closed), (1, 1));
        assert_eq!(
            provider.kernel.order,
            if mode == 3 {
                vec!["snapshot", "tid", "open", "query", "close"]
            } else {
                vec!["snapshot", "tid", "open", "query", "close", "tid"]
            }
        );
        match mode {
            0 => {
                assert_eq!(result.unwrap().unwrap(), -i64::from(libc::EFAULT));
                assert_completed(&owner, &[PAGE..2 * PAGE], 7);
            }
            3 | 5 => {
                assert!(result.is_err());
                assert_poisoned_without_retry(&mut owner, &mut provider, 7);
            }
            _ => {
                assert_eq!(result.unwrap().unwrap_err().result, None);
                assert_poisoned_without_retry(&mut owner, &mut provider, 7);
            }
        }
    }
}

#[test]
fn connected_active_output_wrong_owner_has_no_observation_or_effect() {
    let mut provider = Connected::new(vec![mapped(PAGE..2 * PAGE)], 0);
    let mut owner = provider.owner();
    owner.guards = State::Observed(vec![]);
    assert_eq!(
        owner
            .output_with(&mut provider, 2, PAGE, PAGE, || panic!(
                "wrong owner output"
            ))
            .unwrap_err()
            .reason,
        "wrong kernel thread"
    );
    assert_completed(&owner, &[], 0);
    assert!(provider.kernel.order.is_empty());
}

pub(in crate::mapping) fn registered_guard_route(assert_readiness: impl Fn(bool)) {
    assert_readiness(false);
    let retained = enter(8 * PAGE, 10 * PAGE).unwrap();
    assert_eq!(
        inject(libc::SYS_madvise, guard_args(102))
            .unwrap_err()
            .reason,
        "owner not prepared"
    );
    drop(retained);
    let mut provider = Connected::new(vec![mapped(PAGE..2 * PAGE)], 0);
    assert!(OWNER.set(Mutex::new(provider.owner())).is_ok());
    assert_readiness(true);
    let mut owner = OWNER.get().unwrap().lock().unwrap();
    assert_eq!(
        owner
            .execute(&mut provider, libc::SYS_madvise, guard_args(101), &[])
            .unwrap_err()
            .reason,
        "unsupported madvise advice"
    );
    assert!(provider.calls.is_empty());
    assert_eq!(provider.kernel.order, ["snapshot"]);
    assert_eq!(owner.generation, 0);
    assert!(!owner.poisoned);
    assert!(matches!(owner.guards, State::FreshExecUnqueried));
    provider.kernel.order.clear();
    provider.answer(vec![row(PAGE, 2 * PAGE, 0)]);
    provider.answer(vec![row(PAGE, 2 * PAGE, PAGE_IS_GUARD)]);
    provider.answer(vec![row(PAGE, 2 * PAGE, PAGE_IS_GUARD)]);
    provider.answer(vec![row(PAGE, 2 * PAGE, 0)]);
    for (advice, guards, generation) in [(102, vec![PAGE..2 * PAGE], 1), (103, vec![], 2)] {
        assert_readiness(true);
        let args = guard_args(advice);
        assert_eq!(
            owner
                .execute(&mut provider, libc::SYS_madvise, args, &[])
                .unwrap(),
            0
        );
        assert_completed(&owner, &guards, generation);
    }
    assert_eq!(
        provider.calls,
        [
            (libc::SYS_madvise, guard_args(102)),
            (libc::SYS_madvise, guard_args(103))
        ]
    );
    let transaction = [
        "snapshot", "tid", "open", "query", "close", "tid", "native", "snapshot", "tid", "open",
        "query", "close", "tid",
    ];
    assert_eq!(provider.kernel.order, transaction.repeat(2));
    assert_eq!(
        (
            provider.kernel.opened,
            provider.kernel.calls,
            provider.kernel.closed
        ),
        (4, 4, 4)
    );
    assert!(provider.kernel.responses.is_empty());
}
