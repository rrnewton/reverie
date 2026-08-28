//! Complete envelope type for one append-only validation-ledger event.
//!
//! `ledger/validate_rows.py` and `ledger.migrate_legacy` produce these events.
//! The append-only publisher, canonical compatibility view, GitHub index, and
//! Rust remote-freshness probe consume them. Payload extensions remain open in
//! envelope version 1; the envelope fields and their closed values do not.
//!
//! Production consumers are enumerated in `ci-hub/ledger/README.md`. Raw event
//! inspection deliberately remains able to show refused bytes, but no path may
//! publish them or turn them into validation evidence without this contract.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const SCHEMA: &str = "validate-ledger/v1";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum EventType {
    #[serde(rename = "run.start")]
    RunStart,
    #[serde(rename = "run.result")]
    RunResult,
    #[serde(rename = "run.enrich")]
    RunEnrich,
    #[serde(rename = "run.correct")]
    RunCorrect,
    #[serde(rename = "run.annotate")]
    RunAnnotate,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProducerSource {
    Observed,
    Reconstructed,
    Imported,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Pass,
    Fail,
    Timeout,
    Killed,
    Incomplete,
    NoResult,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Producer {
    pub source: ProducerSource,
    pub tool: String,
    pub tool_version: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
struct Envelope {
    schema: String,
    #[serde(default)]
    event_type: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LedgerEvent {
    pub schema: String,
    pub event_id: String,
    pub event_type: EventType,
    pub emitted_at: String,
    pub team: String,
    pub host: String,
    pub run_id: String,
    pub producer: Producer,
    #[serde(default)]
    pub enriches: Option<String>,
    #[serde(default)]
    pub supersedes: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub outcome: Option<Outcome>,
    /// The embedded row has its own producer-owned schema and correction rules.
    /// Keep its exact JSON here so a malformed historical row remains visible
    /// to the correction reader instead of making the outer event disappear.
    #[serde(default)]
    pub legacy_row: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn nonempty(value: &str) -> bool {
    !value.trim().is_empty()
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

impl LedgerEvent {
    /// Read the envelope version before interpreting its version-specific
    /// fields. A newer event remains distinguishable from a malformed v1 event.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let envelope: Envelope = serde_json::from_slice(bytes)
            .map_err(|error| format!("validation ledger event envelope is malformed: {error}"))?;
        if envelope.schema != SCHEMA {
            return Err(format!(
                "unsupported validation ledger event schema {:?}",
                envelope.schema
            ));
        }
        if let Some(event_type) = envelope.event_type {
            serde_json::from_value::<EventType>(event_type)
                .map_err(|error| format!("validate-ledger/v1 event_type is invalid: {error}"))?;
        }
        serde_json::from_slice(bytes)
            .map_err(|error| format!("validate-ledger/v1 event is malformed: {error}"))
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA {
            return Err(format!(
                "unsupported validation ledger event schema {:?}",
                self.schema
            ));
        }
        for (name, value) in [
            ("event_id", self.event_id.as_str()),
            ("emitted_at", self.emitted_at.as_str()),
            ("team", self.team.as_str()),
            ("host", self.host.as_str()),
            ("run_id", self.run_id.as_str()),
            ("producer.tool", self.producer.tool.as_str()),
            ("producer.tool_version", self.producer.tool_version.as_str()),
        ] {
            if !nonempty(value) {
                return Err(format!("validation ledger event {name} is empty"));
            }
        }
        if self.team.contains('/') || matches!(self.team.as_str(), "." | "..") {
            return Err("validation ledger event team is not one path segment".into());
        }
        if self.host.contains('.')
            || self.host.contains('/')
            || matches!(self.host.as_str(), "." | "..")
        {
            return Err("validation ledger event host is not a short path segment".into());
        }
        let emitted_at = chrono::DateTime::parse_from_rfc3339(&self.emitted_at);
        if emitted_at.is_err()
            || !self.emitted_at.ends_with('Z')
            || emitted_at
                .as_ref()
                .is_ok_and(|value| value.offset().local_minus_utc() != 0)
        {
            return Err("validation ledger event emitted_at is not RFC3339 UTC".into());
        }
        if self
            .commit
            .as_deref()
            .is_some_and(|commit| !is_lower_hex(commit, 40))
        {
            return Err(
                "validation ledger event commit is not 40 lowercase hexadecimal characters".into(),
            );
        }

        match self.event_type {
            EventType::RunStart => {
                if self.run_id != self.event_id {
                    return Err(
                        "validation ledger run.start must mint run_id equal to event_id".into(),
                    );
                }
                if self.enriches.is_some() || self.supersedes.is_some() || self.reason.is_some() {
                    return Err(
                        "validation ledger run.start carries a later-event reference".into(),
                    );
                }
            }
            EventType::RunResult => {
                self.commit
                    .as_deref()
                    .ok_or_else(|| "validation ledger run.result omitted commit".to_string())?;
                if self.outcome.is_none() {
                    return Err("validation ledger run.result omitted outcome".into());
                }
                if self.enriches.is_some() || self.supersedes.is_some() || self.reason.is_some() {
                    return Err(
                        "validation ledger run.result carries a later-event reference".into(),
                    );
                }
            }
            EventType::RunEnrich => {
                if !self.enriches.as_deref().is_some_and(nonempty) {
                    return Err("validation ledger run.enrich omitted enriches".into());
                }
                if self.supersedes.is_some() || self.reason.is_some() {
                    return Err("validation ledger run.enrich carries correction fields".into());
                }
            }
            EventType::RunCorrect => {
                if !self.supersedes.as_deref().is_some_and(nonempty) {
                    return Err("validation ledger run.correct omitted supersedes".into());
                }
                if !self.reason.as_deref().is_some_and(nonempty) {
                    return Err("validation ledger run.correct omitted reason".into());
                }
                if self.enriches.is_some() {
                    return Err("validation ledger run.correct carries enriches".into());
                }
            }
            EventType::RunAnnotate => {
                if self.enriches.is_some() || self.supersedes.is_some() {
                    return Err(
                        "validation ledger run.annotate carries a value-changing reference".into(),
                    );
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> Value {
        serde_json::json!({
            "schema": SCHEMA,
            "event_id": "devbig014-1-2",
            "event_type": "run.result",
            "emitted_at": "2026-08-28T03:30:00Z",
            "team": "hermit",
            "host": "devbig014",
            "run_id": "devbig014-1-2",
            "producer": {
                "source": "observed",
                "tool": "ci-hub/validate-run",
                "tool_version": "7"
            },
            "commit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "outcome": "pass",
            "legacy_row": {
                "schema_version": 7,
                "commit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "result": "pass"
            },
            "future_optional_payload": {"retained": true}
        })
    }

    #[test]
    fn every_envelope_field_is_required_by_name() {
        for field in [
            "schema",
            "event_id",
            "event_type",
            "emitted_at",
            "team",
            "host",
            "run_id",
            "producer",
        ] {
            let mut value = event();
            value.as_object_mut().unwrap().remove(field);
            let error = LedgerEvent::parse(&serde_json::to_vec(&value).unwrap())
                .expect_err("an incomplete envelope must be refused");
            assert!(error.contains(field), "{field}: {error}");
        }

        for field in ["source", "tool", "tool_version"] {
            let mut value = event();
            value["producer"].as_object_mut().unwrap().remove(field);
            let error = LedgerEvent::parse(&serde_json::to_vec(&value).unwrap())
                .expect_err("an incomplete producer identity must be refused");
            assert!(error.contains(field), "producer.{field}: {error}");
        }
    }

    #[test]
    fn version_is_read_before_the_version_specific_shape() {
        let error = LedgerEvent::parse(br#"{"schema":"validate-ledger/v2"}"#)
            .expect_err("a newer envelope must be refused");
        assert_eq!(
            error,
            "unsupported validation ledger event schema \"validate-ledger/v2\""
        );
    }

    #[test]
    fn optional_payload_is_preserved_and_type_specific_fields_are_checked() {
        let event = LedgerEvent::parse(&serde_json::to_vec(&event()).unwrap()).unwrap();
        event.validate().unwrap();
        assert_eq!(
            event.extra.get("future_optional_payload"),
            Some(&serde_json::json!({"retained": true}))
        );

        let mut enrich = self::event();
        enrich["event_type"] = serde_json::json!("run.enrich");
        enrich.as_object_mut().unwrap().remove("outcome");
        enrich.as_object_mut().unwrap().remove("commit");
        let enrich = LedgerEvent::parse(&serde_json::to_vec(&enrich).unwrap()).unwrap();
        assert_eq!(
            enrich.validate().unwrap_err(),
            "validation ledger run.enrich omitted enriches"
        );

        let mut correction = self::event();
        correction["event_type"] = serde_json::json!("run.correct");
        correction.as_object_mut().unwrap().remove("outcome");
        correction.as_object_mut().unwrap().remove("commit");
        correction["supersedes"] = serde_json::json!("devbig014-1-1");
        let correction = LedgerEvent::parse(&serde_json::to_vec(&correction).unwrap()).unwrap();
        assert_eq!(
            correction.validate().unwrap_err(),
            "validation ledger run.correct omitted reason"
        );

        let mut result = self::event();
        result.as_object_mut().unwrap().remove("commit");
        let result = LedgerEvent::parse(&serde_json::to_vec(&result).unwrap()).unwrap();
        assert_eq!(
            result.validate().unwrap_err(),
            "validation ledger run.result omitted commit"
        );

        let mut bad_timestamp = self::event();
        bad_timestamp["emitted_at"] = serde_json::json!("2026-99-99T03:30:00Z");
        let bad_timestamp =
            LedgerEvent::parse(&serde_json::to_vec(&bad_timestamp).unwrap()).unwrap();
        assert_eq!(
            bad_timestamp.validate().unwrap_err(),
            "validation ledger event emitted_at is not RFC3339 UTC"
        );
    }
}
