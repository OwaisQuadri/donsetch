//! DonSeTch MCP daemon — JSON-RPC 2.0 over stdio (NDJSON).
//!
//! One message per line. Requests spawn tasks; responses
//! funnel through a single writer task so lines never
//! interleave. The daemon never dies on bad input.

pub mod server;
pub mod supervisor;
pub mod tools;
