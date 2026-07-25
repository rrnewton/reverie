/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Length-prefixed `bincode` framing.
//!
//! Each frame on the wire is a `u32` big-endian payload length followed by that
//! many bytes of `bincode` payload. Encoding uses [`bincode::config::legacy`] so
//! the format is identical to the one `reverie-ptrace` already uses to round-trip
//! the Detcore request/response types.

use std::io;
use std::io::Read;
use std::io::Write;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Maximum accepted frame payload size. Guards the reader against a corrupt or
/// hostile length prefix triggering an unbounded allocation.
pub const MAX_FRAME_LEN: u32 = 16 * 1024 * 1024;

fn bincode_config() -> impl bincode::config::Config {
    // NOTE: server and client must agree on this configuration exactly; it
    // matches `reverie-ptrace`'s `bincode::config::legacy()` usage.
    bincode::config::legacy()
}

/// Serialize `msg` and write it as one length-prefixed frame, flushing `w`.
///
/// [`Write::write_all`] handles short writes, so a partial socket write does not
/// corrupt the frame.
pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let payload = bincode::serde::encode_to_vec(msg, bincode_config())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("frame encode: {e}")))?;
    let len: u32 = payload
        .len()
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame exceeds u32 length"))?;
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame length {len} exceeds MAX_FRAME_LEN {MAX_FRAME_LEN}"),
        ));
    }
    w.write_all(&len.to_be_bytes())?;
    w.write_all(&payload)?;
    w.flush()
}

/// Read exactly one length-prefixed frame and deserialize it.
///
/// [`Read::read_exact`] handles short reads, so a partial socket read blocks for
/// the rest of the frame rather than returning a truncated value. A clean EOF at
/// a frame boundary surfaces as [`io::ErrorKind::UnexpectedEof`], which callers
/// treat as an orderly disconnect.
pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<T> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame length {len} exceeds MAX_FRAME_LEN {MAX_FRAME_LEN}"),
        ));
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload)?;
    let (value, _consumed) = bincode::serde::decode_from_slice(&payload, bincode_config())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("frame decode: {e}")))?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use serde::Serialize;

    use super::*;

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Sample {
        a: u64,
        b: Option<i64>,
        c: String,
    }

    #[test]
    fn frame_round_trips_through_a_buffer() {
        let msg = Sample {
            a: 42,
            b: Some(-7),
            c: "hello".to_owned(),
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &msg).unwrap();

        // The first four bytes are the big-endian payload length.
        let declared = u32::from_be_bytes(buf[..4].try_into().unwrap()) as usize;
        assert_eq!(declared, buf.len() - 4);

        let mut cursor = std::io::Cursor::new(buf);
        let decoded: Sample = read_frame(&mut cursor).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn multiple_frames_read_back_in_order() {
        let mut buf = Vec::new();
        for i in 0..5u64 {
            write_frame(&mut buf, &i).unwrap();
        }
        let mut cursor = std::io::Cursor::new(buf);
        for i in 0..5u64 {
            let got: u64 = read_frame(&mut cursor).unwrap();
            assert_eq!(got, i);
        }
    }

    #[test]
    fn eof_at_frame_boundary_is_unexpected_eof() {
        let mut cursor = std::io::Cursor::new(Vec::<u8>::new());
        let err = read_frame::<_, u64>(&mut cursor).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn oversized_length_prefix_is_rejected() {
        let mut framed = (MAX_FRAME_LEN + 1).to_be_bytes().to_vec();
        framed.extend_from_slice(&[0u8; 8]);
        let mut cursor = std::io::Cursor::new(framed);
        let err = read_frame::<_, u64>(&mut cursor).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
