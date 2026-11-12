    #[test]
    fn inactive_publication_refuses_poisoned_file_table_without_effects() {
        let executor = executor();
        let id = identity(&executor);
        let control = executor.signal_registry.control();
        let files = executor.file_table.clone();
        assert!(std::thread::spawn(move || {
            let _guard = files.lock().unwrap();
            panic!("controlled authoritative-table poison");
        }).join().is_err());
        for _ in 0..2 {
            assert!(matches!(
                control.publish_alarm(id, alarm(id)),
                ProcessPublication::Rejected(PublicationRejection::Backend(errno))
                    if errno == Errno::EIO
            ));
        }
        let process = executor.state.process_signals.lock().unwrap();
        assert_eq!(
            process.shared_pending.pending_mask(&process.pending_generations).to_bytes(),
            KernelSigset::default().to_bytes()
        );
        assert!(executor.signal_registry.failure.lock().unwrap().is_none());
        assert!(executor.file_table.is_poisoned());
    }

