//! The validation ledger schema is owned by Hermit, which produces the rows.
//!
//! Keep this module as the parent-facing import path so the existing ci-hub
//! modules all compile against Hermit's exported types rather than a second
//! definition.

pub use hermit_manifest_plan::ledger::*;
