// Source-only launcher protocol shared by the two compiled artifacts.
// These strings locate a capability; they are never admission authority.
pub const CHANNEL_ENV: &str = "REVERIE_KVM_TEST_BROKER_CHANNEL";
pub const NONCE_ENV: &str = "REVERIE_KVM_TEST_BROKER_NONCE";
pub const CHILD_ENV: &str = "REVERIE_LEADER_EXEC_CHILD";

pub fn selected(test: &str) -> bool {
    matches!(
        test,
        "pdeathsig::unchanged_six_check_state_matches_native_controlled"
            | "pdeathsig::direct_host_and_unopted_control_preserve_nonzero_refusal"
            | "pdeathsig::creator_thread_death_and_clear_match_native_shared_pending"
            | "pdeathsig::ignored_parent_death_retains_blocked_and_does_not_resurrect_unblocked"
            | "pdeathsig::enrolled_retained_image_exec_preserves_setting_and_actual_creator_delivery"
            | "pdeathsig::actual_creator_death_wakes_controlled_pause_and_nanosleep"
            | "pdeathsig::linger_process::creator_process_queued_tcp_exit_publishes_before_peer_drain"
            | "executor::tests::terminal_cleanup_preserves_live_shared_table_and_does_not_wait_for_its_lock"
            | "terminal_cleanup::tests::dropped_waiter_keeps_native_wait_and_owned_service_alive"
    )
}
