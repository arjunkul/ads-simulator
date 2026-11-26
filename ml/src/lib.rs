//! Impression-click logging and logistic regression CTR modeling for ad ranking.

mod model;
mod records;

pub use model::{
    CtrModel, LogisticRegressionModel, PreparedScorer, PyTorchTrainer, SufficientStatsPayload,
    TrainingConfig, TrainingSummary,
};
pub use records::{
    AdCandidate, BorrowedImpressionClickRecord, DailyImpressionWriter, ImpressionClickRecord,
    ImpressionClickRef, ImpressionLogStore, RankingContext,
};
