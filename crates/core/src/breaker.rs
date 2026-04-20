use crate::CircuitBreakerConfig;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Reusable circuit breaker. Open after N consecutive failures; half-open after recovery window.
pub struct CircuitBreaker {
    failures: AtomicU32,
    opened_at_ms: AtomicU64,
    threshold: u32,
    recovery_ms: u64,
    metric_open: &'static str,
}

impl CircuitBreaker {
    pub fn new(config: &CircuitBreakerConfig, metric_open: &'static str) -> Self {
        Self {
            failures: AtomicU32::new(0),
            opened_at_ms: AtomicU64::new(0),
            threshold: config.failure_threshold,
            recovery_ms: config.recovery_ms,
            metric_open,
        }
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    pub fn is_open(&self) -> bool {
        let opened_at = self.opened_at_ms.load(Ordering::Relaxed);
        if opened_at == 0 {
            return false;
        }
        let elapsed = Self::now_ms().saturating_sub(opened_at);
        elapsed < self.recovery_ms
    }

    pub fn record_success(&self) {
        self.failures.store(0, Ordering::Relaxed);
        self.opened_at_ms.store(0, Ordering::Relaxed);
    }

    pub fn record_failure(&self) {
        let prev = self.failures.fetch_add(1, Ordering::Relaxed);
        if prev + 1 >= self.threshold {
            let _ = self.opened_at_ms.compare_exchange(
                0,
                Self::now_ms(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
            metrics::counter!(self.metric_open).increment(1);
        }
    }
}
