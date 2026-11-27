import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import numpy as np
import xgboost as xgb

sys.path.insert(0, str(Path(__file__).resolve().parent))
import train_xgb  # noqa: E402


def _record(
    day: int,
    user_id: str,
    organic_category_id: str,
    campaign_id: str,
    advertiser_id: str,
    ad_category_id: str,
    clicked: bool,
) -> dict:
    return {
        "day": day,
        "user_id": user_id,
        "organic_item_id": f"org-{organic_category_id}",
        "organic_category_id": organic_category_id,
        "campaign_id": campaign_id,
        "advertiser_id": advertiser_id,
        "ad_item_id": f"item-{campaign_id}",
        "ad_category_id": ad_category_id,
        "ad_price_cents": 5000,
        "cpc_cents": 100,
        "clicked": clicked,
    }


def _write_day(log_dir: Path, day: int, records: list[dict]) -> None:
    path = train_xgb.day_file_path(log_dir, day)
    with path.open("w", encoding="utf-8") as handle:
        for rec in records:
            handle.write(json.dumps(rec) + "\n")


class XgboostTrainerTest(unittest.TestCase):
    def test_empty_and_zero_click_datasets_behave_cleanly(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            log_dir = Path(tmp)
            empty = train_xgb.train_xgboost(log_dir, [])
            self.assertEqual(empty["summary"]["impressions"], 0)
            self.assertEqual(empty["trainer"], "xgboost")
            self.assertEqual(empty["trees"], [])

            _write_day(
                log_dir,
                1,
                [
                    _record(1, "u1", "cat-1", "c1", "a1", "cat-1", False),
                    _record(1, "u2", "cat-2", "c2", "a2", "cat-3", False),
                ],
            )
            zero_clicks = train_xgb.train_xgboost(log_dir, [1])
            self.assertEqual(zero_clicks["summary"]["impressions"], 2)
            self.assertEqual(zero_clicks["summary"]["clicks"], 0)
            self.assertTrue(zero_clicks["summary"]["converged"])
            self.assertEqual(zero_clicks["trees"], [])
            self.assertLess(zero_clicks["base_margin"], 0.0)

    def test_exported_trees_match_xgboost_booster_predictions_and_are_deterministic(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            log_dir = Path(tmp)
            day1 = []
            for i in range(120):
                day1.append(
                    _record(1, "u1", "cat-c", "camp-a", "adv-a", "cat-a", i % 10 == 0)
                )
                day1.append(
                    _record(1, "u1", "cat-c", "camp-b", "adv-b", "cat-b", False)
                )
            _write_day(log_dir, 1, day1)

            day2 = []
            for i in range(120):
                day2.append(
                    _record(2, "u2", "cat-c", "camp-b", "adv-b", "cat-b", i % 10 == 0)
                )
                day2.append(
                    _record(2, "u2", "cat-c", "camp-a", "adv-a", "cat-a", False)
                )
                day2.append(
                    _record(2, "u3", "cat-d", "camp-d", "adv-d", "cat-d", i % 10 == 0)
                )
                day2.append(
                    _record(2, "u3", "cat-d", "camp-e", "adv-e", "cat-e", False)
                )
            _write_day(log_dir, 2, day2)

            cfg = train_xgb.XgboostTrainingConfig(num_boost_round=30, max_depth=3)
            raw_xgb_path = log_dir / "booster.json"
            artifact = train_xgb.train_xgboost(
                log_dir, [1, 2], cfg, model_binary_out=raw_xgb_path
            )
            self.assertEqual(artifact["trainer"], "xgboost")
            self.assertEqual(artifact["summary"]["impressions"], 720)
            self.assertEqual(artifact["summary"]["clicks"], 36)
            self.assertEqual(len(artifact["trees"]), 30)
            self.assertTrue(raw_xgb_path.exists())

            # Compare exported flat-tree margins directly against native xgboost Booster.predict.
            stats = train_xgb.load_sufficient_stats_from_logs(log_dir, [1, 2])
            aggs = train_xgb._build_entity_aggregates(stats)
            X, _, _ = train_xgb._build_training_matrix(
                stats,
                *aggs,
                prior_ctr=artifact["prior_ctr"],
                prior_strength=artifact["prior_strength"],
            )
            booster = xgb.Booster()
            booster.load_model(str(raw_xgb_path))
            dm = xgb.DMatrix(X, feature_names=list(train_xgb.FEATURE_NAMES))
            xgb_margins = booster.predict(dm, output_margin=True)
            flat_margins = train_xgb.evaluate_exported_trees(
                artifact["base_margin"], artifact["trees"], X
            )
            max_diff = float(np.max(np.abs(xgb_margins - flat_margins)))
            self.assertLess(max_diff, 1e-5)

            # Repeat cold train reproduces identical artifact.
            repeat = train_xgb.train_xgboost(log_dir, [1, 2], cfg)
            self.assertEqual(artifact["base_margin"], repeat["base_margin"])
            self.assertEqual(artifact["trees"], repeat["trees"])

    def test_stdio_worker_supports_both_log_dir_and_stats_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            log_dir = Path(tmp)
            _write_day(
                log_dir,
                1,
                [
                    _record(1, "u1", "cat-a", "camp-a", "adv-a", "cat-a", True),
                    _record(1, "u1", "cat-a", "camp-b", "adv-b", "cat-b", False),
                ]
                * 15,
            )
            stats_file = log_dir / "stats.json"
            stats_payload = {
                "user_vocab": ["u1", "u2"],
                "ad_category_vocab": ["cat-a", "cat-b"],
                "advertiser_vocab": ["adv-a", "adv-b"],
                "campaign_vocab": ["camp-a", "camp-b"],
                "groups": [
                    [0, 0, 0, 0, 0, 100, 10],
                    [0, 1, 1, 1, 0, 100, 0],
                    [1, 1, 1, 1, 0, 100, 10],
                    [1, 0, 0, 0, 0, 100, 0],
                ],
            }
            stats_file.write_text(json.dumps(stats_payload), encoding="utf-8")

            script = Path(__file__).resolve().parent / "train_xgb.py"
            with subprocess.Popen(
                [sys.executable, str(script), "--serve-stdio"],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            ) as proc:
                assert proc.stdin is not None and proc.stdout is not None

                model_out_1 = log_dir / "model-logs.json"
                proc.stdin.write(
                    json.dumps(
                        {
                            "log_dir": str(log_dir),
                            "through_day": 1,
                            "model_out": str(model_out_1),
                            "config": {"num_boost_round": 15},
                        }
                    )
                    + "\n"
                )
                proc.stdin.flush()
                resp1 = json.loads(proc.stdout.readline())

                model_out_2 = log_dir / "model-stats.json"
                proc.stdin.write(
                    json.dumps(
                        {
                            "stats_file": str(stats_file),
                            "model_out": str(model_out_2),
                            "config": {"num_boost_round": 15},
                        }
                    )
                    + "\n"
                )
                proc.stdin.flush()
                resp2 = json.loads(proc.stdout.readline())
                proc.stdin.close()
                ret = proc.wait(timeout=15)

            self.assertEqual(ret, 0)
            self.assertTrue(resp1["ok"])
            self.assertEqual(resp1["trainer"], "xgboost")
            self.assertEqual(resp1["summary"]["impressions"], 30)
            self.assertTrue(model_out_1.exists())

            self.assertTrue(resp2["ok"])
            self.assertEqual(resp2["trainer"], "xgboost")
            self.assertEqual(resp2["summary"]["impressions"], 400)
            self.assertEqual(resp2["summary"]["clicks"], 20)
            self.assertTrue(model_out_2.exists())


if __name__ == "__main__":
    unittest.main()
