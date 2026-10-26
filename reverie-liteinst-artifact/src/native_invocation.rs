// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

//! Read the normal Rust action wrapper's version 1 invocation handoff as data.
//!
//! The wrapper writes this observation before its final diagnostics and archive
//! postprocessing checks. Parsing it establishes neither compiler success nor
//! action completion, input custody, source authority, or qualification.
//!
//! The wire contract was frozen from the source-only wrapper-v2 patch and Python
//! body in invocation-source-1117, manifest SHA256
//! 59764c1c108e7c0603da279ccbdb2a69825fddf584c815013aa3a9fe8f7650ca.
//! Decoded POSIX strings remain bytes. No shell parsing, environment expansion,
//! path resolution, filesystem reads, or UTF-8 compiler-flag validation occurs.

use std::collections::BTreeSet;

use serde::Deserialize;

use super::Result;
use super::require;

/// Maximum encoded handoff size, checked before JSON parsing.
pub const MAX_INVOCATION_BYTES: usize = 16 * 1024 * 1024;
/// Maximum number of direct argv entries or retained response arguments.
pub const MAX_ARGUMENTS: usize = 65_536;
/// Maximum number of effective environment entries.
pub const MAX_ENV_ENTRIES: usize = 4096;
/// Maximum decoded size of one argv, environment, or path value.
pub const MAX_VALUE_BYTES: usize = 1024 * 1024;
/// Maximum decoded size of the retained generated response file.
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// Retained bytes; the temporary path need not still exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResponseFile {
    pub path: Vec<u8>,
    pub contents: Vec<u8>,
}

/// Caller-constructible observation data, never an authority token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationObservation {
    /// Actual subprocess argv, including its one terminal generated @response.
    pub argv: Vec<Vec<u8>>,
    /// Effective environment, preserving the wrapper's insertion order.
    pub env: Vec<(Vec<u8>, Vec<u8>)>,
    pub cwd: Vec<u8>,
    pub response_file: ResponseFile,
    /// Generated response contents split into LF-delimited argument bytes.
    /// Nested @references are retained, not opened, expanded, or checked.
    pub response_arguments: Vec<Vec<u8>>,
    /// Actual argv with only the generated response reference replaced by its
    /// retained arguments. This is not fully expanded if nested @references
    /// remain; their declared inputs require independent factory handling.
    pub expanded_argv: Vec<Vec<u8>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireInvocation {
    version: u32,
    argv_base64: Vec<String>,
    env_base64: Vec<[String; 2]>,
    cwd_base64: String,
    response_file: WireResponseFile,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireResponseFile {
    path_base64: String,
    contents_base64: String,
}

/// Decode the exact handoff shape with bounded, canonical base64 and unique
/// JSON/environment keys. The normal wrapper emits its generated response as
/// the final argv entry; handoff mode prohibits a program-argument tail.
///
/// The wrapper writes `"\n".join(rustc_args).encode() + b"\n"`. Preserve empty
/// lines, spaces, tabs, quotes and backslashes; do not apply shell splitting or
/// normalize them a second time. CR is refused because accepting it could make
/// line-ending interpretation differ from the retained argument bytes. Other
/// non-NUL bytes remain intact for later explicit compiler-flag validation.
pub fn parse_invocation_handoff(bytes: &[u8]) -> Result<InvocationObservation> {
    let value = super::native_wire::parse(bytes, MAX_INVOCATION_BYTES)?;
    let wire: WireInvocation = serde_json::from_value(value)
        .map_err(|error| format!("native invocation handoff shape: {error}"))?;
    require(
        wire.version == 1,
        "native invocation version is unsupported",
    )?;
    require(
        (2..=MAX_ARGUMENTS).contains(&wire.argv_base64.len()),
        "native invocation argv count is outside bounds",
    )?;
    require(
        wire.env_base64.len() <= MAX_ENV_ENTRIES,
        "native invocation environment count exceeds bound",
    )?;

    let argv: Vec<Vec<u8>> = wire
        .argv_base64
        .iter()
        .map(|arg| decode_os(arg, "argv", MAX_VALUE_BYTES))
        .collect::<Result<_>>()?;
    require(!argv[0].is_empty(), "native invocation executable is empty")?;

    let mut env = Vec::with_capacity(wire.env_base64.len());
    let mut names = BTreeSet::new();
    for [name, value] in &wire.env_base64 {
        let name = decode_os(name, "environment name", MAX_VALUE_BYTES)?;
        let value = decode_os(value, "environment value", MAX_VALUE_BYTES)?;
        require(
            !name.is_empty() && !name.contains(&b'='),
            "native invocation environment name is invalid",
        )?;
        require(
            names.insert(name.clone()),
            "native invocation environment name is duplicated",
        )?;
        env.push((name, value));
    }

    let cwd = decode_os(&wire.cwd_base64, "cwd", MAX_VALUE_BYTES)?;
    require(
        cwd.first() == Some(&b'/'),
        "native invocation cwd is not an absolute POSIX path",
    )?;
    let path = decode_os(
        &wire.response_file.path_base64,
        "response path",
        MAX_VALUE_BYTES,
    )?;
    require(!path.is_empty(), "native invocation response path is empty")?;
    let contents = decode_os(
        &wire.response_file.contents_base64,
        "response contents",
        MAX_RESPONSE_BYTES,
    )?;
    require(
        contents.last() == Some(&b'\n') && !contents.contains(&b'\r'),
        "native invocation response requires final LF and unambiguous LF lines",
    )?;

    let mut reference = Vec::with_capacity(path.len() + 1);
    reference.push(b'@');
    reference.extend_from_slice(&path);
    require(
        argv.last() == Some(&reference)
            && argv.iter().filter(|arg| *arg == &reference).count() == 1,
        "native invocation requires exactly one terminal matching response reference",
    )?;

    // Remove exactly the terminator the wrapper appends. An earlier empty line
    // is an empty argument, not whitespace to discard.
    let lines = &contents[..contents.len() - 1];
    let argument_count = lines.iter().filter(|byte| **byte == b'\n').count() + 1;
    require(
        argument_count <= MAX_ARGUMENTS && argv.len() - 1 + argument_count <= MAX_ARGUMENTS,
        "native invocation expanded argv count exceeds bound",
    )?;
    let response_arguments = lines
        .split(|byte| *byte == b'\n')
        .map(|arg| {
            require(
                arg.len() <= MAX_VALUE_BYTES,
                "native invocation response argument exceeds byte bound",
            )?;
            Ok(arg.to_vec())
        })
        .collect::<Result<Vec<_>>>()?;
    let mut expanded_argv = argv[..argv.len() - 1].to_vec();
    expanded_argv.extend(response_arguments.iter().cloned());

    Ok(InvocationObservation {
        argv,
        env,
        cwd,
        response_file: ResponseFile { path, contents },
        response_arguments,
        expanded_argv,
    })
}

fn decode_os(encoded: &str, field: &str, max_bytes: usize) -> Result<Vec<u8>> {
    let decoded = decode_base64(encoded, max_bytes)
        .map_err(|error| format!("native invocation {field}: {error}"))?;
    require(
        !decoded.contains(&0),
        format!("native invocation {field} contains NUL"),
    )?;
    Ok(decoded)
}

// Python base64.b64encode emits the standard alphabet with canonical padding.
// Reject whitespace, alternate alphabets, misplaced padding, and nonzero pad
// bits; permissive decoding would admit wire bytes the wrapper never writes.
fn decode_base64(encoded: &str, max_bytes: usize) -> Result<Vec<u8>> {
    let input = encoded.as_bytes();
    require(
        input.len().is_multiple_of(4) && input.len() / 4 <= max_bytes.div_ceil(3),
        "base64 length exceeds bound or lacks padding",
    )?;
    let digit = |byte: u8| -> Result<u8> {
        match byte {
            b'A'..=b'Z' => Ok(byte - b'A'),
            b'a'..=b'z' => Ok(byte - b'a' + 26),
            b'0'..=b'9' => Ok(byte - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err("base64 alphabet or padding is invalid".into()),
        }
    };
    let mut decoded = Vec::with_capacity((input.len() / 4 * 3).min(max_bytes));
    for (index, chunk) in input.as_chunks::<4>().0.iter().enumerate() {
        let final_chunk = (index + 1) * 4 == input.len();
        let a = digit(chunk[0])?;
        let b = digit(chunk[1])?;
        decoded.push((a << 2) | (b >> 4));
        if chunk[2] == b'=' {
            require(
                final_chunk && chunk[3] == b'=' && b & 15 == 0,
                "base64 one-byte padding is noncanonical",
            )?;
        } else {
            let c = digit(chunk[2])?;
            decoded.push((b << 4) | (c >> 2));
            if chunk[3] == b'=' {
                require(
                    final_chunk && c & 3 == 0,
                    "base64 two-byte padding is noncanonical",
                )?;
            } else {
                decoded.push((c << 6) | digit(chunk[3])?);
            }
        }
    }
    require(decoded.len() <= max_bytes, "decoded bytes exceed bound")?;
    Ok(decoded)
}
