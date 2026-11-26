#!/usr/bin/env python3
"""PyTorch L2-regularized logistic regression trainer for Score-R1 CTR modeling.

Reads per-day impression-click JSONL logs (`day-01.jsonl`, `day-02.jsonl`, ...)
from disk, aggregates exact sufficient statistics per unique discrete feature
combination, cold-trains a fresh `LogisticRegressionCTR` `torch.nn.Module` with
`torch.optim.LBFGS`, and exports a portable JSON model artifact (plus optional
`.pt` PyTorch checkpoint) consumed by `ads-ml` in Rust.

Supports both one-shot CLI execution and a persistent `--serve-stdio` worker
mode so long simulations pay `import torch` startup cost only once while still
cold-training from scratch on every day.
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

import torch
import torch.nn as nn
import torch.nn.functional as F

MODEL_SCHEMA_VERSION = 1


@dataclass(frozen=True)
class TrainingConfig:
    l2_penalty: float = 1.0
    max_iterations: int = 200
    gradient_tolerance: float = 1e-4
    relative_loss_tolerance: float = 1e-9
    history_size: int = 10

    def validate(self) -> None:
        if not math.isfinite(self.l2_penalty) or self.l2_penalty <= 0.0:
            raise ValueError("l2_penalty must be finite and positive")
        if self.max_iterations <= 0:
            raise ValueError("max_iterations must be positive")
        if not math.isfinite(self.gradient_tolerance) or self.gradient_tolerance <= 0.0:
            raise ValueError("gradient_tolerance must be finite and positive")
        if (
            not math.isfinite(self.relative_loss_tolerance)
            or self.relative_loss_tolerance < 0.0
        ):
            raise ValueError("relative_loss_tolerance must be finite and non-negative")
        if self.history_size <= 0:
            raise ValueError("history_size must be positive")

    @classmethod
    def from_dict(cls, raw: dict[str, Any] | None) -> "TrainingConfig":
        if not raw:
            return cls()
        config = cls(
            l2_penalty=float(raw.get("l2_penalty", cls.l2_penalty)),
            max_iterations=int(raw.get("max_iterations", cls.max_iterations)),
            gradient_tolerance=float(
                raw.get("gradient_tolerance", cls.gradient_tolerance)
            ),
            relative_loss_tolerance=float(
                raw.get("relative_loss_tolerance", cls.relative_loss_tolerance)
            ),
            history_size=int(raw.get("history_size", cls.history_size)),
        )
        config.validate()
        return config


@dataclass(frozen=True)
class SufficientStats:
    user_vocab: list[str]
    ad_category_vocab: list[str]
    advertiser_vocab: list[str]
    campaign_vocab: list[str]
    user_ad_category_pairs: list[tuple[int, int]]
    group_user: list[int]
    group_ad_category: list[int]
    group_advertiser: list[int]
    group_campaign: list[int]
    group_user_ad_category: list[int]
    group_same_category: list[float]
    group_impressions: list[float]
    group_clicks: list[float]
    total_impressions: int
    total_clicks: int


class LogisticRegressionCTR(nn.Module):
    """Sparse-embedding L2-regularized logistic regression CTR model in PyTorch."""

    def __init__(
        self,
        num_users: int,
        num_ad_categories: int,
        num_advertisers: int,
        num_campaigns: int,
        num_user_ad_categories: int,
        init_intercept: float = 0.0,
    ) -> None:
        super().__init__()
        self.intercept = nn.Parameter(
            torch.tensor(init_intercept, dtype=torch.float64)
        )
        self.same_category_weight = nn.Parameter(
            torch.tensor(0.0, dtype=torch.float64)
        )
        self.user_weights = nn.Embedding(num_users, 1, dtype=torch.float64)
        self.ad_category_weights = nn.Embedding(
            num_ad_categories, 1, dtype=torch.float64
        )
        self.advertiser_weights = nn.Embedding(
            num_advertisers, 1, dtype=torch.float64
        )
        self.campaign_weights = nn.Embedding(num_campaigns, 1, dtype=torch.float64)
        self.user_ad_category_weights = nn.Embedding(
            num_user_ad_categories, 1, dtype=torch.float64
        )
        for embedding in (
            self.user_weights,
            self.ad_category_weights,
            self.advertiser_weights,
            self.campaign_weights,
            self.user_ad_category_weights,
        ):
            nn.init.zeros_(embedding.weight)

    def forward(
        self,
        user_idx: torch.Tensor,
        ad_cat_idx: torch.Tensor,
        adv_idx: torch.Tensor,
        camp_idx: torch.Tensor,
        uc_idx: torch.Tensor,
        same_cat: torch.Tensor,
    ) -> torch.Tensor:
        return (
            self.intercept
            + self.same_category_weight * same_cat
            + self.user_weights(user_idx).squeeze(-1)
            + self.ad_category_weights(ad_cat_idx).squeeze(-1)
            + self.advertiser_weights(adv_idx).squeeze(-1)
            + self.campaign_weights(camp_idx).squeeze(-1)
            + self.user_ad_category_weights(uc_idx).squeeze(-1)
        )

    def l2_penalty_term(self, l2_penalty: float) -> torch.Tensor:
        sq_norm = (
            self.same_category_weight.pow(2)
            + self.user_weights.weight.pow(2).sum()
            + self.ad_category_weights.weight.pow(2).sum()
            + self.advertiser_weights.weight.pow(2).sum()
            + self.campaign_weights.weight.pow(2).sum()
            + self.user_ad_category_weights.weight.pow(2).sum()
        )
        return 0.5 * l2_penalty * sq_norm


def day_file_path(log_dir: Path, day: int) -> Path:
    if day <= 0:
        raise ValueError(f"day must be 1 or greater, got {day}")
    return log_dir / f"day-{day:02d}.jsonl"


def load_sufficient_stats(log_dir: Path, days: Sequence[int]) -> SufficientStats:
    """Read day JSONL files from disk and aggregate exact logistic regression sufficient stats."""
    user_to_id: dict[str, int] = {}
    user_vocab: list[str] = []
    ad_cat_to_id: dict[str, int] = {}
    ad_cat_vocab: list[str] = []
    adv_to_id: dict[str, int] = {}
    adv_vocab: list[str] = []
    camp_to_id: dict[str, int] = {}
    camp_vocab: list[str] = []
    uc_to_id: dict[tuple[int, int], int] = {}
    uc_pairs: list[tuple[int, int]] = []

    group_to_idx: dict[tuple[int, int, int, int, bool], int] = {}
    group_user: list[int] = []
    group_ad_cat: list[int] = []
    group_adv: list[int] = []
    group_camp: list[int] = []
    group_uc: list[int] = []
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
                    uc_key = (u, c)
                    uc = uc_to_id.get(uc_key)
                    if uc is None:
                        uc = len(uc_pairs)
                        uc_to_id[uc_key] = uc
                        uc_pairs.append(uc_key)

                    g_idx = len(group_impressions)
                    group_to_idx[key] = g_idx
                    group_user.append(u)
                    group_ad_cat.append(c)
                    group_adv.append(a)
                    group_camp.append(k)
                    group_uc.append(uc)
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
        user_ad_category_pairs=uc_pairs,
        group_user=group_user,
        group_ad_category=group_ad_cat,
        group_advertiser=group_adv,
        group_campaign=group_camp,
        group_user_ad_category=group_uc,
        group_same_category=group_same,
        group_impressions=group_impressions,
        group_clicks=group_clicks,
        total_impressions=total_impressions,
        total_clicks=total_clicks,
    )


def load_sufficient_stats_from_file(stats_file: Path) -> SufficientStats:
    """Load pre-aggregated exact sufficient statistics from a compact JSON file."""
    with stats_file.open("r", encoding="utf-8") as handle:
        raw = json.load(handle)

    user_vocab: list[str] = raw["user_vocab"]
    ad_cat_vocab: list[str] = raw["ad_category_vocab"]
    adv_vocab: list[str] = raw["advertiser_vocab"]
    camp_vocab: list[str] = raw["campaign_vocab"]
    uc_pairs: list[tuple[int, int]] = []
    uc_to_id: dict[tuple[int, int], int] = {}

    group_user: list[int] = []
    group_ad_cat: list[int] = []
    group_adv: list[int] = []
    group_camp: list[int] = []
    group_uc: list[int] = []
    group_same: list[float] = []
    group_impressions: list[float] = []
    group_clicks: list[float] = []

    total_impressions = 0
    total_clicks = 0

    for u, c, a, k, same_int, imp, clk in raw["groups"]:
        if imp <= 0:
            continue
        uc_key = (u, c)
        uc = uc_to_id.get(uc_key)
        if uc is None:
            uc = len(uc_pairs)
            uc_to_id[uc_key] = uc
            uc_pairs.append(uc_key)
        group_user.append(u)
        group_ad_cat.append(c)
        group_adv.append(a)
        group_camp.append(k)
        group_uc.append(uc)
        group_same.append(1.0 if same_int else 0.0)
        group_impressions.append(float(imp))
        group_clicks.append(float(clk))
        total_impressions += int(imp)
        total_clicks += int(clk)

    return SufficientStats(
        user_vocab=user_vocab,
        ad_category_vocab=ad_cat_vocab,
        advertiser_vocab=adv_vocab,
        campaign_vocab=camp_vocab,
        user_ad_category_pairs=uc_pairs,
        group_user=group_user,
        group_ad_category=group_ad_cat,
        group_advertiser=group_adv,
        group_campaign=group_camp,
        group_user_ad_category=group_uc,
        group_same_category=group_same,
        group_impressions=group_impressions,
        group_clicks=group_clicks,
        total_impressions=total_impressions,
        total_clicks=total_clicks,
    )


def _configure_determinism() -> None:
    torch.manual_seed(0)
    torch.set_num_threads(1)
    torch.use_deterministic_algorithms(True)


def _empty_artifact() -> dict[str, Any]:
    return {
        "schema_version": MODEL_SCHEMA_VERSION,
        "trainer": "pytorch",
        "torch_version": torch.__version__,
        "intercept": 0.0,
        "same_category_weight": 0.0,
        "users": {},
        "ad_categories": {},
        "advertisers": {},
        "campaigns": {},
        "user_ad_categories": [],
        "summary": {
            "impressions": 0,
            "clicks": 0,
            "unique_groups": 0,
            "feature_count": 2,
            "iterations": 0,
            "converged": True,
            "mean_log_loss": 0.0,
        },
    }


def train_logistic_regression(
    log_dir: Path,
    days: Sequence[int],
    config: TrainingConfig = TrainingConfig(),
    checkpoint_out: Path | None = None,
) -> dict[str, Any]:
    """Cold-train a fresh PyTorch LogisticRegressionCTR model on the specified log days."""
    config.validate()
    _configure_determinism()

    if not days:
        return _empty_artifact()

    stats = load_sufficient_stats(log_dir, days)
    return train_logistic_regression_from_stats(
        stats=stats, config=config, checkpoint_out=checkpoint_out
    )


def train_logistic_regression_from_stats(
    stats: SufficientStats,
    config: TrainingConfig = TrainingConfig(),
    checkpoint_out: Path | None = None,
) -> dict[str, Any]:
    """Cold-train a fresh PyTorch LogisticRegressionCTR model from exact sufficient stats."""
    config.validate()
    _configure_determinism()

    if stats.total_impressions == 0:
        return _empty_artifact()

    feature_count = (
        2
        + len(stats.user_vocab)
        + len(stats.ad_category_vocab)
        + len(stats.advertiser_vocab)
        + len(stats.campaign_vocab)
        + len(stats.user_ad_category_pairs)
    )

    user_idx = torch.tensor(stats.group_user, dtype=torch.long)
    ad_cat_idx = torch.tensor(stats.group_ad_category, dtype=torch.long)
    adv_idx = torch.tensor(stats.group_advertiser, dtype=torch.long)
    camp_idx = torch.tensor(stats.group_campaign, dtype=torch.long)
    uc_idx = torch.tensor(stats.group_user_ad_category, dtype=torch.long)
    same_cat = torch.tensor(stats.group_same_category, dtype=torch.float64)
    impressions = torch.tensor(stats.group_impressions, dtype=torch.float64)
    clicks = torch.tensor(stats.group_clicks, dtype=torch.float64)

    # Degenerate all-negative or all-positive datasets have optimal non-intercept weights = 0.
    if stats.total_clicks == 0 or stats.total_clicks == stats.total_impressions:
        smoothed_p = (stats.total_clicks + 0.5) / (stats.total_impressions + 1.0)
        init_intercept = math.log(smoothed_p / (1.0 - smoothed_p))
        model = LogisticRegressionCTR(
            num_users=len(stats.user_vocab),
            num_ad_categories=len(stats.ad_category_vocab),
            num_advertisers=len(stats.advertiser_vocab),
            num_campaigns=len(stats.campaign_vocab),
            num_user_ad_categories=len(stats.user_ad_category_pairs),
            init_intercept=init_intercept,
        )
        with torch.no_grad():
            logits = model(user_idx, ad_cat_idx, adv_idx, camp_idx, uc_idx, same_cat)
            data_loss = (impressions * F.softplus(logits) - clicks * logits).sum().item()
        n_iter = 0
        converged = True
    else:
        prior_p = stats.total_clicks / stats.total_impressions
        init_intercept = math.log(prior_p / (1.0 - prior_p))
        model = LogisticRegressionCTR(
            num_users=len(stats.user_vocab),
            num_ad_categories=len(stats.ad_category_vocab),
            num_advertisers=len(stats.advertiser_vocab),
            num_campaigns=len(stats.campaign_vocab),
            num_user_ad_categories=len(stats.user_ad_category_pairs),
            init_intercept=init_intercept,
        )

        optimizer = torch.optim.LBFGS(
            model.parameters(),
            lr=1.0,
            max_iter=config.max_iterations,
            tolerance_grad=config.gradient_tolerance,
            tolerance_change=config.relative_loss_tolerance,
            history_size=config.history_size,
            line_search_fn="strong_wolfe",
        )

        def closure() -> torch.Tensor:
            optimizer.zero_grad(set_to_none=True)
            logits = model(user_idx, ad_cat_idx, adv_idx, camp_idx, uc_idx, same_cat)
            data_loss_tensor = (impressions * F.softplus(logits) - clicks * logits).sum()
            total_loss = data_loss_tensor + model.l2_penalty_term(config.l2_penalty)
            total_loss.backward()
            return total_loss

        optimizer.step(closure)

        # Evaluate exact final data loss and gradient infinity-norm.
        optimizer.zero_grad(set_to_none=True)
        final_logits = model(user_idx, ad_cat_idx, adv_idx, camp_idx, uc_idx, same_cat)
        final_data_loss = (
            impressions * F.softplus(final_logits) - clicks * final_logits
        ).sum()
        final_total_loss = final_data_loss + model.l2_penalty_term(config.l2_penalty)
        final_total_loss.backward()

        grad_inf = max(
            float(param.grad.abs().max().item())
            for param in model.parameters()
            if param.grad is not None and param.grad.numel() > 0
        )
        first_param = next(iter(model.parameters()))
        state = optimizer.state.get(first_param, {})
        n_iter = int(state.get("n_iter", 0))
        converged = bool(
            n_iter < config.max_iterations or grad_inf <= config.gradient_tolerance
        )
        data_loss = float(final_data_loss.item())

    with torch.no_grad():
        intercept = float(model.intercept.item())
        same_category_weight = float(model.same_category_weight.item())
        user_vals = model.user_weights.weight.squeeze(-1).tolist()
        ad_cat_vals = model.ad_category_weights.weight.squeeze(-1).tolist()
        adv_vals = model.advertiser_weights.weight.squeeze(-1).tolist()
        camp_vals = model.campaign_weights.weight.squeeze(-1).tolist()
        uc_vals = model.user_ad_category_weights.weight.squeeze(-1).tolist()

    summary = {
        "impressions": stats.total_impressions,
        "clicks": stats.total_clicks,
        "unique_groups": len(stats.group_impressions),
        "feature_count": feature_count,
        "iterations": n_iter,
        "converged": converged,
        "mean_log_loss": data_loss / stats.total_impressions,
    }

    artifact: dict[str, Any] = {
        "schema_version": MODEL_SCHEMA_VERSION,
        "trainer": "pytorch",
        "torch_version": torch.__version__,
        "intercept": intercept,
        "same_category_weight": same_category_weight,
        "users": dict(zip(stats.user_vocab, user_vals)),
        "ad_categories": dict(zip(stats.ad_category_vocab, ad_cat_vals)),
        "advertisers": dict(zip(stats.advertiser_vocab, adv_vals)),
        "campaigns": dict(zip(stats.campaign_vocab, camp_vals)),
        "user_ad_categories": [
            [stats.user_vocab[u], stats.ad_category_vocab[c], w]
            for (u, c), w in zip(stats.user_ad_category_pairs, uc_vals)
        ],
        "summary": summary,
    }

    if checkpoint_out is not None:
        checkpoint_out.parent.mkdir(parents=True, exist_ok=True)
        torch.save(
            {
                "schema_version": MODEL_SCHEMA_VERSION,
                "config": asdict(config),
                "state_dict": model.state_dict(),
                "vocab": {
                    "users": stats.user_vocab,
                    "ad_categories": stats.ad_category_vocab,
                    "advertisers": stats.advertiser_vocab,
                    "campaigns": stats.campaign_vocab,
                    "user_ad_categories": stats.user_ad_category_pairs,
                },
                "summary": summary,
            },
            checkpoint_out,
        )

    return artifact


def write_json_atomic(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(dir=path.parent, prefix=".model-", suffix=".tmp")
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
    config = TrainingConfig.from_dict(request.get("config"))
    model_out = Path(request["model_out"])
    checkpoint_out = (
        Path(request["checkpoint_out"]) if request.get("checkpoint_out") else None
    )

    if request.get("stats_file"):
        stats = load_sufficient_stats_from_file(Path(request["stats_file"]))
        artifact = train_logistic_regression_from_stats(
            stats=stats,
            config=config,
            checkpoint_out=checkpoint_out,
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

        artifact = train_logistic_regression(
            log_dir=log_dir,
            days=days,
            config=config,
            checkpoint_out=checkpoint_out,
        )

    write_json_atomic(model_out, artifact)
    return {
        "ok": True,
        "model_out": str(model_out),
        "trainer": artifact["trainer"],
        "torch_version": artifact["torch_version"],
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
        description="Cold-train a PyTorch logistic regression CTR model from daily JSONL logs."
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
        "--model-out",
        type=Path,
        help="Destination JSON path for exported model weights.",
    )
    parser.add_argument(
        "--checkpoint-out",
        type=Path,
        help="Optional destination .pt path for raw PyTorch checkpoint.",
    )
    parser.add_argument("--l2-penalty", type=float, default=TrainingConfig.l2_penalty)
    parser.add_argument(
        "--max-iterations", type=int, default=TrainingConfig.max_iterations
    )
    parser.add_argument(
        "--gradient-tolerance", type=float, default=TrainingConfig.gradient_tolerance
    )
    parser.add_argument(
        "--relative-loss-tolerance",
        type=float,
        default=TrainingConfig.relative_loss_tolerance,
    )
    parser.add_argument("--history-size", type=int, default=TrainingConfig.history_size)
    return parser


def main(argv: Iterable[str] | None = None) -> int:
    parser = build_arg_parser()
    args = parser.parse_args(list(argv) if argv is not None else None)

    if args.serve_stdio:
        return serve_stdio()

    if args.log_dir is None or args.model_out is None:
        parser.error("--log-dir and --model-out are required unless --serve-stdio is set")
    if args.days is not None and args.through_day is not None:
        parser.error("specify at most one of --days or --through-day")
    if args.days is not None:
        days = parse_days_arg(args.days)
    elif args.through_day is not None:
        days = list(range(1, args.through_day + 1)) if args.through_day > 0 else []
    else:
        parser.error("either --through-day or --days is required")

    config = TrainingConfig(
        l2_penalty=args.l2_penalty,
        max_iterations=args.max_iterations,
        gradient_tolerance=args.gradient_tolerance,
        relative_loss_tolerance=args.relative_loss_tolerance,
        history_size=args.history_size,
    )
    artifact = train_logistic_regression(
        log_dir=args.log_dir,
        days=days,
        config=config,
        checkpoint_out=args.checkpoint_out,
    )
    write_json_atomic(args.model_out, artifact)
    sys.stdout.write(json.dumps(artifact["summary"]) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
