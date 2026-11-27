use std::{
    collections::HashMap,
    env,
    ffi::OsStr,
    fs::File,
    io::{self, BufRead, BufReader, Error, ErrorKind, Read, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use serde::{Deserialize, Serialize};

use crate::records::{AdCandidate, ImpressionClickRecord, ImpressionLogStore, RankingContext};

const DENSE_USER_CATEGORY_LIMIT: usize = 2_000_000;
const MODEL_SCHEMA_VERSION: u32 = 1;
const DEFAULT_PYTHON_BIN: &str = "python3";
const PYTHON_ENV_VAR: &str = "ADS_ML_PYTHON";
const DEFAULT_TRAIN_SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/pytorch/train_lr.py");

/// Interface for click-through-rate models used to score ad candidates.
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

/// Hyperparameters for cold-training an L2-regularized logistic regression CTR model in PyTorch.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrainingConfig {
    /// L2 regularization strength applied to all non-intercept weights.
    pub l2_penalty: f64,
    /// Maximum number of PyTorch L-BFGS iterations.
    pub max_iterations: usize,
    /// Stopping threshold on the infinity-norm of the objective gradient.
    pub gradient_tolerance: f64,
    /// Stopping threshold on relative objective decrease between consecutive iterations.
    pub relative_loss_tolerance: f64,
    /// Number of recent curvature pairs kept by PyTorch L-BFGS.
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

/// Diagnostics produced by a single PyTorch cold-training run.
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

#[derive(Serialize)]
struct WorkerRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    log_dir: Option<&'a Path>,
    #[serde(skip_serializing_if = "Option::is_none")]
    through_day: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    days: Option<&'a [usize]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stats_file: Option<&'a Path>,
    model_out: &'a Path,
    config: TrainingConfig,
}

/// Compact pre-aggregated exact sufficient statistics payload for [`PyTorchTrainer::train_from_sufficient_stats`].
#[derive(Serialize)]
pub struct SufficientStatsPayload<'a> {
    pub user_vocab: &'a [&'a str],
    pub ad_category_vocab: &'a [&'a str],
    pub advertiser_vocab: &'a [&'a str],
    pub campaign_vocab: &'a [&'a str],
    /// Tuples `(user_idx, ad_category_idx, advertiser_idx, campaign_idx, same_category_u8, impressions, clicks)`.
    pub groups: &'a [(usize, usize, usize, usize, u8, u64, u64)],
}

#[derive(Deserialize)]
struct WorkerResponse {
    ok: bool,
    error: Option<String>,
}

#[derive(Deserialize)]
struct ModelArtifact {
    schema_version: u32,
    trainer: String,
    torch_version: Option<String>,
    intercept: f64,
    same_category_weight: f64,
    users: HashMap<String, f64>,
    ad_categories: HashMap<String, f64>,
    advertisers: HashMap<String, f64>,
    campaigns: HashMap<String, f64>,
    user_ad_categories: Vec<(String, String, f64)>,
    summary: TrainingSummary,
}

/// Persistent PyTorch training worker (`ml/pytorch/train_lr.py --serve-stdio`).
///
/// Spawning one worker per simulation pays `import torch` startup cost once while
/// still cold-training a fresh `torch.nn.Module` from disk logs on every request.
pub struct PyTorchTrainer {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl PyTorchTrainer {
    /// Spawn the default PyTorch trainer using `ADS_ML_PYTHON` (or `python3`) and `ml/pytorch/train_lr.py`.
    pub fn spawn() -> io::Result<Self> {
        let python_bin = env::var_os(PYTHON_ENV_VAR).unwrap_or_else(|| DEFAULT_PYTHON_BIN.into());
        Self::spawn_with(python_bin, Path::new(DEFAULT_TRAIN_SCRIPT))
    }

    /// Spawn a PyTorch trainer with an explicit Python executable and training script path.
    pub fn spawn_with(
        python_bin: impl AsRef<OsStr>,
        script_path: impl AsRef<Path>,
    ) -> io::Result<Self> {
        let script = script_path.as_ref();
        if !script.exists() {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("PyTorch training script not found at {}", script.display()),
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
            .ok_or_else(|| Error::other("failed to capture PyTorch trainer stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::other("failed to capture PyTorch trainer stdout"))?;

        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
        })
    }

    /// Cold-train a fresh PyTorch logistic regression model on specific logged days and save the exported model JSON to `model_out`.
    pub fn train_from_days(
        &mut self,
        store: &ImpressionLogStore,
        days: impl IntoIterator<Item = usize>,
        model_out: &Path,
        config: TrainingConfig,
    ) -> io::Result<LogisticRegressionModel> {
        config.validate()?;
        let day_list: Vec<usize> = days.into_iter().collect();
        if day_list.is_empty() {
            return Ok(LogisticRegressionModel::untrained());
        }
        let request = WorkerRequest {
            log_dir: Some(store.root()),
            through_day: None,
            days: Some(&day_list),
            stats_file: None,
            model_out,
            config,
        };
        self.send_request(&request)?;
        LogisticRegressionModel::load_from_file(model_out)
    }

    /// Cold-train a fresh PyTorch logistic regression model on all logged days `1..=through_day` and save the exported model JSON to `model_out`.
    pub fn train_from_log_store(
        &mut self,
        store: &ImpressionLogStore,
        through_day: usize,
        model_out: &Path,
        config: TrainingConfig,
    ) -> io::Result<LogisticRegressionModel> {
        config.validate()?;
        if through_day == 0 {
            return Ok(LogisticRegressionModel::untrained());
        }
        let request = WorkerRequest {
            log_dir: Some(store.root()),
            through_day: Some(through_day),
            days: None,
            stats_file: None,
            model_out,
            config,
        };
        self.send_request(&request)?;
        LogisticRegressionModel::load_from_file(model_out)
    }

    /// Cold-train a fresh PyTorch logistic regression model from exact pre-aggregated sufficient
    /// statistics serialized to `stats_file`, saving the exported model JSON to `model_out`.
    pub fn train_from_sufficient_stats(
        &mut self,
        payload: &SufficientStatsPayload<'_>,
        stats_file: &Path,
        model_out: &Path,
        config: TrainingConfig,
    ) -> io::Result<LogisticRegressionModel> {
        config.validate()?;
        if payload.groups.is_empty() {
            return Ok(LogisticRegressionModel::untrained());
        }
        if let Some(parent) = stats_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = File::create(stats_file)?;
        let mut writer = io::BufWriter::with_capacity(256 * 1024, file);
        serde_json::to_writer(&mut writer, payload)
            .map_err(|err| Error::new(ErrorKind::InvalidInput, err))?;
        writer.flush()?;
        drop(writer);

        let request = WorkerRequest {
            log_dir: None,
            through_day: None,
            days: None,
            stats_file: Some(stats_file),
            model_out,
            config,
        };
        self.send_request(&request)?;
        LogisticRegressionModel::load_from_file(model_out)
    }

    fn send_request(&mut self, request: &WorkerRequest<'_>) -> io::Result<()> {
        let payload = serde_json::to_string(request)
            .map_err(|err| Error::new(ErrorKind::InvalidInput, err))?;
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| Error::new(ErrorKind::BrokenPipe, "PyTorch trainer stdin is closed"))?;
        if let Err(err) = writeln!(stdin, "{payload}").and_then(|()| stdin.flush()) {
            let detail = self.read_stderr_detail();
            return Err(Error::new(
                err.kind(),
                format!("failed to send request to PyTorch trainer: {err}{detail}"),
            ));
        }

        let mut response_line = String::new();
        let bytes = self.stdout.read_line(&mut response_line)?;
        if bytes == 0 {
            let detail = self.read_stderr_detail();
            return Err(Error::new(
                ErrorKind::UnexpectedEof,
                format!("PyTorch trainer exited before replying{detail}"),
            ));
        }

        let response: WorkerResponse = serde_json::from_str(response_line.trim())
            .map_err(|err| Error::new(ErrorKind::InvalidData, err))?;
        if !response.ok {
            let message = response
                .error
                .unwrap_or_else(|| "unknown PyTorch training error".to_owned());
            return Err(Error::other(format!("PyTorch trainer failed: {message}")));
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
            format!("\nPyTorch trainer stderr:\n{trimmed}")
        }
    }
}

impl Drop for PyTorchTrainer {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.wait();
    }
}

/// L2-regularized logistic regression CTR model trained by PyTorch (`ml/pytorch/train_lr.py`).
#[derive(Clone, Debug)]
pub struct LogisticRegressionModel {
    trainer: String,
    torch_version: Option<String>,
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
            trainer: "pytorch".to_owned(),
            torch_version: None,
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

    /// Load a PyTorch-exported logistic regression model artifact JSON from disk.
    pub fn load_from_file(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let artifact: ModelArtifact = serde_json::from_reader(BufReader::new(file))
            .map_err(|err| Error::new(ErrorKind::InvalidData, err))?;
        if artifact.schema_version != MODEL_SCHEMA_VERSION {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "unsupported model artifact schema version {} (expected {})",
                    artifact.schema_version, MODEL_SCHEMA_VERSION
                ),
            ));
        }

        let (user_ids, user_weights) = index_weight_map(artifact.users);
        let (ad_category_ids, ad_category_weights) = index_weight_map(artifact.ad_categories);
        let (advertiser_ids, advertiser_weights) = index_weight_map(artifact.advertisers);
        let (campaign_ids, campaign_weights) = index_weight_map(artifact.campaigns);

        let mut user_ad_category_ids = HashMap::with_capacity(artifact.user_ad_categories.len());
        let mut user_ad_category_weights = Vec::with_capacity(artifact.user_ad_categories.len());
        for (user_id, ad_cat_id, weight) in artifact.user_ad_categories {
            if let (Some(&u), Some(&c)) = (user_ids.get(&user_id), ad_category_ids.get(&ad_cat_id))
            {
                let idx = user_ad_category_weights.len() as u32;
                user_ad_category_ids.insert((u, c), idx);
                user_ad_category_weights.push(weight);
            }
        }

        Ok(Self {
            trainer: artifact.trainer,
            torch_version: artifact.torch_version,
            intercept: artifact.intercept,
            same_category_weight: artifact.same_category_weight,
            user_ids,
            user_weights,
            ad_category_ids,
            ad_category_weights,
            advertiser_ids,
            advertiser_weights,
            campaign_ids,
            campaign_weights,
            user_ad_category_ids,
            user_ad_category_weights,
            summary: artifact.summary,
        })
    }

    /// Convenience one-shot helper that writes `records` to a temporary log store and cold-trains via PyTorch.
    pub fn train_from_records<'a>(
        records: impl IntoIterator<Item = &'a ImpressionClickRecord>,
        config: TrainingConfig,
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
        let mut trainer = PyTorchTrainer::spawn()?;
        let model_out = temp.path().join("model.json");
        trainer.train_from_days(&store, [1], &model_out, config)
    }

    /// Convenience one-shot helper that cold-trains via PyTorch on specific logged day files on disk.
    pub fn train_from_days(
        store: &ImpressionLogStore,
        days: impl IntoIterator<Item = usize>,
        config: TrainingConfig,
    ) -> io::Result<Self> {
        config.validate()?;
        let day_list: Vec<usize> = days.into_iter().collect();
        if day_list.is_empty() {
            return Ok(Self::untrained());
        }
        let temp = tempfile::tempdir()?;
        let model_out = temp.path().join("model.json");
        let mut trainer = PyTorchTrainer::spawn()?;
        trainer.train_from_days(store, day_list, &model_out, config)
    }

    /// Convenience one-shot helper that cold-trains via PyTorch on all logged days `1..=through_day` on disk.
    pub fn train_from_log_store(
        store: &ImpressionLogStore,
        through_day: usize,
        config: TrainingConfig,
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

    pub fn torch_version(&self) -> Option<&str> {
        self.torch_version.as_deref()
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
    pub fn prepare_scorer(
        &self,
        user_ids: &[&str],
        category_ids: &[&str],
        candidates: &[AdCandidate<'_>],
    ) -> PreparedScorer {
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

        PreparedScorer {
            inner: PreparedScorerInner::Linear(LinearPreparedScorer {
                same_category_weight: self.same_category_weight,
                user_base,
                campaign_base,
                campaign_category,
                user_category_weights,
            }),
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

#[derive(Clone, Debug)]
struct LinearPreparedScorer {
    same_category_weight: f64,
    user_base: Vec<f64>,
    campaign_base: Vec<f64>,
    campaign_category: Vec<Option<usize>>,
    user_category_weights: UserCategoryTable,
}

impl LinearPreparedScorer {
    #[inline]
    fn score_logit(&self, user_idx: usize, organic_cat_idx: usize, campaign_idx: usize) -> f64 {
        let mut logit = self.user_base[user_idx] + self.campaign_base[campaign_idx];
        if let Some(ad_cat_idx) = self.campaign_category[campaign_idx] {
            if ad_cat_idx == organic_cat_idx {
                logit += self.same_category_weight;
            }
            logit += self.user_category_weight(user_idx, ad_cat_idx);
        }
        logit
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

#[derive(Clone, Debug)]
enum PreparedScorerInner {
    Linear(LinearPreparedScorer),
    Xgboost(crate::xgboost::XgboostPreparedScorer),
}

/// Pre-indexed scorer for fast repeated candidate scoring over fixed user/category/campaign tables.
///
/// Supports both PyTorch logistic regression (`Score-R1`) and XGBoost GBDT (`Score-XGB`) models
/// behind a single zero-allocation scoring interface.
#[derive(Clone, Debug)]
pub struct PreparedScorer {
    inner: PreparedScorerInner,
}

impl PreparedScorer {
    pub(crate) fn from_xgboost(scorer: crate::xgboost::XgboostPreparedScorer) -> Self {
        Self {
            inner: PreparedScorerInner::Xgboost(scorer),
        }
    }

    /// Score the log-odds (monotone in predicted CTR) for `(user_idx, organic_cat_idx, campaign_idx)`.
    #[inline]
    pub fn score_logit(&self, user_idx: usize, organic_cat_idx: usize, campaign_idx: usize) -> f64 {
        match &self.inner {
            PreparedScorerInner::Linear(linear) => {
                linear.score_logit(user_idx, organic_cat_idx, campaign_idx)
            }
            PreparedScorerInner::Xgboost(xgb) => {
                xgb.score_logit(user_idx, organic_cat_idx, campaign_idx)
            }
        }
    }

    /// Score the predicted CTR in `(0, 1)` for `(user_idx, organic_cat_idx, campaign_idx)`.
    #[inline]
    pub fn score_ctr(&self, user_idx: usize, organic_cat_idx: usize, campaign_idx: usize) -> f64 {
        sigmoid(self.score_logit(user_idx, organic_cat_idx, campaign_idx))
    }
}

fn index_weight_map(map: HashMap<String, f64>) -> (HashMap<String, u32>, Vec<f64>) {
    let mut ids = HashMap::with_capacity(map.len());
    let mut weights = Vec::with_capacity(map.len());
    for (key, weight) in map {
        let idx = weights.len() as u32;
        ids.insert(key, idx);
        weights.push(weight);
    }
    (ids, weights)
}

#[inline]
pub(crate) fn sigmoid(z: f64) -> f64 {
    if z >= 0.0 {
        1.0 / (1.0 + (-z).exp())
    } else {
        let ez = z.exp();
        ez / (1.0 + ez)
    }
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
        assert_eq!(untrained.trainer(), "pytorch");
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
        assert_eq!(model.trainer(), "pytorch");
        assert!(model.torch_version().is_some());
        assert!(model.summary().converged);
        let p1 = model.predict_ctr(&ctx, &ad);
        assert!(p1 > 0.0 && p1 < 0.5);
        assert_eq!(model.same_category_weight(), 0.0);
    }

    #[test]
    fn pytorch_trainer_learns_signals_reproduces_weights_and_matches_prepared_scorer() {
        let temp = tempfile::tempdir().unwrap();
        let store = ImpressionLogStore::new(temp.path());
        store.clear().unwrap();

        // Day 1: u1 clicks cat-a (even when browsing cat-c); u2 clicks cat-b on day 2.
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

        let mut trainer = PyTorchTrainer::spawn().unwrap();

        let empty_model = trainer
            .train_from_log_store(
                &store,
                0,
                &store.model_file_path(1),
                TrainingConfig::default(),
            )
            .unwrap();
        assert!(!empty_model.is_trained());

        let day1_only = trainer
            .train_from_days(
                &store,
                [1],
                &store.model_file_path(1),
                TrainingConfig::default(),
            )
            .unwrap();
        assert_eq!(day1_only.trainer(), "pytorch");
        assert_eq!(day1_only.summary().impressions, 200);
        assert_eq!(day1_only.summary().clicks, 10);

        let model = trainer
            .train_from_log_store(
                &store,
                2,
                &store.model_file_path(2),
                TrainingConfig::default(),
            )
            .unwrap();
        assert_eq!(model.trainer(), "pytorch");
        assert!(model.torch_version().is_some());
        assert_eq!(model.summary().impressions, 600);
        assert_eq!(model.summary().clicks, 30);
        assert!(model.same_category_weight() > 0.0);
        assert!(store.model_file_path(2).exists());

        // Cold training again from disk reproduces identical weights.
        let repeat = trainer
            .train_from_log_store(
                &store,
                2,
                &store.model_file_path(2),
                TrainingConfig::default(),
            )
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
        assert_eq!(model.rank_candidates(&unseen_ctx, &candidates)[0], 0);

        // PreparedScorer must match LogisticRegressionModel::predict_ctr exactly.
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

        // Training from exact pre-aggregated sufficient stats must match JSONL log training.
        let payload = super::SufficientStatsPayload {
            user_vocab: &["u1", "u2", "u3"],
            ad_category_vocab: &["cat-a", "cat-b", "cat-d", "cat-e"],
            advertiser_vocab: &["adv-a", "adv-b", "adv-d", "adv-e"],
            campaign_vocab: &["camp-a", "camp-b", "camp-d", "camp-e"],
            groups: &[
                (0, 0, 0, 0, 0, 100, 10),
                (0, 1, 1, 1, 0, 100, 0),
                (1, 1, 1, 1, 0, 100, 10),
                (1, 0, 0, 0, 0, 100, 0),
                (2, 2, 2, 2, 1, 100, 10),
                (2, 3, 3, 3, 0, 100, 0),
            ],
        };
        let stats_model = trainer
            .train_from_sufficient_stats(
                &payload,
                &temp.path().join("stats-day-02.json"),
                &temp.path().join("model-stats-02.json"),
                TrainingConfig::default(),
            )
            .unwrap();
        assert_eq!(stats_model.summary().impressions, 600);
        assert_eq!(stats_model.summary().clicks, 30);
        assert!((stats_model.intercept() - model.intercept()).abs() < 1e-5);
        assert!((stats_model.same_category_weight() - model.same_category_weight()).abs() < 1e-5);
        for &user_id in &users {
            for &org_cat in &cats {
                let ctx = RankingContext {
                    user_id,
                    organic_category_id: org_cat,
                };
                for candidate in &candidates {
                    let a = model.predict_ctr(&ctx, candidate);
                    let b = stats_model.predict_ctr(&ctx, candidate);
                    assert!(
                        (a - b).abs() < 1e-5,
                        "sufficient-stats mismatch for ({user_id}, {org_cat}, {}): {a} vs {b}",
                        candidate.campaign_id
                    );
                }
            }
        }
    }
}
