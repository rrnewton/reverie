/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use bytes::BytesMut;

use crate::gdbstub::commands::*;
use crate::gdbstub::hex::*;

/// `T thread-id`: is the thread still alive?
///
/// GDB sends this whenever it switches to a thread by number (the `thread N`
/// command, and every DAP request that names a `threadId`). Any reply other
/// than `OK` makes GDB report the thread as terminated, so this must be
/// answered rather than left to the empty "unsupported" reply.
#[derive(PartialEq, Debug)]
pub struct T {
    pub thread: ThreadId,
}

impl ParseCommand for T {
    fn parse(bytes: BytesMut) -> Option<Self> {
        if bytes.starts_with(b"p") {
            // Multiprocess form: `ppid.tid`.
            ThreadId::decode(&bytes).map(|thread| T { thread })
        } else {
            // Without multiprocess extensions GDB sends the bare thread id.
            let tid: i32 = decode_hex(&bytes).ok()?;
            Some(T {
                thread: ThreadId {
                    pid: IdKind::Any,
                    tid: IdKind::from_raw(tid),
                },
            })
        }
    }
}

#[cfg(test)]
mod test {
    use reverie::Pid;

    use super::*;

    #[test]
    fn parses_multiprocess_thread_id() {
        assert_eq!(
            T::parse(BytesMut::from("p3.3")),
            Some(T {
                thread: ThreadId::pid_tid(3, 3)
            })
        );
        assert_eq!(
            T::parse(BytesMut::from("p1a.2b")),
            Some(T {
                thread: ThreadId::pid_tid(0x1a, 0x2b)
            })
        );
    }

    #[test]
    fn parses_bare_thread_id() {
        assert_eq!(
            T::parse(BytesMut::from("1f")),
            Some(T {
                thread: ThreadId {
                    pid: IdKind::Any,
                    tid: IdKind::Id(Pid::from_raw(0x1f)),
                },
            })
        );
    }

    #[test]
    fn rejects_malformed_thread_id() {
        assert_eq!(T::parse(BytesMut::from("")), None);
        assert_eq!(T::parse(BytesMut::from("pzz.1")), None);
        assert_eq!(T::parse(BytesMut::from("xyz")), None);
    }
}
