//! Time source for every validity decision (credential expiry, rotation overlap,
//! rate windows, challenge expiry). Timestamps are truncated to microseconds, the precision PostgreSQL
//! `TIMESTAMPTZ` stores, so in-memory and persisted values compare exactly.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, SubsecRound, Utc};

pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> DateTime<Utc>;
}

pub type SharedClock = Arc<dyn Clock>;

/// Wall clock used by the server and the admin CLI.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now().trunc_subsecs(6)
    }
}

/// Manually driven clock for deterministic tests (expiry, overlap, rate windows, challenge expiry). Never
/// wired by the server or CLI composition roots.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_advances_and_truncates_to_micros() {
        let start = DateTime::parse_from_rfc3339("2030-01-01T00:00:00.123456789Z")
            .unwrap()
            .with_timezone(&Utc);
        let clock = ManualClock::new(start);
        assert_eq!(clock.now().timestamp_subsec_nanos(), 123_456_000);
        clock.advance(Duration::minutes(5));
        assert_eq!(clock.now() - clock.now(), Duration::zero());
        assert_eq!(clock.now(), start.trunc_subsecs(6) + Duration::minutes(5));
    }

    #[test]
    fn system_clock_has_microsecond_precision() {
        assert_eq!(SystemClock.now().timestamp_subsec_nanos() % 1_000, 0);
    }
}
