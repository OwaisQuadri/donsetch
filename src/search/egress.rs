//! Egress pool — the rate-limit solver.
//!
//! Every request to an engine leaves through one egress
//! (direct or a residential proxy). Health is tracked per
//! (engine, egress): a 429 at Brave never burns Bing, and
//! a dead proxy never burns the direct line.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::transport::proxy::Proxy;

const BURN_COOLDOWN: Duration = Duration::from_secs(600);
const MIN_INTERVAL: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Healthy,
    Suspect,
    Burned,
}

#[derive(Debug, Clone)]
pub struct Egress {
    /// "direct" or proxy id.
    pub id: String,
    pub proxy: Option<Proxy>,
}

struct PairState {
    health: Health,
    burned_until: Option<Instant>,
    last_used: Option<Instant>,
}

pub struct EgressPool {
    egresses: Vec<Egress>,
    /// (engine, egress_id) -> state
    pairs: Mutex<HashMap<(String, String), PairState>>,
    /// Global proxy liveness (connect failures burn a proxy
    /// for ALL engines; a dead line is a dead line).
    dead: Mutex<HashMap<String, Instant>>,
}

impl EgressPool {
    pub fn new(proxies: Vec<Proxy>) -> Self {
        let mut egresses = vec![Egress {
            id: "direct".into(),
            proxy: None,
        }];
        for p in proxies {
            egresses.push(Egress {
                id: p.id(),
                proxy: Some(p),
            });
        }
        Self {
            egresses,
            pairs: Mutex::new(HashMap::new()),
            dead: Mutex::new(HashMap::new()),
        }
    }

    pub fn from_env() -> Self {
        let raw = std::env::var("DONSEEK_PROXIES").unwrap_or_default();
        let proxies = raw
            .split(',')
            .filter_map(|s| Proxy::parse(s.trim()).ok())
            .collect();
        Self::new(proxies)
    }

    /// Pick the healthiest egress for an engine. Spreading
    /// is the caller's job (pass `exclude` for egresses
    /// already assigned this query).
    pub fn pick(&self, engine: &str, exclude: &[String]) -> Option<Egress> {
        let pairs = self.pairs.lock().unwrap();
        let dead = self.dead.lock().unwrap();
        let mut best: Option<(&Egress, u8)> = None;
        for e in &self.egresses {
            if exclude.contains(&e.id) {
                continue;
            }
            // A globally dead proxy is skipped until cooldown.
            if let Some(&until) = dead.get(&e.id) {
                if until > Instant::now() {
                    continue;
                }
            }
            let score = match pairs.get(&(engine.to_string(), e.id.clone())) {
                None => 2, // unknown = optimistic
                Some(s) => match s.health {
                    Health::Healthy => 2,
                    Health::Suspect => 1,
                    Health::Burned => match s.burned_until {
                        Some(t) if t > Instant::now() => continue,
                        _ => 1, // cooldown over: probation
                    },
                },
            };
            // Prefer direct on ties (don't burn proxy bandwidth).
            let score = if e.proxy.is_none() { score + 0 } else { score };
            if best.is_none_or(|(_, s)| score > s)
                || (best.is_some_and(|(_, s)| score == s) && e.proxy.is_none())
            {
                best = Some((e, score));
            }
        }
        best.map(|(e, _)| e.clone())
    }

    /// Record a successful engine call through this egress.
    pub fn report_ok(&self, engine: &str, egress_id: &str) {
        let mut pairs = self.pairs.lock().unwrap();
        let s = pairs
            .entry((engine.to_string(), egress_id.to_string()))
            .or_insert(PairState {
                health: Health::Suspect,
                burned_until: None,
                last_used: None,
            });
        s.health = Health::Healthy;
        s.burned_until = None;
        s.last_used = Some(Instant::now());
    }

    /// Engine rejected us (429 / challenge / empty parse):
    /// burn the pair, not the engine.
    pub fn report_blocked(&self, engine: &str, egress_id: &str) {
        let mut pairs = self.pairs.lock().unwrap();
        let s = pairs
            .entry((engine.to_string(), egress_id.to_string()))
            .or_insert(PairState {
                health: Health::Suspect,
                burned_until: None,
                last_used: None,
            });
        s.health = match s.health {
            Health::Healthy => Health::Suspect,
            _ => {
                s.burned_until = Some(Instant::now() + BURN_COOLDOWN);
                Health::Burned
            }
        };
        s.last_used = Some(Instant::now());
    }

    /// The egress line itself is dead (connect failure).
    pub fn report_dead(&self, egress_id: &str) {
        if egress_id == "direct" {
            return; // direct failure = network down; don't mark
        }
        self.dead
            .lock()
            .unwrap()
            .insert(egress_id.to_string(), Instant::now() + BURN_COOLDOWN);
    }

    /// Pacing: wait so this (engine, egress) pair is not hit
    /// more than once per MIN_INTERVAL.
    pub async fn pace(&self, engine: &str, egress_id: &str) {
        let wait = {
            let pairs = self.pairs.lock().unwrap();
            pairs
                .get(&(engine.to_string(), egress_id.to_string()))
                .and_then(|s| s.last_used)
                .map(|t| MIN_INTERVAL.saturating_sub(t.elapsed()))
                .unwrap_or(Duration::ZERO)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}
