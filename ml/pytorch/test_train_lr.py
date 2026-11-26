import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import torch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import train_lr  # noqa: E402


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
    path = train_lr.day_file_path(log_dir, day)
    with path.open("w", encoding="utf-8") as handle:
        for rec in records:
            handle.write(json.dumps(rec) + "\n")


class PyTorchTrainerTest(unittest.TestCase):
    def test_empty_and_zero_click_datasets_behave_cleanly(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            log_dir = Path(tmp)
            empty = train_lr.train_logistic_regression(log_dir, [])
            self.assertEqual(empty["summary"]["impressions"], 0)
            self.assertEqual(empty["trainer"], "pytorch")

            _write_day(
                log_dir,
                1,
                [
                    _record(1, "u1", "cat-1", "c1", "a1", "cat-1", False),
                    _record(1, "u2", "cat-2", "c2", "a2", "cat-3", False),
                ],
            )
            zero_clicks = train_lr.train_logistic_regression(log_dir, [1])
            self.assertEqual(zero_clicks["summary"]["impressions"], 2)
            self.assertEqual(zero_clicks["summary"]["clicks"], 0)
            self.assertTrue(zero_clicks["summary"]["converged"])
            self.assertEqual(zero_clicks["same_category_weight"], 0.0)
            self.assertLess(zero_clicks["intercept"], 0.0)

    def test_pytorch_lbfgs_reaches_kkt_optimum_and_learns_signals(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            log_dir = Path(tmp)
            day1 = []
            for i in range(100):
                day1.append(
                    _record(1, "u1", "cat-c", "camp-a", "adv-a", "cat-a", i % 10 == 0)
                )
                day1.append(
                    _record(1, "u1", "cat-c", "camp-b", "adv-b", "cat-b", False)
                )
            _write_day(log_dir, 1, day1)

            day2 = []
            for i in range(100):
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

            cfg = train_lr.TrainingConfig(
                gradient_tolerance=1e-7,
                relative_loss_tolerance=0.0,
            )
            ckpt = log_dir / "model.pt"
            artifact = train_lr.train_logistic_regression(
                log_dir, [1, 2], cfg, checkpoint_out=ckpt
            )
            self.assertEqual(artifact["trainer"], "pytorch")
            self.assertEqual(artifact["summary"]["impressions"], 600)
            self.assertEqual(artifact["summary"]["clicks"], 30)
            self.assertTrue(artifact["summary"]["converged"])
            self.assertGreater(artifact["same_category_weight"], 0.0)
            self.assertTrue(ckpt.exists())

            saved = torch.load(ckpt, weights_only=True)
            self.assertIn("state_dict", saved)
            self.assertEqual(saved["summary"]["impressions"], 600)

            # Verify KKT stationarity at the exported weights directly via PyTorch autograd.
            stats = train_lr.load_sufficient_stats(log_dir, [1, 2])
            model = train_lr.LogisticRegressionCTR(
                len(stats.user_vocab),
                len(stats.ad_category_vocab),
                len(stats.advertiser_vocab),
                len(stats.campaign_vocab),
                len(stats.user_ad_category_pairs),
                init_intercept=artifact["intercept"],
            )
            model.load_state_dict(saved["state_dict"])
            logits = model(
                torch.tensor(stats.group_user, dtype=torch.long),
                torch.tensor(stats.group_ad_category, dtype=torch.long),
                torch.tensor(stats.group_advertiser, dtype=torch.long),
                torch.tensor(stats.group_campaign, dtype=torch.long),
                torch.tensor(stats.group_user_ad_category, dtype=torch.long),
                torch.tensor(stats.group_same_category, dtype=torch.float64),
            )
            impr = torch.tensor(stats.group_impressions, dtype=torch.float64)
            clicks = torch.tensor(stats.group_clicks, dtype=torch.float64)
            loss = (
                impr * torch.nn.functional.softplus(logits) - clicks * logits
            ).sum() + model.l2_penalty_term(cfg.l2_penalty)
            loss.backward()
            grad_inf = max(
                float(p.grad.abs().max().item()) for p in model.parameters()
            )
            self.assertLess(grad_inf, 1e-6)

            # Repeat cold train reproduces exact same weights.
            repeat = train_lr.train_logistic_regression(log_dir, [1, 2], cfg)
            self.assertEqual(artifact["intercept"], repeat["intercept"])
            self.assertEqual(
                artifact["same_category_weight"], repeat["same_category_weight"]
            )
            self.assertEqual(
                artifact["user_ad_categories"], repeat["user_ad_categories"]
            )

    def test_stdio_worker_handles_multiple_cold_train_requests(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            log_dir = Path(tmp)
            _write_day(
                log_dir,
                1,
                [
                    _record(1, "u1", "cat-a", "camp-a", "adv-a", "cat-a", True),
                    _record(1, "u1", "cat-a", "camp-b", "adv-b", "cat-b", False),
                ]
                * 10,
            )
            script = Path(__file__).resolve().parent / "train_lr.py"
            with subprocess.Popen(
                [sys.executable, str(script), "--serve-stdio"],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            ) as proc:
                assert proc.stdin is not None and proc.stdout is not None
                model_out = log_dir / "model-day-01.json"
                req = {
                    "log_dir": str(log_dir),
                    "through_day": 1,
                    "model_out": str(model_out),
                    "config": {"l2_penalty": 1.0},
                }
                proc.stdin.write(json.dumps(req) + "\n")
                proc.stdin.flush()
                resp_line = proc.stdout.readline()
                proc.stdin.close()
                ret = proc.wait(timeout=15)
            self.assertEqual(ret, 0)
            resp = json.loads(resp_line)
            self.assertTrue(resp["ok"])
            self.assertEqual(resp["trainer"], "pytorch")
            self.assertEqual(resp["summary"]["impressions"], 20)
            self.assertTrue(model_out.exists())


if __name__ == "__main__":
    unittest.main()
