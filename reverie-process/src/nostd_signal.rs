/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `Signal` for builds without `std`.
//!
//! With `std`, `Signal` is `nix::sys::signal::Signal`. nix requires `std`, so
//! without it this enum stands in. It has the same variants, discriminants,
//! names, and method surface as nix's `Signal` on x86_64 Linux. The
//! `matches_nix` test compiles this module into the host test build and checks
//! it variant by variant against nix, so a nix change fails that test instead
//! of drifting.

use core::fmt;
use core::str::FromStr;

use syscalls::Errno;

/// Types of operating system signals (x86_64 Linux numbering).
// The variant names must be nix's, so that `Signal::SIGSEGV` means the same
// thing with and without `std`.
#[allow(clippy::upper_case_acronyms)]
#[repr(i32)]
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Signal {
    /// Hangup
    SIGHUP = 1,
    /// Interrupt
    SIGINT = 2,
    /// Quit
    SIGQUIT = 3,
    /// Illegal instruction (not reset when caught)
    SIGILL = 4,
    /// Trace trap (not reset when caught)
    SIGTRAP = 5,
    /// Abort
    SIGABRT = 6,
    /// Bus error
    SIGBUS = 7,
    /// Floating point exception
    SIGFPE = 8,
    /// Kill (cannot be caught or ignored)
    SIGKILL = 9,
    /// User defined signal 1
    SIGUSR1 = 10,
    /// Segmentation violation
    SIGSEGV = 11,
    /// User defined signal 2
    SIGUSR2 = 12,
    /// Write on a pipe with no one to read it
    SIGPIPE = 13,
    /// Alarm clock
    SIGALRM = 14,
    /// Software termination signal from kill
    SIGTERM = 15,
    /// Stack fault (obsolete)
    SIGSTKFLT = 16,
    /// To parent on child stop or exit
    SIGCHLD = 17,
    /// Continue a stopped process
    SIGCONT = 18,
    /// Sendable stop signal not from tty
    SIGSTOP = 19,
    /// Stop signal from tty
    SIGTSTP = 20,
    /// To readers pgrp upon background tty read
    SIGTTIN = 21,
    /// Like TTIN if (tp->t_local&LTOSTOP)
    SIGTTOU = 22,
    /// Urgent condition on IO channel
    SIGURG = 23,
    /// Exceeded CPU time limit
    SIGXCPU = 24,
    /// Exceeded file size limit
    SIGXFSZ = 25,
    /// Virtual time alarm
    SIGVTALRM = 26,
    /// Profiling time alarm
    SIGPROF = 27,
    /// Window size changes
    SIGWINCH = 28,
    /// Input/output possible signal
    SIGIO = 29,
    /// Power failure imminent.
    SIGPWR = 30,
    /// Bad system call
    SIGSYS = 31,
}

/// Every signal, in the order nix's `Signal::iterator` yields them.
const SIGNALS: [Signal; 31] = [
    Signal::SIGHUP,
    Signal::SIGINT,
    Signal::SIGQUIT,
    Signal::SIGILL,
    Signal::SIGTRAP,
    Signal::SIGABRT,
    Signal::SIGBUS,
    Signal::SIGFPE,
    Signal::SIGKILL,
    Signal::SIGUSR1,
    Signal::SIGSEGV,
    Signal::SIGUSR2,
    Signal::SIGPIPE,
    Signal::SIGALRM,
    Signal::SIGTERM,
    Signal::SIGSTKFLT,
    Signal::SIGCHLD,
    Signal::SIGCONT,
    Signal::SIGSTOP,
    Signal::SIGTSTP,
    Signal::SIGTTIN,
    Signal::SIGTTOU,
    Signal::SIGURG,
    Signal::SIGXCPU,
    Signal::SIGXFSZ,
    Signal::SIGVTALRM,
    Signal::SIGPROF,
    Signal::SIGWINCH,
    Signal::SIGIO,
    Signal::SIGPWR,
    Signal::SIGSYS,
];

impl Signal {
    /// Returns name of signal.
    pub const fn as_str(self) -> &'static str {
        match self {
            Signal::SIGHUP => "SIGHUP",
            Signal::SIGINT => "SIGINT",
            Signal::SIGQUIT => "SIGQUIT",
            Signal::SIGILL => "SIGILL",
            Signal::SIGTRAP => "SIGTRAP",
            Signal::SIGABRT => "SIGABRT",
            Signal::SIGBUS => "SIGBUS",
            Signal::SIGFPE => "SIGFPE",
            Signal::SIGKILL => "SIGKILL",
            Signal::SIGUSR1 => "SIGUSR1",
            Signal::SIGSEGV => "SIGSEGV",
            Signal::SIGUSR2 => "SIGUSR2",
            Signal::SIGPIPE => "SIGPIPE",
            Signal::SIGALRM => "SIGALRM",
            Signal::SIGTERM => "SIGTERM",
            Signal::SIGSTKFLT => "SIGSTKFLT",
            Signal::SIGCHLD => "SIGCHLD",
            Signal::SIGCONT => "SIGCONT",
            Signal::SIGSTOP => "SIGSTOP",
            Signal::SIGTSTP => "SIGTSTP",
            Signal::SIGTTIN => "SIGTTIN",
            Signal::SIGTTOU => "SIGTTOU",
            Signal::SIGURG => "SIGURG",
            Signal::SIGXCPU => "SIGXCPU",
            Signal::SIGXFSZ => "SIGXFSZ",
            Signal::SIGVTALRM => "SIGVTALRM",
            Signal::SIGPROF => "SIGPROF",
            Signal::SIGWINCH => "SIGWINCH",
            Signal::SIGIO => "SIGIO",
            Signal::SIGPWR => "SIGPWR",
            Signal::SIGSYS => "SIGSYS",
        }
    }

    /// Iterate through all signals defined by this OS.
    pub const fn iterator() -> SignalIterator {
        SignalIterator { next: 0 }
    }
}

impl TryFrom<i32> for Signal {
    type Error = Errno;

    fn try_from(x: i32) -> Result<Self, Errno> {
        // The discriminants are exactly 1..=31, in order.
        if (1..=31).contains(&x) {
            Ok(SIGNALS[(x - 1) as usize])
        } else {
            Err(Errno::EINVAL)
        }
    }
}

impl FromStr for Signal {
    type Err = Errno;

    fn from_str(s: &str) -> Result<Self, Errno> {
        SIGNALS
            .iter()
            .copied()
            .find(|sig| sig.as_str() == s)
            .ok_or(Errno::EINVAL)
    }
}

impl AsRef<str> for Signal {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.as_ref())
    }
}

/// Iterate through all signals defined by this operating system.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SignalIterator {
    next: usize,
}

impl Iterator for SignalIterator {
    type Item = Signal;

    fn next(&mut self) -> Option<Signal> {
        let sig = SIGNALS.get(self.next).copied();
        if sig.is_some() {
            self.next += 1;
        }
        sig
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::Signal;

    /// The look-alike must agree with nix's `Signal` on every variant, in
    /// iterator order, by discriminant and by name, and in both conversions.
    #[test]
    fn matches_nix() {
        let ours: Vec<Signal> = Signal::iterator().collect();
        let theirs: Vec<nix::sys::signal::Signal> = nix::sys::signal::Signal::iterator().collect();
        assert_eq!(ours.len(), theirs.len());
        for (a, b) in ours.iter().zip(theirs.iter()) {
            assert_eq!(*a as i32, *b as i32);
            assert_eq!(a.as_str(), b.as_str());
            assert_eq!(a.to_string(), b.to_string());
            assert_eq!(format!("{a:?}"), format!("{b:?}"));
            assert_eq!(a.as_str().parse::<Signal>(), Ok(*a));
        }
        for raw in -2..70 {
            let ours = Signal::try_from(raw).map(|s| s as i32);
            let theirs = nix::sys::signal::Signal::try_from(raw).map(|s| s as i32);
            assert_eq!(ours.ok(), theirs.ok(), "raw signal {raw}");
        }
        assert!("SIGNOPE".parse::<Signal>().is_err());
    }
}
