from pathlib import Path
p=Path(__file__).resolve().parent/'source/reverie-kvm/src/executor.rs';s=p.read_text()
s=s.replace('observe_unlocked_retirement_then(executor, || {})','observe_unlocked_retirement_then(executor, |_| {})')
s=s.replace("after: impl Fn() + Send + Sync + 'static,","after: impl Fn(&[i32]) + Send + Sync + 'static,")
s=s.replace('                after();','                after(descriptors);')
a=s.index('    fn descriptor_retirement_accept_cleanup_releases_both_guards()');b=s.index('    #[test]',a);body=s[a:b]
x=body.index('            let between_installs = if fail_second_install {');y=body.index('            let before: Vec<_>',x)
body=body[:x]+'''            let target = f.open("a", libc::O_RDWR);
            assert!(target > server);
            let old_sentinel_host = f.executor.state.files[&(target as i32)].as_raw_fd();
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
            let first_installed_id = f.executor.file_table.lock().unwrap().fd_entry_ids[&(target as i32)].clone();
            let between_installs = Mutex::new(sibling);
''' +body[y:]
x=body.index('            let replaced_between_installs =');y=body.index('            if fail_second_install {',x)
body=body[:x]+'''            let replaced_between_installs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let replaced = replaced_between_installs.clone();
            let memory = f.memory.clone();
            let observed = observe_unlocked_retirement_then(&f.executor, move |descriptors| {
                if descriptors.contains(&old_sentinel_host)
                    && replaced.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).is_ok()
                {
                    // The first required install clone has completed; its old
                    // local owner is at the real unlocked retirement drain.
                    // Replace that entry again through an actual sibling before
                    // accept resumes, making the second install require a clone.
                    let mut sibling = between_installs.lock().unwrap();
                    assert_eq!(
                        sibling.execute(
                            &SyscallRequest::new(libc::SYS_close as u64, [target as u64, 0, 0, 0, 0, 0]),
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
                        target
                    );
                }
            });
''' +body[y:]
body=body.replace('''            assert!(!observed.lock().unwrap().is_empty());
            if fail_second_install {
                assert!(replaced_between_installs.load(Ordering::SeqCst));''','''            assert!(!observed.lock().unwrap().is_empty());
            assert_eq!(replaced_between_installs.load(Ordering::SeqCst), 1);
            if fail_second_install {
                assert!(Arc::ptr_eq(
                    &f.executor.state.fd_entry_ids[&(target as i32)],
                    &first_installed_id
                ));
                assert!(!Arc::ptr_eq(
                    &f.executor.file_table.lock().unwrap().fd_entry_ids[&(target as i32)],
                    &first_installed_id
                ));''')
body=body.replace('''            } else {
                assert!(result > server);''','''            } else {
                assert!(Arc::ptr_eq(
                    &f.executor.state.fd_entry_ids[&(target as i32)],
                    &f.executor.file_table.lock().unwrap().fd_entry_ids[&(target as i32)]
                ));
                assert!(!Arc::ptr_eq(
                    &f.executor.state.fd_entry_ids[&(target as i32)],
                    &first_installed_id
                ));
                assert!(result > server);''')
s=s[:a]+body+s[b:];p.write_text(s)
