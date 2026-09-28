    type RetirementObservations = Arc<Mutex<Vec<Vec<(i32, libc::mode_t)>>>>;

    // Pause the real owned retirement batch before its File/Arc<File> values
    // are destroyed. A separate host thread must acquire both actual mutexes;
    // try_lock makes the violating ordering fail without a linger timing test.
    fn observe_unlocked_retirement(
        executor: &ElfExecutor,
    ) -> RetirementObservations {
        let observations = Arc::new(Mutex::new(Vec::new()));
        let observed = observations.clone();
        let table = executor.file_table.clone();
        let transaction = executor.state.signal_transaction.clone();
        executor.state.file_retirement.set_probe(Some(Arc::new(move |descriptors| {
            assert!(!descriptors.is_empty());
            let mut batch = Vec::new();
            for &fd in descriptors {
                let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
                // SAFETY: each descriptor is still owned by the paused batch.
                assert_eq!(unsafe { libc::fstat(fd, stat.as_mut_ptr()) }, 0);
                // SAFETY: successful fstat initialized stat.
                batch.push((fd, unsafe { stat.assume_init() }.st_mode));
            }
            let files = table.clone();
            let signals = transaction.clone();
            let checked = std::thread::spawn(move || {
                let files = files.try_lock();
                let signals = signals.try_lock();
                (files.is_ok(), signals.is_ok())
            })
            .join()
            .expect("retirement lock checker panicked");
            assert_eq!(checked, (true, true), "both guards must be released before close");
            observed.lock().unwrap().push(batch);
        })));
        observations
    }

    fn observed_retired_fds(observations: &Mutex<Vec<Vec<(i32, libc::mode_t)>>>) -> Vec<i32> {
        observations.lock().unwrap().iter().flatten().map(|&(fd, _)| fd).collect()
    }

    #[test]
    fn descriptor_retirement_close_and_dup_release_both_guards() {
        for number in [libc::SYS_close, libc::SYS_close_range, libc::SYS_dup2, libc::SYS_dup3] {
            let mut f = FdinfoFixture::new(false);
            let source = f.open("a", libc::O_RDWR);
            let target = f.open("b", libc::O_RDWR);
            assert!(source >= 3 && target > source);
            let old_local = f.executor.state.files[&(target as i32)].as_raw_fd();
            let old_shared = f.executor.file_table.lock().unwrap().files[&(target as i32)].as_raw_fd();
            let old_entry = f.executor.state.fd_entry_ids[&(target as i32)].clone();
            let observed = observe_unlocked_retirement(&f.executor);
            let args = match number {
                libc::SYS_close => [target as u64, 0, 0, 0, 0, 0],
                libc::SYS_close_range => [target as u64, target as u64, 0, 0, 0, 0],
                libc::SYS_dup2 => [source as u64, target as u64, 0, 0, 0, 0],
                libc::SYS_dup3 => [source as u64, target as u64, libc::O_CLOEXEC as u64, 0, 0, 0],
                _ => unreachable!(),
            };
            let expected = if number == libc::SYS_dup2 || number == libc::SYS_dup3 { target } else { 0 };
            assert_eq!(f.call(number, args), expected);
            f.executor.state.file_retirement.set_probe(None);
            let retired = observed_retired_fds(&observed);
            assert!(retired.contains(&old_local));
            assert!(retired.contains(&old_shared));
            if expected == 0 {
                assert!(!f.executor.state.files.contains_key(&(target as i32)));
                assert!(!f.executor.file_table.lock().unwrap().files.contains_key(&(target as i32)));
                assert!(!f.executor.state.fd_entry_ids.contains_key(&(target as i32)));
            } else {
                assert!(!Arc::ptr_eq(&old_entry, &f.executor.state.fd_entry_ids[&(target as i32)]));
                assert_eq!(f.seek(source, 7, libc::SEEK_SET), 7);
                assert_eq!(f.seek(target, 0, libc::SEEK_CUR), 7);
                assert_eq!(f.executor.state.cloexec_fds.contains(&(target as i32)), number == libc::SYS_dup3);
            }
        }
    }

    #[test]
    fn descriptor_retirement_exec_and_exit_release_both_guards() {
        {
            let mut f = FdinfoFixture::new(false);
            let target = f.open("a", libc::O_RDWR | libc::O_CLOEXEC);
            assert!(target >= 3);
            assert_eq!(f.call(libc::SYS_fcntl, [0, libc::F_SETFD as u64, libc::FD_CLOEXEC as u64, 0, 0, 0]), 0);
            f.executor.state.executable_file = Some(Arc::new(std::fs::File::open(f.root.0.join("b")).unwrap()));
            let old_executable = f.executor.state.executable_file.as_ref().unwrap().as_raw_fd();
            let old_local = f.executor.state.files[&(target as i32)].as_raw_fd();
            let old_shared = f.executor.file_table.lock().unwrap().files[&(target as i32)].as_raw_fd();
            let old_stdin = f.executor.state.stdin.as_ref().unwrap().as_raw_fd();
            let replacement = test_state(&f.root.0);
            let replacement_cwd = replacement.cwd_fd.as_raw_fd();
            let replacement_stdin = replacement.stdin.as_ref().unwrap().as_raw_fd();
            let observed = observe_unlocked_retirement(&f.executor);
            f.executor.replace_after_exec(replacement);
            f.executor.state.file_retirement.set_probe(None);
            let retired = observed_retired_fds(&observed);
            for fd in [old_executable, old_local, old_shared, old_stdin, replacement_cwd, replacement_stdin] {
                assert!(retired.contains(&fd), "missing exec-owned retired descriptor {fd}");
            }
            assert!(f.executor.state.stdin.is_none());
            assert!(!f.executor.state.files.contains_key(&(target as i32)));
            assert!(!f.executor.state.fd_entry_ids.contains_key(&(target as i32)));
            assert!(!f.executor.file_table.lock().unwrap().files.contains_key(&(target as i32)));
        }
        {
            let mut f = FdinfoFixture::new(false);
            let target = f.open("a", libc::O_RDWR);
            let file = Arc::new(std::fs::File::open(f.root.0.join("b")).unwrap());
            let action_file = file.as_raw_fd();
            let old_local = f.executor.state.files[&(target as i32)].as_raw_fd();
            let old_stdin = f.executor.state.stdin.as_ref().unwrap().as_raw_fd();
            f.executor.process_action = Some(ProcessAction::Exec {
                executable_path: f.root.0.join("b"), executable_file: Some(file),
                image: Vec::new(), argv: Vec::new(), envp: Vec::new(),
            });
            let observed = observe_unlocked_retirement(&f.executor);
            f.executor.release_files_on_exit();
            f.executor.state.file_retirement.set_probe(None);
            let retired = observed_retired_fds(&observed);
            for fd in [action_file, old_local, old_stdin] {
                assert!(retired.contains(&fd), "missing exit-owned descriptor {fd}");
            }
            assert!(f.executor.process_action.is_none());
            assert!(f.executor.state.files.is_empty());
            assert!(f.executor.state.fd_entry_ids.is_empty());
            assert!(f.executor.state.stdin.is_none());
        }
    }

    #[test]
    fn descriptor_retirement_install_error_releases_both_guards() {
        let mut f = FdinfoFixture::new(false);
        let first = f.open("a", libc::O_RDWR);
        let second = f.open("b", libc::O_RDWR);
        let before: Vec<_> = f.executor.state.files.iter().map(|(&fd, file)| (fd, file.as_raw_fd())).collect();
        let mut sibling = f.executor.thread_child(2).unwrap();
        for target in [first, second] {
            assert_eq!(sibling.execute(&SyscallRequest::new(libc::SYS_close as u64, [target as u64, 0, 0, 0, 0, 0]), &f.memory), 0);
            f.memory.write(0x100, b"c\0").unwrap();
            assert_eq!(sibling.execute(&SyscallRequest::new(libc::SYS_openat as u64, [libc::AT_FDCWD as u64, 0x100, libc::O_RDWR as u64, 0, 0, 0]), &f.memory), target);
        }
        let observed = observe_unlocked_retirement(&f.executor);
        // A deterministic injected clone error after stdin and one changed
        // entry exercises real install cleanup. The separate unchanged EMFILE
        // child still establishes actual host descriptor exhaustion behavior.
        f.executor.state.file_retirement.fail_clone_after(Some(2));
        assert_eq!(f.call(libc::SYS_getpid, [0; 6]), negative_errno(libc::EMFILE));
        f.executor.state.file_retirement.fail_clone_after(None);
        f.executor.state.file_retirement.set_probe(None);
        let after: Vec<_> = f.executor.state.files.iter().map(|(&fd, file)| (fd, file.as_raw_fd())).collect();
        assert_eq!(after, before, "failed install must retain all original handles");
        assert_eq!(observed_retired_fds(&observed).len(), 2, "both staged clones must be retired after unlock");
        assert_eq!(f.call(libc::SYS_getpid, [0; 6]), 1);
    }

    #[test]
    fn descriptor_retirement_accept_cleanup_releases_both_guards() {
        for fail_second_install in [false, true] {
            let mut f = FdinfoFixture::new(false);
            let socket_path = f.root.0.join("retire-accept.sock");
            let path = socket_path.as_os_str().as_bytes();
            let mut address = libc::sockaddr_un {
                sun_family: libc::AF_UNIX as libc::sa_family_t,
                sun_path: [0; 108],
            };
            assert!(path.len() < address.sun_path.len());
            for (destination, source) in address.sun_path.iter_mut().zip(path) {
                *destination = *source as libc::c_char;
            }
            let length = std::mem::offset_of!(libc::sockaddr_un, sun_path) + path.len() + 1;
            assert_eq!(write_struct(&mut f.memory, 0x100, &address), 0);
            let server = f.call(libc::SYS_socket, [libc::AF_UNIX as u64, libc::SOCK_STREAM as u64, 0, 0, 0, 0]);
            assert!(server >= 3);
            assert_eq!(f.call(libc::SYS_bind, [server as u64, 0x100, length as u64, 0, 0, 0]), 0);
            assert_eq!(f.call(libc::SYS_listen, [server as u64, 1, 0, 0, 0, 0]), 0);
            let mut client = std::os::unix::net::UnixStream::connect(&socket_path).unwrap();
            client.set_nonblocking(true).unwrap();
            let before: Vec<_> = f.executor.file_table.lock().unwrap().files.keys().copied().collect();
            let observed = observe_unlocked_retirement(&f.executor);
            if fail_second_install {
                // With unchanged entries, each install clones only stdin.
                // Fail the second install, after the real host accept succeeds.
                f.executor.state.file_retirement.fail_clone_after(Some(1));
            }
            let result = f.call(libc::SYS_accept4, [server as u64, 0, 0, libc::SOCK_CLOEXEC as u64, 0, 0]);
            f.executor.state.file_retirement.fail_clone_after(None);
            f.executor.state.file_retirement.set_probe(None);
            assert!(!observed.lock().unwrap().is_empty());
            if fail_second_install {
                assert_eq!(result, negative_errno(libc::EMFILE));
                assert_eq!(f.executor.file_table.lock().unwrap().files.keys().copied().collect::<Vec<_>>(), before);
                assert!(observed.lock().unwrap().iter().flatten().any(|&(_, mode)| mode & libc::S_IFMT == libc::S_IFSOCK), "the actually accepted socket must reach deferred cleanup");
                let mut byte = [0_u8; 1];
                assert_eq!(std::io::Read::read(&mut client, &mut byte).unwrap(), 0, "failed accept installation must close its host socket");
            } else {
                assert!(result > server);
                assert!(f.executor.file_table.lock().unwrap().files.contains_key(&(result as i32)));
                assert!(f.executor.state.cloexec_fds.contains(&(result as i32)));
                assert_eq!(f.call(libc::SYS_close, [result as u64, 0, 0, 0, 0, 0]), 0);
            }
        }
    }

