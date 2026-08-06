//! Per-domain memory: what happened on this origin before.
//! Powers verdicts now; tier-2 routing later.
//!
//! Superseded by the persistent DomainProfile in ghost::cache
//! for the self-improving fetch loop. Retained for reference.

#![allow(dead_code)]

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default)]
pub struct DomainMemory {
    /// A wall challenged us here at least once.
    pub challenged: bool,
    /// A cookie-warm retry succeeded here (JS-less cookie wall).
    pub warm_retry_worked: bool,
    /// Wall needs tier 2 (JS challenge seen).
    pub needs_tier2: bool,
}

pub struct DomainMap {
    map: HashMap<String, DomainMemory>,
}

impl DomainMap {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    #[allow(dead_code)] // used by MCP/verdict surface
    pub fn get(&self, domain: &str) -> DomainMemory {
        self.map.get(domain).copied().unwrap_or_default()
    }

    pub fn update(&mut self, domain: &str, f: impl FnOnce(&mut DomainMemory)) {
        f(self.map.entry(domain.to_string()).or_default());
    }
}

impl Default for DomainMap {
    fn default() -> Self {
        Self::new()
    }
}
