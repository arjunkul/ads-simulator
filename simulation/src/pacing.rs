use clap::ValueEnum;
use serde::Serialize;

use crate::timestamp::SECONDS_PER_DAY;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, ValueEnum)]
pub enum PacingStrategy {
    /// No pacing: any campaign that can afford a click is eligible at any time.
    #[serde(rename = "none")]
    #[value(name = "none")]
    None,
    /// Divide each campaign's total budget into equal daily budgets and enforce
    /// an even linear spend schedule across the 24-hour PST day.
    #[default]
    #[serde(rename = "pacing-1")]
    #[value(name = "pacing-1")]
    Pacing1,
}

/// Split a campaign's upfront budget across `days` as evenly as possible,
/// giving any remainder cents to the earliest days one cent at a time, and
/// capping by the campaign's remaining lifetime budget.
pub fn daily_budget_cents(
    total_budget_cents: u64,
    remaining_budget_cents: u64,
    day: usize,
    days: usize,
) -> u64 {
    let days = days as u64;
    let base = total_budget_cents / days;
    let extra = u64::from((day as u64) <= total_budget_cents % days);
    (base + extra).min(remaining_budget_cents)
}

/// Return the earliest integer second of day in `[0, SECONDS_PER_DAY)` at which
/// a campaign with `daily_spend_cents` and `daily_budget_cents` is back on or
/// behind the even intraday spend curve (`daily_spend / daily_budget <= t / 86_400`),
/// or `None` if it cannot afford another click today or would not unlock before midnight.
pub fn pacing_1_unlock_second(
    daily_spend_cents: u64,
    daily_budget_cents: u64,
    remaining_budget_cents: u64,
    cpc_cents: u64,
) -> Option<u32> {
    if remaining_budget_cents < cpc_cents {
        return None;
    }
    if daily_spend_cents.saturating_add(cpc_cents) > daily_budget_cents {
        return None;
    }
    if daily_spend_cents == 0 {
        return Some(0);
    }
    let unlock = (u128::from(daily_spend_cents) * u128::from(SECONDS_PER_DAY))
        .div_ceil(u128::from(daily_budget_cents));
    if unlock < u128::from(SECONDS_PER_DAY) {
        Some(unlock as u32)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{daily_budget_cents, pacing_1_unlock_second};
    use crate::timestamp::SECONDS_PER_DAY;

    #[test]
    fn daily_budget_splits_evenly_with_earliest_day_remainder() {
        let budgets: Vec<_> = (1..=30)
            .map(|day| daily_budget_cents(10_000, 10_000, day, 30))
            .collect();
        assert_eq!(budgets.iter().sum::<u64>(), 10_000);
        assert!(budgets[..10].iter().all(|&b| b == 334));
        assert!(budgets[10..].iter().all(|&b| b == 333));
    }

    #[test]
    fn pacing_1_unlocks_clicks_evenly_across_the_day() {
        // $3.00 daily budget at $1.00 CPC -> unlocks at 00:00, 08:00, 16:00 PST.
        assert_eq!(pacing_1_unlock_second(0, 300, 9_000, 100), Some(0));
        assert_eq!(
            pacing_1_unlock_second(100, 300, 8_900, 100),
            Some(8 * 3_600)
        );
        assert_eq!(
            pacing_1_unlock_second(200, 300, 8_800, 100),
            Some(16 * 3_600)
        );
        // After 3 clicks ($3.00), daily budget is exhausted.
        assert_eq!(pacing_1_unlock_second(300, 300, 8_700, 100), None);
    }

    #[test]
    fn pacing_1_handles_fractional_seconds_and_midnight_boundary() {
        // $3.33 daily budget at $1.00 CPC.
        let t1 = pacing_1_unlock_second(100, 333, 10_000, 100).unwrap();
        assert!(100 * u64::from(SECONDS_PER_DAY) <= 333 * u64::from(t1));
        assert!(100 * u64::from(SECONDS_PER_DAY) > 333 * u64::from(t1 - 1));

        // If unlock would land on or after 86_400, campaign is done for the day.
        assert_eq!(pacing_1_unlock_second(99_999, 100_000, 100_000, 1), None);
        // Unaffordable daily budget slice cannot serve.
        assert_eq!(pacing_1_unlock_second(0, 33, 1_000, 100), None);
    }
}
