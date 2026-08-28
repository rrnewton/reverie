//! Complete wire type for one published validation receipt.
//!
//! Read the version before the version-specific shape. A newer receipt must be
//! refused as unsupported rather than rejected for whichever schema-1 field it
//! happens not to carry.
//!
//! `validation/publish_receipt.py` writes this value. The immediate Rust
//! publisher check, the immutable-receipt reuse path, and
//! `validation/verify_receipt.sh` all consume it through this module. Keep that
//! list together: before this type, the Rust and shell consumers disagreed
//! about required outer fields and the source-log binding.

use crate::records::HistoryRow;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const SCHEMA_VERSION: u32 = 1;
pub const CANONICALIZATION: &str = "serde_json::to_vec(HistoryRow)-v1";

#[derive(Clone, Debug, Deserialize)]
struct ReceiptEnvelope {
    schema_version: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptIdentity {
    pub digest_algorithm: String,
    pub canonicalization: String,
    pub digest: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProducerDefinitionEvidence {
    pub definition: BTreeMap<String, String>,
    pub coverage_status: String,
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_commits: Option<Vec<String>>,
    pub resolved_from: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProducerDefinitionExpectation {
    pub definition: BTreeMap<String, String>,
    pub coverage_status: String,
    pub paths: Vec<String>,
    #[serde(default)]
    pub valid_commits: Option<Vec<String>>,
}

impl From<&ProducerDefinitionEvidence> for ProducerDefinitionExpectation {
    fn from(value: &ProducerDefinitionEvidence) -> Self {
        Self {
            definition: value.definition.clone(),
            coverage_status: value.coverage_status.clone(),
            paths: value.paths.clone(),
            valid_commits: value.valid_commits.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublishedValidationReceipt {
    pub schema_version: u32,
    pub repository: String,
    pub commit: String,
    pub run_id: String,
    pub source_log_file: String,
    pub durable_log_file: String,
    pub log_sha256: String,
    pub producer: ProducerDefinitionEvidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_receipt_identity: Option<ReceiptIdentity>,
    pub ledger_record: HistoryRow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityRequirement {
    Required,
    LegacyOptional,
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

impl PublishedValidationReceipt {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let envelope: ReceiptEnvelope = serde_json::from_slice(bytes).map_err(|error| {
            format!("published validation receipt envelope is malformed: {error}")
        })?;
        if envelope.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported published validation receipt schema {}",
                envelope.schema_version
            ));
        }
        serde_json::from_slice(bytes)
            .map_err(|error| format!("schema-1 published validation receipt is malformed: {error}"))
    }

    pub fn validate(
        &self,
        repository: &str,
        commit: &str,
        selected_row: &HistoryRow,
        selected_digest: &str,
        identity_requirement: IdentityRequirement,
        expected_producer: Option<&ProducerDefinitionExpectation>,
    ) -> Result<(), String> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported published validation receipt schema {}",
                self.schema_version
            ));
        }
        if self.repository != repository || self.commit != commit {
            return Err("publisher artifact is not bound to repository and SHA".into());
        }

        let expected_row = serde_json::to_value(selected_row)
            .map_err(|error| format!("cannot serialize selected HistoryRow: {error}"))?;
        let actual_row = serde_json::to_value(&self.ledger_record)
            .map_err(|error| format!("cannot serialize receipt HistoryRow: {error}"))?;
        if actual_row != expected_row {
            return Err("publisher artifact ledger row differs from Rust-selected row".into());
        }

        let started_at = selected_row
            .started_at
            .as_deref()
            .ok_or_else(|| "selected HistoryRow omitted started_at".to_string())?;
        let host = selected_row
            .host
            .as_deref()
            .filter(|host| !host.trim().is_empty())
            .ok_or_else(|| "selected HistoryRow omitted host".to_string())?;
        if self.run_id != format!("{commit}@{started_at}@{host}") {
            return Err("publisher artifact run identity differs from selected row".into());
        }
        if selected_row.log_file.as_deref() != Some(self.source_log_file.as_str()) {
            return Err(
                "published validation receipt source_log_file differs from selected row".into(),
            );
        }
        if !Path::new(&self.durable_log_file).is_absolute() {
            return Err("published validation receipt durable_log_file is not absolute".into());
        }
        if !is_lower_hex(&self.log_sha256, 64) {
            return Err("published validation receipt log_sha256 is malformed".into());
        }

        match (&self.selected_receipt_identity, identity_requirement) {
            (None, IdentityRequirement::Required) => {
                return Err(
                    "published validation receipt omitted selected_receipt_identity".into(),
                );
            }
            (None, IdentityRequirement::LegacyOptional) => {}
            (Some(identity), _) => {
                if identity.digest_algorithm != "sha256"
                    || identity.canonicalization != CANONICALIZATION
                    || !is_lower_hex(&identity.digest, 64)
                    || identity.digest != selected_digest
                {
                    return Err(
                        "publisher artifact receipt identity does not match Rust selection".into(),
                    );
                }
            }
        }

        if self.producer.definition.is_empty() {
            return Err("published validation receipt producer definition is empty".into());
        }
        let definition_paths = self.producer.definition.keys().cloned().collect::<Vec<_>>();
        if self.producer.paths != definition_paths {
            return Err(
                "published validation receipt producer paths differ from its definition".into(),
            );
        }
        if !matches!(
            self.producer.coverage_status.as_str(),
            "legacy-selected-paths" | "complete"
        ) {
            return Err("published validation receipt producer coverage_status is invalid".into());
        }
        if self.producer.resolved_from.trim().is_empty() {
            return Err("published validation receipt producer resolved_from is empty".into());
        }
        if let Some(commits) = &self.producer.valid_commits {
            if commits.is_empty()
                || !commits.iter().any(|candidate| candidate == commit)
                || commits.iter().any(|candidate| !is_lower_hex(candidate, 40))
                || commits.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err(
                    "published validation receipt producer valid_commits is invalid for target"
                        .into(),
                );
            }
        }
        if let Some(expected) = expected_producer {
            if self.producer.definition != expected.definition
                || self.producer.coverage_status != expected.coverage_status
                || self.producer.paths != expected.paths
                || (self.producer.valid_commits.is_some()
                    && self.producer.valid_commits != expected.valid_commits)
            {
                return Err(
                    "published validation receipt producer condition differs from exact target commit"
                        .into(),
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_schema_is_identified_before_schema_one_shape() {
        let error = PublishedValidationReceipt::parse(br#"{"schema_version":2}"#)
            .expect_err("newer schema must be refused");
        assert_eq!(error, "unsupported published validation receipt schema 2");
    }

    #[test]
    fn missing_schema_fails_by_field_name() {
        let error = PublishedValidationReceipt::parse(br#"{}"#)
            .expect_err("missing schema must be refused");
        assert!(error.contains("missing field `schema_version`"), "{error}");
    }

    #[test]
    fn schema_one_extension_requires_a_schema_bump() {
        let error = PublishedValidationReceipt::parse(
            br#"{"schema_version":1,"unversioned_extension":true}"#,
        )
        .expect_err("an unversioned extension must be refused");
        assert!(
            error.contains("unknown field `unversioned_extension`"),
            "{error}"
        );
    }
}
