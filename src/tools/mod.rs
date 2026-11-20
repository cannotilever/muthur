//! Tools: the registry sent to the model, its validation, and the code that runs calls.

use crate::llm::ToolSpec;
pub mod deny;
pub mod facts;
pub mod fixed;
pub mod netgate;
pub mod registry;
pub mod shell;

pub use fixed::{SnapshotSlot, ToolOutcome, format_snapshot};
pub use registry::*;

use serde_json::{Value, json};

pub(crate) fn spec(name: &str, description: &str, properties: Value) -> ToolSpec {
    let required: Vec<&str> = properties.as_object().map(|o| o.keys().map(String::as_str).collect()).unwrap_or_default();
    ToolSpec {
        name: name.into(),
        description: description.into(),
        schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
    }
}
