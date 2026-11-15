//! Shared GPS sample conversion and network transmission policy. No GPS collector.
use super::{Result, protocol::LocationUpdate};
use crate::{geo::Point, model::Fix};
use serde::{Deserialize, Serialize};

/// Sharing GPS samples every 5 minutes while stationary; a fix stays current a while longer.
pub const MAX_FIX_AGE_MS: i64 = 900_000;
pub const RETENTION_MS: i64 = 86_400_000;

#[derive(Clone, Copy)]
pub struct Sample {
    pub fix: Fix,
    pub speed_m_s: Option<f64>,
    pub heading_degrees: Option<f64>,
}

impl Sample {
    pub fn to_update(self, sequence: u64, now_ms: i64) -> Result<LocationUpdate> {
        if !self.fix.valid()
            || self.fix.accuracy_m > 100_000.0
            || self
                .speed_m_s
                .is_some_and(|v| !v.is_finite() || !(0.0..=1000.0).contains(&v))
            || self
                .heading_degrees
                .is_some_and(|v| !v.is_finite() || !(0.0..360.0).contains(&v))
        {
            return Err("invalid location sample".into());
        }
        let update = LocationUpdate {
            sequence,
            timestamp_ms: self.fix.timestamp_ms.try_into()?,
            latitude_e7: (self.fix.point.lat * 1e7).round() as i32,
            longitude_e7: (self.fix.point.lon * 1e7).round() as i32,
            accuracy_cm: (self.fix.accuracy_m * 100.0).round() as u32,
            speed_cm_s: self.speed_m_s.map(|v| (v * 100.0).round() as u32),
            heading_cdeg: self
                .heading_degrees
                .map(|v| ((v * 100.0).round() as u32 % 36000) as u16),
        };
        update.validate(now_ms)?;
        Ok(update)
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct UpdatePolicy {
    pub minimum_interval_ms: i64,
    pub maximum_interval_ms: i64,
    pub movement_m: f64,
}

impl Default for UpdatePolicy {
    fn default() -> Self {
        Self {
            minimum_interval_ms: 30_000,
            maximum_interval_ms: 600_000,
            movement_m: 25.0,
        }
    }
}

impl UpdatePolicy {
    pub fn validate(self) -> Result<()> {
        if self.minimum_interval_ms < 1000
            || self.maximum_interval_ms < self.minimum_interval_ms
            || self.maximum_interval_ms > RETENTION_MS
            || !self.movement_m.is_finite()
            || self.movement_m < 0.0
        {
            return Err("invalid location update policy".into());
        }
        Ok(())
    }

    pub(super) fn due(
        self,
        last: Option<(Point, i64)>,
        sample: Sample,
        now_ms: i64,
        requested: bool,
    ) -> bool {
        let Some((point, sent_at)) = last else {
            return true;
        };
        let elapsed = now_ms.saturating_sub(sent_at);
        elapsed >= self.minimum_interval_ms
            && (requested
                || elapsed >= self.maximum_interval_ms
                || point.distance(sample.fix.point) >= self.movement_m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filters_small_moves_and_requests_without_bypassing_rate_limit() {
        let fix = Fix {
            point: Point {
                lat: 40.0,
                lon: -111.0,
            },
            timestamp_ms: 1_000_000,
            accuracy_m: 5.0,
        };
        let sample = Sample {
            fix,
            speed_m_s: None,
            heading_degrees: None,
        };
        let policy = UpdatePolicy::default();
        let last = Some((fix.point, 1_000_000));
        assert!(!policy.due(last, sample, 1_001_000, true));
        assert!(!policy.due(last, sample, 1_030_000, false));
        assert!(policy.due(last, sample, 1_030_000, true));
        assert!(policy.due(last, sample, 1_600_000, false));
        let mut moved = sample;
        moved.fix.point.lat += 0.001;
        assert!(policy.due(last, moved, 1_030_000, false));
        let mut invalid = sample;
        invalid.fix.point.lat = f64::NAN;
        assert!(invalid.to_update(1, 1_000_000).is_err());
        assert!(sample.to_update(0, 1_000_000).is_err());
    }
}
