CARGO_INCREMENTAL=0 timeout 180s cargo test --locked -p reverie-kvm --lib executor::tests::synthetic_proc_fdinfo_missing_closed_and_malformed_targets_remain_enoent -- --exact --nocapture
