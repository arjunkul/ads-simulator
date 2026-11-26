use ads_simulator::{Item, User, score_preference};

pub const MAX_CLICK_PROBABILITY: f64 = 0.04;

/// Predicts the click-through rate (pCTR) for showing `item` to `user`.
pub trait RankingModel {
    fn predict_pctr(&self, user: &User, item: &Item) -> f64;
}

/// Default ranking model using the marketplace preference score.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultRankingModel;

impl RankingModel for DefaultRankingModel {
    fn predict_pctr(&self, user: &User, item: &Item) -> f64 {
        MAX_CLICK_PROBABILITY * score_preference(user, item)
    }
}

#[cfg(test)]
mod tests {
    use ads_simulator::{Item, User};

    use super::{DefaultRankingModel, MAX_CLICK_PROBABILITY, RankingModel};

    #[test]
    fn default_ranking_model_predicts_max_ctr_for_preferred_categories_only() {
        let model = DefaultRankingModel;
        let user = User {
            id: "u1".into(),
            preferred_category_ids: vec!["cat-1".into()],
        };
        let matching = Item {
            id: "i1".into(),
            category_id: "cat-1".into(),
            name: "Matching".into(),
            price_cents: 1_000,
        };
        let non_matching = Item {
            id: "i2".into(),
            category_id: "cat-2".into(),
            name: "Other".into(),
            price_cents: 1_000,
        };
        assert_eq!(model.predict_pctr(&user, &matching), MAX_CLICK_PROBABILITY);
        assert_eq!(model.predict_pctr(&user, &non_matching), 0.0);
    }
}
