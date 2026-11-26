use clap::ValueEnum;
use rand::Rng;
use serde::Serialize;

const BID_TIE_EPSILON: f64 = 1e-12;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, ValueEnum)]
pub enum BiddingStrategy {
    /// Baseline strategy: every eligible campaign bids only its flat basebid.
    #[default]
    #[serde(rename = "bidding-basebid")]
    #[value(name = "bidding-basebid")]
    Basebid,
    /// Autobid v0: final_bid = basebid * pCTR * pacing_factor, resolved via a
    /// 1st-price auction that charges basebid * pacing_factor on clicks.
    #[serde(rename = "bidding-autobid-v0")]
    #[value(name = "bidding-autobid-v0")]
    AutobidV0,
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
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    use super::{BiddingStrategy, compute_bid, run_first_price_auction};

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

        // Zero bids cannot win.
        let all_zero = [
            (0, compute_bid(BiddingStrategy::AutobidV0, 100, 0.0, 1.0)),
            (1, compute_bid(BiddingStrategy::AutobidV0, 100, 0.04, 0.0)),
        ];
        assert!(run_first_price_auction(all_zero, &mut rng).is_none());

        // Equal top bids tie-break fairly.
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
