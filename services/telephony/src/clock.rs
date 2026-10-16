//! Time source of every validity decision. Authorization evaluates the SERVER clock at
//! decision time (never a consumer-supplied time); tests drive a manual clock instead of
//! sleeping. Timestamps are truncated to microseconds, the precision PostgreSQL
//! `TIMESTAMPTZ` stores, so in-memory and persisted values compare exactly.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, SubsecRound, Utc};

pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> DateTime<Utc>;
}

pub type SharedClock = Arc<dyn Clock>;

/// Wall clock used by the server.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now().trunc_subsecs(6)
    }
}

/// Manually driven clock for deterministic tests. Never wired by the server.
#[derive(Debug, Clone)]
pub struct ManualClock(Arc<Mutex<DateTime<Utc>>>);

impl ManualClock {
    pub fn new(start: DateTime<Utc>) -> Self {
        Self(Arc::new(Mutex::new(start.trunc_subsecs(6))))
    }

    pub fn set(&self, at: DateTime<Utc>) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = at.trunc_subsecs(6);
    }

    pub fn advance(&self, by: Duration) {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        *guard = (*guard + by).trunc_subsecs(6);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}
