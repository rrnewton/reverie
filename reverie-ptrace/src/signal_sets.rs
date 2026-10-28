/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! [`reverie::Guest::signal_sets`] of a traced thread: the five signal lines
//! of the kernel's `/proc/<tid>/status` for the thread `tid` names, which
//! the tracer reads while the asking thread is stopped in the tool's
//! callback.
//!
//! A traced guest thread is a host task, and the tracer shares the guest's
//! `/proc` view, so the lines are the kernel's own for that thread. The
//! file's `Tgid:` line ties the thread to the asking thread's process, and
//! its `State:` line tells a thread that has exited but whose file is still
//! there (a zombie, or a task being released) from a live one.

use std::io::Read;

use reverie::Errno;
use reverie::FactError;
use reverie::Pid;
use reverie::SignalSets;

/// The most `status` text read for one thread. Its `Groups:` line lists
/// every supplementary group, up to 65,536 of them, so the text has no small
/// fixed bound; a longer one is not provided.
const STATUS_LIMIT: u64 = 1024 * 1024;

/// What [`parse`] answers for text that is not Linux's `status` format.
const MALFORMED: FactError = FactError::Errno(Errno::EIO);

/// What a thread ID that names no live thread of the process answers.
const NO_THREAD: FactError = FactError::Errno(Errno::ESRCH);

/// Linux's `PID_MAX_LIMIT` on a 64-bit kernel (`include/linux/threads.h`):
/// every task number is below it.
const PID_MAX_LIMIT: u32 = 4 * 1024 * 1024;

/// Every `State:` value Linux prints (`task_state_array` in
/// `fs/proc/array.c`).
const TASK_STATES: [&[u8]; 9] = [
    b"R (running)",
    b"S (sleeping)",
    b"D (disk sleep)",
    b"T (stopped)",
    b"t (tracing stop)",
    b"X (dead)",
    b"Z (zombie)",
    b"P (parked)",
    b"I (idle)",
];

/// A line [`parse`] reads, by its key.
#[derive(Clone, Copy)]
enum Field {
    State,
    Tgid,
    /// One of the five signal lines, by its place in [`SignalSets::new`]'s
    /// arguments, which is the order Linux prints them in (`task_sig`).
    Set(usize),
}

/// The keys of the lines [`parse`] reads.
const FIELDS: [(&[u8], Field); 7] = [
    (b"State", Field::State),
    (b"Tgid", Field::Tgid),
    (b"SigPnd", Field::Set(0)),
    (b"ShdPnd", Field::Set(1)),
    (b"SigBlk", Field::Set(2)),
    (b"SigIgn", Field::Set(3)),
    (b"SigCgt", Field::Set(4)),
];

/// Reads and parses the signal sets of thread `tid`, which must be a live
/// thread of process `process`.
pub(crate) fn read(process: Pid, tid: Pid) -> Result<SignalSets, FactError> {
    // Linux numbers no task zero or below; there is no file to read.
    if tid.as_raw() <= 0 {
        return Err(NO_THREAD);
    }
    let path = format!("/proc/{}/status", tid.as_raw());
    let text = {
        // The guard ends before anything else runs.
        let _open = crate::launch_window::TransientOpen::begin();
        read_bounded(&path, STATUS_LIMIT)
    };
    match text {
        Ok(Some(text)) => parse(&text, process),
        Ok(None) => Err(FactError::NotProvided),
        Err(error) => Err(FactError::Errno(read_errno(&error))),
    }
}

/// The whole of `path`, or `None` when it is longer than `limit` bytes.
fn read_bounded(path: &str, limit: u64) -> std::io::Result<Option<Vec<u8>>> {
    let mut text = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut text)?;
    Ok((text.len() as u64 <= limit).then_some(text))
}

/// What a failed read of a thread's `status` means. Linux answers `ENOENT`
/// at the open for a number no task has, and `ESRCH` at the read for a task
/// released since the open; both mean that no such thread lives. Any other
/// error is the kernel's answer to the tracer's read.
fn read_errno(error: &std::io::Error) -> Errno {
    match error.raw_os_error() {
        Some(libc::ENOENT | libc::ESRCH) => Errno::ESRCH,
        Some(errno) => Errno::new(errno),
        None => Errno::EIO,
    }
}

/// The signal sets in a thread's `status` text, which Linux printed for a
/// live thread of process `process`.
///
/// The text must be Linux's, or it is `EIO`: every line ends with a newline
/// and is a key of ASCII letters, digits and underscores, a colon, and its
/// value. The `State:` and `Tgid:` lines and the five signal lines each
/// appear once, with the tab Linux prints after the colon, and each line is
/// checked whole: the state is one Linux prints (`task_state_array`), the
/// thread-group ID a decimal below `PID_MAX_LIMIT` without leading zeros
/// (`seq_put_decimal_ull`), and each set sixteen lowercase hexadecimal
/// digits (`render_sigset_t`, `hex_asc`); there is no `SIGKILL` or `SIGSTOP`
/// in the mask, ignored or caught sets, and no signal both ignored and
/// caught. Lines with other keys are skipped. A text that is Linux's but
/// describes a thread of another process, or one that has exited (`Z`, a
/// zombie, or `X`, being released), is `ESRCH`.
fn parse(text: &[u8], process: Pid) -> Result<SignalSets, FactError> {
    let body = text.strip_suffix(b"\n").ok_or(MALFORMED)?;
    let mut state = None;
    let mut tgid = None;
    let mut sets = [None; 5];
    for line in body.split(|byte| *byte == b'\n') {
        let colon = line
            .iter()
            .position(|byte| *byte == b':')
            .ok_or(MALFORMED)?;
        let (key, rest) = (&line[..colon], &line[colon + 1..]);
        if key.is_empty()
            || !key
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            return Err(MALFORMED);
        }
        let Some(&(_, field)) = FIELDS.iter().find(|(name, _)| *name == key) else {
            continue;
        };
        let value = rest.strip_prefix(b"\t").ok_or(MALFORMED)?;
        let first = match field {
            Field::State => {
                if !TASK_STATES.contains(&value) {
                    return Err(MALFORMED);
                }
                state.replace(value[0]).is_none()
            }
            Field::Tgid => tgid.replace(parse_pid(value).ok_or(MALFORMED)?).is_none(),
            Field::Set(index) => sets[index]
                .replace(parse_set(value).ok_or(MALFORMED)?)
                .is_none(),
        };
        if !first {
            return Err(MALFORMED);
        }
    }
    let (
        Some(state),
        Some(tgid),
        [
            Some(pending),
            Some(shared_pending),
            Some(blocked),
            Some(ignored),
            Some(caught),
        ],
    ) = (state, tgid, sets)
    else {
        return Err(MALFORMED);
    };
    // Linux strips both from every mask it installs, refuses an action for
    // either, and keeps one action per signal.
    let unchangeable = bit(libc::SIGKILL) | bit(libc::SIGSTOP);
    if (blocked | ignored | caught) & unchangeable != 0 || ignored & caught != 0 {
        return Err(MALFORMED);
    }
    if u32::try_from(process.as_raw()) != Ok(tgid) || matches!(state, b'Z' | b'X') {
        return Err(NO_THREAD);
    }
    Ok(SignalSets::new(
        pending,
        shared_pending,
        blocked,
        ignored,
        caught,
    ))
}

/// The bit of `signal` in a set.
fn bit(signal: libc::c_int) -> u64 {
    1 << (signal - 1)
}

/// A task number as `seq_put_decimal_ull` prints it: decimal digits with no
/// sign, padding or leading zero (0 is `0`), below `PID_MAX_LIMIT`. Linux
/// prints 0 for a task its reader's PID namespace does not see.
fn parse_pid(field: &[u8]) -> Option<u32> {
    if field.is_empty()
        || !field.iter().all(u8::is_ascii_digit)
        || (field.len() > 1 && field[0] == b'0')
    {
        return None;
    }
    std::str::from_utf8(field)
        .ok()?
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid < PID_MAX_LIMIT)
}

/// A set as `render_sigset_t` prints it on x86-64 and aarch64, whose
/// `_NSIG` is 64: exactly sixteen lowercase hexadecimal digits (`hex_asc`),
/// the highest-numbered signals first.
fn parse_set(field: &[u8]) -> Option<u64> {
    if field.len() != 16
        || !field
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return None;
    }
    u64::from_str_radix(std::str::from_utf8(field).ok()?, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A thread's `status` as Linux 7.1.3 prints it, with the five signal
    /// lines, the `State:` letter and the `Tgid:` number left to fill in.
    fn status(state: &str, tgid: i32, sets: [&str; 5]) -> String {
        let [pending, shared, blocked, ignored, caught] = sets;
        format!(
            "Name:\tworker\\nname\nUmask:\t0022\nState:\t{state}\nTgid:\t{tgid}\nNgid:\t0\n\
             Pid:\t4243\nPPid:\t4000\nTracerPid:\t4000\nUid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\n\
             FDSize:\t64\nGroups:\t0 \nNStgid:\t{tgid}\nNSpid:\t4243\nNSpgid:\t{tgid}\n\
             NSsid:\t4000\nKthread:\t0\nVmPeak:\t    5768 kB\nThreads:\t2\nSigQ:\t2/3091463\n\
             SigPnd:\t{pending}\nShdPnd:\t{shared}\nSigBlk:\t{blocked}\nSigIgn:\t{ignored}\n\
             SigCgt:\t{caught}\nCapInh:\t0000000000000000\nCapPrm:\t000001ffffffffff\n\
             NoNewPrivs:\t0\nSeccomp:\t0\nCpus_allowed:\tffffffff,ffffffff\n\
             Cpus_allowed_list:\t0-63\nvoluntary_ctxt_switches:\t4\n\
             nonvoluntary_ctxt_switches:\t0\n"
        )
    }

    const ZERO: &str = "0000000000000000";

    /// The process the texts' `Tgid:` line names.
    fn process() -> Pid {
        Pid::from_raw(4242)
    }

    fn parse_status(text: &str) -> Result<SignalSets, FactError> {
        parse(text.as_bytes(), process())
    }

    fn sets(lines: [&str; 5]) -> Result<SignalSets, FactError> {
        parse_status(&status("S (sleeping)", 4242, lines))
    }

    /// Each line fills its own set, signal `n` at bit `n - 1`.
    #[test]
    fn each_line_fills_its_own_set() {
        assert_eq!(
            sets([
                "0000000000000200",
                "0000000000000800",
                "0000000000004002",
                "0000000000000001",
                "0000000000014000",
            ]),
            Ok(SignalSets::new(
                bit(libc::SIGUSR1),
                bit(libc::SIGUSR2),
                bit(libc::SIGINT) | bit(libc::SIGTERM),
                bit(libc::SIGHUP),
                bit(libc::SIGTERM) | bit(libc::SIGCHLD),
            ))
        );
        assert_eq!(sets([ZERO; 5]), Ok(SignalSets::new(0, 0, 0, 0, 0)));
    }

    /// Real-time signals take the high bits: Linux's first, 32, at bit 31,
    /// glibc's `SIGRTMIN`, 34, at bit 33, and the last, 64, at bit 63. One
    /// queued twice is still one bit, as Linux prints it.
    #[test]
    fn real_time_signals_take_the_high_bits() {
        let parsed = sets([
            "0000000200000000",
            "8000000000000000",
            "fffffffe7ffbfeff",
            "0000000080000000",
            "0000000180000000",
        ]);
        assert_eq!(parsed, Err(MALFORMED), "signal 32 both ignored and caught");
        let parsed = sets([
            "0000000200000000",
            "8000000000000000",
            "fffffffe7ffbfeff",
            "0000000080000000",
            "0000000100000000",
        ])
        .unwrap();
        assert_eq!(parsed.pending, bit(34));
        assert_eq!(parsed.shared_pending, bit(64));
        // Every signal but SIGKILL, SIGSTOP and glibc's two internal ones.
        assert_eq!(
            parsed.blocked,
            !(bit(libc::SIGKILL) | bit(libc::SIGSTOP) | bit(32) | bit(33))
        );
        assert_eq!(parsed.ignored, bit(32));
        assert_eq!(parsed.caught, bit(33));
    }

    /// SIGKILL and SIGSTOP can be pending (a dying thread, a stop that no
    /// thread has taken yet), but never blocked, ignored or caught.
    #[test]
    fn sigkill_and_sigstop_are_pending_only() {
        let kill_and_stop = format!("{:016x}", bit(libc::SIGKILL) | bit(libc::SIGSTOP));
        let parsed = sets([&kill_and_stop, &kill_and_stop, ZERO, ZERO, ZERO]).unwrap();
        assert_eq!(parsed.pending, 0x40100);
        assert_eq!(parsed.shared_pending, 0x40100);
        for signal in [libc::SIGKILL, libc::SIGSTOP] {
            let set = format!("{:016x}", bit(signal));
            for index in 2..5 {
                let mut lines = [ZERO; 5];
                lines[index] = &set;
                assert_eq!(
                    sets(lines),
                    Err(MALFORMED),
                    "signal {signal} in set {index}"
                );
            }
        }
    }

    /// A thread of another process, a zombie and a task being released are
    /// no live thread of the process; any other state is.
    #[test]
    fn another_process_and_exited_threads_are_esrch() {
        assert_eq!(
            parse_status(&status("S (sleeping)", 4241, [ZERO; 5])),
            Err(NO_THREAD)
        );
        // A task the reader's PID namespace does not see.
        assert_eq!(
            parse_status(&status("S (sleeping)", 0, [ZERO; 5])),
            Err(NO_THREAD)
        );
        assert_eq!(
            parse_status(&status("Z (zombie)", 4242, [ZERO; 5])),
            Err(NO_THREAD)
        );
        assert_eq!(
            parse_status(&status("X (dead)", 4242, [ZERO; 5])),
            Err(NO_THREAD)
        );
        for state in [
            "R (running)",
            "S (sleeping)",
            "D (disk sleep)",
            "T (stopped)",
            "t (tracing stop)",
            "P (parked)",
            "I (idle)",
        ] {
            assert_eq!(
                parse_status(&status(state, 4242, [ZERO; 5])),
                Ok(SignalSets::new(0, 0, 0, 0, 0)),
                "{state}"
            );
        }
        // A malformed text is EIO whoever it describes.
        assert_eq!(
            parse_status(&status("Z (zombie)", 4241, [ZERO, ZERO, ZERO, ZERO, "xyz"])),
            Err(MALFORMED)
        );
    }

    /// Every one of the seven lines read must be there exactly once.
    #[test]
    fn missing_and_repeated_lines_are_eio() {
        let full = status("S (sleeping)", 4242, [ZERO; 5]);
        for label in [
            "State:\t",
            "Tgid:\t",
            "SigPnd:\t",
            "ShdPnd:\t",
            "SigBlk:\t",
            "SigIgn:\t",
            "SigCgt:\t",
        ] {
            let line = full
                .lines()
                .find(|line| line.starts_with(label))
                .unwrap()
                .to_owned()
                + "\n";
            assert_eq!(
                parse_status(&full.replacen(&line, "", 1)),
                Err(MALFORMED),
                "without {label}"
            );
            assert_eq!(
                parse_status(&format!("{full}{line}")),
                Err(MALFORMED),
                "{label} twice"
            );
        }
    }

    /// A line with a key the parser reads must have the tab Linux prints
    /// after the colon, even beside a well-formed line of the same key: it is
    /// refused, not skipped.
    #[test]
    fn a_read_key_without_linuxs_separator_is_eio() {
        let full = status("S (sleeping)", 4242, [ZERO; 5]);
        for line in [
            "SigPnd: 0000000000000000",
            "SigPnd:0000000000000000",
            "SigPnd:\t\t0000000000000000",
            "SigBlk: 0000000000000000",
            "Tgid: 4242",
            "Tgid:4242",
            "State: S (sleeping)",
            "State:",
        ] {
            assert_eq!(
                parse_status(&format!("{full}{line}\n")),
                Err(MALFORMED),
                "{line:?} after a full text"
            );
        }
    }

    /// Every line is a key, a colon and a value; a line that is not is
    /// refused, wherever it is.
    #[test]
    fn a_line_that_is_not_a_key_and_value_is_eio() {
        let full = status("S (sleeping)", 4242, [ZERO; 5]);
        for line in [
            "",
            "SigPnd\t0000000000000000",
            "SigPnd \t0000000000000000",
            "SigPnd :\t0000000000000000",
            " SigPnd:\t0000000000000000",
            "Sig-Pnd:\t0000000000000000",
            ":\t0000000000000000",
            "no colon at all",
        ] {
            assert_eq!(
                parse_status(&full.replacen("Kthread:\t0\n", &format!("Kthread:\t0\n{line}\n"), 1)),
                Err(MALFORMED),
                "{line:?}"
            );
        }
    }

    /// A set is sixteen lowercase hexadecimal digits, a state one Linux
    /// prints, a thread-group ID a decimal below `PID_MAX_LIMIT` without a
    /// leading zero, and the text ends with a newline.
    #[test]
    fn malformed_values_are_eio() {
        for set in [
            "",
            "000000000000000",
            "00000000000000000",
            "0000000000000000000000000000000",
            "000000000000000A",
            "FFFFFFFFFFFFFFFF",
            "000000000000000g",
            "0x00000000000000",
            " 000000000000000",
            "000000000000000 ",
            "-000000000000001",
        ] {
            assert_eq!(
                sets([ZERO, ZERO, set, ZERO, ZERO]),
                Err(MALFORMED),
                "set {set:?}"
            );
        }
        let full = status("S (sleeping)", 4242, [ZERO; 5]);
        for (from, to) in [
            ("Tgid:\t4242\n", "Tgid:\t\n"),
            ("Tgid:\t4242\n", "Tgid:\t+4242\n"),
            ("Tgid:\t4242\n", "Tgid:\t-1\n"),
            ("Tgid:\t4242\n", "Tgid:\t04242\n"),
            ("Tgid:\t4242\n", "Tgid:\t42x2\n"),
            ("Tgid:\t4242\n", "Tgid:\t 4242\n"),
            ("Tgid:\t4242\n", "Tgid:\t4242 \n"),
            // PID_MAX_LIMIT and beyond.
            ("Tgid:\t4242\n", "Tgid:\t4194304\n"),
            ("Tgid:\t4242\n", "Tgid:\t2147483648\n"),
            ("Tgid:\t4242\n", "Tgid:\t18446744073709551616\n"),
            ("State:\tS (sleeping)\n", "State:\t\n"),
            ("State:\tS (sleeping)\n", "State:\tS\n"),
            ("State:\tS (sleeping)\n", "State:\tS (sleepy)\n"),
            ("State:\tS (sleeping)\n", "State:\ts (sleeping)\n"),
            ("State:\tS (sleeping)\n", "State:\tS (sleeping) \n"),
            ("State:\tS (sleeping)\n", "State:\tQ (queued)\n"),
        ] {
            assert_eq!(
                parse_status(&full.replacen(from, to, 1)),
                Err(MALFORMED),
                "{to:?}"
            );
        }
        // The largest task number and zero are numbers Linux prints.
        for tgid in ["4194303", "0"] {
            let text = full.replacen("Tgid:\t4242\n", &format!("Tgid:\t{tgid}\n"), 1);
            assert_eq!(parse_status(&text), Err(NO_THREAD), "Tgid {tgid}");
        }
        assert_eq!(
            parse_status(full.strip_suffix('\n').unwrap()),
            Err(MALFORMED),
            "no final newline"
        );
        assert_eq!(parse(b"", process()), Err(MALFORMED));
        assert_eq!(parse(b"\n", process()), Err(MALFORMED));
        // A byte that is not UTF-8 in a set.
        let mut text = full.into_bytes();
        let at = text
            .windows(8)
            .position(|window| window == b"SigCgt:\t")
            .unwrap()
            + 8
            + 15;
        text[at] = 0xff;
        assert_eq!(parse(&text, process()), Err(MALFORMED));
    }

    /// Lines with other keys are skipped, whatever their values: `NStgid:`
    /// and `Ngid:` are not `Tgid:`, `SigQ:` is not a set, keys differ by
    /// case, and a name with an escaped newline stays on its line.
    #[test]
    fn lines_with_other_keys_are_skipped() {
        let full = status("S (sleeping)", 4242, [ZERO; 5]);
        let text = full.replacen(
            "Kthread:\t0\n",
            "Kthread:\t0\nsigpnd:\tffffffffffffffff\nSigPndX:\tffff\nopaque:\t\u{fe}\n\
             Speculation_Store_Bypass:\tthread vulnerable\nx86_Thread_features:\t\n",
            1,
        );
        assert_eq!(parse_status(&text), Ok(SignalSets::new(0, 0, 0, 0, 0)));
    }

    /// A text of exactly the limit is read whole; one byte more is not read.
    #[test]
    fn the_read_bound_is_inclusive() {
        let path = std::env::temp_dir().join(format!(
            "reverie-ptrace-signal-sets-bound-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"0123456789").unwrap();
        let path_text = path.to_str().unwrap();
        assert_eq!(
            read_bounded(path_text, 10).unwrap(),
            Some(b"0123456789".to_vec())
        );
        assert_eq!(read_bounded(path_text, 9).unwrap(), None);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            read_bounded(path_text, 10).unwrap_err().raw_os_error(),
            Some(libc::ENOENT)
        );
    }

    fn this_thread() -> Pid {
        // SAFETY: gettid takes no arguments and cannot fail.
        Pid::from_raw(unsafe { libc::syscall(libc::SYS_gettid) } as i32)
    }

    fn this_process() -> Pid {
        Pid::from_raw(std::process::id() as i32)
    }

    /// The read is the kernel's file for this thread: a signal it blocks and
    /// sends to itself is pending for it alone, in its mask.
    #[test]
    fn a_thread_reads_its_own_sets() {
        // A thread of its own, so that no other test shares the mask.
        std::thread::spawn(|| {
            // SAFETY: the sets are plain data and the calls take valid pointers.
            unsafe {
                let mut mask: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut mask);
                libc::sigaddset(&mut mask, libc::SIGUSR2);
                let mut old: libc::sigset_t = std::mem::zeroed();
                assert_eq!(libc::pthread_sigmask(libc::SIG_BLOCK, &mask, &mut old), 0);
                assert_eq!(
                    libc::syscall(
                        libc::SYS_tgkill,
                        std::process::id(),
                        this_thread().as_raw(),
                        libc::SIGUSR2
                    ),
                    0
                );
                let sets = read(this_process(), this_thread()).unwrap();
                assert_eq!(sets.pending & bit(libc::SIGUSR2), bit(libc::SIGUSR2));
                assert_eq!(sets.blocked & bit(libc::SIGUSR2), bit(libc::SIGUSR2));
                let zero = libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                };
                assert_eq!(
                    libc::sigtimedwait(&mask, std::ptr::null_mut(), &zero),
                    libc::SIGUSR2
                );
                let sets = read(this_process(), this_thread()).unwrap();
                assert_eq!(sets.pending & bit(libc::SIGUSR2), 0);
                assert_eq!(
                    libc::pthread_sigmask(libc::SIG_SETMASK, &old, std::ptr::null_mut()),
                    0
                );
            }
        })
        .join()
        .unwrap();
    }

    /// A number no task has (`pid_max`: Linux hands out numbers below it),
    /// zero, a negative number and a task of another process (init) are no
    /// thread of this process.
    #[test]
    fn threads_outside_the_process_are_esrch() {
        let pid_max: i32 = std::fs::read_to_string("/proc/sys/kernel/pid_max")
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        for tid in [pid_max, 0, -1, i32::MIN] {
            assert_eq!(
                read(this_process(), Pid::from_raw(tid)),
                Err(NO_THREAD),
                "tid {tid}"
            );
        }
        assert_eq!(read(this_process(), Pid::from_raw(1)), Err(NO_THREAD));
        // The same thread asked about as part of another process.
        assert_eq!(read(Pid::from_raw(1), this_thread()), Err(NO_THREAD));
    }
}
