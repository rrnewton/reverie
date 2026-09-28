from pathlib import Path
D=Path(__file__).resolve().parent;p=D/'source/reverie-kvm/src/executor.rs';s=p.read_text()
a=s.index('    fn descriptor_retirement_install_error_releases_both_guards()');b=s.index('    #[test]',a);body=s[a:b]
body=body.replace('        let second = f.open("b", libc::O_RDWR);','        let second = f.open("b", libc::O_RDWR);\n        let third = f.open("c", libc::O_RDWR);')
body=body.replace('for target in [first, second] {','for target in [first, second, third] {')
body=body.replace('''        // A deterministic injected clone error after stdin and one changed
        // entry exercises real install cleanup. The separate unchanged EMFILE
        // child still establishes actual host descriptor exhaustion behavior.''','''        // Three genuinely replaced entries require three clones. Fail after
        // two successful staged clones, preserving the same partial-install
        // cleanup boundary now that unchanged stdin requires no clone.
        // The separate real-EMFILE child remains unchanged apart from its
        // comment describing which required clones exhaust the host limit.''')
s=s[:a]+body+s[b:]
old='''    fn observe_unlocked_retirement(executor: &ElfExecutor) -> RetirementObservations {
        let observations'''
new='''    fn observe_unlocked_retirement(executor: &ElfExecutor) -> RetirementObservations {
        observe_unlocked_retirement_then(executor, || {})
    }

    fn observe_unlocked_retirement_then(
        executor: &ElfExecutor,
        after: impl Fn() + Send + Sync + 'static,
    ) -> RetirementObservations {
        let observations'''
assert s.count(old)==1;s=s.replace(old,new)
old='''                observed.lock().unwrap().push(batch);
            })));''';new='''                observed.lock().unwrap().push(batch);
                after();
            })));''';assert s.count(old)==1;s=s.replace(old,new)
a=s.index('    fn descriptor_retirement_accept_cleanup_releases_both_guards()');b=s.index('    #[test]',a);body=s[a:b]
old='''            let before: Vec<_> = f
                .executor'''
new='''            let between_installs = if fail_second_install {
                let target = f.open("a", libc::O_RDWR);
                assert!(target > server);
                let mut sibling = f.executor.thread_child(2).unwrap();
                f.memory.write(0x100, b"a\\0").unwrap();
                assert_eq!(
                    sibling.execute(
                        &SyscallRequest::new(libc::SYS_close as u64, [target as u64, 0, 0, 0, 0, 0]),
                        &f.memory
                    ),
                    0
                );
                assert_eq!(
                    sibling.execute(
                        &SyscallRequest::new(
                            libc::SYS_openat as u64,
                            [libc::AT_FDCWD as u64, 0x100, libc::O_RDWR as u64, 0, 0, 0]
                        ),
                        &f.memory
                    ),
                    target
                );
                Some((Mutex::new(sibling), target))
            } else {
                None
            };
            let before: Vec<_> = f
                .executor'''
assert body.count(old)==1;body=body.replace(old,new)
old='''            let observed = observe_unlocked_retirement(&f.executor);
            if fail_second_install {
                // With unchanged entries, each install clones only stdin.
                // Fail the second install, after the real host accept succeeds.
                f.executor.state.file_retirement.fail_clone_after(Some(1));
            }'''
new='''            let replaced_between_installs = Arc::new(AtomicBool::new(false));
            let replaced = replaced_between_installs.clone();
            let memory = f.memory.clone();
            let observed = observe_unlocked_retirement_then(&f.executor, move || {
                if let Some((sibling, target)) = &between_installs
                    && !replaced.swap(true, Ordering::SeqCst)
                {
                    // The first required install clone has completed; its old
                    // local owner is at the real unlocked retirement drain.
                    // Replace that entry again through an actual sibling before
                    // accept resumes, making the second install require a clone.
                    let mut sibling = sibling.lock().unwrap();
                    assert_eq!(
                        sibling.execute(
                            &SyscallRequest::new(libc::SYS_close as u64, [*target as u64, 0, 0, 0, 0, 0]),
                            &memory
                        ),
                        0
                    );
                    assert_eq!(
                        sibling.execute(
                            &SyscallRequest::new(
                                libc::SYS_openat as u64,
                                [libc::AT_FDCWD as u64, 0x100, libc::O_RDWR as u64, 0, 0, 0]
                            ),
                            &memory
                        ),
                        *target
                    );
                }
            });
            if fail_second_install {
                // The first changed entry consumes the one permitted clone;
                // the second install fails after the real host accept succeeds.
                f.executor.state.file_retirement.fail_clone_after(Some(1));
            }'''
assert body.count(old)==1;body=body.replace(old,new)
body=body.replace('''            if fail_second_install {
                assert_eq!(result, negative_errno(libc::EMFILE));''','''            if fail_second_install {
                assert!(replaced_between_installs.load(Ordering::SeqCst));
                assert_eq!(result, negative_errno(libc::EMFILE));''')
s=s[:a]+body+s[b:]
s=s.replace('''        // soft limit. The real install, including its stdin clone, is used.''','''        // soft limit. The real install must stage both changed mapped entries;
        // unchanged stdin no longer consumes a host descriptor.''')
# Ensure the ENOENT negative uses the same valid event as the earlier MOD.
s=s.replace('''            assert_eq!(
                f.call(
                    libc::SYS_epoll_ctl,
                    [epoll as u64, libc::EPOLL_CTL_MOD as u64, 0, PAGE_SIZE, 0, 0]
                ),
                negative_errno(libc::ENOENT),''','''            assert_eq!(write_struct(&mut f.memory, PAGE_SIZE, &modified), 0);
            assert_eq!(
                f.call(
                    libc::SYS_epoll_ctl,
                    [epoll as u64, libc::EPOLL_CTL_MOD as u64, 0, PAGE_SIZE, 0, 0]
                ),
                negative_errno(libc::ENOENT),''')
p.write_text(s)
