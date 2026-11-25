use std::{
    collections::{HashMap, VecDeque},
    io::{self, Error, ErrorKind},
};

use serde::{Deserialize, Serialize};

use crate::records::{AdCandidate, ImpressionClickRecord, ImpressionLogStore, RankingContext};

const DENSE_USER_CATEGORY_LIMIT: usize = 2_000_000;
const INTERCEPT_INDEX: usize = 0;
const SAME_CATEGORY_INDEX: usize = 1;

/// Interface for click-through-rate models used to rank ad candidates.
pub trait CtrModel {
    /// Predict the probability of a click in `(0, 1)` for a candidate ad in the given context.
    fn predict_ctr(&self, context: &RankingContext<'_>, candidate: &AdCandidate<'_>) -> f64;

    /// Return candidate indices sorted from highest to lowest predicted CTR.
    fn rank_candidates(
        &self,
        context: &RankingContext<'_>,
        candidates: &[AdCandidate<'_>],
    ) -> Vec<usize> {
        let mut scored: Vec<(usize, f64)> = candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| (index, self.predict_ctr(context, candidate)))
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        scored.into_iter().map(|(index, _)| index).collect()
    }
}

/// Hyperparameters for cold-training an L2-regularized logistic regression CTR model.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrainingConfig {
    /// L2 regularization strength applied to all non-intercept weights.
    pub l2_penalty: f64,
    /// Maximum number of L-BFGS iterations.
    pub max_iterations: usize,
    /// Stopping threshold on the infinity-norm of the objective gradient.
    pub gradient_tolerance: f64,
    /// Stopping threshold on relative objective decrease between consecutive iterations.
    pub relative_loss_tolerance: f64,
    /// Number of recent curvature pairs kept by L-BFGS.
    pub history_size: usize,
}

impl Default for TrainingConfig {
    fn default() -> Self {
        Self {
            l2_penalty: 1.0,
            max_iterations: 200,
            gradient_tolerance: 1e-4,
            relative_loss_tolerance: 1e-9,
            history_size: 10,
        }
    }
}

impl TrainingConfig {
    fn validate(&self) -> io::Result<()> {
        if !self.l2_penalty.is_finite() || self.l2_penalty <= 0.0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "l2_penalty must be finite and positive",
            ));
        }
        if self.max_iterations == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "max_iterations must be positive",
            ));
        }
        if !self.gradient_tolerance.is_finite() || self.gradient_tolerance <= 0.0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "gradient_tolerance must be finite and positive",
            ));
        }
        if !self.relative_loss_tolerance.is_finite() || self.relative_loss_tolerance < 0.0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "relative_loss_tolerance must be finite and non-negative",
            ));
        }
        if self.history_size == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "history_size must be positive",
            ));
        }
        Ok(())
    }
}

/// Diagnostics produced by a single cold-training run.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrainingSummary {
    pub impressions: u64,
    pub clicks: u64,
    pub unique_groups: usize,
    pub feature_count: usize,
    pub iterations: usize,
    pub converged: bool,
    pub mean_log_loss: f64,
}

impl Default for TrainingSummary {
    fn default() -> Self {
        Self {
            impressions: 0,
            clicks: 0,
            unique_groups: 0,
            feature_count: 2,
            iterations: 0,
            converged: true,
            mean_log_loss: 0.0,
        }
    }
}

#[derive(Default)]
struct StringInterner {
    to_id: HashMap<String, u32>,
}

impl StringInterner {
    fn intern(&mut self, value: &str) -> u32 {
        if let Some(&id) = self.to_id.get(value) {
            return id;
        }
        let id = self.to_id.len() as u32;
        self.to_id.insert(value.to_owned(), id);
        id
    }

    fn len(&self) -> usize {
        self.to_id.len()
    }

    fn into_map(self) -> HashMap<String, u32> {
        self.to_id
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct GroupKey {
    user: u32,
    ad_category: u32,
    advertiser: u32,
    campaign: u32,
    same_category: bool,
}

#[derive(Clone, Copy, Debug)]
struct GroupStats {
    user: u32,
    ad_category: u32,
    advertiser: u32,
    campaign: u32,
    user_ad_category: u32,
    same_category: bool,
    impressions: u64,
    clicks: u64,
}

/// Streams impression-click observations into exact sufficient statistics for logistic regression.
#[derive(Default)]
struct DatasetAccumulator {
    users: StringInterner,
    ad_categories: StringInterner,
    advertisers: StringInterner,
    campaigns: StringInterner,
    user_ad_category_ids: HashMap<(u32, u32), u32>,
    group_indices: HashMap<GroupKey, usize>,
    groups: Vec<GroupStats>,
    total_impressions: u64,
    total_clicks: u64,
}

impl DatasetAccumulator {
    fn observe(
        &mut self,
        user_id: &str,
        organic_category_id: &str,
        campaign_id: &str,
        advertiser_id: &str,
        ad_category_id: &str,
        clicked: bool,
    ) {
        let user = self.users.intern(user_id);
        let ad_category = self.ad_categories.intern(ad_category_id);
        let advertiser = self.advertisers.intern(advertiser_id);
        let campaign = self.campaigns.intern(campaign_id);
        let same_category = organic_category_id == ad_category_id;

        let next_uc = self.user_ad_category_ids.len() as u32;
        let user_ad_category = *self
            .user_ad_category_ids
            .entry((user, ad_category))
            .or_insert(next_uc);

        let key = GroupKey {
            user,
            ad_category,
            advertiser,
            campaign,
            same_category,
        };
        if let Some(&group_index) = self.group_indices.get(&key) {
            let stats = &mut self.groups[group_index];
            stats.impressions += 1;
            if clicked {
                stats.clicks += 1;
            }
        } else {
            let group_index = self.groups.len();
            self.group_indices.insert(key, group_index);
            self.groups.push(GroupStats {
                user,
                ad_category,
                advertiser,
                campaign,
                user_ad_category,
                same_category,
                impressions: 1,
                clicks: u64::from(clicked),
            });
        }

        self.total_impressions += 1;
        if clicked {
            self.total_clicks += 1;
        }
    }

    fn feature_layout(&self) -> FeatureLayout {
        let user_offset = 2;
        let ad_category_offset = user_offset + self.users.len();
        let advertiser_offset = ad_category_offset + self.ad_categories.len();
        let campaign_offset = advertiser_offset + self.advertisers.len();
        let user_ad_category_offset = campaign_offset + self.campaigns.len();
        let total_features = user_ad_category_offset + self.user_ad_category_ids.len();
        FeatureLayout {
            user_offset,
            ad_category_offset,
            advertiser_offset,
            campaign_offset,
            user_ad_category_offset,
            total_features,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct FeatureLayout {
    user_offset: usize,
    ad_category_offset: usize,
    advertiser_offset: usize,
    campaign_offset: usize,
    user_ad_category_offset: usize,
    total_features: usize,
}

#[derive(Clone, Copy, Debug)]
struct IndexedGroup {
    features: [usize; 5],
    same_category: bool,
    impressions: f64,
    clicks: f64,
}

/// L2-regularized logistic regression model trained from impression-click logs.
#[derive(Clone, Debug)]
pub struct LogisticRegressionModel {
    intercept: f64,
    same_category_weight: f64,
    user_ids: HashMap<String, u32>,
    user_weights: Vec<f64>,
    ad_category_ids: HashMap<String, u32>,
    ad_category_weights: Vec<f64>,
    advertiser_ids: HashMap<String, u32>,
    advertiser_weights: Vec<f64>,
    campaign_ids: HashMap<String, u32>,
    campaign_weights: Vec<f64>,
    user_ad_category_ids: HashMap<(u32, u32), u32>,
    user_ad_category_weights: Vec<f64>,
    summary: TrainingSummary,
}

impl LogisticRegressionModel {
    /// Create an untrained neutral model that predicts 0.5 for every candidate.
    pub fn untrained() -> Self {
        Self {
            intercept: 0.0,
            same_category_weight: 0.0,
            user_ids: HashMap::new(),
            user_weights: Vec::new(),
            ad_category_ids: HashMap::new(),
            ad_category_weights: Vec::new(),
            advertiser_ids: HashMap::new(),
            advertiser_weights: Vec::new(),
            campaign_ids: HashMap::new(),
            campaign_weights: Vec::new(),
            user_ad_category_ids: HashMap::new(),
            user_ad_category_weights: Vec::new(),
            summary: TrainingSummary::default(),
        }
    }

    /// Cold-train a fresh logistic regression model from in-memory impression-click records.
    pub fn train_from_records<'a>(
        records: impl IntoIterator<Item = &'a ImpressionClickRecord>,
        config: TrainingConfig,
    ) -> io::Result<Self> {
        config.validate()?;
        let mut accumulator = DatasetAccumulator::default();
        for record in records {
            accumulator.observe(
                &record.user_id,
                &record.organic_category_id,
                &record.campaign_id,
                &record.advertiser_id,
                &record.ad_category_id,
                record.clicked,
            );
        }
        Ok(Self::fit_accumulator(accumulator, config))
    }

    /// Cold-train a fresh logistic regression model from specific logged day files on disk.
    pub fn train_from_days(
        store: &ImpressionLogStore,
        days: impl IntoIterator<Item = usize>,
        config: TrainingConfig,
    ) -> io::Result<Self> {
        config.validate()?;
        let mut accumulator = DatasetAccumulator::default();
        for day in days {
            store.for_each_record_in_day(day, |record| {
                accumulator.observe(
                    &record.user_id,
                    &record.organic_category_id,
                    &record.campaign_id,
                    &record.advertiser_id,
                    &record.ad_category_id,
                    record.clicked,
                );
            })?;
        }
        Ok(Self::fit_accumulator(accumulator, config))
    }

    /// Cold-train a fresh logistic regression model from all logged days `1..=through_day` on disk.
    pub fn train_from_log_store(
        store: &ImpressionLogStore,
        through_day: usize,
        config: TrainingConfig,
    ) -> io::Result<Self> {
        Self::train_from_days(store, 1..=through_day, config)
    }

    fn fit_accumulator(accumulator: DatasetAccumulator, config: TrainingConfig) -> Self {
        if accumulator.total_impressions == 0 {
            return Self::untrained();
        }

        let layout = accumulator.feature_layout();
        let indexed_groups: Vec<IndexedGroup> = accumulator
            .groups
            .iter()
            .map(|group| IndexedGroup {
                features: [
                    layout.user_offset + group.user as usize,
                    layout.ad_category_offset + group.ad_category as usize,
                    layout.advertiser_offset + group.advertiser as usize,
                    layout.campaign_offset + group.campaign as usize,
                    layout.user_ad_category_offset + group.user_ad_category as usize,
                ],
                same_category: group.same_category,
                impressions: group.impressions as f64,
                clicks: group.clicks as f64,
            })
            .collect();

        let (weights, iterations, converged, data_loss) = solve_lbfgs(
            &indexed_groups,
            layout.total_features,
            accumulator.total_impressions,
            accumulator.total_clicks,
            config,
        );

        let intercept = weights[INTERCEPT_INDEX];
        let same_category_weight = weights[SAME_CATEGORY_INDEX];
        let user_weights = weights[layout.user_offset..layout.ad_category_offset].to_vec();
        let ad_category_weights =
            weights[layout.ad_category_offset..layout.advertiser_offset].to_vec();
        let advertiser_weights = weights[layout.advertiser_offset..layout.campaign_offset].to_vec();
        let campaign_weights =
            weights[layout.campaign_offset..layout.user_ad_category_offset].to_vec();
        let user_ad_category_weights =
            weights[layout.user_ad_category_offset..layout.total_features].to_vec();

        let summary = TrainingSummary {
            impressions: accumulator.total_impressions,
            clicks: accumulator.total_clicks,
            unique_groups: indexed_groups.len(),
            feature_count: layout.total_features,
            iterations,
            converged,
            mean_log_loss: data_loss / accumulator.total_impressions as f64,
        };

        Self {
            intercept,
            same_category_weight,
            user_ids: accumulator.users.into_map(),
            user_weights,
            ad_category_ids: accumulator.ad_categories.into_map(),
            ad_category_weights,
            advertiser_ids: accumulator.advertisers.into_map(),
            advertiser_weights,
            campaign_ids: accumulator.campaigns.into_map(),
            campaign_weights,
            user_ad_category_ids: accumulator.user_ad_category_ids,
            user_ad_category_weights,
            summary,
        }
    }

    pub fn summary(&self) -> &TrainingSummary {
        &self.summary
    }

    pub fn is_trained(&self) -> bool {
        self.summary.impressions > 0
    }

    pub fn intercept(&self) -> f64 {
        self.intercept
    }

    pub fn same_category_weight(&self) -> f64 {
        self.same_category_weight
    }

    /// Compute the linear log-odds (logit) score for a candidate ad in the given context.
    pub fn predict_logit(&self, context: &RankingContext<'_>, candidate: &AdCandidate<'_>) -> f64 {
        let mut logit = self.intercept;
        if context.organic_category_id == candidate.ad_category_id {
            logit += self.same_category_weight;
        }
        let user_idx = self.user_ids.get(context.user_id).copied();
        if let Some(u) = user_idx {
            logit += self.user_weights[u as usize];
        }
        let ad_cat_idx = self.ad_category_ids.get(candidate.ad_category_id).copied();
        if let Some(c) = ad_cat_idx {
            logit += self.ad_category_weights[c as usize];
        }
        if let Some(&a) = self.advertiser_ids.get(candidate.advertiser_id) {
            logit += self.advertiser_weights[a as usize];
        }
        if let Some(&k) = self.campaign_ids.get(candidate.campaign_id) {
            logit += self.campaign_weights[k as usize];
        }
        if let (Some(u), Some(c)) = (user_idx, ad_cat_idx)
            && let Some(&uc) = self.user_ad_category_ids.get(&(u, c))
        {
            logit += self.user_ad_category_weights[uc as usize];
        }
        logit
    }

    /// Pre-index known users, categories, and candidate campaigns for fast repeated scoring.
    pub fn prepare_ranker(
        &self,
        user_ids: &[&str],
        category_ids: &[&str],
        candidates: &[AdCandidate<'_>],
    ) -> PreparedRanker {
        let category_index: HashMap<&str, usize> = category_ids
            .iter()
            .enumerate()
            .map(|(index, &id)| (id, index))
            .collect();

        let user_base: Vec<f64> = user_ids
            .iter()
            .map(|&id| {
                let user_weight = self
                    .user_ids
                    .get(id)
                    .map(|&u| self.user_weights[u as usize])
                    .unwrap_or(0.0);
                self.intercept + user_weight
            })
            .collect();

        let campaign_base: Vec<f64> = candidates
            .iter()
            .map(|candidate| {
                let mut base = 0.0;
                if let Some(&c) = self.ad_category_ids.get(candidate.ad_category_id) {
                    base += self.ad_category_weights[c as usize];
                }
                if let Some(&a) = self.advertiser_ids.get(candidate.advertiser_id) {
                    base += self.advertiser_weights[a as usize];
                }
                if let Some(&k) = self.campaign_ids.get(candidate.campaign_id) {
                    base += self.campaign_weights[k as usize];
                }
                base
            })
            .collect();

        let campaign_category: Vec<Option<usize>> = candidates
            .iter()
            .map(|candidate| category_index.get(candidate.ad_category_id).copied())
            .collect();

        let model_cat_to_ext: Vec<Option<usize>> = {
            let mut mapping = vec![None; self.ad_category_weights.len()];
            for (ext_idx, &cat_id) in category_ids.iter().enumerate() {
                if let Some(&model_cat) = self.ad_category_ids.get(cat_id) {
                    mapping[model_cat as usize] = Some(ext_idx);
                }
            }
            mapping
        };

        let num_users = user_ids.len();
        let num_categories = category_ids.len();
        let use_dense = num_users
            .checked_mul(num_categories)
            .is_some_and(|cells| cells <= DENSE_USER_CATEGORY_LIMIT);

        let user_category_weights = if use_dense {
            let mut table = vec![0.0; num_users * num_categories];
            for (ext_user, &user_id) in user_ids.iter().enumerate() {
                if let Some(&model_user) = self.user_ids.get(user_id) {
                    let row_start = ext_user * num_categories;
                    for (model_cat, maybe_ext_cat) in model_cat_to_ext.iter().enumerate() {
                        if let Some(ext_cat) = *maybe_ext_cat
                            && let Some(&uc) = self
                                .user_ad_category_ids
                                .get(&(model_user, model_cat as u32))
                        {
                            table[row_start + ext_cat] = self.user_ad_category_weights[uc as usize];
                        }
                    }
                }
            }
            UserCategoryTable::Dense {
                num_categories,
                weights: table,
            }
        } else {
            let mut rows = vec![Vec::new(); num_users];
            for (ext_user, &user_id) in user_ids.iter().enumerate() {
                if let Some(&model_user) = self.user_ids.get(user_id) {
                    let row = &mut rows[ext_user];
                    for (model_cat, maybe_ext_cat) in model_cat_to_ext.iter().enumerate() {
                        if let Some(ext_cat) = *maybe_ext_cat
                            && let Some(&uc) = self
                                .user_ad_category_ids
                                .get(&(model_user, model_cat as u32))
                        {
                            let weight = self.user_ad_category_weights[uc as usize];
                            if weight != 0.0 {
                                row.push((ext_cat, weight));
                            }
                        }
                    }
                    row.sort_unstable_by_key(|&(ext_cat, _)| ext_cat);
                }
            }
            UserCategoryTable::Sparse { rows }
        };

        PreparedRanker {
            same_category_weight: self.same_category_weight,
            user_base,
            campaign_base,
            campaign_category,
            user_category_weights,
        }
    }
}

impl CtrModel for LogisticRegressionModel {
    fn predict_ctr(&self, context: &RankingContext<'_>, candidate: &AdCandidate<'_>) -> f64 {
        sigmoid(self.predict_logit(context, candidate))
    }
}

#[derive(Clone, Debug)]
enum UserCategoryTable {
    Dense {
        num_categories: usize,
        weights: Vec<f64>,
    },
    Sparse {
        rows: Vec<Vec<(usize, f64)>>,
    },
}

/// Pre-indexed scorer for fast repeated candidate ranking over fixed user/category/campaign tables.
#[derive(Clone, Debug)]
pub struct PreparedRanker {
    same_category_weight: f64,
    user_base: Vec<f64>,
    campaign_base: Vec<f64>,
    campaign_category: Vec<Option<usize>>,
    user_category_weights: UserCategoryTable,
}

impl PreparedRanker {
    /// Score the log-odds (monotone in predicted CTR) for `(user_idx, organic_cat_idx, campaign_idx)`.
    #[inline]
    pub fn score_logit(&self, user_idx: usize, organic_cat_idx: usize, campaign_idx: usize) -> f64 {
        let mut logit = self.user_base[user_idx] + self.campaign_base[campaign_idx];
        if let Some(ad_cat_idx) = self.campaign_category[campaign_idx] {
            if ad_cat_idx == organic_cat_idx {
                logit += self.same_category_weight;
            }
            logit += self.user_category_weight(user_idx, ad_cat_idx);
        }
        logit
    }

    /// Score the predicted CTR in `(0, 1)` for `(user_idx, organic_cat_idx, campaign_idx)`.
    #[inline]
    pub fn score_ctr(&self, user_idx: usize, organic_cat_idx: usize, campaign_idx: usize) -> f64 {
        sigmoid(self.score_logit(user_idx, organic_cat_idx, campaign_idx))
    }

    #[inline]
    fn user_category_weight(&self, user_idx: usize, ad_cat_idx: usize) -> f64 {
        match &self.user_category_weights {
            UserCategoryTable::Dense {
                num_categories,
                weights,
            } => weights[user_idx * *num_categories + ad_cat_idx],
            UserCategoryTable::Sparse { rows } => rows[user_idx]
                .binary_search_by_key(&ad_cat_idx, |&(cat, _)| cat)
                .map(|pos| rows[user_idx][pos].1)
                .unwrap_or(0.0),
        }
    }
}

fn solve_lbfgs(
    groups: &[IndexedGroup],
    num_features: usize,
    total_impressions: u64,
    total_clicks: u64,
    config: TrainingConfig,
) -> (Vec<f64>, usize, bool, f64) {
    let mut weights = vec![0.0; num_features];

    // Degenerate all-negative or all-positive datasets have optimal non-intercept weights = 0.
    if total_clicks == 0 || total_clicks == total_impressions {
        let smoothed_p = (total_clicks as f64 + 0.5) / (total_impressions as f64 + 1.0);
        weights[INTERCEPT_INDEX] = (smoothed_p / (1.0 - smoothed_p)).ln();
        let (data_loss, _) = evaluate_loss_and_grad(
            groups,
            &weights,
            config.l2_penalty,
            &mut vec![0.0; num_features],
        );
        return (weights, 0, true, data_loss);
    }

    // Cold-start intercept at the empirical log-odds, all other weights at 0.
    let prior_p = total_clicks as f64 / total_impressions as f64;
    weights[INTERCEPT_INDEX] = (prior_p / (1.0 - prior_p)).ln();

    let mut grad = vec![0.0; num_features];
    let (mut data_loss, mut loss) =
        evaluate_loss_and_grad(groups, &weights, config.l2_penalty, &mut grad);

    let mut history: VecDeque<(Vec<f64>, Vec<f64>, f64)> =
        VecDeque::with_capacity(config.history_size);
    let mut next_weights = vec![0.0; num_features];
    let mut next_grad = vec![0.0; num_features];
    let mut direction = vec![0.0; num_features];
    let mut q = vec![0.0; num_features];
    let mut alphas = vec![0.0; config.history_size];

    let mut iterations = 0;
    let mut converged = inf_norm(&grad) <= config.gradient_tolerance;

    while !converged && iterations < config.max_iterations {
        iterations += 1;

        compute_lbfgs_direction(&grad, &history, &mut q, &mut alphas, &mut direction);
        let mut dir_deriv = dot(&grad, &direction);
        if !dir_deriv.is_finite() || dir_deriv >= 0.0 {
            history.clear();
            let scale = inf_norm(&grad).max(1.0);
            for (d, &g) in direction.iter_mut().zip(&grad) {
                *d = -g / scale;
            }
            dir_deriv = dot(&grad, &direction);
            if dir_deriv >= 0.0 {
                break;
            }
        }

        // Armijo backtracking line search.
        let mut step = 1.0;
        let c1 = 1e-4;
        let mut accepted = false;
        let mut candidate_data_loss = data_loss;
        let mut candidate_loss = loss;

        for _ in 0..40 {
            for i in 0..num_features {
                next_weights[i] = weights[i] + step * direction[i];
            }
            let (dl, total_l) = evaluate_loss_only(groups, &next_weights, config.l2_penalty);
            if total_l.is_finite() && total_l <= loss + c1 * step * dir_deriv {
                candidate_data_loss = dl;
                candidate_loss = total_l;
                accepted = true;
                break;
            }
            step *= 0.5;
        }

        if !accepted {
            if !history.is_empty() {
                history.clear();
                continue;
            }
            break;
        }

        let (dl, total_l) =
            evaluate_loss_and_grad(groups, &next_weights, config.l2_penalty, &mut next_grad);
        debug_assert!((dl - candidate_data_loss).abs() <= 1e-6 * (1.0 + dl.abs()));
        debug_assert!((total_l - candidate_loss).abs() <= 1e-6 * (1.0 + total_l.abs()));

        let mut s = vec![0.0; num_features];
        let mut y = vec![0.0; num_features];
        let mut sy = 0.0;
        let mut ss = 0.0;
        let mut yy = 0.0;
        for i in 0..num_features {
            let si = next_weights[i] - weights[i];
            let yi = next_grad[i] - grad[i];
            s[i] = si;
            y[i] = yi;
            sy += si * yi;
            ss += si * si;
            yy += yi * yi;
        }

        let prev_loss = loss;
        std::mem::swap(&mut weights, &mut next_weights);
        std::mem::swap(&mut grad, &mut next_grad);
        data_loss = dl;
        loss = total_l;

        if sy > 1e-12 * (ss * yy).sqrt() {
            if history.len() == config.history_size {
                history.pop_front();
            }
            history.push_back((s, y, 1.0 / sy));
        }

        let grad_inf = inf_norm(&grad);
        let rel_improvement = (prev_loss - loss).abs() / prev_loss.abs().max(1.0);
        if grad_inf <= config.gradient_tolerance
            || rel_improvement <= config.relative_loss_tolerance
        {
            converged = true;
            break;
        }
    }

    (weights, iterations, converged, data_loss)
}

fn compute_lbfgs_direction(
    grad: &[f64],
    history: &VecDeque<(Vec<f64>, Vec<f64>, f64)>,
    q: &mut [f64],
    alphas: &mut [f64],
    direction: &mut [f64],
) {
    if history.is_empty() {
        let scale = inf_norm(grad).max(1.0);
        for (d, &g) in direction.iter_mut().zip(grad) {
            *d = -g / scale;
        }
        return;
    }

    q.copy_from_slice(grad);
    let m = history.len();
    for i in (0..m).rev() {
        let (s, y, rho) = &history[i];
        let alpha = rho * dot(s, q);
        alphas[i] = alpha;
        for (qj, &yj) in q.iter_mut().zip(y) {
            *qj -= alpha * yj;
        }
    }

    let (last_s, last_y, _) = history.back().expect("history is non-empty");
    let yy = dot(last_y, last_y);
    let gamma = if yy > 0.0 {
        (dot(last_s, last_y) / yy).clamp(1e-8, 1e8)
    } else {
        1.0
    };

    for (r, &qj) in direction.iter_mut().zip(q.iter()) {
        *r = gamma * qj;
    }

    for i in 0..m {
        let (s, y, rho) = &history[i];
        let beta = rho * dot(y, direction);
        let coeff = alphas[i] - beta;
        for (rj, &sj) in direction.iter_mut().zip(s) {
            *rj += sj * coeff;
        }
    }

    for d in direction.iter_mut() {
        *d = -*d;
    }
}

fn evaluate_loss_only(groups: &[IndexedGroup], weights: &[f64], l2_penalty: f64) -> (f64, f64) {
    let intercept = weights[INTERCEPT_INDEX];
    let same_weight = weights[SAME_CATEGORY_INDEX];
    let mut data_loss = 0.0;
    for group in groups {
        let mut z = intercept;
        if group.same_category {
            z += same_weight;
        }
        for &feature in &group.features {
            z += weights[feature];
        }
        data_loss += group.impressions * log1p_exp(z) - group.clicks * z;
    }
    let reg = 0.5 * l2_penalty * weights[1..].iter().map(|&w| w * w).sum::<f64>();
    (data_loss, data_loss + reg)
}

fn evaluate_loss_and_grad(
    groups: &[IndexedGroup],
    weights: &[f64],
    l2_penalty: f64,
    grad: &mut [f64],
) -> (f64, f64) {
    grad.fill(0.0);
    let intercept = weights[INTERCEPT_INDEX];
    let same_weight = weights[SAME_CATEGORY_INDEX];
    let mut data_loss = 0.0;
    for group in groups {
        let mut z = intercept;
        if group.same_category {
            z += same_weight;
        }
        for &feature in &group.features {
            z += weights[feature];
        }
        data_loss += group.impressions * log1p_exp(z) - group.clicks * z;
        let residual = group.impressions * sigmoid(z) - group.clicks;
        grad[INTERCEPT_INDEX] += residual;
        if group.same_category {
            grad[SAME_CATEGORY_INDEX] += residual;
        }
        for &feature in &group.features {
            grad[feature] += residual;
        }
    }

    let mut reg_sq = 0.0;
    for i in 1..weights.len() {
        let w = weights[i];
        reg_sq += w * w;
        grad[i] += l2_penalty * w;
    }
    let reg = 0.5 * l2_penalty * reg_sq;
    (data_loss, data_loss + reg)
}

#[inline]
fn sigmoid(z: f64) -> f64 {
    if z >= 0.0 {
        1.0 / (1.0 + (-z).exp())
    } else {
        let ez = z.exp();
        ez / (1.0 + ez)
    }
}

#[inline]
fn log1p_exp(z: f64) -> f64 {
    if z > 0.0 {
        z + (-z).exp().ln_1p()
    } else {
        z.exp().ln_1p()
    }
}

#[inline]
fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(&x, &y)| x * y).sum()
}

#[inline]
fn inf_norm(v: &[f64]) -> f64 {
    v.iter().fold(0.0_f64, |acc, &x| acc.max(x.abs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(
        day: usize,
        user_id: &str,
        organic_category_id: &str,
        campaign_id: &str,
        advertiser_id: &str,
        ad_category_id: &str,
        clicked: bool,
    ) -> ImpressionClickRecord {
        ImpressionClickRecord {
            day,
            user_id: user_id.to_owned(),
            organic_item_id: format!("org-{organic_category_id}"),
            organic_category_id: organic_category_id.to_owned(),
            campaign_id: campaign_id.to_owned(),
            advertiser_id: advertiser_id.to_owned(),
            ad_item_id: format!("item-{campaign_id}"),
            ad_category_id: ad_category_id.to_owned(),
            ad_price_cents: 5_000,
            cpc_cents: 100,
            clicked,
        }
    }

    #[test]
    fn untrained_and_zero_click_models_behave_cleanly() {
        let untrained = LogisticRegressionModel::untrained();
        assert!(!untrained.is_trained());
        let ctx = RankingContext {
            user_id: "u1",
            organic_category_id: "cat-1",
        };
        let ad = AdCandidate {
            campaign_id: "c1",
            advertiser_id: "a1",
            ad_item_id: "i1",
            ad_category_id: "cat-1",
            ad_price_cents: 1_000,
        };
        assert_eq!(untrained.predict_ctr(&ctx, &ad), 0.5);

        let zero_clicks = vec![
            sample_record(1, "u1", "cat-1", "c1", "a1", "cat-1", false),
            sample_record(1, "u2", "cat-2", "c2", "a2", "cat-3", false),
        ];
        let model =
            LogisticRegressionModel::train_from_records(&zero_clicks, TrainingConfig::default())
                .unwrap();
        assert!(model.is_trained());
        assert!(model.summary().converged);
        let p1 = model.predict_ctr(&ctx, &ad);
        assert!(p1 > 0.0 && p1 < 0.5);
        assert_eq!(model.same_category_weight(), 0.0);
    }

    #[test]
    fn analytic_gradient_matches_finite_differences_and_reaches_optimum() {
        let mut records = Vec::new();
        for _ in 0..40 {
            records.push(sample_record(
                1, "u1", "cat-a", "camp-a", "adv-1", "cat-a", false,
            ));
        }
        for _ in 0..8 {
            records.push(sample_record(
                1, "u1", "cat-a", "camp-a", "adv-1", "cat-a", true,
            ));
        }
        for _ in 0..50 {
            records.push(sample_record(
                1, "u1", "cat-a", "camp-b", "adv-2", "cat-b", false,
            ));
        }
        for _ in 0..35 {
            records.push(sample_record(
                1, "u2", "cat-b", "camp-b", "adv-2", "cat-b", false,
            ));
        }
        for _ in 0..5 {
            records.push(sample_record(
                1, "u2", "cat-b", "camp-b", "adv-2", "cat-b", true,
            ));
        }

        let mut acc = DatasetAccumulator::default();
        for r in &records {
            acc.observe(
                &r.user_id,
                &r.organic_category_id,
                &r.campaign_id,
                &r.advertiser_id,
                &r.ad_category_id,
                r.clicked,
            );
        }
        let layout = acc.feature_layout();
        let groups: Vec<IndexedGroup> = acc
            .groups
            .iter()
            .map(|g| IndexedGroup {
                features: [
                    layout.user_offset + g.user as usize,
                    layout.ad_category_offset + g.ad_category as usize,
                    layout.advertiser_offset + g.advertiser as usize,
                    layout.campaign_offset + g.campaign as usize,
                    layout.user_ad_category_offset + g.user_ad_category as usize,
                ],
                same_category: g.same_category,
                impressions: g.impressions as f64,
                clicks: g.clicks as f64,
            })
            .collect();

        // Check analytic gradient against central finite differences at a non-zero point.
        let test_w: Vec<f64> = (0..layout.total_features)
            .map(|i| -0.3 + 0.07 * (i as f64))
            .collect();
        let mut grad = vec![0.0; layout.total_features];
        evaluate_loss_and_grad(&groups, &test_w, 1.0, &mut grad);
        let eps = 1e-6;
        for j in 0..layout.total_features {
            let mut w_plus = test_w.clone();
            let mut w_minus = test_w.clone();
            w_plus[j] += eps;
            w_minus[j] -= eps;
            let (_, l_plus) = evaluate_loss_only(&groups, &w_plus, 1.0);
            let (_, l_minus) = evaluate_loss_only(&groups, &w_minus, 1.0);
            let fd = (l_plus - l_minus) / (2.0 * eps);
            assert!(
                (grad[j] - fd).abs() < 1e-6,
                "gradient mismatch at {j}: analytic={} fd={fd}",
                grad[j]
            );
        }

        // Check KKT stationarity at the L-BFGS solution.
        let config = TrainingConfig {
            gradient_tolerance: 1e-7,
            relative_loss_tolerance: 0.0,
            ..TrainingConfig::default()
        };
        let (w_opt, _, converged, _) = solve_lbfgs(
            &groups,
            layout.total_features,
            acc.total_impressions,
            acc.total_clicks,
            config,
        );
        assert!(converged);
        let mut opt_grad = vec![0.0; layout.total_features];
        evaluate_loss_and_grad(&groups, &w_opt, config.l2_penalty, &mut opt_grad);
        assert!(
            inf_norm(&opt_grad) < 1e-6,
            "expected near-zero gradient at optimum, got {}",
            inf_norm(&opt_grad)
        );
    }

    #[test]
    fn model_learns_same_category_and_user_category_preferences_and_matches_prepared_ranker() {
        let temp = tempfile::tempdir().unwrap();
        let store = ImpressionLogStore::new(temp.path());
        store.clear().unwrap();

        // Day 1: u1 clicks cat-a (even when browsing cat-c); u2 clicks cat-b.
        let mut day1 = store.create_day_writer(1).unwrap();
        for i in 0..100 {
            let clicked = i % 10 == 0;
            day1.write(&sample_record(
                1, "u1", "cat-c", "camp-a", "adv-a", "cat-a", clicked,
            ))
            .unwrap();
            day1.write(&sample_record(
                1, "u1", "cat-c", "camp-b", "adv-b", "cat-b", false,
            ))
            .unwrap();
        }
        day1.finish().unwrap();

        // Day 2: u2 clicks cat-b, ignores cat-a; plus same-category clicks for u3.
        let mut day2 = store.create_day_writer(2).unwrap();
        for i in 0..100 {
            let clicked = i % 10 == 0;
            day2.write(&sample_record(
                2, "u2", "cat-c", "camp-b", "adv-b", "cat-b", clicked,
            ))
            .unwrap();
            day2.write(&sample_record(
                2, "u2", "cat-c", "camp-a", "adv-a", "cat-a", false,
            ))
            .unwrap();
            day2.write(&sample_record(
                2, "u3", "cat-d", "camp-d", "adv-d", "cat-d", clicked,
            ))
            .unwrap();
            day2.write(&sample_record(
                2, "u3", "cat-d", "camp-e", "adv-e", "cat-e", false,
            ))
            .unwrap();
        }
        day2.finish().unwrap();

        assert_eq!(store.available_days().unwrap(), vec![1, 2]);
        assert_eq!(store.read_day(1).unwrap().len(), 200);
        assert_eq!(store.read_through_day(2).unwrap().len(), 600);

        let empty_model =
            LogisticRegressionModel::train_from_log_store(&store, 0, TrainingConfig::default())
                .unwrap();
        assert!(!empty_model.is_trained());

        let day1_only =
            LogisticRegressionModel::train_from_days(&store, [1], TrainingConfig::default())
                .unwrap();
        assert_eq!(day1_only.summary().impressions, 200);
        assert_eq!(day1_only.summary().clicks, 10);

        let model =
            LogisticRegressionModel::train_from_log_store(&store, 2, TrainingConfig::default())
                .unwrap();
        assert_eq!(model.summary().impressions, 600);
        assert_eq!(model.summary().clicks, 30);
        assert!(model.same_category_weight() > 0.0);

        // Cold training again from disk reproduces identical weights.
        let repeat =
            LogisticRegressionModel::train_from_log_store(&store, 2, TrainingConfig::default())
                .unwrap();
        assert_eq!(model.intercept(), repeat.intercept());
        assert_eq!(model.same_category_weight(), repeat.same_category_weight());

        let candidates = [
            AdCandidate {
                campaign_id: "camp-a",
                advertiser_id: "adv-a",
                ad_item_id: "item-camp-a",
                ad_category_id: "cat-a",
                ad_price_cents: 5_000,
            },
            AdCandidate {
                campaign_id: "camp-b",
                advertiser_id: "adv-b",
                ad_item_id: "item-camp-b",
                ad_category_id: "cat-b",
                ad_price_cents: 5_000,
            },
            AdCandidate {
                campaign_id: "camp-unseen",
                advertiser_id: "adv-unseen",
                ad_item_id: "item-unseen",
                ad_category_id: "cat-unseen",
                ad_price_cents: 5_000,
            },
        ];

        let u1_ctx = RankingContext {
            user_id: "u1",
            organic_category_id: "cat-c",
        };
        let u2_ctx = RankingContext {
            user_id: "u2",
            organic_category_id: "cat-c",
        };
        let unseen_ctx = RankingContext {
            user_id: "u-new",
            organic_category_id: "cat-a",
        };

        assert_eq!(model.rank_candidates(&u1_ctx, &candidates)[0], 0);
        assert_eq!(model.rank_candidates(&u2_ctx, &candidates)[0], 1);
        // Unseen user benefits from learned positive same_category_weight on cat-a.
        assert_eq!(model.rank_candidates(&unseen_ctx, &candidates)[0], 0);

        // PreparedRanker must match LogisticRegressionModel::predict_ctr exactly.
        let users = ["u1", "u2", "u3", "u-new"];
        let cats = ["cat-a", "cat-b", "cat-c", "cat-d", "cat-e", "cat-unseen"];
        let prepared = model.prepare_ranker(&users, &cats, &candidates);
        for (u_idx, &user_id) in users.iter().enumerate() {
            for (c_idx, &org_cat) in cats.iter().enumerate() {
                let ctx = RankingContext {
                    user_id,
                    organic_category_id: org_cat,
                };
                for (k_idx, candidate) in candidates.iter().enumerate() {
                    let direct = model.predict_ctr(&ctx, candidate);
                    let fast = prepared.score_ctr(u_idx, c_idx, k_idx);
                    assert!(
                        (direct - fast).abs() < 1e-12,
                        "mismatch for ({user_id}, {org_cat}, {}): direct={direct} fast={fast}",
                        candidate.campaign_id
                    );
                }
            }
        }
    }
}
