// Several threads each blocked on a DISTINCT futex address, so futex_waiters
// holds more than one key when the scheduler is stopped mid-flight.
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
static F: [AtomicU32; 6] = [
    AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0),
    AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0),
];
fn main() {
    let mut hs = Vec::new();
    for i in 0..6 {
        hs.push(std::thread::spawn(move || unsafe {
            // FUTEX_WAIT on a distinct address; value matches so it blocks.
            libc::syscall(libc::SYS_futex, &F[i] as *const AtomicU32,
                          libc::FUTEX_WAIT, 0u32, std::ptr::null::<libc::timespec>());
        }));
    }
    std::thread::sleep(Duration::from_millis(50));
    for i in 0..6 { F[i].store(1, Ordering::SeqCst); unsafe {
        libc::syscall(libc::SYS_futex, &F[i] as *const AtomicU32, libc::FUTEX_WAKE, 1i32); } }
    for h in hs { let _ = h.join(); }
    println!("done");
}
