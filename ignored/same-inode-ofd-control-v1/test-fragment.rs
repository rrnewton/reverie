    #[test]
    fn shared_file_table_reopen_same_inode_replaces_description() {
        let mut f = FdinfoFixture::new(false);
        std::fs::write(f.root.0.join("a"), b"0123456789").unwrap();
        let target = f.open("a", libc::O_RDWR);
        assert!(target >= 3, "initial open: {target}");
        assert_eq!(f.seek(target, 1, libc::SEEK_SET), 1);
        let original_flags = f.call(
            libc::SYS_fcntl,
            [target as u64, libc::F_GETFL as u64, 0, 0, 0, 0],
        );
        assert!(original_flags >= 0);
        assert_eq!(
            original_flags & i64::from(libc::O_ACCMODE),
            i64::from(libc::O_RDWR)
        );
        assert_eq!(original_flags & i64::from(libc::O_APPEND), 0);
        assert_eq!(
            f.call(libc::SYS_fstat, [target as u64, PAGE_SIZE, 0, 0, 0, 0]),
            0
        );
        let original_stat = read_guest_struct::<libc::stat>(&f.memory, PAGE_SIZE).unwrap();
        let alias = f.call(libc::SYS_dup, [target as u64, 0, 0, 0, 0, 0]);
        assert!(alias > target, "retained dup: {alias}");
        // Confirm that the alias initially shares the original description.
        assert_eq!(f.seek(alias, 2, libc::SEEK_SET), 2);
        assert_eq!(f.seek(target, 0, libc::SEEK_CUR), 2);
        assert_eq!(f.seek(alias, 1, libc::SEEK_SET), 1);

        let mut sibling = f.executor.thread_child(2).unwrap();
        // Use the real shared-table execute path. No direct state/table edits
        // or manual snapshot refresh can make the later assertions pass.
        f.memory.write(0x100, b"a\0").unwrap();
        {
            let mut call = |number: libc::c_long, args: [u64; 6]| {
                sibling.execute(&SyscallRequest::new(number as u64, args), &f.memory)
            };
            assert_eq!(call(libc::SYS_close, [target as u64, 0, 0, 0, 0, 0]), 0);
            assert_eq!(
                call(
                    libc::SYS_openat,
                    [libc::AT_FDCWD as u64, 0x100, libc::O_RDWR as u64, 0, 0, 0]
                ),
                target
            );
            assert_eq!(
                call(
                    libc::SYS_lseek,
                    [target as u64, 5, libc::SEEK_SET as u64, 0, 0, 0]
                ),
                5
            );
            assert_eq!(
                call(
                    libc::SYS_fcntl,
                    [
                        target as u64,
                        libc::F_SETFL as u64,
                        libc::O_APPEND as u64,
                        0,
                        0,
                        0
                    ]
                ),
                0
            );
            assert_eq!(
                call(
                    libc::SYS_fcntl,
                    [target as u64, libc::F_GETFL as u64, 0, 0, 0, 0]
                ),
                original_flags | i64::from(libc::O_APPEND)
            );
        }

        // Collect the read/offset observations before F_GETFL: current execute
        // classifies every fcntl as table-mutating and writes its snapshot back.
        let target_before = f.seek(target, 0, libc::SEEK_CUR);
        f.memory.write(PAGE_SIZE, b"?").unwrap();
        let read_result = f.call(libc::SYS_read, [target as u64, PAGE_SIZE, 1, 0, 0, 0]);
        let mut byte = [0_u8; 1];
        f.memory.read(PAGE_SIZE, &mut byte).unwrap();
        let target_after = f.seek(target, 0, libc::SEEK_CUR);
        let alias_position = f.seek(alias, 0, libc::SEEK_CUR);
        assert_eq!(
            f.call(libc::SYS_fstat, [target as u64, PAGE_SIZE, 0, 0, 0, 0]),
            0
        );
        let target_stat = read_guest_struct::<libc::stat>(&f.memory, PAGE_SIZE).unwrap();
        assert_eq!(
            f.call(libc::SYS_fstat, [alias as u64, PAGE_SIZE, 0, 0, 0, 0]),
            0
        );
        let alias_stat = read_guest_struct::<libc::stat>(&f.memory, PAGE_SIZE).unwrap();
        let target_flags = f.call(
            libc::SYS_fcntl,
            [target as u64, libc::F_GETFL as u64, 0, 0, 0, 0],
        );
        let alias_flags = f.call(
            libc::SYS_fcntl,
            [alias as u64, libc::F_GETFL as u64, 0, 0, 0, 0],
        );
        assert_eq!(
            (target_stat.st_dev, target_stat.st_ino),
            (original_stat.st_dev, original_stat.st_ino)
        );
        assert_eq!(
            (alias_stat.st_dev, alias_stat.st_ino),
            (original_stat.st_dev, original_stat.st_ino)
        );
        assert_eq!(
            (
                target_before,
                target_flags,
                read_result,
                byte,
                target_after,
                alias_position,
                alias_flags,
            ),
            (
                5,
                original_flags | i64::from(libc::O_APPEND),
                1,
                [b'5'],
                6,
                1,
                original_flags,
            ),
            "same inode must not retain the replaced OFD; the dup keeps the original OFD"
        );
    }
