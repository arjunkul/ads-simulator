use std::fmt;

use rand::Rng;
use serde::{Serialize, Serializer};

pub const HOURS_PER_DAY: usize = 24;
pub const SECONDS_PER_HOUR: u32 = 3_600;
pub const SECONDS_PER_DAY: u32 = (HOURS_PER_DAY as u32) * SECONDS_PER_HOUR;
pub const TIMEZONE_LABEL: &str = "PST";

// Simulation calendar runs across January 1–30, 2026 in fixed PST (UTC-08:00).
const START_YEAR: u32 = 2026;
const START_MONTH: u32 = 1;

// Diurnal traffic shape: truncated split-normal peaking at 08:00 PST and
// tapering off through the afternoon so traffic dies out around 18:00 PST.
const PEAK_HOUR_PST: f64 = 8.0;
const MORNING_SIGMA_HOURS: f64 = 1.8;
const AFTERNOON_SIGMA_HOURS: f64 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PstTimestamp {
    pub day: usize,
    pub second_of_day: u32,
}

impl PstTimestamp {
    pub fn new(day: usize, second_of_day: u32) -> Self {
        debug_assert!((1..=31).contains(&day));
        debug_assert!(second_of_day < SECONDS_PER_DAY);
        Self { day, second_of_day }
    }

    pub fn date_string(day: usize) -> String {
        format!("{START_YEAR:04}-{START_MONTH:02}-{day:02}")
    }

    pub fn hour(self) -> usize {
        (self.second_of_day / SECONDS_PER_HOUR) as usize
    }

    pub fn minute(self) -> u32 {
        (self.second_of_day % SECONDS_PER_HOUR) / 60
    }

    pub fn second(self) -> u32 {
        self.second_of_day % 60
    }
}

impl fmt::Display for PstTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{START_YEAR:04}-{START_MONTH:02}-{:02}T{:02}:{:02}:{:02}-08:00",
            self.day,
            self.hour(),
            self.minute(),
            self.second(),
        )
    }
}

impl Serialize for PstTimestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

/// Sample a second of day in `[0, 86_400)` PST from a split-normal distribution
/// peaking around 08:00 PST and dying out around 18:00 PST.
pub fn sample_pst_second_of_day(rng: &mut impl Rng) -> u32 {
    let left_weight = MORNING_SIGMA_HOURS / (MORNING_SIGMA_HOURS + AFTERNOON_SIGMA_HOURS);
    loop {
        // `1.0 - rng.random::<f64>()` lies strictly in `(0.0, 1.0]`, avoiding `ln(0)`.
        let u1 = 1.0 - rng.random::<f64>();
        let u2 = rng.random::<f64>();
        let half_normal = (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos().abs();
        let hour = if rng.random::<f64>() < left_weight {
            PEAK_HOUR_PST - MORNING_SIGMA_HOURS * half_normal
        } else {
            PEAK_HOUR_PST + AFTERNOON_SIGMA_HOURS * half_normal
        };
        if (0.0..HOURS_PER_DAY as f64).contains(&hour) {
            let second = (hour * f64::from(SECONDS_PER_HOUR)).floor() as u32;
            return second.min(SECONDS_PER_DAY - 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    use super::{HOURS_PER_DAY, PstTimestamp, SECONDS_PER_DAY, sample_pst_second_of_day};

    #[test]
    fn pst_timestamp_formats_as_rfc3339_offset_minus_8() {
        let ts = PstTimestamp::new(5, 8 * 3_600 + 14 * 60 + 23);
        assert_eq!(ts.hour(), 8);
        assert_eq!(ts.minute(), 14);
        assert_eq!(ts.second(), 23);
        assert_eq!(ts.to_string(), "2026-01-05T08:14:23-08:00");
        assert_eq!(
            serde_json::to_value(ts).unwrap(),
            serde_json::json!("2026-01-05T08:14:23-08:00")
        );
    }

    #[test]
    fn diurnal_distribution_peaks_around_8am_and_dies_out_around_6pm_pst() {
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let mut hourly = [0u64; HOURS_PER_DAY];
        let samples = 200_000u64;
        for _ in 0..samples {
            let sec = sample_pst_second_of_day(&mut rng);
            assert!(sec < SECONDS_PER_DAY);
            hourly[(sec / 3_600) as usize] += 1;
        }

        let (peak_hour, &peak_count) = hourly
            .iter()
            .enumerate()
            .max_by_key(|(_, count)| *count)
            .unwrap();
        assert_eq!(peak_hour, 8);
        assert!(hourly[7] < peak_count && hourly[9] < peak_count);

        // Traffic dies out around 6pm PST: evening volume (18:00–24:00) is < 2%
        // of the day, and hour 18 is < 5% of the 8am peak.
        let after_6pm: u64 = hourly[18..].iter().sum();
        assert!(after_6pm * 100 < samples * 2);
        assert!(hourly[18] * 20 < peak_count);

        // Daytime window (05:00–18:00 PST) carries the vast majority of traffic.
        let daytime: u64 = hourly[5..18].iter().sum();
        assert!(daytime * 100 > samples * 90);
    }
}
