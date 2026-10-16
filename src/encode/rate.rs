//! Outer rate loop: the driver's VBR overshoots on screen content, so the bitrate we request is
//! scaled down until the delivered rate over each window matches the target.

use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(1);
const MIN_TRIM: f64 = 0.25;

#[derive(Debug)]
pub struct RateTrim {
    target: u64,
    trim: f64,
    window_start: Option<Instant>,
    window_bytes: u64,
    /// Target integrated over the window, so target changes mid-window are weighed correctly.
    window_target_bits: f64,
    last: Option<Instant>,
}

impl RateTrim {
    pub fn new(target: u64) -> Self {
        Self {
            target,
            trim: 1.0,
            window_start: None,
            window_bytes: 0,
            window_target_bits: 0.0,
            last: None,
        }
    }

    /// Keeps the measured correction: the driver's overshoot doesn't depend on the target, and
    /// RTCP rate control changes the target several times a second.
    pub fn set_target(&mut self, target: u64) {
        self.target = target;
    }

    /// Bitrate to ask the driver for.
    pub fn requested(&self) -> u64 {
        (self.target as f64 * self.trim) as u64
    }

    /// Records `bytes` delivered at `now`. Returns true when `requested()` changed enough to resend.
    pub fn record(&mut self, bytes: usize, now: Instant) -> bool {
        let start = *self.window_start.get_or_insert(now);
        let dt = self.last.map_or(Duration::ZERO, |l| now.duration_since(l));
        self.last = Some(now);
        self.window_target_bits += self.target as f64 * dt.as_secs_f64();
        self.window_bytes += bytes as u64;
        if now.duration_since(start) < WINDOW {
            return false;
        }
        let ratio = self.window_target_bits / (self.window_bytes as f64 * 8.0).max(1.0);
        (
            self.window_start,
            self.window_bytes,
            self.window_target_bits,
        ) = (Some(now), 0, 0.0);
        let old = self.trim;
        if ratio < 0.95 {
            // Overshoot: correct most of the way at once.
            self.trim = (self.trim * ratio.powf(0.8)).max(MIN_TRIM);
        } else if ratio > 1.1 && self.trim < 1.0 {
            // Undershoot after an earlier cut (content got easier): relax slowly back to 1.
            self.trim = (self.trim * ratio.min(1.25)).min(1.0);
        }
        (self.trim - old).abs() > 0.03
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converges_on_overshoot() {
        // Encoder that delivers 1.3x whatever it's asked for.
        let mut r = RateTrim::new(10_000_000);
        let mut now = Instant::now();
        let mut delivered = 0.0;
        for _ in 0..10 {
            for _ in 0..60 {
                now += Duration::from_micros(16_667);
                let bytes = (r.requested() as f64 * 1.3 / 8.0 / 60.0) as usize;
                r.record(bytes, now);
                delivered = r.requested() as f64 * 1.3;
            }
        }
        assert!((delivered / 10_000_000.0 - 1.0).abs() < 0.1, "{delivered}");
    }

    #[test]
    fn converges_with_target_churn() {
        let mut r = RateTrim::new(10_000_000);
        let mut now = Instant::now();
        let mut sent = 0.0;
        for i in 0..600 {
            if i % 12 == 0 {
                r.set_target(if (i / 12) % 2 == 0 {
                    12_000_000
                } else {
                    8_000_000
                });
            }
            now += Duration::from_micros(16_667);
            let bytes = r.requested() as f64 * 1.3 / 8.0 / 60.0;
            if i >= 300 {
                sent += bytes;
            }
            r.record(bytes as usize, now);
        }
        let mbps = sent * 8.0 / 5.0;
        assert!((mbps / 10_000_000.0 - 1.0).abs() < 0.1, "{mbps}");
    }

    #[test]
    fn easy_content_does_not_raise_above_target() {
        let mut r = RateTrim::new(10_000_000);
        let mut now = Instant::now();
        for _ in 0..300 {
            now += Duration::from_micros(16_667);
            r.record(1000, now);
        }
        assert_eq!(r.requested(), 10_000_000);
    }
}
