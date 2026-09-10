use super::*;

fn isolate_host_mapping(name: &str) -> bool {
    const CHILD: &str = "REVERIE_ISOLATED_HOST_MAPPING";
    let test = format!("mapping::tests::{name}");
    if std::env::var(CHILD).is_ok_and(|selected| selected == test) {
        return false;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &test, "--nocapture"])
        .env(CHILD, &test)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{test}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for line in String::from_utf8_lossy(&output.stderr).lines() {
        eprintln!("host-mapping-child {test}: {line}");
    }
    let stdout = std::str::from_utf8(&output.stdout).unwrap();
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("test result: ok. 1 passed; 0 failed; 0 ignored;")),
        "{test}: {stdout}"
    );
    true
}

fn map(start: u64, end: u64, protection: i32) -> Map {
    Map {
        range: start..end,
        protection,
        offset: 0,
        device: (0, 0),
        inode: 0,
        stack: false,
        shared: false,
    }
}
fn snapshot(maps: &[Map], brk: u64) -> Snapshot {
    let mut result = Snapshot::empty();
    result.maps.extend_from_slice(maps);
    result.brk = brk;
    result
}
struct Model {
    before: Vec<Map>,
    after: Vec<Map>,
    brk_before: u64,
    brk_after: u64,
    result: i64,
    effects: usize,
    snapshots: usize,
    fail_after: bool,
    fail_before: bool,
    calls: Vec<(i64, [u64; 6])>,
    guard_before: Vec<Range<u64>>,
    guard_after: Vec<Range<u64>>,
    guard_queries: usize,
    guard_failure: Option<(usize, guard::Error)>,
}
impl Model {
    fn new(before: Vec<Map>, after: Vec<Map>, result: i64) -> Self {
        Self {
            before,
            after,
            result,
            brk_before: 0x90000,
            brk_after: 0x90000,
            effects: 0,
            snapshots: 0,
            fail_after: false,
            fail_before: false,
            calls: Vec::new(),
            guard_before: Vec::new(),
            guard_after: Vec::new(),
            guard_queries: 0,
            guard_failure: None,
        }
    }
}
impl Provider for Model {
    fn guards(
        &mut self,
        tid: i64,
        _: &[Map],
        _: &[Range<u64>],
        _: &mut guard::Workspace,
    ) -> Result<Vec<Range<u64>>, guard::Error> {
        assert_eq!(tid, 1);
        self.guard_queries += 1;
        if let Some((call, error)) = self.guard_failure
            && call == self.guard_queries
        {
            return Err(error);
        }
        Ok(if self.effects == 0 {
            &self.guard_before
        } else {
            &self.guard_after
        }
        .clone())
    }
    fn snapshot(&mut self, output: &mut Snapshot) -> io::Result<()> {
        self.snapshots += 1;
        if self.fail_before || (self.effects != 0 && self.fail_after) {
            return Err(io::Error::other("injected inventory failure"));
        }
        output.maps.clear();
        output.maps.extend_from_slice(if self.effects == 0 {
            &self.before
        } else {
            &self.after
        });
        output.brk = if self.effects == 0 {
            self.brk_before
        } else {
            self.brk_after
        };
        Ok(())
    }
    fn file(&mut self, _: i32, _: &mut [u8]) -> io::Result<Option<fd_identity::Resolved>> {
        Ok(None)
    }
    fn execute(&mut self, number: i64, args: [u64; 6]) -> i64 {
        self.calls.push((number, args));
        self.effects += 1;
        self.result
    }
}
fn owner(model: &Model, guest: Vec<Range<u64>>) -> Owner {
    Owner {
        tid: 1,
        guest,
        snapshot: snapshot(&model.before, model.brk_before),
        original_brk: model.brk_before,
        generation: 0,
        poisoned: false,
        guards: guard::State::FreshExecUnqueried,
    }
}

#[test]
fn stack_anonymous_request_preserves_provider_args_and_ownership() {
    let length = 8392704;
    let address = 0x1000000;
    let args = [
        0,
        length,
        (libc::PROT_READ | libc::PROT_WRITE) as u64,
        (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_STACK) as u64,
        u64::MAX,
        0,
    ];
    let before = vec![map(0x1000, 0x2000, 3)];
    let mut after = before.clone();
    after.push(map(address, address + length, 3));
    let mut model = Model::new(before, after, address as i64);
    let mut owner = owner(&model, vec![0x1000..0x2000; 1]);
    let result = owner.execute(&mut model, libc::SYS_mmap, args, &[]);
    assert_eq!(
        model.calls,
        vec![(libc::SYS_mmap, args)],
        "must forward MAP_STACK unchanged: {result:?}"
    );
    assert_eq!(result.unwrap(), address as i64);
    assert_eq!(model.effects, 1);
    assert_eq!(model.snapshots, 2);
    assert_eq!(owner.guest, vec![0x1000..0x2000, address..address + length]);
    assert_eq!(owner.generation, 1);
    assert!(!owner.poisoned);
    assert_eq!(model.guard_queries, 0);
}

#[test]
fn guard_native_install_remove_preserves_all_args_and_updates_views() {
    let maps = vec![map(0x1000, 0x5000, 3)];
    let mut model = Model::new(maps.clone(), maps, 0);
    model.guard_after = vec![0x2000..0x3000; 1];
    let mut owner = owner(&model, vec![0x1000..0x5000; 1]);
    assert!(matches!(owner.guards, guard::State::FreshExecUnqueried));
    let args = [0x2000, PAGE, (1_u64 << 32) | 102, 17, 18, 19];
    assert_eq!(
        owner
            .execute(&mut model, libc::SYS_madvise, args, &[])
            .unwrap(),
        0
    );
    assert_eq!(model.calls, vec![(libc::SYS_madvise, args)]);
    assert_eq!(owner.guards.ranges(), &[0x2000..0x3000; 1]);
    assert_eq!(owner.snapshot.maps[0].protection, 3);
    assert_eq!(
        owner.view().unwrap().writable,
        vec![0x1000..0x2000, 0x3000..0x5000]
    );
    model.effects = 0;
    model.guard_before = model.guard_after.clone();
    model.guard_after.clear();
    let remove = [0x2000, PAGE, 103, 17, 18, 19];
    assert_eq!(
        owner
            .execute(&mut model, libc::SYS_madvise, remove, &[])
            .unwrap(),
        0
    );
    assert_eq!(model.calls.len(), 2);
    assert_eq!(model.calls[1], (libc::SYS_madvise, remove));
    assert_eq!(model.guard_queries, 4);
    assert_eq!(owner.generation, 2);
    assert!(owner.guards.ranges().is_empty());
    assert_eq!(owner.view().unwrap().readable, vec![0x1000..0x5000]);
}

#[test]
fn guard_partial_native_errors_publish_actual_markers_not_rollback() {
    for advice in [102, 103] {
        let maps = vec![map(0x1000, 0x5000, 3)];
        let mut model = Model::new(maps.clone(), maps, -i64::from(libc::ENOMEM));
        model.guard_before = if advice == 102 {
            vec![]
        } else {
            vec![0x2000..0x4000; 1]
        };
        model.guard_after = vec![0x3000..0x4000; 1];
        let mut owner = owner(&model, vec![0x1000..0x5000; 1]);
        owner.guards = guard::State::Observed(model.guard_before.clone());
        let args = [0x2000, 2 * PAGE, advice, 0, 0, 0];
        assert_eq!(
            owner
                .execute(&mut model, libc::SYS_madvise, args, &[])
                .unwrap(),
            -i64::from(libc::ENOMEM)
        );
        assert_eq!(model.calls, vec![(libc::SYS_madvise, args)]);
        assert_eq!(owner.guards.ranges(), &[0x3000..0x4000; 1]);
        assert_eq!(
            owner.view().unwrap().writable,
            vec![0x1000..0x3000, 0x4000..0x5000]
        );
        assert_eq!(owner.generation, 1);
        assert!(!owner.poisoned);
    }
}

#[test]
fn guard_query_failures_keep_native_result_and_old_state_poisoned() {
    for call in [1, 2] {
        let maps = vec![map(0x1000, 0x5000, 3)];
        let mut model = Model::new(maps.clone(), maps, -i64::from(libc::ENOMEM));
        let observer = guard::Error {
            reason: "query then close failed",
            syscall_result: Some(-i64::from(libc::EIO)),
            close_result: Some(-i64::from(libc::EINTR)),
        };
        model.guard_failure = Some((call, observer));
        let mut owner = owner(&model, vec![0x1000..0x5000; 1]);
        let args = [0x2000, PAGE, 102, 0, 0, 0];
        let error = owner
            .execute(&mut model, libc::SYS_madvise, args, &[])
            .unwrap_err();
        assert_eq!(error.observation, Some(observer));
        assert_eq!(
            error.result,
            (call == 2).then_some(-i64::from(libc::ENOMEM))
        );
        assert_eq!(model.effects, usize::from(call == 2));
        assert!(owner.poisoned);
        assert!(owner.view().is_err());
        assert_eq!(owner.generation, 0);
        assert!(matches!(owner.guards, guard::State::FreshExecUnqueried));
        assert!(
            owner
                .execute(&mut model, libc::SYS_madvise, args, &[])
                .is_err()
        );
        assert_eq!(model.guard_queries, call);
    }
}

#[test]
fn guard_unexpected_baseline_outside_changes_and_false_success_refuse() {
    for scenario in 0..4 {
        let maps = vec![map(0x1000, 0x5000, 3)];
        let mut model = Model::new(maps.clone(), maps, 0);
        match scenario {
            0 => model.guard_before = vec![0x2000..0x3000; 1],
            1 => model.guard_after = vec![0x2000..0x4000; 1],
            2 => {}
            _ => model.result = 1,
        }
        let mut owner = owner(&model, vec![0x1000..0x5000; 1]);
        assert!(
            owner
                .execute(
                    &mut model,
                    libc::SYS_madvise,
                    [0x2000, PAGE, 102, 0, 0, 0],
                    &[]
                )
                .is_err()
        );
        assert_eq!(model.effects, usize::from(scenario != 0));
        assert!(owner.poisoned);
        assert_eq!(owner.generation, 0);
    }
}

#[test]
fn guard_private_pins_unknown_advice_and_generation_stop_before_effect() {
    for advice in [102, 103] {
        for pinned in [false, true] {
            let maps = vec![map(0x1000, 0x3000, 3)];
            let mut model = Model::new(maps.clone(), maps, 0);
            let mut owner = owner(
                &model,
                if pinned {
                    vec![0x1000..0x3000; 1]
                } else {
                    vec![0x1000..0x2000; 1]
                },
            );
            let pins = if pinned {
                vec![0x2000..0x3000; 1]
            } else {
                vec![]
            };
            assert!(
                owner
                    .execute(
                        &mut model,
                        libc::SYS_madvise,
                        [0x2000, PAGE, advice, 0, 0, 0],
                        &pins
                    )
                    .is_err()
            );
            assert_eq!((model.effects, model.guard_queries), (0, 0));
        }
    }
    let maps = vec![map(0x1000, 0x5000, 3)];
    let mut model = Model::new(maps.clone(), maps, 0);
    let mut owner = owner(&model, vec![0x1000..0x5000; 1]);
    for advice in [0, 4, 101, 104, u64::MAX] {
        assert!(
            owner
                .execute(
                    &mut model,
                    libc::SYS_madvise,
                    [0x2000, PAGE, advice, 0, 0, 0],
                    &[]
                )
                .is_err()
        );
        assert_eq!((model.effects, model.guard_queries), (0, 0));
    }
    owner.generation = u64::MAX;
    assert!(
        owner
            .execute(
                &mut model,
                libc::SYS_madvise,
                [0x2000, PAGE, 102, 0, 0, 0],
                &[]
            )
            .is_err()
    );
    assert_eq!((model.effects, model.guard_queries), (0, 0));
    assert!(owner.poisoned);
}

#[test]
fn guard_existing_mapping_transactions_reconcile_observed_markers() {
    let cases = vec![
        (
            libc::SYS_mprotect,
            [0x1000, 4 * PAGE, 1, 0, 0, 0],
            0,
            vec![map(0x1000, 0x5000, 1)],
            vec![0x2000..0x3000; 1],
        ),
        (
            libc::SYS_mmap,
            [
                0x1000,
                4 * PAGE,
                3,
                (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED) as u64,
                u64::MAX,
                0,
            ],
            0x1000,
            vec![map(0x1000, 0x5000, 3)],
            vec![],
        ),
        (
            libc::SYS_munmap,
            [0x1000, 4 * PAGE, 0, 0, 0, 0],
            0,
            vec![],
            vec![],
        ),
        (
            libc::SYS_mremap,
            [
                0x1000,
                4 * PAGE,
                4 * PAGE,
                libc::MREMAP_MAYMOVE as u64,
                0,
                0,
            ],
            0x10000,
            vec![map(0x10000, 0x14000, 3)],
            vec![0x11000..0x12000; 1],
        ),
        (
            libc::SYS_mremap,
            [0x1000, 4 * PAGE, PAGE, 0, 0, 0],
            0x1000,
            vec![map(0x1000, 0x2000, 3)],
            vec![],
        ),
        (
            libc::SYS_mremap,
            [0x1000, 4 * PAGE, 6 * PAGE, 0, 0, 0],
            0x1000,
            vec![map(0x1000, 0x7000, 3)],
            vec![0x2000..0x3000; 1],
        ),
    ];
    for (number, args, result, after, guards) in cases {
        let mut model = Model::new(vec![map(0x1000, 0x5000, 3)], after, result);
        model.guard_before = vec![0x2000..0x3000; 1];
        model.guard_after = guards.clone();
        let mut owner = owner(&model, vec![0x1000..0x5000; 1]);
        owner.guards = guard::State::Observed(model.guard_before.clone());
        assert_eq!(
            owner.execute(&mut model, number, args, &[]).unwrap(),
            result
        );
        assert_eq!(owner.guards.ranges(), guards);
        assert_eq!(model.guard_queries, 2);
        assert_eq!(model.calls, vec![(number, args)]);
        assert_eq!(owner.generation, 1);
    }
}

#[test]
fn guard_brk_shrink_and_failed_mapping_keep_actual_inventory() {
    let mut model = Model::new(
        vec![map(0x90000, 0x93000, 3)],
        vec![map(0x90000, 0x92000, 3)],
        0x92000,
    );
    model.brk_before = 0x93000;
    model.brk_after = 0x92000;
    model.guard_before = vec![0x92000..0x93000; 1];
    let mut owner = owner(&model, vec![0x90000..0x93000; 1]);
    owner.original_brk = 0x90000;
    owner.guards = guard::State::Observed(model.guard_before.clone());
    assert_eq!(
        owner
            .execute(&mut model, libc::SYS_brk, [0x92000, 0, 0, 0, 0, 0], &[])
            .unwrap(),
        0x92000
    );
    assert!(owner.guards.ranges().is_empty());
    let maps = vec![map(0x1000, 0x5000, 3)];
    let mut model = Model::new(maps.clone(), maps, -i64::from(libc::ENOMEM));
    model.guard_before = vec![0x2000..0x3000; 1];
    model.guard_after = model.guard_before.clone();
    let mut owner = super::tests::owner(&model, vec![0x1000..0x5000; 1]);
    owner.guards = guard::State::Observed(model.guard_before.clone());
    assert_eq!(
        owner
            .execute(
                &mut model,
                libc::SYS_mremap,
                [0x1000, 4 * PAGE, 8 * PAGE, 0, 0, 0],
                &[]
            )
            .unwrap(),
        -i64::from(libc::ENOMEM)
    );
    assert_eq!(owner.guards.ranges(), &[0x2000..0x3000; 1]);
}

#[test]
fn stack_provider_errors_preserve_flags_and_inventory() {
    for kind in [libc::MAP_PRIVATE, libc::MAP_SHARED, libc::MAP_DROPPABLE] {
        for result in [-libc::EINVAL, -libc::ENOMEM, -libc::EACCES] {
            let args = [
                0,
                4096,
                3,
                (kind | libc::MAP_ANONYMOUS | libc::MAP_STACK) as u64,
                u64::MAX,
                0,
            ];
            let maps = vec![map(0x1000, 0x2000, 3)];
            let mut model = Model::new(maps.clone(), maps, result as i64);
            let mut owner = owner(&model, vec![0x1000..0x2000; 1]);
            assert_eq!(
                owner
                    .execute(&mut model, libc::SYS_mmap, args, &[])
                    .unwrap(),
                result as i64
            );
            assert_eq!(model.calls, vec![(libc::SYS_mmap, args)]);
            assert_eq!(model.effects, 1);
            assert_eq!(model.snapshots, 2);
            assert_eq!(owner.guest, vec![0x1000..0x2000]);
            assert_eq!(owner.generation, 1);
            assert!(!owner.poisoned);
        }
    }
}

#[test]
fn stack_flag_retains_type_unknown_flag_and_protection_refusals() {
    let ordinary = (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_STACK) as u64;
    for (flags, protection, reason) in [
        (
            (libc::MAP_ANONYMOUS | libc::MAP_STACK) as u64,
            3,
            "unsupported mmap flags",
        ),
        (
            ordinary | libc::MAP_SHARED as u64,
            3,
            "unsupported mmap flags",
        ),
        (
            ordinary | libc::MAP_GROWSDOWN as u64,
            3,
            "unsupported mmap flags",
        ),
        (ordinary | (1_u64 << 63), 3, "unsupported mmap flags"),
        (ordinary, 7, "unsupported mapping protection"),
    ] {
        let mut model = Model::new(vec![], vec![], 0);
        let mut owner = owner(&model, vec![]);
        let failure = owner
            .execute(
                &mut model,
                libc::SYS_mmap,
                [0, 4096, protection, flags, u64::MAX, 0],
                &[],
            )
            .unwrap_err();
        assert_eq!(failure.reason, reason);
        assert_eq!(failure.result, None);
        assert!(model.calls.is_empty());
        assert_eq!(model.effects, 0);
        assert_eq!(owner.generation, 0);
        assert!(!owner.poisoned);
    }
}

#[test]
fn droppable_exact_anonymous_request_preserves_provider_args_and_ownership() {
    let args = [0, 4096, 3, 0x28, u64::MAX, 0];
    let before = vec![map(0x1000, 0x2000, 3)];
    let mut after = before.clone();
    after.push(map(0x4000, 0x5000, 3));
    let mut model = Model::new(before, after, 0x4000);
    let mut owner = owner(&model, vec![0x1000..0x2000; 1]);
    let result = owner.execute(&mut model, libc::SYS_mmap, args, &[]);
    assert_eq!(
        model.calls,
        vec![(libc::SYS_mmap, args)],
        "must reach provider without argument rewriting: {result:?}"
    );
    assert_eq!(result.unwrap(), 0x4000);
    assert_eq!(model.effects, 1);
    assert_eq!(model.snapshots, 2);
    assert_eq!(owner.guest, vec![0x1000..0x2000, 0x4000..0x5000]);
    assert_eq!(owner.generation, 1);
    assert!(!owner.poisoned);
}

#[test]
fn droppable_kernel_errors_preserve_result_and_unchanged_inventory() {
    for result in [-libc::EINVAL, -libc::ENOMEM, -libc::EACCES] {
        let args = [0x1234, 4096, 3, 0x28, u64::MAX, 0];
        let maps = vec![map(0x1000, 0x2000, 3)];
        let mut model = Model::new(maps.clone(), maps, result as i64);
        let mut owner = owner(&model, vec![0x1000..0x2000; 1]);
        let outcome = owner.execute(&mut model, libc::SYS_mmap, args, &[]);
        assert_eq!(
            model.calls,
            vec![(libc::SYS_mmap, args)],
            "kernel error must come from provider: {outcome:?}"
        );
        assert_eq!(outcome.unwrap(), result as i64);
        assert_eq!(owner.guest, vec![0x1000..0x2000]);
        assert_eq!(owner.generation, 1);
        assert!(!owner.poisoned);
    }
}

#[test]
fn droppable_full_type_mask_and_anonymous_requirement() {
    for kind in 0..=libc::MAP_TYPE as u64 {
        for anonymous in [0, libc::MAP_ANONYMOUS as u64] {
            let flags = kind | anonymous;
            let admitted = kind == libc::MAP_PRIVATE as u64
                || kind == libc::MAP_SHARED as u64
                || kind == libc::MAP_DROPPABLE as u64 && anonymous != 0;
            if admitted {
                continue;
            }
            let mut model = Model::new(vec![], vec![], 0);
            let mut owner = owner(&model, vec![]);
            let failure = owner
                .execute(
                    &mut model,
                    libc::SYS_mmap,
                    [0, 4096, 3, flags, u64::MAX, 0],
                    &[],
                )
                .unwrap_err();
            assert_eq!(failure.reason, "unsupported mmap flags", "flags={flags:#x}");
            assert_eq!(failure.result, None);
            assert!(model.calls.is_empty());
            assert_eq!(model.effects, 0);
            assert_eq!(owner.generation, 0);
            assert!(!owner.poisoned);
        }
    }
}

#[test]
fn droppable_change_retains_private_shared_classification() {
    for kind in [libc::MAP_PRIVATE, libc::MAP_SHARED] {
        for anonymous in [0, libc::MAP_ANONYMOUS] {
            let args = [0, 4096, 3, (kind | anonymous) as u64, 17, 4096];
            let request = Request::decode(libc::SYS_mmap, args, 0x90000, 0x90000).unwrap();
            let Request::Map {
                shared, fd, offset, ..
            } = request
            else {
                panic!("not mmap")
            };
            assert_eq!(shared, kind == libc::MAP_SHARED);
            assert_eq!(fd, (anonymous == 0).then_some(17));
            assert_eq!(offset, 4096);
            let mut model = Model::new(vec![], vec![], -libc::EBADF as i64);
            let mut owner = owner(&model, vec![]);
            assert_eq!(
                owner
                    .execute(&mut model, libc::SYS_mmap, args, &[])
                    .unwrap(),
                -libc::EBADF as i64
            );
            assert_eq!(model.calls, vec![(libc::SYS_mmap, args)]);
        }
    }
}

#[test]
fn droppable_preserves_overlap_protection_and_poison_guards() {
    for (guest, pinned, protection, extra, poisoned) in [
        (vec![], vec![], 3, libc::MAP_FIXED, false),
        (
            vec![0x1000..0x2000; 1],
            vec![0x1000..0x2000; 1],
            3,
            libc::MAP_FIXED,
            false,
        ),
        (vec![0x1000..0x2000; 1], vec![], 7, 0, false),
        (
            vec![0x1000..0x2000; 1],
            vec![],
            3,
            libc::MAP_GROWSDOWN,
            false,
        ),
        (vec![0x1000..0x2000; 1], vec![], 3, 0, true),
    ] {
        let maps = vec![map(0x1000, 0x2000, 3)];
        let mut model = Model::new(maps.clone(), maps, 0x1000);
        let mut owner = owner(&model, guest);
        owner.poisoned = poisoned;
        assert!(
            owner
                .execute(
                    &mut model,
                    libc::SYS_mmap,
                    [0x1000, 4096, protection, 0x28 | extra as u64, u64::MAX, 0],
                    &pinned
                )
                .is_err()
        );
        assert!(model.calls.is_empty());
        assert_eq!(model.effects, 0);
        assert_eq!(owner.generation, 0);
        assert_eq!(owner.poisoned, poisoned);
    }
}

#[test]
fn droppable_post_effect_failure_preserves_actual_result_and_poison() {
    for (result, after, fail_after) in [
        (0x4000, vec![], true),
        (0x4000, vec![], false),
        (-libc::ENOMEM as i64, vec![map(0x4000, 0x5000, 3)], false),
    ] {
        let args = [0, 4096, 3, 0x28, u64::MAX, 0];
        let mut model = Model::new(vec![], after, result);
        model.fail_after = fail_after;
        let mut owner = owner(&model, vec![]);
        let failure = owner
            .execute(&mut model, libc::SYS_mmap, args, &[])
            .unwrap_err();
        assert_eq!(
            model.calls,
            vec![(libc::SYS_mmap, args)],
            "must observe actual provider result: {failure:?}"
        );
        assert_eq!(failure.result, Some(result));
        assert!(owner.poisoned);
        assert_eq!(owner.generation, 0);
        assert!(owner.guest.is_empty());
        assert!(
            owner
                .execute(&mut model, libc::SYS_mmap, args, &[])
                .is_err()
        );
        assert_eq!(model.effects, 1);
    }
}

#[test]
fn output_guard_preserves_kernel_faults_and_checks_actual_private_intersections() {
    let maps = vec![
        map(0x1000, 0x2000, 1),
        map(0x3000, 0x4000, 3),
        map(0x5000, 0x6000, 0),
        map(0x8000, 0xa000, 3),
    ];
    let mut model = Model::new(maps.clone(), maps, 0);
    let mut owner = owner(&model, vec![0x1000..0x2000, 0x3000..0x4000, 0x5000..0x6000]);
    for address in [
        0,
        0xffc,
        0x1000,
        0x1ffc,
        0x2000,
        0x3000,
        0x5000,
        0x6000,
        1 << 47,
        u64::MAX - 3,
        u64::MAX,
    ] {
        let mut effects = 0;
        assert_eq!(
            owner
                .output_with(&mut model, 1, address, 8, || {
                    effects += 1;
                    -i64::from(libc::EFAULT)
                })
                .unwrap(),
            -i64::from(libc::EFAULT)
        );
        assert_eq!(effects, 1);
        assert!(!owner.poisoned);
    }
    for address in [0x7ff9, 0x8000, 0x9ffc] {
        let error = owner
            .output_with(&mut model, 1, address, 8, || {
                panic!("private output reached kernel")
            })
            .unwrap_err();
        assert_eq!(error.reason, "private output overlap");
        assert_eq!(error.result, None);
        assert!(!owner.poisoned);
    }
    assert_eq!(owner.generation, 0);
}

#[test]
fn output_guard_observes_new_private_maps_and_retains_owner_failures() {
    let maps = vec![map(0x1000, 0x2000, 3)];
    let mut model = Model::new(maps.clone(), maps, 0);
    let mut owner = owner(&model, vec![0x1000..0x2000; 1]);
    model.before.push(map(0x8000, 0x9000, 3));
    assert_eq!(
        owner
            .output_with(&mut model, 1, 0x8000, 8, || panic!(
                "new private output reached kernel"
            ))
            .unwrap_err()
            .reason,
        "private output overlap"
    );
    assert_eq!(
        owner
            .output_with(&mut model, 2, 0, 8, || panic!("wrong owner reached kernel"))
            .unwrap_err()
            .reason,
        "wrong kernel thread"
    );
    let snapshots = model.snapshots;
    owner.poisoned = true;
    assert_eq!(
        owner
            .output_with(&mut model, 1, 0, 8, || panic!(
                "poisoned owner reached kernel"
            ))
            .unwrap_err()
            .reason,
        "owner poisoned"
    );
    assert_eq!(model.snapshots, snapshots);
    owner.poisoned = false;
    model.fail_before = true;
    assert_eq!(
        owner
            .output_with(&mut model, 1, 0, 8, || panic!(
                "inventory failure reached kernel"
            ))
            .unwrap_err()
            .reason,
        "output inventory unavailable"
    );
    assert!(owner.poisoned);
}

#[test]
fn output_guard_does_not_hide_guest_inventory_or_brk_changes_as_faults() {
    for change_brk in [false, true] {
        let maps = vec![map(0x1000, 0x2000, 3)];
        let mut model = Model::new(maps.clone(), maps, 0);
        let mut owner = owner(&model, vec![0x1000..0x2000; 1]);
        if change_brk {
            model.brk_before += PAGE;
        } else {
            model.before[0].protection = 1;
        }
        assert_eq!(
            owner
                .output_with(&mut model, 1, u64::MAX, 8, || panic!(
                    "changed inventory reached kernel"
                ))
                .unwrap_err()
                .reason,
            "guest mappings or brk changed outside transaction"
        );
        assert!(owner.poisoned);
    }
}

#[test]
fn private_and_active_ranges_refuse_before_effect() {
    for (guest, pinned) in [
        (vec![0x1000..0x2000; 1], vec![]),
        (vec![0x1000..0x4000; 1], vec![0x2000..0x3000; 1]),
    ] {
        for number in [
            libc::SYS_munmap,
            libc::SYS_mprotect,
            libc::SYS_mmap,
            libc::SYS_mremap,
        ] {
            let maps = vec![map(0x1000, 0x4000, 3)];
            let mut model = Model::new(maps.clone(), maps, 0);
            let mut owner = owner(&model, guest.clone());
            let args = match number {
                libc::SYS_mmap => [
                    0x2000,
                    4096,
                    3,
                    (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED) as u64,
                    u64::MAX,
                    0,
                ],
                libc::SYS_mremap => [0x2000, 4096, 4096, 0, 0, 0],
                _ => [0x2000, 4096, 1, 0, 0, 0],
            };
            let error = owner
                .execute(&mut model, number, args, &pinned)
                .unwrap_err();
            assert_eq!(error.result, None);
            assert_eq!(model.effects, 0);
            assert_eq!(owner.generation, 0);
            assert!(!owner.poisoned);
        }
    }
}

#[test]
fn partial_mprotect_error_commits_actual_prefix() {
    let before = vec![map(0x1000, 0x2000, 3), map(0x3000, 0x4000, 3)];
    let after = vec![map(0x1000, 0x2000, 1), map(0x3000, 0x4000, 3)];
    let mut model = Model::new(before, after, -i64::from(libc::ENOMEM));
    let mut owner = owner(&model, vec![0x1000..0x2000, 0x3000..0x4000]);
    assert_eq!(
        owner
            .execute(
                &mut model,
                libc::SYS_mprotect,
                [0x1000, 0x3000, 1, 0, 0, 0],
                &[]
            )
            .unwrap(),
        -i64::from(libc::ENOMEM)
    );
    assert_eq!(owner.view().unwrap().writable, vec![0x3000..0x4000]);
    assert_eq!(
        owner.view().unwrap().readable,
        vec![0x1000..0x2000, 0x3000..0x4000]
    );
    assert_eq!(model.effects, 1);
    assert_eq!(owner.generation, 1);
}

#[test]
fn errors_reconcile_unmap_holes_instead_of_rolling_back() {
    let mut model = Model::new(
        vec![map(0x1000, 0x4000, 3)],
        vec![map(0x1000, 0x2000, 3), map(0x3000, 0x4000, 3)],
        -i64::from(libc::ENOMEM),
    );
    let mut owner = owner(&model, vec![0x1000..0x4000; 1]);
    assert_eq!(
        owner
            .execute(
                &mut model,
                libc::SYS_munmap,
                [0x2000, PAGE, 0, 0, 0, 0],
                &[]
            )
            .unwrap(),
        -i64::from(libc::ENOMEM)
    );
    assert_eq!(owner.guest, vec![0x1000..0x2000, 0x3000..0x4000]);
}

#[test]
fn unavailable_post_observation_poison_retains_actual_result() {
    let maps = vec![map(0x1000, 0x4000, 3)];
    let mut model = Model::new(maps.clone(), maps, -i64::from(libc::ENOMEM));
    model.fail_after = true;
    let mut owner = owner(&model, vec![0x1000..0x4000; 1]);
    let error = owner
        .execute(
            &mut model,
            libc::SYS_mprotect,
            [0x2000, PAGE, 1, 0, 0, 0],
            &[],
        )
        .unwrap_err();
    assert_eq!(error.result, Some(-i64::from(libc::ENOMEM)));
    assert!(owner.view().is_err());
    assert!(
        owner
            .execute(
                &mut model,
                libc::SYS_munmap,
                [0x2000, PAGE, 0, 0, 0, 0],
                &[]
            )
            .is_err()
    );
    assert_eq!(model.effects, 1);
}

#[test]
fn restart_and_out_of_scope_mutation_are_terminal_not_guest_errno() {
    for (result, after) in [
        (-512, vec![map(0x1000, 0x4000, 3)]),
        (0, vec![map(0x1000, 0x2000, 1), map(0x2000, 0x4000, 3)]),
    ] {
        let mut model = Model::new(vec![map(0x1000, 0x4000, 3)], after, result);
        let mut owner = owner(&model, vec![0x1000..0x4000; 1]);
        let error = owner
            .execute(
                &mut model,
                libc::SYS_munmap,
                [0x3000, PAGE, 0, 0, 0, 0],
                &[],
            )
            .unwrap_err();
        assert_eq!(error.result, Some(result));
        assert!(owner.poisoned);
        assert_eq!(model.effects, 1);
    }
}

#[test]
fn boundary_independent_file_identity_checks_splits_offsets_and_sharing() {
    let mut whole = map(0x1000, 0x4000, 1);
    whole.inode = 8;
    whole.offset = 0x5000;
    whole.device = (8, 1);
    let mut first = whole.clone();
    first.range.end = 0x2000;
    let mut second = whole.clone();
    second.range.start = 0x2000;
    second.offset += PAGE;
    let range = 0x1000..0x4000;
    assert!(transaction::unchanged(
        &[whole.clone()],
        &[first.clone(), second.clone()],
        std::slice::from_ref(&range)
    ));
    second.offset += PAGE;
    assert!(!transaction::unchanged(
        &[whole.clone()],
        &[first.clone(), second.clone()],
        std::slice::from_ref(&range)
    ));
    second.offset -= PAGE;
    second.shared = true;
    assert!(!transaction::unchanged(
        &[whole],
        &[first, second],
        &[range]
    ));
}

#[test]
fn actual_result_drives_move_and_old_range_retirement() {
    let mut model = Model::new(
        vec![map(0x1000, 0x3000, 3)],
        vec![map(0x8000, 0xb000, 3)],
        0x8000,
    );
    let mut owner = owner(&model, vec![0x1000..0x3000; 1]);
    assert_eq!(
        owner
            .execute(
                &mut model,
                libc::SYS_mremap,
                [
                    0x1000,
                    2 * PAGE,
                    3 * PAGE,
                    libc::MREMAP_MAYMOVE as u64,
                    0,
                    0
                ],
                &[]
            )
            .unwrap(),
        0x8000
    );
    assert_eq!(owner.guest, vec![0x8000..0xb000]);
    assert_eq!(owner.view().unwrap().writable, owner.guest);
}

#[test]
fn brk_query_growth_shrink_and_kernel_rejection_use_retained_original() {
    for (requested, returned) in [
        (0, 0x90000),
        (0x8f000, 0x90000),
        (0x92001, 0x90000),
        (0x92001, 0x92001),
    ] {
        let after = if returned > 0x90000 {
            vec![map(0x90000, 0x93000, 3)]
        } else {
            vec![]
        };
        let mut model = Model::new(vec![], after, returned);
        model.brk_after = returned as u64;
        let mut owner = owner(&model, vec![]);
        assert_eq!(
            owner
                .execute(&mut model, libc::SYS_brk, [requested, 0, 0, 0, 0, 0], &[])
                .unwrap(),
            returned
        );
        assert_eq!(owner.original_brk, 0x90000);
        assert_eq!(model.effects, 1);
    }
    let mut model = Model::new(
        vec![map(0x90000, 0x93000, 3)],
        vec![map(0x90000, 0x91000, 3)],
        0x90001,
    );
    model.brk_before = 0x92001;
    model.brk_after = 0x90001;
    let mut owner = owner(&model, vec![0x90000..0x93000; 1]);
    owner.original_brk = 0x90000;
    assert_eq!(
        owner
            .execute(&mut model, libc::SYS_brk, [0x90001, 0, 0, 0, 0, 0], &[])
            .unwrap(),
        0x90001
    );
    assert_eq!(owner.guest, vec![0x90000..0x91000]);
}

#[test]
fn unsupported_numeric_flags_and_lifecycles_have_no_effect() {
    for (number, args) in [
        (
            libc::SYS_mmap,
            [
                0,
                PAGE,
                7,
                (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64,
                u64::MAX,
                0,
            ],
        ),
        (
            libc::SYS_mmap,
            [
                0,
                PAGE,
                3,
                (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_GROWSDOWN) as u64,
                u64::MAX,
                0,
            ],
        ),
        (libc::SYS_mprotect, [0x1001, PAGE, 3, 0, 0, 0]),
        (libc::SYS_munmap, [LIMIT - PAGE, 2 * PAGE, 0, 0, 0, 0]),
        (
            libc::SYS_mremap,
            [0x1000, PAGE, PAGE, libc::MREMAP_DONTUNMAP as u64, 0, 0],
        ),
        (libc::SYS_mremap, [0x1000, 0, PAGE, 1, 0, 0]),
    ] {
        let mut model = Model::new(vec![map(0x1000, 0x4000, 3)], vec![], 0);
        let mut owner = owner(&model, vec![0x1000..0x4000; 1]);
        assert!(owner.execute(&mut model, number, args, &[]).is_err());
        assert_eq!(model.effects, 0);
    }
}

#[test]
fn ordinary_host_mapping_effects_and_exact_bytes() {
    if isolate_host_mapping("ordinary_host_mapping_effects_and_exact_bytes") {
        return;
    }
    struct Reservation(u64);
    impl Drop for Reservation {
        fn drop(&mut self) {
            assert_eq!(
                inventory::raw(libc::SYS_munmap, [self.0, 7 * PAGE, 0, 0, 0, 0]),
                0
            );
        }
    }
    let start = inventory::raw(
        libc::SYS_mmap,
        [
            0,
            7 * PAGE,
            3,
            (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64,
            u64::MAX,
            0,
        ],
    );
    assert!(start > 0);
    let reservation = Reservation(start as u64);
    let begin = reservation.0 + PAGE;
    let warm = [Snapshot::empty(), Snapshot::empty(), Snapshot::empty()];
    drop(warm);
    let mut snapshot = Snapshot::empty();
    snapshot.capture().unwrap();
    let original_brk = snapshot.brk;
    let mut owner = Owner {
        tid: inventory::raw(libc::SYS_gettid, [0; 6]),
        guest: vec![begin..begin + 5 * PAGE; 1],
        snapshot,
        original_brk,
        generation: 0,
        poisoned: false,
        guards: guard::State::FreshExecUnqueried,
    };
    unsafe {
        std::ptr::write_bytes(begin as *mut u8, 0x5a, (5 * PAGE) as usize);
    }
    assert_eq!(
        owner
            .execute(
                &mut Linux,
                libc::SYS_mprotect,
                [begin, PAGE, 1, 0, 0, 0],
                &[]
            )
            .unwrap(),
        0
    );
    assert!(!owned(
        &owner.view().unwrap().writable,
        &(begin..begin + PAGE)
    ));
    assert_eq!(
        unsafe { std::slice::from_raw_parts(begin as *const u8, PAGE as usize) },
        vec![0x5a; PAGE as usize]
    );
    assert_eq!(
        owner
            .execute(
                &mut Linux,
                libc::SYS_munmap,
                [begin + PAGE, PAGE, 0, 0, 0, 0],
                &[]
            )
            .unwrap(),
        0
    );
    let partial = owner
        .execute(
            &mut Linux,
            libc::SYS_mprotect,
            [begin, 3 * PAGE, 3, 0, 0, 0],
            &[],
        )
        .unwrap();
    assert_eq!(partial, -i64::from(libc::ENOMEM));
    assert!(owned(
        &owner.view().unwrap().writable,
        &(begin..begin + PAGE)
    ));
    assert!(!owned(&owner.guest, &(begin + PAGE..begin + 2 * PAGE)));
    let result = owner
        .execute(
            &mut Linux,
            libc::SYS_mmap,
            [
                begin + 2 * PAGE,
                PAGE,
                3,
                (libc::MAP_FIXED | libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64,
                u64::MAX,
                0,
            ],
            &[],
        )
        .unwrap();
    assert_eq!(result as u64, begin + 2 * PAGE);
    assert_eq!(
        unsafe { std::slice::from_raw_parts((begin + 2 * PAGE) as *const u8, PAGE as usize) },
        vec![0; PAGE as usize]
    );
    assert_eq!(unsafe { *((begin + 3 * PAGE) as *const u8) }, 0x5a);
    unsafe {
        std::ptr::write_bytes((begin + 3 * PAGE) as *mut u8, 0x66, PAGE as usize);
    }
    assert_eq!(
        owner
            .execute(
                &mut Linux,
                libc::SYS_mremap,
                [
                    begin + 3 * PAGE,
                    PAGE,
                    PAGE,
                    (libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED) as u64,
                    begin + 4 * PAGE,
                    0
                ],
                &[]
            )
            .unwrap(),
        (begin + 4 * PAGE) as i64
    );
    assert!(!owned(&owner.guest, &(begin + 3 * PAGE..begin + 4 * PAGE)));
    assert_eq!(
        unsafe { std::slice::from_raw_parts((begin + 4 * PAGE) as *const u8, PAGE as usize) },
        vec![0x66; PAGE as usize]
    );
    let moved = owner
        .execute(
            &mut Linux,
            libc::SYS_mremap,
            [
                begin + 4 * PAGE,
                PAGE,
                2 * PAGE,
                libc::MREMAP_MAYMOVE as u64,
                0,
                0,
            ],
            &[],
        )
        .unwrap();
    assert!(moved > 0);
    assert_ne!(moved as u64, begin + 4 * PAGE);
    assert!(owned(
        &owner.guest,
        &(moved as u64..moved as u64 + 2 * PAGE)
    ));
    assert_eq!(
        unsafe { std::slice::from_raw_parts(moved as *const u8, PAGE as usize) },
        vec![0x66; PAGE as usize]
    );
    assert_eq!(
        unsafe { std::slice::from_raw_parts((moved as u64 + PAGE) as *const u8, PAGE as usize) },
        vec![0; PAGE as usize]
    );
    assert_eq!(
        owner
            .execute(
                &mut Linux,
                libc::SYS_munmap,
                [moved as u64, 2 * PAGE, 0, 0, 0, 0],
                &[]
            )
            .unwrap(),
        0
    );
    assert_eq!(
        owner
            .execute(
                &mut Linux,
                libc::SYS_mmap,
                [
                    begin,
                    PAGE,
                    3,
                    (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED_NOREPLACE) as u64,
                    u64::MAX,
                    0
                ],
                &[]
            )
            .unwrap(),
        -i64::from(libc::EEXIST)
    );
    assert_eq!(
        owner
            .execute(
                &mut Linux,
                libc::SYS_mmap,
                [0, PAGE, 1, libc::MAP_PRIVATE as u64, u64::MAX, 0],
                &[]
            )
            .unwrap(),
        -i64::from(libc::EBADF)
    );
    let error = owner
        .execute(
            &mut Linux,
            libc::SYS_mprotect,
            [reservation.0, PAGE, 1, 0, 0, 0],
            &[],
        )
        .unwrap_err();
    assert_eq!(error.result, None);
    assert_eq!(owner.snapshot.brk, original_brk);
    assert!(!owner.poisoned);
}

#[test]
fn ordinary_host_descriptor_alias_cannot_map_private_backing() {
    if isolate_host_mapping("ordinary_host_descriptor_alias_cannot_map_private_backing") {
        return;
    }
    private_alias(tempfile::tempfile().unwrap());
}

#[test]
fn ordinary_host_memfd_alias_cannot_map_private_backing() {
    if isolate_host_mapping("ordinary_host_memfd_alias_cannot_map_private_backing") {
        return;
    }
    use std::os::fd::FromRawFd;
    let fd = inventory::raw(
        libc::SYS_memfd_create,
        [
            c"mapping-identity-control".as_ptr() as u64,
            libc::MFD_CLOEXEC as u64,
            0,
            0,
            0,
            0,
        ],
    );
    assert!(fd >= 0);
    private_alias(unsafe { std::fs::File::from_raw_fd(fd as i32) });
}

fn private_alias(file: std::fs::File) {
    use std::os::fd::AsRawFd;
    file.set_len(PAGE).unwrap();
    let alias = file.try_clone().unwrap();
    let mapped = inventory::raw(
        libc::SYS_mmap,
        [
            0,
            PAGE,
            1,
            libc::MAP_PRIVATE as u64,
            file.as_raw_fd() as u64,
            0,
        ],
    );
    assert!(mapped > 0);
    struct Mapping(u64);
    impl Drop for Mapping {
        fn drop(&mut self) {
            assert_eq!(
                inventory::raw(libc::SYS_munmap, [self.0, PAGE, 0, 0, 0, 0]),
                0
            );
        }
    }
    let _mapping = Mapping(mapped as u64);
    let identity = Linux
        .file(alias.as_raw_fd(), &mut Snapshot::empty().bytes)
        .unwrap();
    let path = std::ffi::CString::new(format!(
        "/proc/self/map_files/{:x}-{:x}",
        mapped,
        mapped as u64 + PAGE
    ))
    .unwrap();
    let mut backing: libc::stat = unsafe { std::mem::zeroed() };
    let status = inventory::raw(
        libc::SYS_newfstatat,
        [
            libc::AT_FDCWD as u64,
            path.as_ptr() as u64,
            (&raw mut backing) as u64,
            0,
            0,
            0,
        ],
    );
    println!(
        "map-file-stat={status}; device={}:{} inode={}",
        libc::major(backing.st_dev),
        libc::minor(backing.st_dev),
        backing.st_ino
    );
    let warm = [Snapshot::empty(), Snapshot::empty()];
    drop(warm);
    let mut snapshot = Snapshot::empty();
    snapshot.capture().unwrap();
    let original_brk = snapshot.brk;
    println!(
        "alias-fstat={identity:?}; mapped={:?}",
        snapshot
            .maps
            .iter()
            .find(|map| map.range.contains(&(mapped as u64)))
    );
    let observed = snapshot
        .maps
        .iter()
        .find(|map| map.range.contains(&(mapped as u64)))
        .unwrap();
    assert_eq!(
        identity.as_ref().unwrap().identity,
        (observed.device, observed.inode)
    );
    let mut owner = Owner {
        tid: inventory::raw(libc::SYS_gettid, [0; 6]),
        guest: vec![],
        snapshot,
        original_brk,
        generation: 0,
        poisoned: false,
        guards: guard::State::FreshExecUnqueried,
    };
    let error = owner
        .execute(
            &mut Linux,
            libc::SYS_mmap,
            [
                0,
                PAGE,
                1,
                libc::MAP_PRIVATE as u64,
                alias.as_raw_fd() as u64,
                0,
            ],
            &[],
        )
        .unwrap_err();
    assert_eq!(error.reason, "private mapping backing descriptor");
    assert_eq!(error.result, None);
    assert_eq!(owner.generation, 0);
    assert_eq!(unsafe { *(mapped as *const u8) }, 0);
    assert!(!owner.poisoned);
}

#[test]
fn malformed_or_changed_inventory_never_permits_an_effect() {
    for changed_brk in [false, true] {
        let maps = vec![map(0x1000, 0x4000, 3)];
        let mut model = Model::new(maps.clone(), maps, 0);
        let mut owner = owner(&model, vec![0x1000..0x4000; 1]);
        if changed_brk {
            model.brk_before += PAGE;
        } else {
            model.before[0].protection = 1;
        }
        let error = owner
            .execute(
                &mut model,
                libc::SYS_munmap,
                [0x2000, PAGE, 0, 0, 0, 0],
                &[],
            )
            .unwrap_err();
        assert_eq!(error.result, None);
        assert_eq!(model.effects, 0);
        assert!(owner.poisoned);
    }
}

#[test]
fn ordinary_host_file_mapping_and_unsafe_extent_refusal() {
    if isolate_host_mapping("ordinary_host_file_mapping_and_unsafe_extent_refusal") {
        return;
    }
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::FileExt;
    let file = tempfile::tempfile().unwrap();
    file.set_len(2 * PAGE).unwrap();
    let bytes = vec![0x71; PAGE as usize];
    file.write_all_at(&bytes, 0).unwrap();
    let warm = [Snapshot::empty(), Snapshot::empty()];
    drop(warm);
    let mut snapshot = Snapshot::empty();
    snapshot.capture().unwrap();
    let original_brk = snapshot.brk;
    let mut owner = Owner {
        tid: inventory::raw(libc::SYS_gettid, [0; 6]),
        guest: vec![],
        snapshot,
        original_brk,
        generation: 0,
        poisoned: false,
        guards: guard::State::FreshExecUnqueried,
    };
    let args = [
        0,
        PAGE,
        3,
        libc::MAP_PRIVATE as u64,
        file.as_raw_fd() as u64,
        0,
    ];
    let address = owner
        .execute(&mut Linux, libc::SYS_mmap, args, &[])
        .unwrap();
    assert!(address > 0);
    assert!(owned(
        &owner.view().unwrap().writable,
        &(address as u64..address as u64 + PAGE)
    ));
    assert_eq!(
        unsafe { std::slice::from_raw_parts(address as *const u8, PAGE as usize) },
        bytes
    );
    let before = owner.generation;
    let error = owner
        .execute(
            &mut Linux,
            libc::SYS_mremap,
            [address as u64, PAGE, 2 * PAGE, 1, 0, 0],
            &[],
        )
        .unwrap_err();
    assert_eq!(error.result, None);
    assert_eq!(
        error.reason,
        "file mapping growth requires retained backing-size ownership"
    );
    let error = owner
        .execute(
            &mut Linux,
            libc::SYS_mmap,
            [
                0,
                3 * PAGE,
                1,
                libc::MAP_PRIVATE as u64,
                file.as_raw_fd() as u64,
                0,
            ],
            &[],
        )
        .unwrap_err();
    assert_eq!(error.result, None);
    assert_eq!(
        error.reason,
        "file mapping extends beyond observed backing pages"
    );
    assert_eq!(owner.generation, before);
    assert_eq!(
        owner
            .execute(
                &mut Linux,
                libc::SYS_munmap,
                [address as u64, PAGE, 0, 0, 0, 0],
                &[]
            )
            .unwrap(),
        0
    );
    assert!(owner.guest.is_empty());
}

#[test]
fn executable_shared_write_alias_is_refused_before_effect() {
    let mut code = map(0x1000, 0x2000, 5);
    code.inode = 77;
    let mut alias = map(0x4000, 0x5000, 1);
    alias.inode = 77;
    alias.shared = true;
    for offset in [0, PAGE] {
        alias.offset = offset;
        let before = vec![code.clone(), alias.clone()];
        let mut after = before.clone();
        after[1].protection = 3;
        let mut model = Model::new(before, after, 0);
        let mut owner = owner(&model, vec![0x1000..0x2000, 0x4000..0x5000]);
        let result = owner.execute(
            &mut model,
            libc::SYS_mprotect,
            [0x4000, PAGE, 3, 0, 0, 0],
            &[],
        );
        if offset == 0 {
            let failure = result.unwrap_err();
            assert_eq!(
                failure.reason,
                "writable shared alias of executable guest storage"
            );
            assert_eq!(failure.result, None);
            assert_eq!(model.effects, 0);
        } else {
            assert_eq!(result.unwrap(), 0);
            assert_eq!(model.effects, 1);
        }
    }
}

#[test]
fn file_execute_alias_distinguishes_shared_writes_from_private_cow() {
    let mut writable = map(0x1000, 0x2000, 3);
    writable.inode = 77;
    let request = Request::decode(
        libc::SYS_mmap,
        [0, PAGE, 5, libc::MAP_PRIVATE as u64, 3, 0],
        0x90000,
        0x90000,
    )
    .unwrap();
    assert!(
        request
            .validate_aliases(&[writable.clone()], Some(((0, 0), 77)))
            .is_ok()
    );
    writable.shared = true;
    assert!(
        request
            .validate_aliases(&[writable.clone()], Some(((0, 0), 77)))
            .is_err()
    );
    let replacement = Request::decode(
        libc::SYS_mmap,
        [
            0x1000,
            PAGE,
            5,
            (libc::MAP_PRIVATE | libc::MAP_FIXED) as u64,
            3,
            0,
        ],
        0x90000,
        0x90000,
    )
    .unwrap();
    assert!(
        replacement
            .validate_aliases(&[writable], Some(((0, 0), 77)))
            .is_ok()
    );
}

#[test]
fn partial_protection_alias_invariant_model() {
    for (previous, requested) in [(3, 5), (5, 3)] {
        for first_shared in [false, true] {
            for last_shared in [false, true] {
                for last_offset in [0, PAGE] {
                    let mut first = map(0x1000, 0x2000, previous);
                    first.inode = 77;
                    first.shared = first_shared;
                    let mut last = map(0x3000, 0x4000, previous);
                    last.inode = 77;
                    last.shared = last_shared;
                    last.offset = last_offset;
                    let before = vec![first, last];
                    let mut after = before.clone();
                    after[0].protection = requested;
                    let conflict = last_offset == 0
                        && (requested == 5 && last_shared || requested == 3 && first_shared);
                    let pre_effect_conflict = last_offset == 0 && (first_shared || last_shared);
                    let mut model = Model::new(before, after, -i64::from(libc::ENOMEM));
                    let guest = vec![0x1000..0x2000, 0x3000..0x4000];
                    let mut owner = owner(&model, guest.clone());
                    let args = [0x1000, 3 * PAGE, requested as u64, 0, 0, 0];
                    let request =
                        Request::decode(libc::SYS_mprotect, args, 0x90000, 0x90000).unwrap();
                    let committed = request.commit(
                        model.result,
                        &guest,
                        &snapshot(&model.before, model.brk_before),
                        &snapshot(&model.after, model.brk_after),
                    );
                    let result = owner.execute(&mut model, libc::SYS_mprotect, args, &[]);
                    if conflict {
                        assert_eq!(
                            committed.unwrap_err(),
                            "writable shared alias of executable guest storage"
                        );
                    } else {
                        assert_eq!(committed.unwrap(), guest);
                    }
                    if pre_effect_conflict {
                        let failure = result.unwrap_err();
                        assert_eq!(failure.result, None);
                        assert_eq!(
                            failure.reason,
                            "writable shared alias of executable guest storage"
                        );
                        assert_eq!(model.effects, 0);
                        assert_eq!(owner.generation, 0);
                    } else {
                        assert_eq!(result.unwrap(), -i64::from(libc::ENOMEM));
                        assert_eq!(model.effects, 1);
                        assert_eq!(owner.generation, 1);
                        assert_eq!(owner.snapshot.maps[0].protection, requested);
                        assert_eq!(owner.snapshot.maps[1].protection, previous);
                    }
                    assert!(!owner.poisoned);
                }
            }
        }
    }
}

#[test]
fn single_shared_mapping_protection_transition_is_not_a_self_alias() {
    for (previous, requested) in [(3, 5), (5, 3)] {
        let mut mapped = map(0x1000, 0x4000, previous);
        mapped.inode = 77;
        mapped.shared = true;
        let before = vec![mapped];
        let mut after = before.clone();
        after[0].protection = requested;
        let mut model = Model::new(before, after, 0);
        let mut owner = owner(&model, vec![0x1000..0x4000; 1]);
        assert_eq!(
            owner
                .execute(
                    &mut model,
                    libc::SYS_mprotect,
                    [0x1000, 3 * PAGE, requested as u64, 0, 0, 0],
                    &[],
                )
                .unwrap(),
            0
        );
        assert_eq!(model.effects, 1);
        assert_eq!(owner.generation, 1);
        assert!(!owner.poisoned);
    }
}

#[test]
fn resulting_alias_inventory_is_checked_before_unpoisoning() {
    for result in [0, -i64::from(libc::ENOMEM)] {
        for guest in [vec![0x1000..0x2000; 1], vec![0x3000..0x4000; 1]] {
            let mut executable = map(0x1000, 0x2000, 5);
            executable.inode = 77;
            let mut writable = map(0x3000, 0x4000, 3);
            writable.inode = 77;
            writable.shared = true;
            let maps = vec![executable, writable];
            let mut model = Model::new(maps.clone(), maps, result);
            let mut owner = owner(&model, guest.clone());
            let args = [0x5000, PAGE, 0, 0, 0, 0];
            let failure = owner
                .execute(&mut model, libc::SYS_munmap, args, &[])
                .unwrap_err();
            assert_eq!(failure.result, Some(result));
            assert_eq!(
                failure.reason,
                "writable shared alias of executable guest storage"
            );
            assert_eq!(model.effects, 1);
            assert_eq!(owner.generation, 0);
            assert_eq!(owner.guest, guest);
            assert!(owner.poisoned);
            assert!(owner.view().is_err());
            let snapshots = model.snapshots;
            assert_eq!(
                owner
                    .execute(&mut model, libc::SYS_munmap, args, &[])
                    .unwrap_err()
                    .reason,
                "owner poisoned"
            );
            assert_eq!(model.effects, 1);
            assert_eq!(model.snapshots, snapshots);
        }
    }
}

#[test]
fn ordinary_host_data_aliases_preserve_benign_partial_errors_and_cow() {
    if isolate_host_mapping("ordinary_host_data_aliases_preserve_benign_partial_errors_and_cow") {
        return;
    }
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::FileExt;
    struct Reservation(u64);
    impl Drop for Reservation {
        fn drop(&mut self) {
            assert_eq!(
                inventory::raw(libc::SYS_munmap, [self.0, 5 * PAGE, 0, 0, 0, 0]),
                0
            );
        }
    }
    for shared in [false, true] {
        let file = tempfile::tempfile().unwrap();
        file.set_len(PAGE).unwrap();
        file.write_all_at(&[0x31], 0).unwrap();
        let start = inventory::raw(
            libc::SYS_mmap,
            [
                0,
                5 * PAGE,
                3,
                (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64,
                u64::MAX,
                0,
            ],
        );
        assert!(start > 0);
        let reservation = Reservation(start as u64);
        let first = reservation.0 + PAGE;
        let last = first + 2 * PAGE;
        for address in [first, last] {
            assert_eq!(
                inventory::raw(
                    libc::SYS_mmap,
                    [
                        address,
                        PAGE,
                        3,
                        (libc::MAP_FIXED
                            | if shared {
                                libc::MAP_SHARED
                            } else {
                                libc::MAP_PRIVATE
                            }) as u64,
                        file.as_raw_fd() as u64,
                        0,
                    ]
                ),
                address as i64
            );
        }
        assert_eq!(
            inventory::raw(libc::SYS_munmap, [first + PAGE, PAGE, 0, 0, 0, 0]),
            0
        );
        unsafe {
            *(first as *mut u8) = 0x52;
        }
        assert_eq!(
            unsafe { *(last as *const u8) },
            if shared { 0x52 } else { 0x31 }
        );
        let warm = [Snapshot::empty(), Snapshot::empty(), Snapshot::empty()];
        drop(warm);
        let mut snapshot = Snapshot::empty();
        snapshot.capture().unwrap();
        let mapped = snapshot
            .maps
            .iter()
            .find(|map| map.range.contains(&first))
            .unwrap();
        let alias = snapshot
            .maps
            .iter()
            .find(|map| map.range.contains(&last))
            .unwrap();
        assert_ne!(mapped.inode, 0);
        assert_eq!(
            (mapped.device, mapped.inode, mapped.offset),
            (alias.device, alias.inode, alias.offset)
        );
        assert_eq!(mapped.shared, shared);
        assert_eq!(alias.shared, shared);
        let proposal = Request::decode(
            libc::SYS_mprotect,
            [first, 3 * PAGE, 5, 0, 0, 0],
            snapshot.brk,
            snapshot.brk,
        )
        .unwrap();
        assert_eq!(
            proposal.validate_aliases(&snapshot.maps, None).is_err(),
            shared
        );
        let original_brk = snapshot.brk;
        let mut owner = Owner {
            tid: inventory::raw(libc::SYS_gettid, [0; 6]),
            guest: vec![first..first + PAGE, last..last + PAGE],
            snapshot,
            original_brk,
            generation: 0,
            poisoned: false,
            guards: guard::State::FreshExecUnqueried,
        };
        assert_eq!(
            owner
                .execute(
                    &mut Linux,
                    libc::SYS_mprotect,
                    [first, 3 * PAGE, 1, 0, 0, 0],
                    &[]
                )
                .unwrap(),
            -i64::from(libc::ENOMEM)
        );
        assert_eq!(owner.generation, 1);
        assert!(!owner.poisoned);
        assert_eq!(owner.view().unwrap().writable, vec![last..last + PAGE]);
        assert!(owner.view().unwrap().executable.is_empty());
        assert_eq!(unsafe { *(first as *const u8) }, 0x52);
        unsafe {
            *(last as *mut u8) = 0x63;
        }
        assert_eq!(
            unsafe { *(first as *const u8) },
            if shared { 0x63 } else { 0x52 }
        );
        let mut backing = [0];
        file.read_exact_at(&mut backing, 0).unwrap();
        assert_eq!(backing, [if shared { 0x63 } else { 0x31 }]);
    }
}

struct NativeGuardProvider {
    calls: [(i64, [u64; 6], i64); 12],
    effects: usize,
    observations: usize,
}
impl NativeGuardProvider {
    fn new() -> Self {
        Self {
            calls: [(0, [0; 6], 0); 12],
            effects: 0,
            observations: 0,
        }
    }
}
impl Provider for NativeGuardProvider {
    fn snapshot(&mut self, output: &mut Snapshot) -> io::Result<()> {
        Linux.snapshot(output)
    }
    fn file(&mut self, fd: i32, bytes: &mut [u8]) -> io::Result<Option<fd_identity::Resolved>> {
        Linux.file(fd, bytes)
    }
    fn execute(&mut self, number: i64, args: [u64; 6]) -> i64 {
        assert!(self.effects < self.calls.len());
        let result = Linux.execute(number, args);
        self.calls[self.effects] = (number, args, result);
        self.effects += 1;
        result
    }
    fn guards(
        &mut self,
        tid: i64,
        maps: &[Map],
        guest: &[Range<u64>],
        workspace: &mut guard::Workspace,
    ) -> Result<Vec<Range<u64>>, guard::Error> {
        self.observations += 1;
        Linux.guards(tid, maps, guest, workspace)
    }
}
struct NativeGuardReservation(u64);
impl NativeGuardReservation {
    fn new() -> Self {
        let result = inventory::raw(
            libc::SYS_mmap,
            [
                0,
                12 * PAGE,
                3,
                (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64,
                u64::MAX,
                0,
            ],
        );
        assert!(result > 0);
        Self(result as u64)
    }
    fn guest(&self) -> Range<u64> {
        self.0 + PAGE..self.0 + 11 * PAGE
    }
    fn owner(&self) -> Owner {
        let warm_snapshots = [Snapshot::empty(), Snapshot::empty(), Snapshot::empty()];
        let warm_guards = [
            guard::Workspace::new(),
            guard::Workspace::qualified(),
            guard::Workspace::qualified(),
        ];
        drop((warm_snapshots, warm_guards));
        let mut snapshot = Snapshot::empty();
        snapshot.capture().unwrap();
        let original_brk = snapshot.brk;
        Owner {
            tid: inventory::raw(libc::SYS_gettid, [0; 6]),
            guest: vec![self.guest()],
            snapshot,
            original_brk,
            generation: 0,
            poisoned: false,
            guards: guard::State::FreshExecUnqueried,
        }
    }
}
impl Drop for NativeGuardReservation {
    fn drop(&mut self) {
        let result = inventory::raw(libc::SYS_munmap, [self.0, 12 * PAGE, 0, 0, 0, 0]);
        eprintln!("native-guard reservation cleanup result={result}");
        assert_eq!(result, 0);
    }
}
fn native_guard_fd_probe() -> i64 {
    let fd = inventory::raw(
        libc::SYS_openat,
        [
            libc::AT_FDCWD as u64,
            c"/proc/self/pagemap".as_ptr() as u64,
            (libc::O_RDONLY | libc::O_CLOEXEC) as u64,
            0,
            0,
            0,
        ],
    );
    assert!(fd >= 0);
    assert_eq!(
        inventory::raw(libc::SYS_close, [fd as u64, 0, 0, 0, 0, 0]),
        0
    );
    fd
}
fn native_guard_effect(
    owner: &mut Owner,
    provider: &mut NativeGuardProvider,
    number: i64,
    args: [u64; 6],
) -> i64 {
    let fd_before = native_guard_fd_probe();
    let effects = provider.effects;
    let result = owner.execute(provider, number, args, &[]);
    let fd_after = native_guard_fd_probe();
    eprintln!(
        "native-guard number={number} args={args:?} result={result:?} calls={} fd-before={fd_before} fd-after={fd_after}",
        provider.effects - effects
    );
    assert_eq!(fd_after, fd_before);
    let result = result.unwrap();
    assert_eq!(provider.effects, effects + 1);
    assert_eq!(provider.calls[effects], (number, args, result));
    assert_eq!(owner.generation, provider.effects as u64);
    assert!(!owner.poisoned);
    result
}
fn native_guard_readback(owner: &Owner) -> Vec<Range<u64>> {
    let mut workspace = if owner.guards.active() {
        guard::Workspace::qualified()
    } else {
        guard::Workspace::new()
    };
    let mut current = Snapshot::empty();
    current.capture().unwrap();
    assert_eq!(current.brk, owner.snapshot.brk);
    let fd_before = native_guard_fd_probe();
    let actual = guard::observe(owner.tid, &current.maps, &owner.guest, &mut workspace).unwrap();
    assert_eq!(native_guard_fd_probe(), fd_before);
    assert_eq!(actual, owner.guards.ranges());
    let view = owner.view().unwrap();
    for guest in &owner.guest {
        for address in (guest.start..guest.end).step_by(PAGE as usize) {
            let page = address..address + PAGE;
            let mapped = current
                .maps
                .iter()
                .find(|map| map.range.start <= address && address < map.range.end);
            let guarded = actual.iter().any(|range| overlap(range, &page));
            for (ranges, mask) in [
                (&view.readable, 1),
                (&view.writable, 3),
                (&view.executable, 5),
            ] {
                assert_eq!(
                    owned(ranges, &page),
                    !guarded && mapped.is_some_and(|map| map.protection & mask == mask)
                );
            }
        }
    }
    eprintln!(
        "native-guard readback generation={} guards={actual:?} guest={:?} fd={fd_before}",
        owner.generation, owner.guest
    );
    actual
}

#[test]
fn ordinary_host_guard_install_remove_and_effective_views() {
    if isolate_host_mapping("ordinary_host_guard_install_remove_and_effective_views") {
        return;
    }
    let fd_before = native_guard_fd_probe();
    let reservation = NativeGuardReservation::new();
    let guest = reservation.guest();
    let page = guest.start + PAGE;
    let mut provider = NativeGuardProvider::new();
    let mut owner = reservation.owner();
    assert!(native_guard_readback(&owner).is_empty());
    for (advice, expected) in [
        (103, vec![]),
        (102, vec![page..page + PAGE; 1]),
        (103, vec![]),
    ] {
        let args = [page, PAGE, advice | (0x1234_u64 << 32), 0xa1, 0xb2, 0xc3];
        assert_eq!(
            native_guard_effect(&mut owner, &mut provider, libc::SYS_madvise, args),
            0
        );
        assert_eq!(native_guard_readback(&owner), expected);
        let mapping = owner
            .snapshot
            .maps
            .iter()
            .find(|map| map.range.start <= page && page < map.range.end)
            .unwrap();
        assert_eq!(mapping.protection, 3);
    }
    assert_eq!(
        native_guard_effect(
            &mut owner,
            &mut provider,
            libc::SYS_munmap,
            [guest.start, guest.end - guest.start, 0, 0, 0, 0]
        ),
        0
    );
    assert!(native_guard_readback(&owner).is_empty());
    assert!(owner.guest.is_empty());
    assert_eq!(provider.observations, 8);
    drop(owner);
    drop(reservation);
    assert_eq!(native_guard_fd_probe(), fd_before);
}

#[test]
fn ordinary_host_guard_mprotect_move_and_unmap_readback() {
    if isolate_host_mapping("ordinary_host_guard_mprotect_move_and_unmap_readback") {
        return;
    }
    let fd_before = native_guard_fd_probe();
    let reservation = NativeGuardReservation::new();
    let guest = reservation.guest();
    let source = guest.start + PAGE;
    let destination = guest.start + 4 * PAGE;
    let mut provider = NativeGuardProvider::new();
    let mut owner = reservation.owner();
    assert_eq!(
        native_guard_effect(
            &mut owner,
            &mut provider,
            libc::SYS_madvise,
            [source, PAGE, 102, 0, 0, 0]
        ),
        0
    );
    assert_eq!(native_guard_readback(&owner), [source..source + PAGE; 1]);
    assert_eq!(
        native_guard_effect(
            &mut owner,
            &mut provider,
            libc::SYS_mprotect,
            [source, PAGE, libc::PROT_READ as u64, 0, 0, 0]
        ),
        0
    );
    native_guard_readback(&owner);
    assert_eq!(
        native_guard_effect(
            &mut owner,
            &mut provider,
            libc::SYS_mremap,
            [
                source,
                PAGE,
                PAGE,
                (libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED) as u64,
                destination,
                0
            ]
        ),
        destination as i64
    );
    native_guard_readback(&owner);
    assert!(!owned(&owner.guest, &(source..source + PAGE)));
    assert!(owned(&owner.guest, &(destination..destination + PAGE)));
    assert_eq!(
        native_guard_effect(
            &mut owner,
            &mut provider,
            libc::SYS_munmap,
            [destination, PAGE, 0, 0, 0, 0]
        ),
        0
    );
    assert!(native_guard_readback(&owner).is_empty());
    assert_eq!(
        native_guard_effect(
            &mut owner,
            &mut provider,
            libc::SYS_munmap,
            [guest.start, guest.end - guest.start, 0, 0, 0, 0]
        ),
        0
    );
    assert!(native_guard_readback(&owner).is_empty());
    assert!(owner.guest.is_empty());
    assert_eq!(provider.observations, 10);
    drop(owner);
    drop(reservation);
    assert_eq!(native_guard_fd_probe(), fd_before);
}
