/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::io;
use std::io::Write;

use serde::Deserialize;
use serde::Serialize;

/// Maximum accepted frame size (16 MiB).
const MAX_FRAME_LEN: usize = 16 * (1 << 20);

// NOTE: Both the server and client must agree on this configuration. Otherwise,
// we'll get deserialization errors. `legacy` matches the configuration used
// elsewhere in the Reverie tree, including `reverie-rpc-transport`.
fn bincode_config() -> impl bincode::config::Config {
    bincode::config::legacy().with_limit::<MAX_FRAME_LEN>()
}

pub fn encode<T>(item: &T, buf: &mut Vec<u8>) -> io::Result<()>
where
    T: Serialize,
{
    let mut cursor = io::Cursor::new(buf);

    // Reserve 4 bytes at the beginning of the buffer so we can fill it in
    // with the length of the payload once we know what it is.
    cursor.write_all(&[0, 0, 0, 0])?;

    // Serialize into our buffer.
    encode_frame_into(&mut cursor, item)?;

    let buf = cursor.into_inner();

    // Fill in the actual size now that we know what it is.
    let size = buf[4..].len() as u32;
    buf[0..4].copy_from_slice(&size.to_be_bytes());

    Ok(())
}

/// Encodes a length-delimited frame.
pub fn encode_frame_into<W, T>(writer: W, item: &T) -> io::Result<()>
where
    T: Serialize + ?Sized,
    W: Write,
{
    bincode::serde::encode_into_std_write(item, &mut { writer }, bincode_config())
        .map(|_written| ())
        .map_err(|e| io::Error::other(format!("failed to encode frame: {}", e)))
}

/// Decodes a length-delimited frame.
pub fn decode_frame<'a, T>(frame: &'a [u8]) -> io::Result<T>
where
    T: Deserialize<'a>,
{
    bincode::serde::borrow_decode_from_slice(frame, bincode_config())
        .map(|(value, _consumed)| value)
        .map_err(|e| io::Error::other(format!("failed to decode frame: {}", e)))
}

pub fn decode_from<'a, T, R>(mut reader: R, buf: &'a mut Vec<u8>) -> io::Result<T>
where
    T: Deserialize<'a>,
    R: io::Read,
{
    let mut head = [0u8; 4];
    reader.read_exact(&mut head)?;

    let len = u32::from_be_bytes(head) as usize;

    buf.resize(len, 0);

    reader.read_exact(buf)?;

    decode_frame(buf)
}
