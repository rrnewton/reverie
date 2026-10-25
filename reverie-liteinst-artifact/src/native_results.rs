// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

//! Untrusted normal build-report data and exact expected-output joins.
//!
//! This reads the concrete `buck2_build_api::build::build_report` JSON shape.
//! Wire fields follow its `build_report.rs` at source revision
//! `13f223c4320d5a78da497e7dde6c25ab2a728eec`; unknown fields are refused.
//! Parsing or joining records does not authenticate an invocation, successful
//! codegen, action keys, source/dependency inputs, materialization, or file bytes.
//! In particular, a successful DEFAULT product can be metadata, and provider
//! artifact information is not a per-path digest manifest. An owning integration
//! must independently bind the normal command/action/result and close real files.
//!
//! The CLI CommandResult protobuf alone does not establish its generated serde
//! wrapper. This module does not guess a final response/event JSON representation.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::marker::PhantomData;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::de::Error;
use serde::de::MapAccess;
use serde::de::Visitor;
use serde_json::Value;

type Result<T> = std::result::Result<T, String>;

/// Bounds parsing of untrusted retained report bytes, before allocating the tree.
pub const MAX_REPORT_BYTES: usize = 64 * 1024 * 1024;

// The ordinary map deserializer overwrites duplicate keys. Target,
// configuration and provider identities must never select the last such value.
fn unique_map<'de, D, T>(deserializer: D) -> std::result::Result<BTreeMap<String, T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Unique<T>(PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for Unique<T> {
        type Value = BTreeMap<String, T>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an object with unique keys")
        }

        fn visit_map<M: MapAccess<'de>>(
            self,
            mut entries: M,
        ) -> std::result::Result<Self::Value, M::Error> {
            let mut result = BTreeMap::new();
            loop {
                let entry = entries.next_entry::<String, T>()?;
                let Some((key, value)) = entry else {
                    break;
                };
                if result.insert(key.clone(), value).is_some() {
                    return Err(M::Error::custom(format!("duplicate map key: {key}")));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Unique(PhantomData))
}

fn optional_unique_map<'de, D, T>(
    deserializer: D,
) -> std::result::Result<Option<BTreeMap<String, T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct OptionalUnique<T>(PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for OptionalUnique<T> {
        type Value = Option<BTreeMap<String, T>>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("null or an object with unique keys")
        }

        fn visit_none<E: Error>(self) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: Error>(self) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D: Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> std::result::Result<Self::Value, D::Error> {
            unique_map(deserializer).map(Some)
        }
    }
    deserializer.deserialize_option(OptionalUnique(PhantomData))
}

/// Source-defined report outcomes; neither SUCCESS nor parse success is authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BuildOutcome {
    Success,
    Fail,
    Canceled,
    InProgress,
}

/// A configured entry. Diagnostic/metric fields are retained without interpretation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredResult {
    pub success: BuildOutcome,
    pub errors: Vec<Value>,
    #[serde(deserialize_with = "unique_map")]
    pub outputs: BTreeMap<String, Vec<String>>,
    #[serde(deserialize_with = "unique_map")]
    pub other_outputs: BTreeMap<String, Vec<String>>,
    pub configured_graph_size: Option<u64>,
    /// Provider-keyed metadata only: multiple outputs can overwrite this entry.
    #[serde(
        default,
        deserialize_with = "unique_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub artifact_info: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_graph_sketch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_metrics: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_analysis_memory_sketch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_analysis_memory_sketch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_load_memory_sketch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_graph_sketch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_count_sketch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_size_sketch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_count_sketch_cardinality: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_size_sketch_cardinality: Option<f64>,
}

/// An exact target-label entry; compatibility fields never replace `configured`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetResult {
    #[serde(deserialize_with = "unique_map")]
    pub configured: BTreeMap<String, ConfiguredResult>,
    pub errors: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_project_relative_path: Option<String>,
    // Optional flattened Buck1 compatibility fields, present only when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<BuildOutcome>,
    #[serde(
        default,
        deserialize_with = "optional_unique_map",
        skip_serializing_if = "Option::is_none"
    )]
    pub outputs: Option<BTreeMap<String, Vec<String>>>,
    #[serde(
        default,
        deserialize_with = "optional_unique_map",
        skip_serializing_if = "Option::is_none"
    )]
    pub other_outputs: Option<BTreeMap<String, Vec<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_graph_size: Option<u64>,
}

/// Plain caller-constructible data, never a verified-source or build token.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildReport {
    pub trace_id: String,
    pub project_root: String,
    pub success: bool,
    pub truncated: bool,
    #[serde(deserialize_with = "unique_map")]
    pub results: BTreeMap<String, TargetResult>,
    #[serde(deserialize_with = "unique_map")]
    pub failures: BTreeMap<String, String>,
    #[serde(deserialize_with = "unique_map")]
    pub strings: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_metrics: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_configured_graph_sketch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_category: Option<String>,
}

fn named(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.contains('\0') {
        return Err("empty or NUL-containing identity".into());
    }
    Ok(())
}

fn relative_path(value: &str) -> Result<()> {
    if value.contains('\0')
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!("not a normalized project-relative path: {value}"));
    }
    Ok(())
}

fn project_root(value: &str) -> Result<()> {
    if value == "/" {
        return Ok(());
    }
    let relative = value
        .strip_prefix('/')
        .ok_or("project root is not absolute")?;
    relative_path(relative).map_err(|_| "project root is not normalized".into())
}

fn output_paths(paths: &[String]) -> Result<BTreeSet<&str>> {
    let mut unique = BTreeSet::new();
    for path in paths {
        relative_path(path)?;
        if !unique.insert(path.as_str()) {
            return Err(format!("duplicate output path: {path}"));
        }
    }
    Ok(unique)
}

fn output_groups(groups: &BTreeMap<String, Vec<String>>) -> Result<()> {
    for (provider, paths) in groups {
        // Names remain opaque and exact; DEFAULT is never aliased to "".
        named(provider)?;
        output_paths(paths)?;
    }
    Ok(())
}

impl BuildReport {
    fn validate_shape(&self) -> Result<()> {
        named(&self.trace_id)?;
        project_root(&self.project_root)?;
        for (target, entry) in &self.results {
            named(target)?;
            for (configuration, configured) in &entry.configured {
                named(configuration)?;
                output_groups(&configured.outputs)?;
                output_groups(&configured.other_outputs)?;
            }
            if let Some(outputs) = &entry.outputs {
                output_groups(outputs)?;
            }
            if let Some(outputs) = &entry.other_outputs {
                output_groups(outputs)?;
            }
        }
        Ok(())
    }

    // Shared by the report join and a present embedded report in the final
    // response. This checks outcomes only, never source/action authority.
    pub(super) fn require_complete_success(&self) -> Result<()> {
        if !self.success
            || self.truncated
            || !self.failures.is_empty()
            || self.error_category.is_some()
        {
            return Err("report is failed, truncated or contains failures".into());
        }
        for entry in self.results.values() {
            if !entry.errors.is_empty()
                || entry.success.is_some_and(|s| s != BuildOutcome::Success)
                || entry.configured.values().any(|configured| {
                    configured.success != BuildOutcome::Success || !configured.errors.is_empty()
                })
            {
                return Err(
                    "report contains failed/incomplete configured results or errors".into(),
                );
            }
        }
        Ok(())
    }
}

/// Parse the concrete report only. Failed reports remain inspectable data.
pub fn parse_build_report(bytes: &[u8]) -> Result<BuildReport> {
    if bytes.len() > MAX_REPORT_BYTES {
        return Err("normal build report exceeds byte bound".into());
    }
    let value = super::native_wire::parse(bytes, MAX_REPORT_BYTES)?;
    let report: BuildReport =
        serde_json::from_value(value).map_err(|e| format!("normal build report JSON: {e}"))?;
    report.validate_shape()?;
    Ok(report)
}

/// An independently expected whole provider output group, not discovered by suffix.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedOutput {
    pub role: String,
    pub target: String,
    pub configuration: String,
    pub provider: String,
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportExpectation {
    pub trace_id: String,
    pub project_root: PathBuf,
    pub outputs: Vec<ExpectedOutput>,
}

/// Exact joined descriptions only. Absolute paths are lexical, not opened/resolved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedOutput {
    pub role: String,
    pub target: String,
    pub configuration: String,
    pub provider: String,
    pub paths: Vec<String>,
    pub absolute_paths: Vec<PathBuf>,
}

/// Require a complete successful report for all independent expected groups.
///
/// This does not check process exit, final BuildResponse, actions, codegen kind,
/// source/dependency mappings, materialization, hashes or transaction close.
/// It never falls back to unconfigured results or guesses a provider/path. Extra
/// groups remain in the parsed report; each selected group's full path set must
/// equal its expectation. Report order is retained, but output groups are sets.
pub fn join_build_report(
    report: &BuildReport,
    expected: &ReportExpectation,
) -> Result<Vec<SelectedOutput>> {
    report.validate_shape()?;
    named(&expected.trace_id)?;
    if report.trace_id != expected.trace_id {
        return Err("report trace/project root differs from independent expectation".into());
    }
    join_build_report_outputs(report, &expected.project_root, &expected.outputs)
}

// The ordinary owned-build surface has no independent request trace. This
// private projection joins the actual independent expectations without claiming
// that the report's own trace authenticates itself. Both paths remain data only.
pub(super) fn join_build_report_outputs(
    report: &BuildReport,
    expected_root: &Path,
    expected_outputs: &[ExpectedOutput],
) -> Result<Vec<SelectedOutput>> {
    report.validate_shape()?;
    let root = expected_root
        .to_str()
        .ok_or("expected project root is not UTF-8")?;
    project_root(root)?;
    if report.project_root != root {
        return Err("report trace/project root differs from independent expectation".into());
    }
    report.require_complete_success()?;
    if expected_outputs.is_empty() {
        return Err("expected output roles are empty".into());
    }
    let mut roles = BTreeSet::new();
    let mut selections = BTreeSet::new();
    let mut selected_paths = BTreeSet::new();
    let mut result = Vec::new();
    for output in expected_outputs {
        for identity in [
            &output.role,
            &output.target,
            &output.configuration,
            &output.provider,
        ] {
            named(identity)?;
        }
        if !roles.insert(&output.role)
            || !selections.insert((&output.target, &output.configuration, &output.provider))
        {
            return Err("duplicate expected role or configured provider".into());
        }
        let paths = output_paths(&output.paths)?;
        if paths.is_empty() || paths.iter().any(|path| !selected_paths.insert(*path)) {
            return Err("empty or aliased expected output group".into());
        }
        let configured = report
            .results
            .get(&output.target)
            .and_then(|target| target.configured.get(&output.configuration))
            .ok_or("expected target/full configuration absent from report")?;
        let actual = configured
            .outputs
            .get(&output.provider)
            .ok_or("expected provider absent from configured report")?;
        if output_paths(actual)? != paths {
            return Err("complete output paths differ from independent expectation".into());
        }
        result.push(SelectedOutput {
            role: output.role.clone(),
            target: output.target.clone(),
            configuration: output.configuration.clone(),
            provider: output.provider.clone(),
            paths: actual.clone(),
            absolute_paths: actual.iter().map(|path| expected_root.join(path)).collect(),
        });
    }
    Ok(result)
}
