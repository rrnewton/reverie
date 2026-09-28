CARGO_INCREMENTAL=0 timeout 180s cargo test --locked -p reverie-kvm --lib executor::tests::synthetic_proc_open_clears_stale_nofollow_metadata_on_fd_reuse -- --exact --nocapture
