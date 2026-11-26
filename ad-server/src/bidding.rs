//! Pluggable bidding strategies (`bidding-basebid`, `bidding-autobid-v0`) and 1st-price auction.

use std::fmt;
use std::str::FromStr;

use rand::Rng;
use serde::{Deserialize, Serialize};

const BID_TIE_EPSILON: f64 = 1e-12;

/// Default bidding strategy (`bidding-basebid` = flat base bid = CPC).
pub const DEFAULT_BIDDING: BiddingStrategy = BiddingStrategy::Basebid;

/// Bidding strategy governing candidate bid calculation and click pricing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BiddingStrategy {
    /// Baseline strategy: every eligible campaign bids only its flat basebid (`cpc_cents`).
    #[default]
    #[serde(rename = "bidding-basebid", alias = "B0", alias = "b0", alias = "basebid")]
    Basebid,
    /// Autobid v0: `final_bid = basebid * pCTR * pacing_factor`, resolved via 1st-price auction
    /// that charges `round(basebid * pacing_factor)` on clicks.
    #[serde(
        rename = "bidding-autobid-v0",
        alias = "B1",
        alias = "b1",
        alias = "autobid-v0",
        alias = "autobid"
    )]
    AutobidV0,
}

impl BiddingStrategy {
    /// All compiled bidding strategies in registration order.
    pub const fn all() -> &'static [BiddingStrategy] {
        &[BiddingStrategy::Basebid, BiddingStrategy::AutobidV0]
    }

    /// Canonical identifier (`"bidding-basebid"`, `"bidding-autobid-v0"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Basebid => "bidding-basebid",
            Self::AutobidV0 => "bidding-autobid-v0",
        }
    }

    /// Short human-readable name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Basebid => "Flat Basebid",
            Self::AutobidV0 => "Autobid v0 (1st-Price)",
        }
    }

    /// Detailed human-readable description.
    pub const fn description(self) -> &'static str {
        match self {
            Self::Basebid => "Every eligible campaign bids its flat CPC basebid and pays basebid on clicks",
            Self::AutobidV0 => {
                "Computes eCPM bid = basebid * pCTR * pacing_factor and charges round(basebid * pacing_factor) on clicks"
            }
        }
    }

    /// Concise bidding formula for dashboards and documentation.
    pub const fn formula(self) -> &'static str {
        match self {
            Self::Basebid => "final_bid = basebid; click_price = basebid",
            Self::AutobidV0 => "final_bid = basebid * pCTR * pacing_factor; click_price = round(basebid * pacing_factor)",
        }
    }
}

impl fmt::Display for BiddingStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for BiddingStrategy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_uppercase().as_str() {
            "BIDDING-BASEBID" | "BASEBID" | "B0" => Ok(BiddingStrategy::Basebid),
            "BIDDING-AUTOBID-V0" | "AUTOBID-V0" | "AUTOBID_V0" | "AUTOBID" | "B1" => {
                Ok(BiddingStrategy::AutobidV0)
            }
            other => Err(format!(
                "unknown bidding strategy '{other}' (supported: bidding-basebid/B0, bidding-autobid-v0/B1)"
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bid {
    pub base_bid_cents: u64,
    pub pctr: f64,
    pub pacing_factor: f64,
    pub final_bid_cents: f64,
    pub click_price_cents: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AuctionWinner {
    pub candidate_index: usize,
    pub bid: Bid,
}

/// Compute a campaign's bid under the selected bidding strategy.
///
/// - `bidding-basebid`: `final_bid = basebid`, `click_price = basebid` (when `pacing_factor > 0`).
/// - `bidding-autobid-v0`: `final_bid = basebid * pctr * pacing_factor`,
///   `click_price = round(basebid * pacing_factor)`.
pub fn compute_bid(
    strategy: BiddingStrategy,
    base_bid_cents: u64,
    pctr: f64,
    pacing_factor: f64,
) -> Bid {
    if pacing_factor <= 0.0 {
        return Bid {
            base_bid_cents,
            pctr,
            pacing_factor: 0.0,
            final_bid_cents: 0.0,
            click_price_cents: 0,
        };
    }

    match strategy {
        BiddingStrategy::Basebid => Bid {
            base_bid_cents,
            pctr,
            pacing_factor,
            final_bid_cents: base_bid_cents as f64,
            click_price_cents: base_bid_cents,
        },
        BiddingStrategy::AutobidV0 => {
            let final_bid_cents = (base_bid_cents as f64) * pctr * pacing_factor;
            let click_price_cents =
                ((base_bid_cents as f64) * pacing_factor).round().max(1.0) as u64;
            Bid {
                base_bid_cents,
                pctr,
                pacing_factor,
                final_bid_cents,
                click_price_cents,
            }
        }
    }
}

/// Run a 1st-price auction over `(candidate_index, Bid)` entries, selecting the
/// strictly positive bid with the highest `final_bid_cents` and breaking ties
/// uniformly at random.
pub fn run_first_price_auction(
    bids: impl IntoIterator<Item = (usize, Bid)>,
    rng: &mut impl Rng,
) -> Option<AuctionWinner> {
    let mut best: Option<AuctionWinner> = None;
    let mut ties = 0u64;

    for (candidate_index, bid) in bids {
        if bid.final_bid_cents <= 0.0 || bid.click_price_cents == 0 {
            continue;
        }
        match best {
            None => {
                best = Some(AuctionWinner {
                    candidate_index,
                    bid,
                });
                ties = 1;
            }
            Some(current)
                if bid.final_bid_cents > current.bid.final_bid_cents + BID_TIE_EPSILON =>
            {
                best = Some(AuctionWinner {
                    candidate_index,
                    bid,
                });
                ties = 1;
            }
            Some(current)
                if (bid.final_bid_cents - current.bid.final_bid_cents).abs() <= BID_TIE_EPSILON =>
            {
                ties += 1;
                if rng.random_range(0..ties) == 0 {
                    best = Some(AuctionWinner {
                        candidate_index,
                        bid,
                    });
                }
            }
            Some(_) => {}
        }
    }

    best
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    use super::{BiddingStrategy, compute_bid, run_first_price_auction};

    #[test]
    fn bidding_strategy_parsing_and_json_round_trip() {
        for (raw, expected) in [
            ("bidding-basebid", BiddingStrategy::Basebid),
            ("B0", BiddingStrategy::Basebid),
            ("bidding-autobid-v0", BiddingStrategy::AutobidV0),
            ("B1", BiddingStrategy::AutobidV0),
        ] {
            assert_eq!(BiddingStrategy::from_str(raw).unwrap(), expected);
            let json = serde_json::to_string(&expected).unwrap();
            let parsed: BiddingStrategy = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, expected);
        }
        assert!(BiddingStrategy::from_str("B99").is_err());
    }

    #[test]
    fn basebid_uses_only_basebid_and_ignores_pctr() {
        let bid = compute_bid(BiddingStrategy::Basebid, 100, 0.0, 1.0);
        assert_eq!(bid.final_bid_cents, 100.0);
        assert_eq!(bid.click_price_cents, 100);
    }

    #[test]
    fn autobid_v0_multiplies_basebid_by_pctr_and_pacing_factor() {
        let full_pace = compute_bid(BiddingStrategy::AutobidV0, 100, 0.04, 1.0);
        assert!((full_pace.final_bid_cents - 4.0).abs() < 1e-12);
        assert_eq!(full_pace.click_price_cents, 100);

        let shaded = compute_bid(BiddingStrategy::AutobidV0, 100, 0.04, 0.5);
        assert!((shaded.final_bid_cents - 2.0).abs() < 1e-12);
        assert_eq!(shaded.click_price_cents, 50);

        let throttled = compute_bid(BiddingStrategy::AutobidV0, 100, 0.04, 0.0);
        assert_eq!(throttled.final_bid_cents, 0.0);
        assert_eq!(throttled.click_price_cents, 0);

        let zero_ctr = compute_bid(BiddingStrategy::AutobidV0, 100, 0.0, 1.0);
        assert_eq!(zero_ctr.final_bid_cents, 0.0);
    }

    #[test]
    fn first_price_auction_picks_highest_positive_bid_and_breaks_ties_uniformly() {
        let mut rng = ChaCha8Rng::seed_from_u64(7);
        let bids = [
            (0, compute_bid(BiddingStrategy::AutobidV0, 100, 0.0, 1.0)),
            (1, compute_bid(BiddingStrategy::AutobidV0, 100, 0.02, 1.0)),
            (2, compute_bid(BiddingStrategy::AutobidV0, 100, 0.04, 1.0)),
            (3, compute_bid(BiddingStrategy::AutobidV0, 100, 0.04, 0.0)),
        ];
        let winner = run_first_price_auction(bids, &mut rng).unwrap();
        assert_eq!(winner.candidate_index, 2);
        assert_eq!(winner.bid.click_price_cents, 100);

        let all_zero = [
            (0, compute_bid(BiddingStrategy::AutobidV0, 100, 0.0, 1.0)),
            (1, compute_bid(BiddingStrategy::AutobidV0, 100, 0.04, 0.0)),
        ];
        assert!(run_first_price_auction(all_zero, &mut rng).is_none());

        let tied = [
            (0, compute_bid(BiddingStrategy::AutobidV0, 100, 0.04, 1.0)),
            (1, compute_bid(BiddingStrategy::AutobidV0, 100, 0.04, 1.0)),
        ];
        let mut wins = [0u64; 2];
        for _ in 0..2_000 {
            let winner = run_first_price_auction(tied, &mut rng).unwrap();
            wins[winner.candidate_index] += 1;
        }
        assert!(wins[0] > 900 && wins[1] > 900);
    }
}
