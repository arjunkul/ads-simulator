//! Impression-click logging and ML CTR modeling (PyTorch LR & XGBoost GBDT) for ad ranking.

mod model;
mod records;
mod xgboost;

pub use model::{
    CtrModel, LogisticRegressionModel, PreparedScorer, PyTorchTrainer, SufficientStatsPayload,
    TrainingConfig, TrainingSummary,
};
pub use records::{
    AdCandidate, BorrowedImpressionClickRecord, DailyImpressionWriter, ImpressionClickRecord,
    ImpressionClickRef, ImpressionLogStore, RankingContext,
};
pub use xgboost::{XgboostCtrModel, XgboostTrainer, XgboostTrainingConfig};
