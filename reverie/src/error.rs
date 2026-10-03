/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Error handling.

pub use reverie_syscalls::Errno;
use thiserror::Error;

/// A general error.
#[derive(Error, Debug)]
pub enum Error {
    /// A low-level errno.
    // syscalls' `Errno` implements `Error` only with its `std` feature, and
    // `transparent`/`from` need that impl. Without it the variant displays
    // the same way, has no source (as `Errno` has none), and gets its `From`
    // impl below.
    #[cfg_attr(feature = "std", error(transparent))]
    #[cfg_attr(not(feature = "std"), error("{0}"))]
    Errno(#[cfg_attr(feature = "std", from)] Errno),

    /// A generic error that may be produced by the tool.
    #[error(transparent)]
    Tool(#[from] anyhow::Error),

    /// An I/O error.
    #[cfg(feature = "std")]
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// Extracts the errno from the error. If this is not an `Error::Errno`, then
    /// returns `Err(Error)`. This is useful for capturing syscall errors and
    /// propagating all other types of errors.
    pub fn into_errno(self) -> Result<Errno, Self> {
        if let Self::Errno(err) = self {
            Ok(err)
        } else {
            Err(self)
        }
    }
}

#[cfg(not(feature = "std"))]
impl From<Errno> for Error {
    fn from(err: Errno) -> Self {
        Self::Errno(err)
    }
}

#[cfg(feature = "std")]
impl From<nix::errno::Errno> for Error {
    fn from(err: nix::errno::Errno) -> Self {
        Self::Errno(Errno::new(err as i32))
    }
}
