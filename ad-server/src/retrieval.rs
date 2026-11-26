//! Pluggable campaign retrieval systems (`Retrieve-*`) for [`crate::AdServer`].
//!
//! Narrowing the full campaign inventory to eligible candidates via indexed data structures
//! avoids per-request full-campaign scans and forms Stage 1 of the 5-axis serving pipeline
//! (`Retrieval -> Pacing -> Scoring -> Bidding -> Ranking`).

use std::fmt;
use std::str::FromStr;

use rand::Rng;
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

/// Default retrieval strategy (`Retrieve-V0` = O(1) active-budget sparse-set index capped at 20 campaigns).
pub const DEFAULT_RETRIEVAL: RetrievalStrategy = RetrievalStrategy::RetrieveV0;

/// Maximum number of funded campaigns returned per request by `Retrieve-V0`.
pub const RETRIEVE_V0_MAX_CANDIDATES: usize = 20;

/// Retrieval strategy (`Retrieve-*`): narrows the full campaign inventory to eligible candidates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RetrievalStrategy {
    /// Retrieve-V0: O(1) sparse-set index uniformly sampling up to 20 campaigns with remaining budget >= CPC.
    #[default]
    #[serde(
        rename = "Retrieve-V0",
        alias = "retrieve-v0",
        alias = "RT0",
        alias = "rt0",
        alias = "v0"
    )]
    RetrieveV0,
}

impl RetrievalStrategy {
    /// All compiled retrieval strategies in registration order.
    pub const fn all() -> &'static [RetrievalStrategy] {
        &[RetrievalStrategy::RetrieveV0]
    }

    /// Canonical short identifier (`"Retrieve-V0"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RetrieveV0 => "Retrieve-V0",
        }
    }

    /// Short human-readable name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::RetrieveV0 => "Active-Budget Index (Top-20 Sample)",
        }
    }

    /// Detailed human-readable description.
    pub const fn description(self) -> &'static str {
        match self {
            Self::RetrieveV0 => {
                "O(1) sparse-set active-budget index uniformly sampling up to 20 funded campaigns (remaining budget >= CPC) per request without full scans"
            }
        }
    }

    /// Concise algorithmic rule for dashboards and documentation.
    pub const fn formula(self) -> &'static str {
        match self {
            Self::RetrieveV0 => {
                "sample_without_replacement({ i : remaining_budget_i >= cpc }, k = 20) (O(20) sparse-set lookup & O(1) update)"
            }
        }
    }
}

impl fmt::Display for RetrievalStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RetrievalStrategy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_uppercase().as_str() {
            "RETRIEVE-V0" | "RETRIEVE_V0" | "RT0" | "V0" | "ACTIVE_BUDGET" => {
                Ok(RetrievalStrategy::RetrieveV0)
            }
            other => Err(format!(
                "unknown retrieval strategy '{other}' (supported: Retrieve-V0/RT0)"
            )),
        }
    }
}

/// Request-time context passed to a campaign retriever.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetrievalRequest<'a> {
    pub day: usize,
    pub user_id: &'a str,
    pub user_index: usize,
    pub organic_category_id: &'a str,
    pub organic_category_index: usize,
}

/// Interface for campaign retrieval systems (`Retrieve-*`).
///
/// Implementations maintain index structures so request-time retrieval avoids
/// scanning every campaign in the marketplace.
pub trait CampaignRetriever: Send + Sync {
    /// Return up to the strategy's candidate limit of eligible campaign indices for the request.
    fn retrieve<'a>(
        &'a mut self,
        request: &RetrievalRequest<'_>,
        rng: &mut ChaCha8Rng,
    ) -> &'a [usize];

    /// Incrementally update index membership when a campaign's remaining budget changes.
    fn on_budget_update(&mut self, campaign_index: usize, remaining_budget_cents: u64);

    /// Number of currently indexed eligible campaigns.
    fn active_count(&self) -> usize;
}

/// Retrieval v0 (`Retrieve-V0`): O(1) indexed active-budget campaign retriever capped at 20 candidates.
///
/// Uses a dense `active` array paired with a reverse `positions` table (sparse set):
/// - `retrieve(...)` uniformly samples up to `max_candidates` (default [`RETRIEVE_V0_MAX_CANDIDATES`] = 20)
///   distinct funded campaigns in `O(min(active, max_candidates))` time with zero per-request allocations
///   and no full-campaign scan (undoing temporary Fisher-Yates swaps to preserve `positions` invariants).
/// - `on_budget_update(...)` inserts or removes campaigns in `O(1)` time when budget crosses
///   the spendable threshold (`>= min_budget_cents.max(1)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveBudgetRetriever {
    min_budget_cents: u64,
    max_candidates: usize,
    active: Vec<usize>,
    positions: Vec<Option<usize>>,
    sample_buf: Vec<usize>,
    swap_buf: Vec<usize>,
}

impl ActiveBudgetRetriever {
    /// Build an indexed retriever over `initial_budgets` capped at [`RETRIEVE_V0_MAX_CANDIDATES`] (20),
    /// indexing every campaign with non-zero spendable budget (`remaining_budget_cents >= min_budget_cents.max(1)`).
    pub fn new(initial_budgets: &[u64], min_budget_cents: u64) -> Self {
        Self::with_limit(
            initial_budgets,
            min_budget_cents,
            RETRIEVE_V0_MAX_CANDIDATES,
        )
    }

    /// Build an indexed retriever with an explicit per-request `max_candidates` output cap.
    pub fn with_limit(
        initial_budgets: &[u64],
        min_budget_cents: u64,
        max_candidates: usize,
    ) -> Self {
        let min_budget_cents = min_budget_cents.max(1);
        let mut active = Vec::with_capacity(initial_budgets.len());
        let mut positions = vec![None; initial_budgets.len()];
        for (campaign_index, &budget) in initial_budgets.iter().enumerate() {
            if budget >= min_budget_cents {
                let pos = active.len();
                active.push(campaign_index);
                positions[campaign_index] = Some(pos);
            }
        }
        let buf_cap = max_candidates.min(initial_budgets.len());
        Self {
            min_budget_cents,
            max_candidates,
            active,
            positions,
            sample_buf: Vec::with_capacity(buf_cap),
            swap_buf: Vec::with_capacity(buf_cap),
        }
    }

    /// Build a retriever indexing all campaigns with strictly non-zero remaining budget (`>= 1` cent).
    pub fn non_zero_budget(initial_budgets: &[u64]) -> Self {
        Self::new(initial_budgets, 1)
    }

    pub fn is_active(&self, campaign_index: usize) -> bool {
        self.positions
            .get(campaign_index)
            .is_some_and(Option::is_some)
    }

    pub fn min_budget_cents(&self) -> u64 {
        self.min_budget_cents
    }

    pub fn max_candidates(&self) -> usize {
        self.max_candidates
    }

    pub fn total_campaigns(&self) -> usize {
        self.positions.len()
    }
}

impl CampaignRetriever for ActiveBudgetRetriever {
    #[inline]
    fn retrieve<'a>(
        &'a mut self,
        _request: &RetrievalRequest<'_>,
        rng: &mut ChaCha8Rng,
    ) -> &'a [usize] {
        let n = self.active.len();
        let k = n.min(self.max_candidates);
        if k == 0 {
            return &[];
        }
        if k == n {
            return &self.active;
        }
        // O(k) partial Fisher-Yates sample without replacement into reusable `sample_buf`,
        // then undo the `k` swaps in reverse order so `active` and `positions` remain intact.
        self.sample_buf.clear();
        self.swap_buf.clear();
        for i in 0..k {
            let j = rng.random_range(i..n);
            self.active.swap(i, j);
            self.swap_buf.push(j);
            self.sample_buf.push(self.active[i]);
        }
        for i in (0..k).rev() {
            self.active.swap(i, self.swap_buf[i]);
        }
        &self.sample_buf
    }

    #[inline]
    fn on_budget_update(&mut self, campaign_index: usize, remaining_budget_cents: u64) {
        let eligible = remaining_budget_cents >= self.min_budget_cents;
        match (self.positions[campaign_index], eligible) {
            (Some(pos), false) => {
                self.active.swap_remove(pos);
                self.positions[campaign_index] = None;
                if let Some(&moved_campaign) = self.active.get(pos) {
                    self.positions[moved_campaign] = Some(pos);
                }
            }
            (None, true) => {
                let pos = self.active.len();
                self.active.push(campaign_index);
                self.positions[campaign_index] = Some(pos);
            }
            (Some(_), true) | (None, false) => {}
        }
    }

    #[inline]
    fn active_count(&self) -> usize {
        self.active.len()
    }
}

/// Construct the configured campaign retriever for a simulation run.
pub fn build_retriever(
    strategy: RetrievalStrategy,
    initial_budgets: &[u64],
    min_budget_cents: u64,
) -> Box<dyn CampaignRetriever> {
    match strategy {
        RetrievalStrategy::RetrieveV0 => Box::new(ActiveBudgetRetriever::new(
            initial_budgets,
            min_budget_cents,
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::str::FromStr;

    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    use super::{
        ActiveBudgetRetriever, CampaignRetriever, RETRIEVE_V0_MAX_CANDIDATES, RetrievalRequest,
        RetrievalStrategy, build_retriever,
    };

    fn dummy_request() -> RetrievalRequest<'static> {
        RetrievalRequest {
            day: 1,
            user_id: "user-0001",
            user_index: 0,
            organic_category_id: "category-001",
            organic_category_index: 0,
        }
    }

    #[test]
    fn strategy_parsing_and_json_round_trip() {
        for raw in ["Retrieve-V0", "retrieve-v0", "RT0", "v0"] {
            assert_eq!(
                RetrievalStrategy::from_str(raw).unwrap(),
                RetrievalStrategy::RetrieveV0
            );
        }
        assert!(RetrievalStrategy::from_str("Retrieve-V99").is_err());
        let json = serde_json::to_string(&RetrievalStrategy::RetrieveV0).unwrap();
        assert_eq!(json, "\"Retrieve-V0\"");
        let parsed: RetrievalStrategy = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, RetrievalStrategy::RetrieveV0);
    }

    #[test]
    fn initializes_only_campaigns_with_nonzero_spendable_budget() {
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let budgets = [0, 50, 100, 0, 200, 99];
        let mut nonzero = ActiveBudgetRetriever::non_zero_budget(&budgets);
        assert_eq!(nonzero.min_budget_cents(), 1);
        assert_eq!(nonzero.max_candidates(), RETRIEVE_V0_MAX_CANDIDATES);
        assert_eq!(nonzero.total_campaigns(), 6);
        assert_eq!(nonzero.active_count(), 4);
        assert_eq!(nonzero.retrieve(&dummy_request(), &mut rng), &[1, 2, 4, 5]);
        assert!(!nonzero.is_active(0));
        assert!(nonzero.is_active(1));
        assert!(!nonzero.is_active(3));

        let mut affordable = ActiveBudgetRetriever::new(&budgets, 100);
        assert_eq!(affordable.active_count(), 2);
        assert_eq!(affordable.retrieve(&dummy_request(), &mut rng), &[2, 4]);

        let mut clamped = ActiveBudgetRetriever::new(&budgets, 0);
        assert_eq!(clamped.min_budget_cents(), 1);
        assert_eq!(clamped.retrieve(&dummy_request(), &mut rng), &[1, 2, 4, 5]);
    }

    #[test]
    fn updates_active_index_in_o1_on_depletion_reactivation_and_idempotent_calls() {
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let mut retriever = ActiveBudgetRetriever::new(&[100, 100, 100, 100, 100], 100);
        let req = dummy_request();
        assert_eq!(retriever.retrieve(&req, &mut rng), &[0, 1, 2, 3, 4]);

        retriever.on_budget_update(2, 100);
        assert_eq!(retriever.retrieve(&req, &mut rng), &[0, 1, 2, 3, 4]);

        retriever.on_budget_update(2, 0);
        assert_eq!(retriever.retrieve(&req, &mut rng), &[0, 1, 4, 3]);
        assert!(!retriever.is_active(2));
        assert!(retriever.is_active(4));

        retriever.on_budget_update(4, 99);
        assert_eq!(retriever.retrieve(&req, &mut rng), &[0, 1, 3]);
        assert!(!retriever.is_active(4));

        retriever.on_budget_update(2, 0);
        assert_eq!(retriever.retrieve(&req, &mut rng), &[0, 1, 3]);

        retriever.on_budget_update(0, 0);
        assert_eq!(retriever.retrieve(&req, &mut rng), &[3, 1]);
        retriever.on_budget_update(1, 0);
        assert_eq!(retriever.retrieve(&req, &mut rng), &[3]);
        retriever.on_budget_update(3, 0);
        assert!(retriever.retrieve(&req, &mut rng).is_empty());
        assert_eq!(retriever.active_count(), 0);

        retriever.on_budget_update(2, 500);
        assert_eq!(retriever.retrieve(&req, &mut rng), &[2]);
        assert!(retriever.is_active(2));
        retriever.on_budget_update(2, 400);
        assert_eq!(retriever.retrieve(&req, &mut rng), &[2]);
    }

    #[test]
    fn limits_output_to_20_distinct_funded_campaigns_and_samples_across_full_active_pool() {
        let count = 500;
        let initial_budgets: Vec<u64> = (0..count)
            .map(|i| if i % 5 == 0 { 0 } else { 100 })
            .collect();
        let mut retriever = build_retriever(RetrievalStrategy::RetrieveV0, &initial_budgets, 100);
        let expected_active: HashSet<usize> = initial_budgets
            .iter()
            .enumerate()
            .filter_map(|(idx, &b)| (b >= 100).then_some(idx))
            .collect();
        assert_eq!(retriever.active_count(), 400);

        let req = dummy_request();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let mut seen_across_requests = HashSet::new();

        for _ in 0..200 {
            let batch = retriever.retrieve(&req, &mut rng);
            assert_eq!(batch.len(), RETRIEVE_V0_MAX_CANDIDATES);
            let unique: HashSet<usize> = batch.iter().copied().collect();
            assert_eq!(unique.len(), RETRIEVE_V0_MAX_CANDIDATES);
            assert!(unique.is_subset(&expected_active));
            seen_across_requests.extend(unique);
        }

        // Sampling across 200 requests covers campaigns well beyond the first 20.
        assert!(seen_across_requests.len() > 350);

        // Repeat with the same seed reproduces the exact same first 20-campaign batch.
        let mut r_a = build_retriever(RetrievalStrategy::RetrieveV0, &initial_budgets, 100);
        let mut r_b = build_retriever(RetrievalStrategy::RetrieveV0, &initial_budgets, 100);
        let mut rng_a = ChaCha8Rng::seed_from_u64(99);
        let mut rng_b = ChaCha8Rng::seed_from_u64(99);
        assert_eq!(
            r_a.retrieve(&req, &mut rng_a).to_vec(),
            r_b.retrieve(&req, &mut rng_b).to_vec()
        );
    }

    #[test]
    fn maintains_exact_membership_and_cap_across_many_campaigns_and_updates() {
        let count = 5_000;
        let initial_budgets: Vec<u64> = (0..count)
            .map(|i| if i % 3 == 0 { 0 } else { 100 })
            .collect();
        let mut retriever = build_retriever(RetrievalStrategy::RetrieveV0, &initial_budgets, 100);
        let mut expected: HashSet<usize> = initial_budgets
            .iter()
            .enumerate()
            .filter_map(|(idx, &b)| (b >= 100).then_some(idx))
            .collect();

        let req = dummy_request();
        let mut rng = ChaCha8Rng::seed_from_u64(7);
        assert_eq!(retriever.active_count(), expected.len());

        // Interleave sampled retrievals with budget updates to verify undo-swaps preserve positions.
        for idx in 0..count {
            if idx % 250 == 0 {
                let sample = retriever.retrieve(&req, &mut rng);
                assert_eq!(sample.len(), RETRIEVE_V0_MAX_CANDIDATES);
                assert!(sample.iter().all(|c| expected.contains(c)));
            }
            if idx % 2 == 0 {
                retriever.on_budget_update(idx, 0);
                expected.remove(&idx);
            }
            if idx % 6 == 0 {
                retriever.on_budget_update(idx, 200);
                expected.insert(idx);
            }
        }

        assert_eq!(retriever.active_count(), expected.len());
        let sample = retriever.retrieve(&req, &mut rng);
        assert_eq!(sample.len(), RETRIEVE_V0_MAX_CANDIDATES);
        let unique: HashSet<usize> = sample.iter().copied().collect();
        assert_eq!(unique.len(), RETRIEVE_V0_MAX_CANDIDATES);
        assert!(unique.is_subset(&expected));

        // Deplete down to 12 active campaigns: retrieval returns all 12.
        let to_deplete: Vec<usize> = expected.iter().skip(12).copied().collect();
        for idx in to_deplete {
            retriever.on_budget_update(idx, 0);
            expected.remove(&idx);
        }
        assert_eq!(retriever.active_count(), 12);
        let tail: HashSet<usize> = retriever.retrieve(&req, &mut rng).iter().copied().collect();
        assert_eq!(tail.len(), 12);
        assert_eq!(tail, expected);
    }
}
