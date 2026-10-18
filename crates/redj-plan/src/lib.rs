//! Mix plan data model, JSON Schema and validation with AI-readable errors.
//!
//! A [`MixPlan`] is what a human or the AI writes: tracks in order, where
//! each one starts, how it is mixed in. [`compile`] validates it against
//! the library and turns it into a [`Timeline`], the engine's automation
//! model.

mod compile;
mod model;
mod timeline;

pub use compile::{Compiled, Issue, Severity, compile};
pub use model::*;
pub use timeline::{Clip, Lane, LanePoint, Marker, PlayheadSeg, TempoMap, TempoSeg, Timeline};

/// JSON Schema of [`MixPlan`].
pub fn plan_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(MixPlan)).expect("schema serializes")
}

/// Parses a plan from JSON, reporting syntax and type errors as an
/// [`Issue`] with the line and column in the message.
pub fn parse_plan(json: &str) -> Result<MixPlan, Issue> {
    serde_json::from_str(json).map_err(|e| Issue {
        severity: Severity::Error,
        path: String::new(),
        message: format!("invalid plan JSON: {e}"),
        hint: Some("the plan must match the MixPlan JSON Schema".into()),
    })
}
