// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

//! Read executor metadata v1 as a regular-content observation.
//!
//! The wire carries project-relative file paths and untagged `hex:size`
//! digests. It carries no successful-action, configured-role, symlink,
//! directory, executable-mode or relocation authority. The caller owns fresh
//! file/namespace custody and independently expected source/tool checks.
//! Neither this parser nor its exact regular-file join performs filesystem I/O.

use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

use super::Result;
use super::require;

pub const MAX_INVENTORY_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_INVENTORY_ENTRIES: usize = 131_072;
pub const MAX_INVENTORY_PATH_BYTES: usize = 16 * 1024;

/// Untagged executor digest observation; its hexadecimal length does not select
/// an algorithm. In particular, 64 digits do not establish SHA-256.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExecutorDigest {
    pub raw: String,
    pub size: u64,
}

/// Caller-constructible data. No inventory grants compilation/source authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Inventory {
    pub entries: BTreeMap<PathBuf, ExecutorDigest>,
}

/// Exact path/size association, retaining both distinct digest observations.
/// The caller supplied `fresh_sha256` after its independent physical read; this
/// helper does not infer or compare the executor's digest algorithm.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RegularFileMatch {
    pub executor_digest: String,
    pub size: u64,
    pub fresh_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireInventory {
    version: u32,
    digests: Vec<WireDigest>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDigest {
    path: String,
    digest: String,
}

fn forward_relative(path: &str) -> Result<()> {
    let bytes = path.as_bytes();
    require(
        !bytes.is_empty()
            && bytes.len() <= MAX_INVENTORY_PATH_BYTES
            && !path.starts_with('/')
            && !path.contains('\0')
            && !path.contains('\\')
            && !(bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
            && path
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."),
        "executor inventory path is not a normal project-relative forward path",
    )
}

fn digest(raw: &str) -> Result<ExecutorDigest> {
    let (hex, size) = raw
        .split_once(':')
        .ok_or("executor digest lacks its size separator")?;
    // The fixed producer's RawDigest Display emits 20- or 32-byte lowercase
    // hex; the latter includes multiple algorithms, including keyed digests.
    require(
        matches!(hex.len(), 40 | 64)
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "executor digest is not the normal raw hexadecimal spelling",
    )?;
    require(
        !size.is_empty()
            && size.bytes().all(|byte| byte.is_ascii_digit())
            && (size == "0" || !size.starts_with('0')),
        "executor digest size is not canonical unsigned decimal",
    )?;
    let size = size
        .parse::<u64>()
        .map_err(|e| format!("executor digest size: {e}"))?;
    Ok(ExecutorDigest {
        raw: raw.into(),
        size,
    })
}

/// Decode only the exact v1 executor schema. Empty inventories cannot satisfy
/// this Rust action's content-input contract; no older field or partial map is
/// substituted. Caller custody must bind the exact retained metadata bytes.
pub fn parse_executor_inventory(bytes: &[u8]) -> Result<Inventory> {
    let value = super::native_wire::parse(bytes, MAX_INVENTORY_BYTES)?;
    let wire: WireInventory = serde_json::from_value(value)
        .map_err(|e| format!("executor input inventory shape: {e}"))?;
    require(wire.version == 1, "unsupported executor inventory version")?;
    require(
        !wire.digests.is_empty() && wire.digests.len() <= MAX_INVENTORY_ENTRIES,
        "executor inventory entry count is empty or exceeds its bound",
    )?;
    let mut entries = BTreeMap::new();
    for entry in wire.digests {
        forward_relative(&entry.path)?;
        let observed = digest(&entry.digest)?;
        require(
            entries
                .insert(PathBuf::from(entry.path), observed)
                .is_none(),
            "duplicate executor inventory path",
        )?;
    }
    Ok(Inventory { entries })
}

fn checked_path(path: &Path) -> Result<()> {
    forward_relative(
        path.to_str()
            .ok_or("executor inventory path is not UTF-8")?,
    )
}

/// Require the complete regular-file path set and each file size to match the
/// caller's freshly hashed regular files. No filtering, root rebasing, alias
/// discovery or suffix matching occurs here. The caller must independently
/// check physical directories/links, configured roles, tools and source bytes.
pub fn match_regular_files(
    inventory: &Inventory,
    fresh: &BTreeMap<PathBuf, (u64, String)>,
) -> Result<BTreeMap<PathBuf, RegularFileMatch>> {
    require(
        !inventory.entries.is_empty()
            && inventory.entries.len() <= MAX_INVENTORY_ENTRIES
            && inventory.entries.keys().eq(fresh.keys()),
        "executor and fresh complete regular-file path sets differ or are empty",
    )?;
    for path in fresh.keys() {
        checked_path(path)?;
    }
    let mut matched = BTreeMap::new();
    for (path, observation) in &inventory.entries {
        checked_path(path)?;
        require(
            digest(&observation.raw)? == *observation,
            "executor digest observation has an inconsistent decoded size",
        )?;
        let (size, sha256) = &fresh[path];
        require(
            *size == observation.size,
            format!("executor/fresh file size differs: {}", path.display()),
        )?;
        require(
            sha256.len() == 64
                && sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "fresh regular-file SHA-256 has an invalid spelling",
        )?;
        matched.insert(
            path.clone(),
            RegularFileMatch {
                executor_digest: observation.raw.clone(),
                size: *size,
                fresh_sha256: sha256.clone(),
            },
        );
    }
    Ok(matched)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(path: &str, raw: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "version":1,
            "digests":[{"path":path,"digest":raw}]
        }))
        .unwrap()
    }

    #[test]
    fn exact_regular_join_keeps_untagged_executor_and_fresh_sha256_separate() {
        for length in [40, 64] {
            let raw = format!("{}:23", "a".repeat(length));
            let inventory = parse_executor_inventory(&record("inputs/source.rs", &raw)).unwrap();
            let fresh = BTreeMap::from([(PathBuf::from("inputs/source.rs"), (23, "b".repeat(64)))]);
            let matched = match_regular_files(&inventory, &fresh).unwrap();
            assert_eq!(matched[Path::new("inputs/source.rs")].executor_digest, raw);
            assert_eq!(
                matched[Path::new("inputs/source.rs")].fresh_sha256,
                "b".repeat(64)
            );
            assert_eq!(matched[Path::new("inputs/source.rs")].size, 23);
        }
        let empty_file =
            parse_executor_inventory(&record("empty", &format!("{}:0", "0".repeat(64)))).unwrap();
        assert_eq!(empty_file.entries[Path::new("empty")].size, 0);
    }

    #[test]
    fn complete_path_and_size_join_refuses_missing_extra_renamed_and_mutated_files() {
        let inventory = parse_executor_inventory(&record(
            "inputs/source.rs",
            &format!("{}:23", "a".repeat(64)),
        ))
        .unwrap();
        let fresh = BTreeMap::from([(PathBuf::from("inputs/source.rs"), (23, "b".repeat(64)))]);
        assert!(match_regular_files(&inventory, &BTreeMap::new()).is_err());
        let mut extra = fresh.clone();
        extra.insert(PathBuf::from("hidden/tool"), (1, "c".repeat(64)));
        assert!(match_regular_files(&inventory, &extra).is_err());
        let renamed =
            BTreeMap::from([(PathBuf::from("elsewhere/source.rs"), (23, "b".repeat(64)))]);
        assert!(match_regular_files(&inventory, &renamed).is_err());
        let wrong_size =
            BTreeMap::from([(PathBuf::from("inputs/source.rs"), (24, "b".repeat(64)))]);
        assert!(match_regular_files(&inventory, &wrong_size).is_err());
        let invalid_sha =
            BTreeMap::from([(PathBuf::from("inputs/source.rs"), (23, "not-sha256".into()))]);
        assert!(match_regular_files(&inventory, &invalid_sha).is_err());
        let normalized_alias =
            BTreeMap::from([(PathBuf::from("inputs/./source.rs"), (23, "b".repeat(64)))]);
        assert!(match_regular_files(&inventory, &normalized_alias).is_err());
        let mut forged = inventory.clone();
        forged
            .entries
            .get_mut(Path::new("inputs/source.rs"))
            .unwrap()
            .size = 24;
        assert!(match_regular_files(&forged, &wrong_size).is_err());
        assert!(
            match_regular_files(
                &Inventory {
                    entries: BTreeMap::new()
                },
                &BTreeMap::new()
            )
            .is_err()
        );
    }

    #[test]
    fn unknown_missing_duplicate_and_old_wire_fields_refuse() {
        let raw = format!("{}:1", "a".repeat(64));
        let valid: serde_json::Value = serde_json::from_slice(&record("input", &raw)).unwrap();
        for version in [
            serde_json::json!(0),
            serde_json::json!(2),
            serde_json::json!(-1),
            serde_json::json!("1"),
        ] {
            let mut value = valid.clone();
            value["version"] = version;
            assert!(parse_executor_inventory(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        for bytes in [
            br#"{"version":1,"version":1,"digests":[]}"#.as_slice(),
            br#"{"version":1,"digests":[]}"#,
            br#"{"version":1,"action_inputs":{}}"#,
            br#"{"digests":[]}"#,
        ] {
            assert!(parse_executor_inventory(bytes).is_err());
        }
        let mut unknown = valid.clone();
        unknown["status"] = serde_json::json!("success");
        assert!(parse_executor_inventory(&serde_json::to_vec(&unknown).unwrap()).is_err());
        let mut unknown = valid.clone();
        unknown["digests"][0]["algorithm"] = serde_json::json!("sha256");
        assert!(parse_executor_inventory(&serde_json::to_vec(&unknown).unwrap()).is_err());
        let duplicated = format!(
            "{{\"version\":1,\"digests\":[{{\"path\":\"input\",\"path\":\"other\",\"digest\":\"{raw}\"}}]}}"
        );
        assert!(parse_executor_inventory(duplicated.as_bytes()).is_err());
        let mut duplicated = valid.clone();
        duplicated["digests"] = serde_json::json!([valid["digests"][0], valid["digests"][0]]);
        assert!(parse_executor_inventory(&serde_json::to_vec(&duplicated).unwrap()).is_err());
        let mut trailing = record("input", &raw);
        trailing.extend_from_slice(b" {}");
        assert!(parse_executor_inventory(&trailing).is_err());
    }

    #[test]
    fn absolute_traversal_nonforward_and_oversized_paths_refuse() {
        let raw = format!("{}:1", "a".repeat(64));
        for path in [
            "",
            "/absolute",
            "//absolute",
            ".",
            "..",
            "a/../b",
            "./a",
            "a/./b",
            "a//b",
            "a/",
            "a\\b",
            "a\0b",
            "C:/absolute",
        ] {
            assert!(
                parse_executor_inventory(&record(path, &raw)).is_err(),
                "{path:?}"
            );
        }
        assert!(
            parse_executor_inventory(&record(&"x".repeat(MAX_INVENTORY_PATH_BYTES + 1), &raw))
                .is_err()
        );
        assert!(parse_executor_inventory(&vec![b' '; MAX_INVENTORY_BYTES + 1]).is_err());
    }

    #[test]
    fn malformed_untagged_digest_and_noncanonical_or_overflow_size_refuse() {
        let hex = "a".repeat(64);
        for raw in [
            "".into(),
            hex.clone(),
            format!("sha256:{hex}:1"),
            format!("{hex}:1:2"),
            format!("{}:1", "a".repeat(63)),
            format!("{}:1", "G".repeat(64)),
            format!("{}:1", "A".repeat(64)),
        ] {
            assert!(
                parse_executor_inventory(&record("input", &raw)).is_err(),
                "{raw}"
            );
        }
        for size in [
            "",
            "-1",
            "+1",
            "01",
            "00",
            " 1",
            "1 ",
            "1.0",
            "1e2",
            "18446744073709551616",
        ] {
            assert!(
                parse_executor_inventory(&record("input", &format!("{hex}:{size}"))).is_err(),
                "{size:?}"
            );
        }
        let maximum =
            parse_executor_inventory(&record("input", &format!("{hex}:{}", u64::MAX))).unwrap();
        assert_eq!(maximum.entries[Path::new("input")].size, u64::MAX);
    }
}
