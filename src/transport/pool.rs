//! Per-origin connection pool. h2 conns are reused; h1 conns are one-shot
//! (servers close them unpredictably).

use std::collections::HashMap;

use super::h2::conn::H2Conn;

pub struct Pool {
    h2: HashMap<String, H2Conn>,
}

impl Pool {
    pub fn new() -> Self {
        Self { h2: HashMap::new() }
    }

    pub fn take_h2(&mut self, origin: &str) -> Option<H2Conn> {
        self.h2.remove(origin)
    }

    pub fn put_h2(&mut self, origin: &str, conn: H2Conn) {
        // Cap the pool; drop arbitrary oldest on overflow (origin churn
        // is rare in agent workloads).
        if self.h2.len() >= 64
            && let Some(k) = self.h2.keys().next().cloned()
        {
            self.h2.remove(&k);
        }
        self.h2.insert(origin.to_string(), conn);
    }
}

impl Default for Pool {
    fn default() -> Self {
        Self::new()
    }
}
