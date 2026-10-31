//! Shared token-bucket rate limiter for download connections.
//!
//! One limiter per task: every range worker draws from the same bucket, so the
//! configured limit applies to the whole task, not per connection.

use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Throttle {
    limit_bps: u64,
    budget: f64,
    last: Instant,
}

impl Throttle {
    pub fn new(limit_bps: u64, now: Instant) -> Self {
        Self {
            limit_bps,
            budget: 0.0,
            last: now,
        }
    }

    /// Change the rate for the rest of the task; accumulated credit is kept.
    pub fn set_limit(&mut self, limit_bps: u64) {
        self.limit_bps = limit_bps;
    }

    /// Bytes that may be consumed right now without sleeping.
    pub fn delay_for(&mut self, bytes: usize, now: Instant) -> Duration {
        if self.limit_bps == 0 || bytes == 0 {
            return Duration::ZERO;
        }
        let limit = self.limit_bps as f64;
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        // Refill, capped at one second of credit so idle time does not build
        // an unbounded burst. Budget may go negative to smooth bursts.
        self.budget = (self.budget + elapsed * limit).min(limit);
        let bytes = bytes as f64;
        if self.budget >= bytes {
            self.budget -= bytes;
            return Duration::ZERO;
        }
        let deficit = bytes - self.budget;
        self.budget -= bytes;
        Duration::from_secs_f64(deficit / limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instant_at(seconds: u64) -> Instant {
        // A fixed reference point; only deltas matter.
        Instant::now() - Duration::from_secs(3600) + Duration::from_secs(seconds)
    }

    #[test]
    fn unlimited_when_limit_is_zero() {
        let mut throttle = Throttle::new(0, instant_at(0));
        assert_eq!(throttle.delay_for(1 << 20, instant_at(1)), Duration::ZERO);
    }

    #[test]
    fn first_burst_is_delayed_by_deficit() {
        let mut throttle = Throttle::new(1000, instant_at(0));
        // 1000 B/s and no credit yet: the first 1000 bytes must wait ~1 s.
        let delay = throttle.delay_for(1000, instant_at(0));
        assert!(delay >= Duration::from_millis(900), "got {delay:?}");
    }

    #[test]
    fn refills_over_time() {
        let mut throttle = Throttle::new(1000, instant_at(0));
        let delay = throttle.delay_for(500, instant_at(0));
        assert!(
            delay >= Duration::from_millis(490) && delay <= Duration::from_millis(500),
            "got {delay:?}"
        );
        // One second later a full second of credit is available.
        assert_eq!(throttle.delay_for(500, instant_at(1)), Duration::ZERO);
    }

    #[test]
    fn credit_is_capped_at_one_second() {
        let mut throttle = Throttle::new(1000, instant_at(0));
        throttle.delay_for(1, instant_at(0));
        // 60 s of idle time still only buys 1 s of credit (1000 bytes).
        assert_eq!(throttle.delay_for(1000, instant_at(60)), Duration::ZERO);
        let delay = throttle.delay_for(1000, instant_at(60));
        assert!(delay >= Duration::from_millis(900), "got {delay:?}");
    }

    #[test]
    fn spending_without_sleeping_accumulates_debt() {
        let mut throttle = Throttle::new(1000, instant_at(0));
        let first = throttle.delay_for(1000, instant_at(0));
        let second = throttle.delay_for(1000, instant_at(0));
        assert!(
            second > first,
            "second wait must grow: {first:?} vs {second:?}"
        );
    }

    /// Simulate a caller that sleeps exactly as advised, advancing the clock
    /// by the returned delay after each chunk.
    fn transfer(mut throttle: Throttle, chunk: usize, chunks: usize) -> Duration {
        let mut clock = instant_at(0);
        let mut total = Duration::ZERO;
        for _ in 0..chunks {
            let delay = throttle.delay_for(chunk, clock);
            total += delay;
            clock += delay;
        }
        total
    }

    #[test]
    fn average_rate_matches_the_limit() {
        // 10 x 1000 bytes at 1000 B/s: ~10 s in total, never less than 9 s.
        let total = transfer(Throttle::new(1000, instant_at(0)), 1000, 10);
        assert!(total >= Duration::from_millis(9000), "got {total:?}");
        assert!(total <= Duration::from_millis(11_000), "got {total:?}");
    }

    #[test]
    fn limit_can_change_mid_transfer() {
        let mut throttle = Throttle::new(1000, instant_at(0));
        // Spend the first second's credit at 1 KB/s, then raise the limit.
        throttle.delay_for(1000, instant_at(0));
        throttle.set_limit(2000);
        assert_eq!(throttle.delay_for(1000, instant_at(1)), Duration::ZERO);
    }

    #[test]
    fn zero_byte_reads_cost_nothing() {
        let mut throttle = Throttle::new(1000, instant_at(0));
        assert_eq!(throttle.delay_for(0, instant_at(0)), Duration::ZERO);
        assert_eq!(throttle.delay_for(0, instant_at(30)), Duration::ZERO);
    }
}
