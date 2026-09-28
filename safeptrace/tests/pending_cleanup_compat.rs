#![cfg(feature = "notifier")]

use std::time::Duration;

use safeptrace::Error;
use safeptrace::TerminalCleanup;
use safeptrace::Wait;

fn require_send_sync<T: Send + Sync>(_: &T) {}

fn legacy_decode_once_then_commit(cleanup: &TerminalCleanup) -> Result<Option<Wait>, Error> {
    let Some(reservation) = cleanup.reserve_pending_for_cleanup(Duration::ZERO) else {
        return Ok(None);
    };
    require_send_sync(&reservation);
    let wait = reservation.decode()?;
    reservation.commit();
    Ok(Some(wait))
}

#[test]
fn legacy_cleanup_reservation_source_pattern_still_compiles() {
    let _ = legacy_decode_once_then_commit;
}
