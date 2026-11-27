use std::{
    collections::HashMap,
    env,
    ffi::OsStr,
    fs::File,
    io::{self, BufRead, BufReader, BufWriter, Error, ErrorKind, Read, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use serde::{Deserialize, Serialize};

use crate::{
    model::{CtrModel, PreparedScorer, SufficientStatsPayload, TrainingSummary, sigmoid},
    records::{AdCandidate, ImpressionClickRecord, ImpressionLogStore, RankingContext},
};

const DENSE_USER_CATEGORY_LIMIT: usize = 2_000_000;
const MODEL_SCHEMA_VERSION: u32 = 1;
const FEATURE_COUNT: usize = 12;
const DEFAULT_PYTHON_BIN: &str = "python3";
const PYTHON_ENV_VAR: &str = "ADS_ML_PYTHON";
const DEFAULT_XGB_TRAIN_SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/xgboost/train_xgb.py");

/// Hyperparameters for cold-training an XGBoost GBDT CTR model (`Score-XGB`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct XgboostTrainingConfig {
    /// Number of boosting rounds (trees).
    pub num_boost_round: usize,
    /// Maximum depth per decision tree.
    pub max_depth: usize,
    /// Shrinkage / learning rate (`eta`).
    pub learning_rate: f64,
    /// L2 leaf weight regularization (`reg_lambda`).
    pub l2_reg: f64,
    /// Minimum sum of instance Hessian weight needed in a child leaf.
    pub min_child_weight: f64,
    /// Row subsampling ratio in `(0, 1]`.
    pub subsample: f64,
    /// Bayesian smoothing strength (`alpha`) for leave-one-out CTR target encoding.
    pub prior_strength: f64,
    /// Deterministic XGBoost seed.
    pub seed: u64,
}

impl Default for XgboostTrainingConfig {
    fn default() -> Self {
        Self {
            num_boost_round: 60,
            max_depth: 4,
            learning_rate: 0.1,
            l2_reg: 2.0,
            min_child_weight: 0.25,
            subsample: 1.0,
            prior_strength: 10.0,
            seed: 0,
        }
    }
}

impl XgboostTrainingConfig {
    fn validate(&self) -> io::Result<()> {
        if self.num_boost_round == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "num_boost_round must be positive",
            ));
        }
        if self.max_depth == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "max_depth must be positive",
            ));
        }
        if !self.learning_rate.is_finite() || self.learning_rate <= 0.0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "learning_rate must be finite and positive",
            ));
        }
        if !self.l2_reg.is_finite() || self.l2_reg < 0.0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "l2_reg must be finite and non-negative",
            ));
        }
        if !self.min_child_weight.is_finite() || self.min_child_weight < 0.0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "min_child_weight must be finite and non-negative",
            ));
        }
        if !self.subsample.is_finite() || !(self.subsample > 0.0 && self.subsample <= 1.0) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "subsample must be in (0, 1]",
            ));
        }
        if !self.prior_strength.is_finite() || self.prior_strength <= 0.0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "prior_strength must be finite and positive",
            ));
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct XgbWorkerRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    log_dir: Option<&'a Path>,
    #[serde(skip_serializing_if = "Option::is_none")]
    through_day: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    days: Option<&'a [usize]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stats_file: Option<&'a Path>,
    model_out: &'a Path,
    config: XgboostTrainingConfig,
}

#[derive(Deserialize)]
struct XgbWorkerResponse {
    ok: bool,
    error: Option<String>,
}

#[derive(Deserialize)]
struct ExportedTree {
    left_children: Vec<i32>,
    right_children: Vec<i32>,
    split_indices: Vec<usize>,
    split_conditions: Vec<f32>,
    default_left: Vec<bool>,
}

#[derive(Deserialize)]
struct XgbModelArtifact {
    schema_version: u32,
    trainer: String,
    xgboost_version: Option<String>,
    base_margin: f64,
    prior_ctr: f64,
    #[allow(dead_code)]
    prior_strength: f64,
    users: HashMap<String, [f64; 3]>,
    ad_categories: HashMap<String, [f64; 3]>,
    advertisers: HashMap<String, [f64; 3]>,
    campaigns: HashMap<String, [f64; 3]>,
    user_ad_categories: Vec<(String, String, f64, f64)>,
    trees: Vec<ExportedTree>,
    summary: TrainingSummary,
}

/// Compact representation of a single decision tree node for native Rust evaluation.
#[derive(Clone, Copy, Debug)]
struct TreeNode {
    /// `-1` indicates a leaf node whose value is stored in `threshold_or_leaf`.
    left: i32,
    right: i32,
    feature_idx: u8,
    default_left: bool,
    threshold_or_leaf: f32,
}

#[derive(Clone, Debug)]
struct FlatTree {
    nodes: Vec<TreeNode>,
}

impl FlatTree {
    fn from_exported(tree: ExportedTree) -> io::Result<Self> {
        let len = tree.left_children.len();
        if tree.right_children.len() != len
            || tree.split_indices.len() != len
            || tree.split_conditions.len() != len
            || tree.default_left.len() != len
        {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "malformed exported XGBoost tree: mismatched array lengths",
            ));
        }
        let mut nodes = Vec::with_capacity(len);
        for i in 0..len {
            let left = tree.left_children[i];
            let feat = tree.split_indices[i];
            if left != -1 && feat >= FEATURE_COUNT {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    format!("XGBoost split feature index {feat} out of range 0..{FEATURE_COUNT}"),
                ));
            }
            nodes.push(TreeNode {
                left,
                right: tree.right_children[i],
                feature_idx: feat as u8,
                default_left: tree.default_left[i],
                threshold_or_leaf: tree.split_conditions[i],
            });
        }
        Ok(Self { nodes })
    }

    #[inline]
    fn evaluate(&self, features: &[f32; FEATURE_COUNT]) -> f64 {
        if self.nodes.is_empty() {
            return 0.0;
        }
        let mut idx = 0usize;
        loop {
            let node = self.nodes[idx];
            if node.left < 0 {
                return node.threshold_or_leaf as f64;
            }
            let val = features[node.feature_idx as usize];
            idx = if val.is_nan() {
                if node.default_left {
                    node.left as usize
                } else {
                    node.right as usize
                }
            } else if val < node.threshold_or_leaf {
                node.left as usize
            } else {
                node.right as usize
            };
        }
    }
}

/// Persistent XGBoost training worker (`ml/xgboost/train_xgb.py --serve-stdio`).
///
/// Spawning one worker per simulation pays Python/XGBoost import cost once while
/// still cold-training a fresh GBDT booster from scratch on every day.
pub struct XgboostTrainer {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl XgboostTrainer {
    /// Spawn the default XGBoost trainer using `ADS_ML_PYTHON` (or `python3`) and `ml/xgboost/train_xgb.py`.
    pub fn spawn() -> io::Result<Self> {
        let python_bin = env::var_os(PYTHON_ENV_VAR).unwrap_or_else(|| DEFAULT_PYTHON_BIN.into());
        Self::spawn_with(python_bin, Path::new(DEFAULT_XGB_TRAIN_SCRIPT))
    }

    /// Spawn an XGBoost trainer with an explicit Python executable and training script path.
    pub fn spawn_with(
        python_bin: impl AsRef<OsStr>,
        script_path: impl AsRef<Path>,
    ) -> io::Result<Self> {
        let script = script_path.as_ref();
        if !script.exists() {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("XGBoost training script not found at {}", script.display()),
            ));
        }

        let mut child = Command::new(python_bin)
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .arg(script)
            .arg("--serve-stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::other("failed to capture XGBoost trainer stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::other("failed to capture XGBoost trainer stdout"))?;

        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
        })
    }

    /// Cold-train a fresh XGBoost CTR model on specific logged days and save the exported artifact to `model_out`.
    pub fn train_from_days(
        &mut self,
        store: &ImpressionLogStore,
        days: impl IntoIterator<Item = usize>,
        model_out: &Path,
        config: XgboostTrainingConfig,
    ) -> io::Result<XgboostCtrModel> {
        config.validate()?;
        let day_list: Vec<usize> = days.into_iter().collect();
        if day_list.is_empty() {
            return Ok(XgboostCtrModel::untrained());
        }
        let request = XgbWorkerRequest {
            log_dir: Some(store.root()),
            through_day: None,
            days: Some(&day_list),
            stats_file: None,
            model_out,
            config,
        };
        self.send_request(&request)?;
        XgboostCtrModel::load_from_file(model_out)
    }

    /// Cold-train a fresh XGBoost CTR model on all logged days `1..=through_day`.
    pub fn train_from_log_store(
        &mut self,
        store: &ImpressionLogStore,
        through_day: usize,
        model_out: &Path,
        config: XgboostTrainingConfig,
    ) -> io::Result<XgboostCtrModel> {
        config.validate()?;
        if through_day == 0 {
            return Ok(XgboostCtrModel::untrained());
        }
        let request = XgbWorkerRequest {
            log_dir: Some(store.root()),
            through_day: Some(through_day),
            days: None,
            stats_file: None,
            model_out,
            config,
        };
        self.send_request(&request)?;
        XgboostCtrModel::load_from_file(model_out)
    }

    /// Cold-train a fresh XGBoost CTR model from exact pre-aggregated sufficient statistics.
    pub fn train_from_sufficient_stats(
        &mut self,
        payload: &SufficientStatsPayload<'_>,
        stats_file: &Path,
        model_out: &Path,
        config: XgboostTrainingConfig,
    ) -> io::Result<XgboostCtrModel> {
        config.validate()?;
        if payload.groups.is_empty() {
            return Ok(XgboostCtrModel::untrained());
        }
        if let Some(parent) = stats_file.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        {
            let file = File::create(stats_file)?;
            let mut writer = BufWriter::new(file);
            serde_json::to_writer(&mut writer, payload)
                .map_err(|err| Error::new(ErrorKind::InvalidInput, err))?;
            writer.flush()?;
        }
        let request = XgbWorkerRequest {
            log_dir: None,
            through_day: None,
            days: None,
            stats_file: Some(stats_file),
            model_out,
            config,
        };
        self.send_request(&request)?;
        XgboostCtrModel::load_from_file(model_out)
    }

    fn send_request(&mut self, request: &XgbWorkerRequest<'_>) -> io::Result<()> {
        let payload = serde_json::to_string(request)
            .map_err(|err| Error::new(ErrorKind::InvalidInput, err))?;
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| Error::new(ErrorKind::BrokenPipe, "XGBoost trainer stdin is closed"))?;
        if let Err(err) = writeln!(stdin, "{payload}").and_then(|()| stdin.flush()) {
            let detail = self.read_stderr_detail();
            return Err(Error::new(
                err.kind(),
                format!("failed to send request to XGBoost trainer: {err}{detail}"),
            ));
        }

        let mut response_line = String::new();
        let bytes = self.stdout.read_line(&mut response_line)?;
        if bytes == 0 {
            let detail = self.read_stderr_detail();
            return Err(Error::new(
                ErrorKind::UnexpectedEof,
                format!("XGBoost trainer exited before replying{detail}"),
            ));
        }

        let response: XgbWorkerResponse = serde_json::from_str(response_line.trim())
            .map_err(|err| Error::new(ErrorKind::InvalidData, err))?;
        if !response.ok {
            let message = response
                .error
                .unwrap_or_else(|| "unknown XGBoost training error".to_owned());
            return Err(Error::other(format!("XGBoost trainer failed: {message}")));
        }
        Ok(())
    }

    fn read_stderr_detail(&mut self) -> String {
        self.stdin.take();
        let _ = self.child.wait();
        let mut stderr_buf = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            let _ = stderr.read_to_string(&mut stderr_buf);
        }
        let trimmed = stderr_buf.trim();
        if trimmed.is_empty() {
            String::new()
        } else {
            format!("\nXGBoost trainer stderr:\n{trimmed}")
        }
    }
}

impl Drop for XgboostTrainer {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.wait();
    }
}

#[derive(Clone, Copy, Debug)]
struct EntityStat {
    ctr: f32,
    log_imp: f32,
    id_code: f32,
}

/// XGBoost gradient-boosted trees CTR model (`Score-XGB`) evaluated natively in Rust.
#[derive(Clone, Debug)]
pub struct XgboostCtrModel {
    trainer: String,
    xgboost_version: Option<String>,
    base_margin: f64,
    prior_ctr: f32,
    user_ids: HashMap<String, u32>,
    user_stats: Vec<EntityStat>,
    ad_category_ids: HashMap<String, u32>,
    ad_category_stats: Vec<EntityStat>,
    advertiser_ids: HashMap<String, u32>,
    advertiser_stats: Vec<EntityStat>,
    campaign_ids: HashMap<String, u32>,
    campaign_stats: Vec<EntityStat>,
    user_ad_category_stats: HashMap<(u32, u32), (f32, f32)>,
    trees: Vec<FlatTree>,
    summary: TrainingSummary,
}

impl XgboostCtrModel {
    /// Create an untrained neutral model predicting 0.5 for every candidate.
    pub fn untrained() -> Self {
        Self {
            trainer: "xgboost".to_owned(),
            xgboost_version: None,
            base_margin: 0.0,
            prior_ctr: 0.5,
            user_ids: HashMap::new(),
            user_stats: Vec::new(),
            ad_category_ids: HashMap::new(),
            ad_category_stats: Vec::new(),
            advertiser_ids: HashMap::new(),
            advertiser_stats: Vec::new(),
            campaign_ids: HashMap::new(),
            campaign_stats: Vec::new(),
            user_ad_category_stats: HashMap::new(),
            trees: Vec::new(),
            summary: TrainingSummary::default(),
        }
    }

    /// Load an XGBoost-exported JSON model artifact from disk.
    pub fn load_from_file(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let artifact: XgbModelArtifact = serde_json::from_reader(BufReader::new(file))
            .map_err(|err| Error::new(ErrorKind::InvalidData, err))?;
        if artifact.schema_version != MODEL_SCHEMA_VERSION {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "unsupported XGBoost model schema version {} (expected {})",
                    artifact.schema_version, MODEL_SCHEMA_VERSION
                ),
            ));
        }

        let (user_ids, user_stats) = index_entity_stats(artifact.users);
        let (ad_category_ids, ad_category_stats) = index_entity_stats(artifact.ad_categories);
        let (advertiser_ids, advertiser_stats) = index_entity_stats(artifact.advertisers);
        let (campaign_ids, campaign_stats) = index_entity_stats(artifact.campaigns);

        let mut user_ad_category_stats = HashMap::with_capacity(artifact.user_ad_categories.len());
        for (user_id, ad_cat_id, ctr, log_imp) in artifact.user_ad_categories {
            if let (Some(&u), Some(&c)) = (user_ids.get(&user_id), ad_category_ids.get(&ad_cat_id))
            {
                user_ad_category_stats.insert((u, c), (ctr as f32, log_imp as f32));
            }
        }

        let mut trees = Vec::with_capacity(artifact.trees.len());
        for raw_tree in artifact.trees {
            trees.push(FlatTree::from_exported(raw_tree)?);
        }

        Ok(Self {
            trainer: artifact.trainer,
            xgboost_version: artifact.xgboost_version,
            base_margin: artifact.base_margin,
            prior_ctr: artifact.prior_ctr as f32,
            user_ids,
            user_stats,
            ad_category_ids,
            ad_category_stats,
            advertiser_ids,
            advertiser_stats,
            campaign_ids,
            campaign_stats,
            user_ad_category_stats,
            trees,
            summary: artifact.summary,
        })
    }

    /// Convenience one-shot helper that writes `records` to a temporary log store and cold-trains via XGBoost.
    pub fn train_from_records<'a>(
        records: impl IntoIterator<Item = &'a ImpressionClickRecord>,
        config: XgboostTrainingConfig,
    ) -> io::Result<Self> {
        config.validate()?;
        let temp = tempfile::tempdir()?;
        let store = ImpressionLogStore::new(temp.path());
        let mut writer = store.create_day_writer(1)?;
        let mut count = 0u64;
        for record in records {
            writer.write(record)?;
            count += 1;
        }
        writer.finish()?;
        if count == 0 {
            return Ok(Self::untrained());
        }
        let mut trainer = XgboostTrainer::spawn()?;
        let model_out = temp.path().join("model.json");
        trainer.train_from_days(&store, [1], &model_out, config)
    }

    /// Convenience one-shot helper that cold-trains via XGBoost on specific logged days.
    pub fn train_from_days(
        store: &ImpressionLogStore,
        days: impl IntoIterator<Item = usize>,
        config: XgboostTrainingConfig,
    ) -> io::Result<Self> {
        config.validate()?;
        let day_list: Vec<usize> = days.into_iter().collect();
        if day_list.is_empty() {
            return Ok(Self::untrained());
        }
        let temp = tempfile::tempdir()?;
        let model_out = temp.path().join("model.json");
        let mut trainer = XgboostTrainer::spawn()?;
        trainer.train_from_days(store, day_list, &model_out, config)
    }

    /// Convenience one-shot helper that cold-trains via XGBoost on days `1..=through_day`.
    pub fn train_from_log_store(
        store: &ImpressionLogStore,
        through_day: usize,
        config: XgboostTrainingConfig,
    ) -> io::Result<Self> {
        if through_day == 0 {
            config.validate()?;
            return Ok(Self::untrained());
        }
        Self::train_from_days(store, 1..=through_day, config)
    }

    pub fn trainer(&self) -> &str {
        &self.trainer
    }

    pub fn xgboost_version(&self) -> Option<&str> {
        self.xgboost_version.as_deref()
    }

    pub fn tree_count(&self) -> usize {
        self.trees.len()
    }

    pub fn summary(&self) -> &TrainingSummary {
        &self.summary
    }

    pub fn is_trained(&self) -> bool {
        self.summary.impressions > 0
    }

    /// Compute the raw XGBoost log-odds margin for a candidate ad in the given context.
    pub fn predict_logit(&self, context: &RankingContext<'_>, candidate: &AdCandidate<'_>) -> f64 {
        let default_stat = EntityStat {
            ctr: self.prior_ctr,
            log_imp: 0.0,
            id_code: -1.0,
        };
        let same_cat = if context.organic_category_id == candidate.ad_category_id {
            1.0_f32
        } else {
            0.0_f32
        };

        let user_idx = self.user_ids.get(context.user_id).copied();
        let u_stat = user_idx
            .map(|u| self.user_stats[u as usize])
            .unwrap_or(default_stat);

        let cat_idx = self.ad_category_ids.get(candidate.ad_category_id).copied();
        let c_stat = cat_idx
            .map(|c| self.ad_category_stats[c as usize])
            .unwrap_or(default_stat);

        let a_stat = self
            .advertiser_ids
            .get(candidate.advertiser_id)
            .map(|&a| self.advertiser_stats[a as usize])
            .unwrap_or(default_stat);

        let k_stat = self
            .campaign_ids
            .get(candidate.campaign_id)
            .map(|&k| self.campaign_stats[k as usize])
            .unwrap_or(default_stat);

        let (uc_ctr, uc_log_imp) = match (user_idx, cat_idx) {
            (Some(u), Some(c)) => self
                .user_ad_category_stats
                .get(&(u, c))
                .copied()
                .unwrap_or((self.prior_ctr, 0.0)),
            _ => (self.prior_ctr, 0.0),
        };

        let features: [f32; FEATURE_COUNT] = [
            same_cat,
            uc_ctr,
            uc_log_imp,
            u_stat.ctr,
            u_stat.log_imp,
            c_stat.ctr,
            c_stat.log_imp,
            a_stat.ctr,
            k_stat.ctr,
            k_stat.log_imp,
            c_stat.id_code,
            a_stat.id_code,
        ];

        let mut margin = self.base_margin;
        for tree in &self.trees {
            margin += tree.evaluate(&features);
        }
        margin
    }

    /// Pre-index known users, categories, and campaigns into a fast [`PreparedScorer`].
    pub fn prepare_scorer(
        &self,
        user_ids: &[&str],
        category_ids: &[&str],
        candidates: &[AdCandidate<'_>],
    ) -> PreparedScorer {
        let default_stat = EntityStat {
            ctr: self.prior_ctr,
            log_imp: 0.0,
            id_code: -1.0,
        };
        let category_index: HashMap<&str, usize> = category_ids
            .iter()
            .enumerate()
            .map(|(idx, &id)| (id, idx))
            .collect();

        let user_features: Vec<(f32, f32)> = user_ids
            .iter()
            .map(|&id| {
                let s = self
                    .user_ids
                    .get(id)
                    .map(|&u| self.user_stats[u as usize])
                    .unwrap_or(default_stat);
                (s.ctr, s.log_imp)
            })
            .collect();

        let campaign_features: Vec<CampaignFeatureRow> = candidates
            .iter()
            .map(|cand| {
                let c_stat = self
                    .ad_category_ids
                    .get(cand.ad_category_id)
                    .map(|&c| self.ad_category_stats[c as usize])
                    .unwrap_or(default_stat);
                let a_stat = self
                    .advertiser_ids
                    .get(cand.advertiser_id)
                    .map(|&a| self.advertiser_stats[a as usize])
                    .unwrap_or(default_stat);
                let k_stat = self
                    .campaign_ids
                    .get(cand.campaign_id)
                    .map(|&k| self.campaign_stats[k as usize])
                    .unwrap_or(default_stat);
                CampaignFeatureRow {
                    ad_category_ext_idx: category_index.get(cand.ad_category_id).copied(),
                    ad_cat_ctr: c_stat.ctr,
                    ad_cat_log_imp: c_stat.log_imp,
                    adv_ctr: a_stat.ctr,
                    camp_ctr: k_stat.ctr,
                    camp_log_imp: k_stat.log_imp,
                    ad_cat_code: c_stat.id_code,
                    adv_code: a_stat.id_code,
                }
            })
            .collect();

        let model_cat_to_ext: Vec<Option<usize>> = {
            let mut mapping = vec![None; self.ad_category_stats.len()];
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

        let user_category_stats = if use_dense {
            let mut table = vec![(self.prior_ctr, 0.0_f32); num_users * num_categories];
            for (ext_user, &user_id) in user_ids.iter().enumerate() {
                if let Some(&model_user) = self.user_ids.get(user_id) {
                    let row_start = ext_user * num_categories;
                    for (model_cat, maybe_ext_cat) in model_cat_to_ext.iter().enumerate() {
                        if let Some(ext_cat) = *maybe_ext_cat
                            && let Some(&pair_stats) = self
                                .user_ad_category_stats
                                .get(&(model_user, model_cat as u32))
                        {
                            table[row_start + ext_cat] = pair_stats;
                        }
                    }
                }
            }
            XgbUserCategoryTable::Dense {
                num_categories,
                stats: table,
            }
        } else {
            let mut rows = vec![Vec::new(); num_users];
            for (ext_user, &user_id) in user_ids.iter().enumerate() {
                if let Some(&model_user) = self.user_ids.get(user_id) {
                    let row = &mut rows[ext_user];
                    for (model_cat, maybe_ext_cat) in model_cat_to_ext.iter().enumerate() {
                        if let Some(ext_cat) = *maybe_ext_cat
                            && let Some(&pair_stats) = self
                                .user_ad_category_stats
                                .get(&(model_user, model_cat as u32))
                        {
                            row.push((ext_cat, pair_stats.0, pair_stats.1));
                        }
                    }
                    row.sort_unstable_by_key(|&(ext_cat, _, _)| ext_cat);
                }
            }
            XgbUserCategoryTable::Sparse { rows }
        };

        PreparedScorer::from_xgboost(XgboostPreparedScorer {
            base_margin: self.base_margin,
            prior_ctr: self.prior_ctr,
            user_features,
            campaign_features,
            user_category_stats,
            trees: self.trees.clone(),
        })
    }
}

impl CtrModel for XgboostCtrModel {
    fn predict_ctr(&self, context: &RankingContext<'_>, candidate: &AdCandidate<'_>) -> f64 {
        sigmoid(self.predict_logit(context, candidate))
    }
}

#[derive(Clone, Copy, Debug)]
struct CampaignFeatureRow {
    ad_category_ext_idx: Option<usize>,
    ad_cat_ctr: f32,
    ad_cat_log_imp: f32,
    adv_ctr: f32,
    camp_ctr: f32,
    camp_log_imp: f32,
    ad_cat_code: f32,
    adv_code: f32,
}

#[derive(Clone, Debug)]
enum XgbUserCategoryTable {
    Dense {
        num_categories: usize,
        stats: Vec<(f32, f32)>,
    },
    Sparse {
        rows: Vec<Vec<(usize, f32, f32)>>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct XgboostPreparedScorer {
    base_margin: f64,
    prior_ctr: f32,
    user_features: Vec<(f32, f32)>,
    campaign_features: Vec<CampaignFeatureRow>,
    user_category_stats: XgbUserCategoryTable,
    trees: Vec<FlatTree>,
}

impl XgboostPreparedScorer {
    #[inline]
    pub(crate) fn score_logit(
        &self,
        user_idx: usize,
        organic_cat_idx: usize,
        campaign_idx: usize,
    ) -> f64 {
        let (u_ctr, u_log_imp) = self.user_features[user_idx];
        let camp = self.campaign_features[campaign_idx];
        let (same_cat, uc_ctr, uc_log_imp) = match camp.ad_category_ext_idx {
            Some(ad_cat_idx) => {
                let same = if ad_cat_idx == organic_cat_idx {
                    1.0_f32
                } else {
                    0.0_f32
                };
                let (ctr, log_imp) = self.user_category_pair(user_idx, ad_cat_idx);
                (same, ctr, log_imp)
            }
            None => (0.0_f32, self.prior_ctr, 0.0_f32),
        };

        let features: [f32; FEATURE_COUNT] = [
            same_cat,
            uc_ctr,
            uc_log_imp,
            u_ctr,
            u_log_imp,
            camp.ad_cat_ctr,
            camp.ad_cat_log_imp,
            camp.adv_ctr,
            camp.camp_ctr,
            camp.camp_log_imp,
            camp.ad_cat_code,
            camp.adv_code,
        ];

        let mut margin = self.base_margin;
        for tree in &self.trees {
            margin += tree.evaluate(&features);
        }
        margin
    }

    #[inline]
    fn user_category_pair(&self, user_idx: usize, ad_cat_idx: usize) -> (f32, f32) {
        match &self.user_category_stats {
            XgbUserCategoryTable::Dense {
                num_categories,
                stats,
            } => stats[user_idx * *num_categories + ad_cat_idx],
            XgbUserCategoryTable::Sparse { rows } => rows[user_idx]
                .binary_search_by_key(&ad_cat_idx, |&(cat, _, _)| cat)
                .map(|pos| {
                    let (_, ctr, log_imp) = rows[user_idx][pos];
                    (ctr, log_imp)
                })
                .unwrap_or((self.prior_ctr, 0.0)),
        }
    }
}

fn index_entity_stats(map: HashMap<String, [f64; 3]>) -> (HashMap<String, u32>, Vec<EntityStat>) {
    let mut ids = HashMap::with_capacity(map.len());
    let mut stats = Vec::with_capacity(map.len());
    for (key, vals) in map {
        let idx = stats.len() as u32;
        ids.insert(key, idx);
        stats.push(EntityStat {
            ctr: vals[0] as f32,
            log_imp: vals[1] as f32,
            id_code: vals[2] as f32,
        });
    }
    (ids, stats)
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
    fn untrained_and_zero_click_xgb_models_behave_cleanly() {
        let untrained = XgboostCtrModel::untrained();
        assert!(!untrained.is_trained());
        assert_eq!(untrained.trainer(), "xgboost");
        assert_eq!(untrained.tree_count(), 0);

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
            XgboostCtrModel::train_from_records(&zero_clicks, XgboostTrainingConfig::default())
                .unwrap();
        assert!(model.is_trained());
        assert_eq!(model.trainer(), "xgboost");
        assert!(model.xgboost_version().is_some());
        assert_eq!(model.tree_count(), 0);
        let p = model.predict_ctr(&ctx, &ad);
        assert!(p > 0.0 && p < 0.5);
    }

    #[test]
    fn xgboost_trainer_learns_signals_and_matches_prepared_scorer() {
        let temp = tempfile::tempdir().unwrap();
        let store = ImpressionLogStore::new(temp.path());
        store.clear().unwrap();

        let mut day1 = store.create_day_writer(1).unwrap();
        for i in 0..120 {
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

        let mut day2 = store.create_day_writer(2).unwrap();
        for i in 0..120 {
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

        let mut trainer = XgboostTrainer::spawn().unwrap();
        let cfg = XgboostTrainingConfig {
            num_boost_round: 30,
            max_depth: 3,
            ..XgboostTrainingConfig::default()
        };
        let model = trainer
            .train_from_log_store(&store, 2, &store.model_file_path(2), cfg)
            .unwrap();
        assert_eq!(model.trainer(), "xgboost");
        assert!(model.xgboost_version().is_some());
        assert_eq!(model.tree_count(), 30);
        assert_eq!(model.summary().impressions, 720);
        assert_eq!(model.summary().clicks, 36);

        let repeat = trainer
            .train_from_log_store(&store, 2, &store.model_file_path(2), cfg)
            .unwrap();

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
        assert_eq!(model.rank_candidates(&u1_ctx, &candidates)[0], 0);
        assert_eq!(model.rank_candidates(&u2_ctx, &candidates)[0], 1);
        assert_eq!(
            model.predict_ctr(&u1_ctx, &candidates[0]),
            repeat.predict_ctr(&u1_ctx, &candidates[0])
        );

        // PreparedScorer must match XgboostCtrModel::predict_ctr exactly.
        let users = ["u1", "u2", "u3", "u-new"];
        let cats = ["cat-a", "cat-b", "cat-c", "cat-d", "cat-e", "cat-unseen"];
        let prepared = model.prepare_scorer(&users, &cats, &candidates);
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
