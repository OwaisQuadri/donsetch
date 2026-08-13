//! Tool schemas for the MCP tools/list response.
//!
//! Descriptions are LLM-optimized: dense, self-contained,
//! and actionable. An agent reading only the description
//! (never our source) should know exactly when to call,
//! which params to set, and how to interpret the response.
//!
//! Schemas are GENERATED from `crate::spec::TOOLS` — the
//! single source of truth shared with the CLI. Never edit
//! schemas here; edit the spec table.

use serde_json::{Value, json};

/// Protocol versions we speak, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &[
    "2026-07-28",
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
];

pub const SERVER_NAME: &str = "donsetch";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// tools/list payload — generated from the spec table.
pub fn list() -> Value {
    json!({
        "tools": crate::spec::TOOLS.iter().map(crate::spec::mcp_schema).collect::<Vec<_>>()
    })
}

#[cfg(test)]
mod tests {
    /// Golden fixture: the generated tools/list must be
    /// byte-identical (as a Value) to the pre-refactor
    /// hand-written schema. If this fails, the spec table
    /// drifted from the shipped MCP contract.
    #[test]
    fn generated_schema_matches_fixture() {
        let fixture: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/tools_list.json"
            ))
            .expect("read fixture"),
        )
        .expect("parse fixture");
        assert_eq!(super::list(), fixture, "tools/list drifted from fixture");
    }
}
