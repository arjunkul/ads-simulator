//! Pluggable campaign retrieval systems (`Retrieve-*`) for [`crate::AdServer`].
//!
//! Narrowing the full campaign inventory to eligible candidates via indexed data structures
//! avoids per-request full-campaign scans and forms Stage 1 of the 5-axis serving pipeline
//! (`Retrieval -> Pacing -> Scoring -> Bidding -> Ranking`).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Default retrieval strategy (`Retrieve-V0` = O(1) active-budget sparse-set index).
pub const DEFAULT_RETRIEVAL: RetrievalStrategy = RetrievalStrategy::RetrieveV0;

/// Retrieval strategy (`Retrieve-*`): narrows the full campaign inventory to eligible candidates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RetrievalStrategy {
    /// Retrieve-V0: O(1) sparse-set index of all campaigns with spendable remaining budget >= CPC.
    #[default]
    #[serde(rename = "Retrieve-V0", alias = "retrieve-v0", alias = "RT0", alias = "rt0", alias = "v0")]
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
            Self::RetrieveV0 => "Active-Budget Index (O(1))",
        }
    }

    /// Detailed human-readable description.
    pub const fn description(self) -> &'static str {
        match self {
            Self::RetrieveV0 => {
                "O(1) sparse-set index returning all campaigns with remaining budget >= CPC without full scans"
            }
        }
    }

    /// Concise algorithmic rule for dashboards and documentation.
    pub const fn formula(self) -> &'static str {
        match self {
            Self::RetrieveV0 => "index = { i : remaining_budget_i >= cpc } (O(1) sparse-set lookup & update)",
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
    /// Return eligible candidate campaign indices for the given request.
    fn retrieve(&self, request: &RetrievalRequest<'_>) -> &[usize];

    /// Incrementally update index membership when a campaign's remaining budget changes.
    fn on_budget_update(&mut self, campaign_index: usize, remaining_budget_cents: u64);

    /// Number of currently indexed eligible campaigns.
    fn active_count(&self) -> usize;
}

/// Retrieval v0 (`Retrieve-V0`): O(1) indexed active-budget campaign retriever.
///
/// Uses a dense `active` array paired with a reverse `positions` table (sparse set):
/// - `retrieve(...)` returns `&[usize]` in O(1) time with zero allocations and no full-campaign scan.
/// - `on_budget_update(...)` inserts or removes campaigns in O(1) time when budget crosses
///   the spendable threshold (`>= min_budget_cents.max(1)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveBudgetRetriever {
    min_budget_cents: u64,
    active: Vec<usize>,
    positions: Vec<Option<usize>>,
}

impl ActiveBudgetRetriever {
    /// Build an indexed retriever over `initial_budgets`, indexing every campaign with
    /// non-zero spendable budget (`remaining_budget_cents >= min_budget_cents.max(1)`).
    pub fn new(initial_budgets: &[u64], min_budget_cents: u64) -> Self {
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
        Self {
            min_budget_cents,
            active,
            positions,
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

    pub fn total_campaigns(&self) -> usize {
        self.positions.len()
    }
}

impl CampaignRetriever for ActiveBudgetRetriever {
    #[inline]
    fn retrieve(&self, _request: &RetrievalRequest<'_>) -> &[usize] {
        &self.active
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

    use super::{
        ActiveBudgetRetriever, CampaignRetriever, RetrievalRequest, RetrievalStrategy,
        build_retriever,
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
        let budgets = [0, 50, 100, 0, 200, 99];
        let nonzero = ActiveBudgetRetriever::non_zero_budget(&budgets);
        assert_eq!(nonzero.min_budget_cents(), 1);
        assert_eq!(nonzero.total_campaigns(), 6);
        assert_eq!(nonzero.active_count(), 4);
        assert_eq!(nonzero.retrieve(&dummy_request()), &[1, 2, 4, 5]);
        assert!(!nonzero.is_active(0));
        assert!(nonzero.is_active(1));
        assert!(!nonzero.is_active(3));

        let affordable = ActiveBudgetRetriever::new(&budgets, 100);
        assert_eq!(affordable.active_count(), 2);
        assert_eq!(affordable.retrieve(&dummy_request()), &[2, 4]);

        let clamped = ActiveBudgetRetriever::new(&budgets, 0);
        assert_eq!(clamped.min_budget_cents(), 1);
        assert_eq!(clamped.retrieve(&dummy_request()), &[1, 2, 4, 5]);
    }

    #[test]
    fn updates_active_index_in_o1_on_depletion_reactivation_and_idempotent_calls() {
        let mut retriever = ActiveBudgetRetriever::new(&[100, 100, 100, 100, 100], 100);
        let req = dummy_request();
        assert_eq!(retriever.retrieve(&req), &[0, 1, 2, 3, 4]);

        retriever.on_budget_update(2, 100);
        assert_eq!(retriever.retrieve(&req), &[0, 1, 2, 3, 4]);

        retriever.on_budget_update(2, 0);
        assert_eq!(retriever.retrieve(&req), &[0, 1, 4, 3]);
        assert!(!retriever.is_active(2));
        assert!(retriever.is_active(4));

        retriever.on_budget_update(4, 99);
        assert_eq!(retriever.retrieve(&req), &[0, 1, 3]);
        assert!(!retriever.is_active(4));

        retriever.on_budget_update(2, 0);
        assert_eq!(retriever.retrieve(&req), &[0, 1, 3]);

        retriever.on_budget_update(0, 0);
        assert_eq!(retriever.retrieve(&req), &[3, 1]);
        retriever.on_budget_update(1, 0);
        assert_eq!(retriever.retrieve(&req), &[3]);
        retriever.on_budget_update(3, 0);
        assert!(retriever.retrieve(&req).is_empty());
        assert_eq!(retriever.active_count(), 0);

        retriever.on_budget_update(2, 500);
        assert_eq!(retriever.retrieve(&req), &[2]);
        assert!(retriever.is_active(2));
        retriever.on_budget_update(2, 400);
        assert_eq!(retriever.retrieve(&req), &[2]);
    }

    #[test]
    fn maintains_exact_membership_across_many_campaigns_and_updates() {
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
        assert_eq!(retriever.active_count(), expected.len());

        for idx in 0..count {
            if idx % 2 == 0 {
                retriever.on_budget_update(idx, 0);
                expected.remove(&idx);
            }
            if idx % 6 == 0 {
                retriever.on_budget_update(idx, 200);
                expected.insert(idx);
            }
        }

        let retrieved: HashSet<usize> = retriever.retrieve(&req).iter().copied().collect();
        assert_eq!(retrieved.len(), retriever.retrieve(&req).len());
        assert_eq!(retrieved, expected);
    }
}
