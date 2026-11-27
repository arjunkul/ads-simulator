#!/usr/bin/env python3
"""XGBoost gradient-boosted trees CTR trainer for Score-XGB (`ads-ml`).

Reads either per-day impression-click JSONL logs (`day-01.jsonl`, ...) or a
pre-aggregated exact sufficient-statistics JSON payload (`--stats-file`), builds
leak-free leave-one-out Bayesian-smoothed historical CTR & log-impression
features plus contextual/entity features, cold-trains a fresh `xgboost`
`binary:logistic` GBDT ensemble, and exports a portable JSON model artifact
(flat decision trees + entity stats tables) evaluated natively in Rust by
`ads-ml`'s `XgboostCtrModel` / `PreparedScorer`.

Supports both one-shot CLI execution and a persistent `--serve-stdio` worker
mode so multi-day simulations pay Python/XGBoost startup cost only once while
still cold-training a fresh booster from scratch on every day.
"""

from __future__ import annotations

import argparse
from dataclasses import asdict, dataclass
import json
import math
import os
from pathlib import Path
import sys
import tempfile
from typing import Any, Iterable, Sequence

import numpy as np
import xgboost as xgb

MODEL_SCHEMA_VERSION = 1

FEATURE_NAMES: tuple[str, ...] = (
    "same_category",
    "user_cat_ctr",
    "user_cat_log_imp",
    "user_ctr",
    "user_log_imp",
    "ad_cat_ctr",
    "ad_cat_log_imp",
    "advertiser_ctr",
    "campaign_ctr",
    "campaign_log_imp",
    "ad_category_idx",
    "advertiser_idx",
)

# Enforce monotonic non-decreasing predictions with respect to all five leave-one-out
# Bayesian-smoothed CTR features so trees cannot invert the LOO gap (x_LOO(+) < x_LOO(-))
# inside a group while still learning arbitrary nonlinear interactions with exposure counts.
MONOTONE_CONSTRAINTS: tuple[int, ...] = (0, 1, 0, 1, 0, 1, 0, 1, 1, 0, 0, 0)


@dataclass(frozen=True)
class XgboostTrainingConfig:
    num_boost_round: int = 60
    max_depth: int = 4
    learning_rate: float = 0.1
    l2_reg: float = 2.0
    min_child_weight: float = 0.25
    subsample: float = 1.0
    prior_strength: float = 10.0
    seed: int = 0

    def validate(self) -> None:
        if self.num_boost_round <= 0:
            raise ValueError("num_boost_round must be positive")
        if self.max_depth <= 0:
            raise ValueError("max_depth must be positive")
        if not math.isfinite(self.learning_rate) or self.learning_rate <= 0.0:
            raise ValueError("learning_rate must be finite and positive")
        if not math.isfinite(self.l2_reg) or self.l2_reg < 0.0:
            raise ValueError("l2_reg must be finite and non-negative")
        if not math.isfinite(self.min_child_weight) or self.min_child_weight < 0.0:
            raise ValueError("min_child_weight must be finite and non-negative")
        if not math.isfinite(self.subsample) or not (0.0 < self.subsample <= 1.0):
            raise ValueError("subsample must be in (0, 1]")
        if not math.isfinite(self.prior_strength) or self.prior_strength <= 0.0:
            raise ValueError("prior_strength must be finite and positive")

    @classmethod
    def from_dict(cls, raw: dict[str, Any] | None) -> "XgboostTrainingConfig":
        if not raw:
            return cls()
        config = cls(
            num_boost_round=int(raw.get("num_boost_round", cls.num_boost_round)),
            max_depth=int(raw.get("max_depth", cls.max_depth)),
            learning_rate=float(raw.get("learning_rate", cls.learning_rate)),
            l2_reg=float(raw.get("l2_reg", cls.l2_reg)),
            min_child_weight=float(raw.get("min_child_weight", cls.min_child_weight)),
            subsample=float(raw.get("subsample", cls.subsample)),
            prior_strength=float(raw.get("prior_strength", cls.prior_strength)),
            seed=int(raw.get("seed", cls.seed)),
        )
        config.validate()
        return config


@dataclass(frozen=True)
class SufficientStats:
    user_vocab: list[str]
    ad_category_vocab: list[str]
    advertiser_vocab: list[str]
    campaign_vocab: list[str]
    group_user: list[int]
    group_ad_category: list[int]
    group_advertiser: list[int]
    group_campaign: list[int]
    group_same_category: list[float]
    group_impressions: list[float]
    group_clicks: list[float]
    total_impressions: int
    total_clicks: int


def day_file_path(log_dir: Path, day: int) -> Path:
    if day <= 0:
        raise ValueError(f"day must be 1 or greater, got {day}")
    return log_dir / f"day-{day:02d}.jsonl"


def load_sufficient_stats_from_logs(
    log_dir: Path, days: Sequence[int]
) -> SufficientStats:
    user_to_id: dict[str, int] = {}
    user_vocab: list[str] = []
    ad_cat_to_id: dict[str, int] = {}
    ad_cat_vocab: list[str] = []
    adv_to_id: dict[str, int] = {}
    adv_vocab: list[str] = []
    camp_to_id: dict[str, int] = {}
    camp_vocab: list[str] = []

    group_to_idx: dict[tuple[int, int, int, int, bool], int] = {}
    group_user: list[int] = []
    group_ad_cat: list[int] = []
    group_adv: list[int] = []
    group_camp: list[int] = []
    group_same: list[float] = []
    group_impressions: list[float] = []
    group_clicks: list[float] = []

    total_impressions = 0
    total_clicks = 0
    loads = json.loads

    for day in days:
        path = day_file_path(log_dir, day)
        with path.open("r", encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if not line:
                    continue
                record = loads(line)
                user_id = record["user_id"]
                ad_cat_id = record["ad_category_id"]
                adv_id = record["advertiser_id"]
                camp_id = record["campaign_id"]
                same_cat = record["organic_category_id"] == ad_cat_id
                clicked = bool(record["clicked"])

                u = user_to_id.get(user_id)
                if u is None:
                    u = len(user_vocab)
                    user_to_id[user_id] = u
                    user_vocab.append(user_id)

                c = ad_cat_to_id.get(ad_cat_id)
                if c is None:
                    c = len(ad_cat_vocab)
                    ad_cat_to_id[ad_cat_id] = c
                    ad_cat_vocab.append(ad_cat_id)

                a = adv_to_id.get(adv_id)
                if a is None:
                    a = len(adv_vocab)
                    adv_to_id[adv_id] = a
                    adv_vocab.append(adv_id)

                k = camp_to_id.get(camp_id)
                if k is None:
                    k = len(camp_vocab)
                    camp_to_id[camp_id] = k
                    camp_vocab.append(camp_id)

                key = (u, c, a, k, same_cat)
                g_idx = group_to_idx.get(key)
                if g_idx is None:
                    g_idx = len(group_impressions)
                    group_to_idx[key] = g_idx
                    group_user.append(u)
                    group_ad_cat.append(c)
                    group_adv.append(a)
                    group_camp.append(k)
                    group_same.append(1.0 if same_cat else 0.0)
                    group_impressions.append(1.0)
                    group_clicks.append(1.0 if clicked else 0.0)
                else:
                    group_impressions[g_idx] += 1.0
                    if clicked:
                        group_clicks[g_idx] += 1.0

                total_impressions += 1
                if clicked:
                    total_clicks += 1

    return SufficientStats(
        user_vocab=user_vocab,
        ad_category_vocab=ad_cat_vocab,
        advertiser_vocab=adv_vocab,
        campaign_vocab=camp_vocab,
        group_user=group_user,
        group_ad_category=group_ad_cat,
        group_advertiser=group_adv,
        group_campaign=group_camp,
        group_same_category=group_same,
        group_impressions=group_impressions,
        group_clicks=group_clicks,
        total_impressions=total_impressions,
        total_clicks=total_clicks,
    )


def load_sufficient_stats_from_file(stats_file: Path) -> SufficientStats:
    with stats_file.open("r", encoding="utf-8") as handle:
        raw = json.load(handle)

    user_vocab = list(raw["user_vocab"])
    ad_cat_vocab = list(raw["ad_category_vocab"])
    adv_vocab = list(raw["advertiser_vocab"])
    camp_vocab = list(raw["campaign_vocab"])

    group_user: list[int] = []
    group_ad_cat: list[int] = []
    group_adv: list[int] = []
    group_camp: list[int] = []
    group_same: list[float] = []
    group_impressions: list[float] = []
    group_clicks: list[float] = []
    total_impressions = 0
    total_clicks = 0

    for u, c, a, k, same_u8, imp, clk in raw["groups"]:
        imp_i = int(imp)
        clk_i = int(clk)
        if imp_i <= 0:
            continue
        group_user.append(int(u))
        group_ad_cat.append(int(c))
        group_adv.append(int(a))
        group_camp.append(int(k))
        group_same.append(1.0 if int(same_u8) != 0 else 0.0)
        group_impressions.append(float(imp_i))
        group_clicks.append(float(clk_i))
        total_impressions += imp_i
        total_clicks += clk_i

    return SufficientStats(
        user_vocab=user_vocab,
        ad_category_vocab=ad_cat_vocab,
        advertiser_vocab=adv_vocab,
        campaign_vocab=camp_vocab,
        group_user=group_user,
        group_ad_category=group_ad_cat,
        group_advertiser=group_adv,
        group_campaign=group_camp,
        group_same_category=group_same,
        group_impressions=group_impressions,
        group_clicks=group_clicks,
        total_impressions=total_impressions,
        total_clicks=total_clicks,
    )


def _empty_artifact(config: XgboostTrainingConfig) -> dict[str, Any]:
    return {
        "schema_version": MODEL_SCHEMA_VERSION,
        "trainer": "xgboost",
        "xgboost_version": xgb.__version__,
        "base_margin": 0.0,
        "prior_ctr": 0.5,
        "prior_strength": config.prior_strength,
        "feature_names": list(FEATURE_NAMES),
        "users": {},
        "ad_categories": {},
        "advertisers": {},
        "campaigns": {},
        "user_ad_categories": [],
        "trees": [],
        "summary": {
            "impressions": 0,
            "clicks": 0,
            "unique_groups": 0,
            "feature_count": len(FEATURE_NAMES),
            "iterations": 0,
            "converged": True,
            "mean_log_loss": 0.0,
        },
    }


def _parse_base_score(raw_base_score: Any, fallback_p: float) -> float:
    if isinstance(raw_base_score, (int, float)):
        p = float(raw_base_score)
    elif isinstance(raw_base_score, str):
        text = raw_base_score.strip().lstrip("[").rstrip("]").strip()
        p = float(text) if text else fallback_p
    else:
        p = fallback_p
    p = min(max(p, 1e-12), 1.0 - 1e-12)
    return math.log(p / (1.0 - p))


def _export_trees(booster: xgb.Booster, fallback_p: float) -> tuple[float, list[dict[str, Any]]]:
    fd, tmp_name = tempfile.mkstemp(prefix=".xgb-", suffix=".json")
    os.close(fd)
    try:
        booster.save_model(tmp_name)
        with open(tmp_name, "r", encoding="utf-8") as handle:
            raw = json.load(handle)
    finally:
        try:
            os.unlink(tmp_name)
        except OSError:
            pass

    base_score_raw = raw["learner"]["learner_model_param"]["base_score"]
    base_margin = _parse_base_score(base_score_raw, fallback_p)
    raw_trees = raw["learner"]["gradient_booster"]["model"]["trees"]
    exported: list[dict[str, Any]] = []
    for tree in raw_trees:
        exported.append(
            {
                "left_children": [int(x) for x in tree["left_children"]],
                "right_children": [int(x) for x in tree["right_children"]],
                "split_indices": [int(x) for x in tree["split_indices"]],
                "split_conditions": [float(x) for x in tree["split_conditions"]],
                "default_left": [bool(x) for x in tree["default_left"]],
            }
        )
    return base_margin, exported


def _build_entity_aggregates(
    stats: SufficientStats,
) -> tuple[
    np.ndarray,
    np.ndarray,
    np.ndarray,
    np.ndarray,
    np.ndarray,
    np.ndarray,
    np.ndarray,
    np.ndarray,
    np.ndarray,
    np.ndarray,
]:
    u_arr = np.asarray(stats.group_user, dtype=np.int64)
    c_arr = np.asarray(stats.group_ad_category, dtype=np.int64)
    a_arr = np.asarray(stats.group_advertiser, dtype=np.int64)
    k_arr = np.asarray(stats.group_campaign, dtype=np.int64)
    imp_arr = np.asarray(stats.group_impressions, dtype=np.float64)
    clk_arr = np.asarray(stats.group_clicks, dtype=np.float64)

    n_users = len(stats.user_vocab)
    n_cats = len(stats.ad_category_vocab)
    n_advs = len(stats.advertiser_vocab)
    n_camps = len(stats.campaign_vocab)

    user_imp = np.bincount(u_arr, weights=imp_arr, minlength=n_users)
    user_clk = np.bincount(u_arr, weights=clk_arr, minlength=n_users)
    cat_imp = np.bincount(c_arr, weights=imp_arr, minlength=n_cats)
    cat_clk = np.bincount(c_arr, weights=clk_arr, minlength=n_cats)
    adv_imp = np.bincount(a_arr, weights=imp_arr, minlength=n_advs)
    adv_clk = np.bincount(a_arr, weights=clk_arr, minlength=n_advs)
    camp_imp = np.bincount(k_arr, weights=imp_arr, minlength=n_camps)
    camp_clk = np.bincount(k_arr, weights=clk_arr, minlength=n_camps)

    uc_flat = u_arr * n_cats + c_arr
    uc_imp = np.bincount(uc_flat, weights=imp_arr, minlength=n_users * n_cats)
    uc_clk = np.bincount(uc_flat, weights=clk_arr, minlength=n_users * n_cats)

    return (
        user_imp,
        user_clk,
        cat_imp,
        cat_clk,
        adv_imp,
        adv_clk,
        camp_imp,
        camp_clk,
        uc_imp,
        uc_clk,
    )


def _build_training_matrix(
    stats: SufficientStats,
    user_imp: np.ndarray,
    user_clk: np.ndarray,
    cat_imp: np.ndarray,
    cat_clk: np.ndarray,
    adv_imp: np.ndarray,
    adv_clk: np.ndarray,
    camp_imp: np.ndarray,
    camp_clk: np.ndarray,
    uc_imp: np.ndarray,
    uc_clk: np.ndarray,
    prior_ctr: float,
    prior_strength: float,
) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """Build exact leave-one-out weighted binary classification rows for XGBoost."""
    u_arr = np.asarray(stats.group_user, dtype=np.int64)
    c_arr = np.asarray(stats.group_ad_category, dtype=np.int64)
    a_arr = np.asarray(stats.group_advertiser, dtype=np.int64)
    k_arr = np.asarray(stats.group_campaign, dtype=np.int64)
    same_arr = np.asarray(stats.group_same_category, dtype=np.float64)
    imp_arr = np.asarray(stats.group_impressions, dtype=np.float64)
    clk_arr = np.asarray(stats.group_clicks, dtype=np.float64)
    neg_arr = imp_arr - clk_arr

    n_cats = len(stats.ad_category_vocab)
    uc_flat = u_arr * n_cats + c_arr
    uc_imp_arr = uc_imp[uc_flat]
    uc_clk_arr = uc_clk[uc_flat]

    pos_mask = clk_arr > 0.0
    neg_mask = neg_arr > 0.0
    n_pos = int(np.count_nonzero(pos_mask))
    n_neg = int(np.count_nonzero(neg_mask))
    total_rows = n_pos + n_neg

    X = np.empty((total_rows, len(FEATURE_NAMES)), dtype=np.float32)
    y = np.empty(total_rows, dtype=np.float32)
    w = np.empty(total_rows, dtype=np.float32)

    alpha = prior_strength
    prior_num = alpha * prior_ctr

    def fill_slice(start: int, mask: np.ndarray, label_val: float, weights: np.ndarray) -> int:
        count = int(np.count_nonzero(mask))
        if count == 0:
            return start
        end = start + count
        u_m = u_arr[mask]
        c_m = c_arr[mask]
        a_m = a_arr[mask]
        k_m = k_arr[mask]

        uc_n = np.maximum(0.0, uc_imp_arr[mask] - 1.0)
        uc_k = np.clip(uc_clk_arr[mask] - label_val, 0.0, uc_n)

        u_n = np.maximum(0.0, user_imp[u_m] - 1.0)
        u_k = np.clip(user_clk[u_m] - label_val, 0.0, u_n)

        c_n = np.maximum(0.0, cat_imp[c_m] - 1.0)
        c_k = np.clip(cat_clk[c_m] - label_val, 0.0, c_n)

        a_n = np.maximum(0.0, adv_imp[a_m] - 1.0)
        a_k = np.clip(adv_clk[a_m] - label_val, 0.0, a_n)

        k_n = np.maximum(0.0, camp_imp[k_m] - 1.0)
        k_k = np.clip(camp_clk[k_m] - label_val, 0.0, k_n)

        X[start:end, 0] = same_arr[mask]
        X[start:end, 1] = (uc_k + prior_num) / (uc_n + alpha)
        X[start:end, 2] = np.log1p(uc_n)
        X[start:end, 3] = (u_k + prior_num) / (u_n + alpha)
        X[start:end, 4] = np.log1p(u_n)
        X[start:end, 5] = (c_k + prior_num) / (c_n + alpha)
        X[start:end, 6] = np.log1p(c_n)
        X[start:end, 7] = (a_k + prior_num) / (a_n + alpha)
        X[start:end, 8] = (k_k + prior_num) / (k_n + alpha)
        X[start:end, 9] = np.log1p(k_n)
        X[start:end, 10] = c_m.astype(np.float32)
        X[start:end, 11] = a_m.astype(np.float32)

        y[start:end] = label_val
        w[start:end] = weights[mask]
        return end

    cursor = fill_slice(0, pos_mask, 1.0, clk_arr)
    fill_slice(cursor, neg_mask, 0.0, neg_arr)
    return X, y, w


def evaluate_exported_trees(
    base_margin: float, trees: Sequence[dict[str, Any]], X: np.ndarray
) -> np.ndarray:
    """Reference evaluator mirroring XGBoost C++ and Rust `PreparedScorer`'s f32 tree walk."""
    n_rows = X.shape[0]
    margins = np.full(n_rows, base_margin, dtype=np.float64)
    for tree in trees:
        left = tree["left_children"]
        right = tree["right_children"]
        split_idx = tree["split_indices"]
        split_cond = [np.float32(c) for c in tree["split_conditions"]]
        default_left = tree["default_left"]
        for row_idx in range(n_rows):
            node = 0
            while left[node] != -1:
                val = np.float32(X[row_idx, split_idx[node]])
                if np.isnan(val):
                    node = left[node] if default_left[node] else right[node]
                elif val < split_cond[node]:
                    node = left[node]
                else:
                    node = right[node]
            margins[row_idx] += float(split_cond[node])
    return margins


def train_xgboost_from_stats(
    stats: SufficientStats,
    config: XgboostTrainingConfig = XgboostTrainingConfig(),
    model_binary_out: Path | None = None,
) -> dict[str, Any]:
    """Cold-train a fresh XGBoost binary:logistic CTR booster on sufficient statistics."""
    config.validate()
    if stats.total_impressions == 0:
        return _empty_artifact(config)

    (
        user_imp,
        user_clk,
        cat_imp,
        cat_clk,
        adv_imp,
        adv_clk,
        camp_imp,
        camp_clk,
        uc_imp,
        uc_clk,
    ) = _build_entity_aggregates(stats)

    alpha = config.prior_strength

    # Degenerate all-negative or all-positive datasets yield a constant smoothed prior.
    if stats.total_clicks == 0 or stats.total_clicks == stats.total_impressions:
        prior_ctr = (stats.total_clicks + 0.5) / (stats.total_impressions + 1.0)
        base_margin = math.log(prior_ctr / (1.0 - prior_ctr))
        softplus = (
            base_margin + math.log1p(math.exp(-base_margin))
            if base_margin > 0.0
            else math.log1p(math.exp(base_margin))
        )
        data_loss = stats.total_impressions * softplus - stats.total_clicks * base_margin
        trees: list[dict[str, Any]] = []
        n_rounds = 0
        mean_log_loss = data_loss / stats.total_impressions
    else:
        prior_ctr = stats.total_clicks / stats.total_impressions
        X, y, w = _build_training_matrix(
            stats,
            user_imp,
            user_clk,
            cat_imp,
            cat_clk,
            adv_imp,
            adv_clk,
            camp_imp,
            camp_clk,
            uc_imp,
            uc_clk,
            prior_ctr,
            alpha,
        )
        dtrain = xgb.DMatrix(X, label=y, weight=w, feature_names=list(FEATURE_NAMES))
        params = {
            "objective": "binary:logistic",
            "eval_metric": "logloss",
            "tree_method": "hist",
            "max_depth": config.max_depth,
            "eta": config.learning_rate,
            "reg_lambda": config.l2_reg,
            "min_child_weight": config.min_child_weight,
            "subsample": config.subsample,
            "monotone_constraints": MONOTONE_CONSTRAINTS,
            "base_score": prior_ctr,
            "seed": config.seed,
            "nthread": 1,
            "verbosity": 0,
        }
        booster = xgb.train(
            params=params,
            dtrain=dtrain,
            num_boost_round=config.num_boost_round,
        )
        if model_binary_out is not None:
            model_binary_out.parent.mkdir(parents=True, exist_ok=True)
            booster.save_model(str(model_binary_out))

        base_margin, trees = _export_trees(booster, prior_ctr)
        n_rounds = len(trees)
        preds = np.clip(booster.predict(dtrain).astype(np.float64), 1e-12, 1.0 - 1e-12)
        y64 = y.astype(np.float64)
        w64 = w.astype(np.float64)
        total_loss = float(
            np.sum(w64 * (-(y64 * np.log(preds) + (1.0 - y64) * np.log(1.0 - preds))))
        )
        mean_log_loss = total_loss / stats.total_impressions

    prior_num = alpha * prior_ctr

    def pack_stats(vocab: list[str], imp_v: np.ndarray, clk_v: np.ndarray) -> dict[str, list[float]]:
        out: dict[str, list[float]] = {}
        for idx, name in enumerate(vocab):
            n_val = float(imp_v[idx])
            k_val = float(clk_v[idx])
            ctr = (k_val + prior_num) / (n_val + alpha)
            out[name] = [ctr, math.log1p(n_val), float(idx)]
        return out

    users_table = pack_stats(stats.user_vocab, user_imp, user_clk)
    ad_categories_table = pack_stats(stats.ad_category_vocab, cat_imp, cat_clk)
    advertisers_table = pack_stats(stats.advertiser_vocab, adv_imp, adv_clk)
    campaigns_table = pack_stats(stats.campaign_vocab, camp_imp, camp_clk)

    n_cats = len(stats.ad_category_vocab)
    nonzero_uc = np.flatnonzero(uc_imp > 0.0)
    user_ad_categories_table: list[list[Any]] = []
    if n_cats > 0:
        for flat_idx in nonzero_uc.tolist():
            u_idx, c_idx = divmod(flat_idx, n_cats)
            n_val = float(uc_imp[flat_idx])
            k_val = float(uc_clk[flat_idx])
            ctr = (k_val + prior_num) / (n_val + alpha)
            user_ad_categories_table.append(
                [
                    stats.user_vocab[u_idx],
                    stats.ad_category_vocab[c_idx],
                    ctr,
                    math.log1p(n_val),
                ]
            )

    summary = {
        "impressions": stats.total_impressions,
        "clicks": stats.total_clicks,
        "unique_groups": len(stats.group_impressions),
        "feature_count": len(FEATURE_NAMES),
        "iterations": n_rounds,
        "converged": True,
        "mean_log_loss": mean_log_loss,
    }

    return {
        "schema_version": MODEL_SCHEMA_VERSION,
        "trainer": "xgboost",
        "xgboost_version": xgb.__version__,
        "base_margin": base_margin,
        "prior_ctr": prior_ctr,
        "prior_strength": alpha,
        "feature_names": list(FEATURE_NAMES),
        "users": users_table,
        "ad_categories": ad_categories_table,
        "advertisers": advertisers_table,
        "campaigns": campaigns_table,
        "user_ad_categories": user_ad_categories_table,
        "trees": trees,
        "summary": summary,
    }


def train_xgboost(
    log_dir: Path,
    days: Sequence[int],
    config: XgboostTrainingConfig = XgboostTrainingConfig(),
    model_binary_out: Path | None = None,
) -> dict[str, Any]:
    config.validate()
    if not days:
        return _empty_artifact(config)
    stats = load_sufficient_stats_from_logs(log_dir, days)
    return train_xgboost_from_stats(stats, config, model_binary_out=model_binary_out)


def write_json_atomic(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(dir=path.parent, prefix=".xgb-model-", suffix=".tmp")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, separators=(",", ":"))
            handle.write("\n")
        os.replace(tmp_name, path)
    except BaseException:
        try:
            os.unlink(tmp_name)
        except OSError:
            pass
        raise


def handle_request(request: dict[str, Any]) -> dict[str, Any]:
    config = XgboostTrainingConfig.from_dict(request.get("config"))
    model_out = Path(request["model_out"])
    model_binary_out = (
        Path(request["checkpoint_out"]) if request.get("checkpoint_out") else None
    )

    if request.get("stats_file"):
        stats = load_sufficient_stats_from_file(Path(request["stats_file"]))
        artifact = train_xgboost_from_stats(
            stats, config=config, model_binary_out=model_binary_out
        )
    else:
        log_dir = Path(request["log_dir"])
        if "days" in request and request["days"] is not None:
            days = [int(d) for d in request["days"]]
        elif "through_day" in request and request["through_day"] is not None:
            through_day = int(request["through_day"])
            days = list(range(1, through_day + 1)) if through_day > 0 else []
        else:
            raise ValueError(
                "request must provide 'stats_file', 'days', or 'through_day'"
            )
        artifact = train_xgboost(
            log_dir=log_dir,
            days=days,
            config=config,
            model_binary_out=model_binary_out,
        )

    write_json_atomic(model_out, artifact)
    return {
        "ok": True,
        "model_out": str(model_out),
        "trainer": artifact["trainer"],
        "xgboost_version": artifact["xgboost_version"],
        "summary": artifact["summary"],
    }


def serve_stdio() -> int:
    for raw_line in sys.stdin:
        line = raw_line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
            response = handle_request(request)
        except Exception as exc:  # noqa: BLE001 - report structured error over protocol
            response = {"ok": False, "error": f"{type(exc).__name__}: {exc}"}
        sys.stdout.write(json.dumps(response, separators=(",", ":")) + "\n")
        sys.stdout.flush()
    return 0


def parse_days_arg(raw: str) -> list[int]:
    return [int(part.strip()) for part in raw.split(",") if part.strip()]


def build_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Cold-train an XGBoost GBDT CTR model from daily JSONL logs or sufficient stats."
    )
    parser.add_argument(
        "--serve-stdio",
        action="store_true",
        help="Run as a persistent newline-delimited JSON worker over stdin/stdout.",
    )
    parser.add_argument("--log-dir", type=Path, help="Directory with day-XX.jsonl files.")
    parser.add_argument(
        "--through-day",
        type=int,
        help="Train on all logged days 1..=through_day.",
    )
    parser.add_argument(
        "--days",
        type=str,
        help="Comma-separated list of specific 1-based day numbers to train on.",
    )
    parser.add_argument(
        "--stats-file",
        type=Path,
        help="Pre-aggregated sufficient-statistics JSON file.",
    )
    parser.add_argument(
        "--model-out",
        type=Path,
        help="Destination JSON path for exported model artifact.",
    )
    parser.add_argument(
        "--checkpoint-out",
        type=Path,
        help="Optional destination path for native XGBoost booster JSON/UBJ model.",
    )
    parser.add_argument(
        "--num-boost-round",
        type=int,
        default=XgboostTrainingConfig.num_boost_round,
    )
    parser.add_argument("--max-depth", type=int, default=XgboostTrainingConfig.max_depth)
    parser.add_argument(
        "--learning-rate", type=float, default=XgboostTrainingConfig.learning_rate
    )
    parser.add_argument("--l2-reg", type=float, default=XgboostTrainingConfig.l2_reg)
    parser.add_argument(
        "--min-child-weight",
        type=float,
        default=XgboostTrainingConfig.min_child_weight,
    )
    parser.add_argument("--subsample", type=float, default=XgboostTrainingConfig.subsample)
    parser.add_argument(
        "--prior-strength",
        type=float,
        default=XgboostTrainingConfig.prior_strength,
    )
    parser.add_argument("--seed", type=int, default=XgboostTrainingConfig.seed)
    return parser


def main(argv: Iterable[str] | None = None) -> int:
    parser = build_arg_parser()
    args = parser.parse_args(list(argv) if argv is not None else None)

    if args.serve_stdio:
        return serve_stdio()

    if args.model_out is None:
        parser.error("--model-out is required unless --serve-stdio is set")

    config = XgboostTrainingConfig(
        num_boost_round=args.num_boost_round,
        max_depth=args.max_depth,
        learning_rate=args.learning_rate,
        l2_reg=args.l2_reg,
        min_child_weight=args.min_child_weight,
        subsample=args.subsample,
        prior_strength=args.prior_strength,
        seed=args.seed,
    )

    if args.stats_file is not None:
        stats = load_sufficient_stats_from_file(args.stats_file)
        artifact = train_xgboost_from_stats(
            stats, config=config, model_binary_out=args.checkpoint_out
        )
    else:
        if args.log_dir is None:
            parser.error("--log-dir or --stats-file is required")
        if args.days is not None and args.through_day is not None:
            parser.error("specify at most one of --days or --through-day")
        if args.days is not None:
            days = parse_days_arg(args.days)
        elif args.through_day is not None:
            days = list(range(1, args.through_day + 1)) if args.through_day > 0 else []
        else:
            parser.error("either --through-day or --days is required")
        artifact = train_xgboost(
            log_dir=args.log_dir,
            days=days,
            config=config,
            model_binary_out=args.checkpoint_out,
        )

    write_json_atomic(args.model_out, artifact)
    sys.stdout.write(json.dumps(artifact["summary"]) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
