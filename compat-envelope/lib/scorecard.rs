//! Shared compatibility scorecard row shape.
//!
//! Every Rust producer and reader imports this module. The JSON schema is a
//! checked projection for Python and shell callers; it is not a second source
//! of column names or order.

#![allow(dead_code)] // Each rust-script uses a different projection of the shared row.

use std::collections::BTreeSet;

macro_rules! scorecard_columns {
    ($($variant:ident => $field:ident => $name:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub enum ScorecardColumn {
            $($variant),+
        }

        impl ScorecardColumn {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),+
                }
            }

            pub fn parse(name: &str) -> Option<Self> {
                match name {
                    $($name => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }

        #[derive(Clone, Debug, Default, Eq, PartialEq)]
        pub struct ScorecardRow {
            $(pub $field: String),+
        }

        impl ScorecardRow {
            pub fn get(&self, column: ScorecardColumn) -> &str {
                match column {
                    $(ScorecardColumn::$variant => &self.$field),+
                }
            }

            pub fn set(&mut self, column: ScorecardColumn, value: String) {
                match column {
                    $(ScorecardColumn::$variant => self.$field = value),+
                }
            }
        }
    };
}

scorecard_columns! {
    RunId => run_id => "run_id",
    RunUtc => run_utc => "run_utc",
    HermitSha => hermit_sha => "hermit_sha",
    ReverieSha => reverie_sha => "reverie_sha",
    Dirty => dirty => "dirty",
    RunMode => run_mode => "run_mode",
    Lane => lane => "lane",
    Bucket => bucket => "bucket",
    TestId => test_id => "test_id",
    TestMode => test_mode => "test_mode",
    Backend => backend => "backend",
    CellState => cell_state => "cell_state",
    Outcome => outcome => "outcome",
    Deterministic => deterministic => "deterministic",
    StdoutParity => stdout_parity => "stdout_parity",
    OutputHash => output_hash => "output_hash",
    DurationMs => duration_ms => "duration_ms",
    MaxRssKb => max_rss_kb => "max_rss_kb",
    Reason => reason => "reason",
    VerifyCompare => verify_compare => "verify_compare",
    BitwiseParity => bitwise_parity => "bitwise_parity",
    ComparedLogMessages => compared_log_messages => "compared_log_messages",
    Tier => tier => "tier",
    LegacyParityUnqualified => legacy_parity_unqualified => "legacy_parity_unqualified",
    RefOutputHash => ref_output_hash => "ref_output_hash",
    ParityComparator => parity_comparator => "parity_comparator",
    ParityTier => parity_tier => "parity_tier",
    ProfileFlags => profile_flags => "profile_flags",
    PopulationId => population_id => "population_id",
    SelectedCount => selected_count => "selected_count",
    ExecutedCount => executed_count => "executed_count",
    EvidenceCount => evidence_count => "evidence_count",
    ComparisonTier => comparison_tier => "comparison_tier",
    ExitCodeParity => exit_code_parity => "exit_code_parity",
    DetlogParity => detlog_parity => "detlog_parity",
    OracleVerdict => oracle_verdict => "oracle_verdict",
    StackParity => stack_parity => "stack_parity",
    HeapParity => heap_parity => "heap_parity",
    StackHash => stack_hash => "stack_hash",
    HeapHash => heap_hash => "heap_hash",
    ToolCountParity => tool_count_parity => "tool_count_parity",
    AbsenceReason => absence_reason => "absence_reason",
    CandidateSites => candidate_sites => "candidate_sites",
    MappedSites => mapped_sites => "mapped_sites",
    ReachState => reach_state => "reach_state",
    EvaluationState => evaluation_state => "evaluation_state",
    EvaluationReason => evaluation_reason => "evaluation_reason",
    EvaluationEvidence => evaluation_evidence => "evaluation_evidence",
    SourceLegacyModes => source_legacy_modes => "source_legacy_modes",
    SourceLegacyModeCount => source_legacy_mode_count => "source_legacy_mode_count",
    SourceLegacyCoverage => source_legacy_coverage => "source_legacy_coverage",
    ProgramClasses => program_classes => "program_classes",
    ConcurrencyKind => concurrency_kind => "concurrency_kind",
    ProgramClassEvidence => program_class_evidence => "program_class_evidence",
    ClassificationId => classification_id => "classification_id",
    HarnessForm => harness_form => "harness_form",
    CorpusId => corpus_id => "corpus_id",
    ParityExercised => parity_exercised => "parity_exercised",
    BackendEngaged => backend_engaged => "backend_engaged",
    NativeOutputHash => native_output_hash => "native_output_hash",
}

const CORE: &[ScorecardColumn] = &[
    ScorecardColumn::RunId,
    ScorecardColumn::RunUtc,
    ScorecardColumn::HermitSha,
    ScorecardColumn::ReverieSha,
    ScorecardColumn::Dirty,
    ScorecardColumn::RunMode,
    ScorecardColumn::Lane,
    ScorecardColumn::Bucket,
    ScorecardColumn::TestId,
    ScorecardColumn::TestMode,
    ScorecardColumn::Backend,
    ScorecardColumn::CellState,
    ScorecardColumn::Outcome,
    ScorecardColumn::Deterministic,
    ScorecardColumn::StdoutParity,
    ScorecardColumn::OutputHash,
    ScorecardColumn::DurationMs,
    ScorecardColumn::MaxRssKb,
    ScorecardColumn::Reason,
    ScorecardColumn::VerifyCompare,
    ScorecardColumn::BitwiseParity,
    ScorecardColumn::ComparedLogMessages,
    ScorecardColumn::Tier,
    ScorecardColumn::LegacyParityUnqualified,
    ScorecardColumn::RefOutputHash,
    ScorecardColumn::ParityComparator,
    ScorecardColumn::ParityTier,
    ScorecardColumn::ProfileFlags,
    ScorecardColumn::PopulationId,
    ScorecardColumn::SelectedCount,
    ScorecardColumn::ExecutedCount,
    ScorecardColumn::EvidenceCount,
    ScorecardColumn::ComparisonTier,
    ScorecardColumn::ExitCodeParity,
    ScorecardColumn::DetlogParity,
    ScorecardColumn::OracleVerdict,
    ScorecardColumn::StackParity,
    ScorecardColumn::HeapParity,
];

const REVERIE_EXTRAS: &[ScorecardColumn] = &[
    ScorecardColumn::ToolCountParity,
    ScorecardColumn::AbsenceReason,
];
const E9PATCH_EXTRAS: &[ScorecardColumn] = &[
    ScorecardColumn::CandidateSites,
    ScorecardColumn::MappedSites,
    ScorecardColumn::ReachState,
];
const REAL_PROGRAM_EXTRAS: &[ScorecardColumn] = &[
    ScorecardColumn::EvaluationState,
    ScorecardColumn::EvaluationReason,
    ScorecardColumn::EvaluationEvidence,
    ScorecardColumn::SourceLegacyModes,
    ScorecardColumn::SourceLegacyModeCount,
    ScorecardColumn::SourceLegacyCoverage,
    ScorecardColumn::ProgramClasses,
    ScorecardColumn::ConcurrencyKind,
    ScorecardColumn::ProgramClassEvidence,
    ScorecardColumn::ClassificationId,
    ScorecardColumn::HarnessForm,
    ScorecardColumn::CorpusId,
];
const RUN_EXTRAS: &[ScorecardColumn] = &[
    ScorecardColumn::StackHash,
    ScorecardColumn::HeapHash,
    ScorecardColumn::ParityExercised,
    ScorecardColumn::BackendEngaged,
    ScorecardColumn::NativeOutputHash,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScorecardVariant {
    Scorecard,
    FullCorpus,
    RealPrograms,
    E9patch,
    Dbi,
    Reverie,
    Run,
}

impl ScorecardVariant {
    pub const PUBLISHED: &'static [Self] = &[
        Self::Scorecard,
        Self::FullCorpus,
        Self::RealPrograms,
        Self::E9patch,
        Self::Dbi,
        Self::Reverie,
    ];

    pub const fn file_name(self) -> &'static str {
        match self {
            Self::Scorecard => "scorecard.csv",
            Self::FullCorpus => "fullcorpus-scorecard.csv",
            Self::RealPrograms => "real-programs-scorecard.csv",
            Self::E9patch => "e9patch-scorecard.csv",
            Self::Dbi => "dbi-scorecard.csv",
            Self::Reverie => "reverie-scorecard.csv",
            Self::Run => "ignored/envelope/<run_id>.csv",
        }
    }

    pub const fn emitter(self) -> &'static str {
        match self {
            Self::Scorecard | Self::Run => "collect-envelope.rs",
            Self::FullCorpus => "collect-fullcorpus.sh",
            Self::RealPrograms => "ingest_real_program_matrix.py",
            Self::E9patch => "collect-e9patch-compat.rs",
            Self::Dbi => "collect-dbi-corpus.rs",
            Self::Reverie => "collect-reverie-compat.rs",
        }
    }

    pub fn for_file_name(name: &str) -> Option<Self> {
        match name {
            "scorecard.csv" | "scorecard-observations.csv" => Some(Self::Scorecard),
            "fullcorpus-scorecard.csv" => Some(Self::FullCorpus),
            "real-programs-scorecard.csv" => Some(Self::RealPrograms),
            "e9patch-scorecard.csv" => Some(Self::E9patch),
            "dbi-scorecard.csv" => Some(Self::Dbi),
            "reverie-scorecard.csv" => Some(Self::Reverie),
            _ => None,
        }
    }

    pub fn columns(self) -> Vec<ScorecardColumn> {
        let mut columns = CORE.to_vec();
        columns.extend_from_slice(match self {
            Self::Scorecard => RUN_EXTRAS,
            Self::FullCorpus | Self::Dbi => &[],
            Self::RealPrograms => REAL_PROGRAM_EXTRAS,
            Self::E9patch => E9PATCH_EXTRAS,
            Self::Reverie => REVERIE_EXTRAS,
            Self::Run => RUN_EXTRAS,
        });
        columns
    }

    pub fn header(self) -> String {
        self.columns()
            .into_iter()
            .map(ScorecardColumn::name)
            .collect::<Vec<_>>()
            .join(",")
    }
}

impl ScorecardRow {
    pub fn from_pairs<const N: usize>(pairs: [(ScorecardColumn, String); N]) -> Self {
        let mut row = Self::default();
        let mut seen = BTreeSet::new();
        for (column, value) in pairs {
            assert!(
                seen.insert(column),
                "duplicate scorecard column {:?}",
                column.name()
            );
            row.set(column, value);
        }
        row
    }

    pub fn encode(&self, variant: ScorecardVariant) -> String {
        self.encode_columns(&variant.columns())
    }

    pub fn encode_columns(&self, columns: &[ScorecardColumn]) -> String {
        columns
            .iter()
            .map(|column| csv_field(self.get(*column)))
            .collect::<Vec<_>>()
            .join(",")
    }

    pub fn parse(
        header: &[String],
        fields: &[String],
        legacy_parity: Option<ScorecardColumn>,
    ) -> Result<Self, String> {
        if fields.len() != header.len() {
            return Err(format!(
                "row has {} fields, header has {}",
                fields.len(),
                header.len()
            ));
        }
        let columns = parse_columns(header, legacy_parity)?;
        let mut row = Self::default();
        for (column, value) in columns.into_iter().zip(fields) {
            row.set(column, value.clone());
        }
        Ok(row)
    }
}

pub fn parse_columns(
    header: &[String],
    legacy_parity: Option<ScorecardColumn>,
) -> Result<Vec<ScorecardColumn>, String> {
    let mut columns = Vec::with_capacity(header.len());
    let mut seen = BTreeSet::new();
    for name in header {
        let column = if name == "parity" {
            legacy_parity
                .ok_or_else(|| "legacy `parity` column has no selected observable".to_string())?
        } else {
            ScorecardColumn::parse(name)
                .ok_or_else(|| format!("unknown scorecard column {name:?}"))?
        };
        if !seen.insert(column) {
            return Err(format!("duplicate scorecard column {:?}", column.name()));
        }
        columns.push(column);
    }
    Ok(columns)
}

pub fn split_csv_line(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == ',' {
            out.push(std::mem::take(&mut field));
        } else {
            field.push(c);
        }
    }
    if in_quotes {
        return Err("unterminated quoted CSV field".to_string());
    }
    out.push(field);
    Ok(out)
}

pub fn csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

pub fn column_names(columns: &[ScorecardColumn]) -> Vec<String> {
    columns
        .iter()
        .map(|column| column.name().to_string())
        .collect()
}

pub fn core_column_names() -> Vec<String> {
    column_names(CORE)
}

pub fn extra_column_names(variant: ScorecardVariant) -> Vec<String> {
    let columns = variant.columns();
    column_names(&columns[CORE.len()..])
}
