import json
import subprocess
import sys
import tempfile
import unittest
from collections import Counter, defaultdict
from pathlib import Path

from marketplace_data import generate_marketplace


class MarketplaceDataTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.data = generate_marketplace(42)

    def test_inventory_counts_and_taxonomy(self):
        categories = {category["id"] for category in self.data["categories"]}
        self.assertEqual(len(self.data["categories"]), 100)
        self.assertEqual(len(categories), 100)
        advertisers = self.data["advertisers"]
        self.assertEqual(len(advertisers), 50)
        self.assertEqual(len({advertiser["id"] for advertiser in advertisers}), 50)
        catalog_items = []
        for advertiser in advertisers:
            self.assertEqual(len(advertiser["catalog"]), 20)
            catalog_items.extend(advertiser["catalog"])
        organic_items = self.data["organic_items"]
        self.assertEqual(len(organic_items), 5000)
        self.assertEqual(len({item["id"] for item in organic_items + catalog_items}), 6000)
        self.assertEqual({item["category_id"] for item in catalog_items}, categories)
        self.assertEqual(
            Counter(item["category_id"] for item in organic_items),
            Counter({category_id: 50 for category_id in categories}),
        )

    def test_users_browse_distinct_organic_items_in_sequence(self):
        categories = {category["id"] for category in self.data["categories"]}
        users = {user["id"]: user for user in self.data["users"]}
        organic = {item["id"]: item for item in self.data["organic_items"]}
        self.assertEqual(len(self.data["users"]), 1000)
        self.assertEqual(len(users), 1000)
        self.assertEqual(len(self.data["browsing_events"]), 12000)
        by_user = defaultdict(list)
        for event in self.data["browsing_events"]:
            by_user[event["user_id"]].append(event)
        self.assertEqual(by_user.keys(), users.keys())

        for user_id, events in by_user.items():
            interests = set(users[user_id]["preferred_category_ids"])
            self.assertEqual(len(interests), 5)
            self.assertTrue(interests <= categories)
            self.assertEqual([event["position"] for event in events], list(range(1, 13)))
            self.assertEqual(len({event["organic_item_id"] for event in events}), 12)
            self.assertEqual(
                Counter(event["reason"] for event in events),
                {"affinity": 9, "exploration": 3},
            )
            for event in events:
                category = organic[event["organic_item_id"]]["category_id"]
                self.assertEqual(category in interests, event["reason"] == "affinity")

    def test_seed_and_cli_export(self):
        self.assertEqual(generate_marketplace(42), self.data)
        self.assertNotEqual(generate_marketplace(43)["users"], self.data["users"])
        script = Path(__file__).resolve().parents[1] / "marketplace_data.py"
        with tempfile.TemporaryDirectory() as temp_dir:
            output_dir = Path(temp_dir) / "fresh"
            cmd = [sys.executable, str(script), "--output-dir", str(output_dir)]
            subprocess.run(cmd, check=True, capture_output=True)
            output = output_dir / "marketplace.json"
            first = output.read_bytes()
            self.assertEqual(json.loads(first), self.data)
            subprocess.run(cmd, check=True, capture_output=True)
            self.assertEqual(output.read_bytes(), first)
            self.assertEqual(list(output_dir.iterdir()), [output])
