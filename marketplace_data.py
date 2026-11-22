"""Generate a deterministic synthetic dataset for ads marketplace SimRuns."""

import argparse
import json
import random
from pathlib import Path
from tempfile import NamedTemporaryFile


# The order is part of the schema because it determines stable category IDs.
TAXONOMY = {
    "Electronics": (
        "Phones", "Laptops", "Tablets", "Monitors", "Headphones", "Speakers",
        "Cameras", "Wearables", "Networking", "Chargers",
    ),
    "Home": (
        "Lighting", "Bedding", "Bath Linens", "Curtains", "Rugs", "Furniture",
        "Decor", "Storage", "Cleaning Supplies", "Home Security",
    ),
    "Kitchen": (
        "Cookware", "Bakeware", "Cutlery", "Dinnerware", "Drinkware",
        "Small Appliances", "Food Storage", "Coffee Equipment", "Kitchen Tools",
        "Water Filters",
    ),
    "Apparel": (
        "Shirts", "Pants", "Dresses", "Outerwear", "Activewear", "Footwear",
        "Hats", "Bags", "Jewelry", "Sunglasses",
    ),
    "Sports": (
        "Running Gear", "Cycling Gear", "Fitness Equipment", "Yoga Gear",
        "Team Sports", "Racquet Sports", "Swimming Gear", "Winter Sports",
        "Skateboarding Gear", "Golf Gear",
    ),
    "Outdoors": (
        "Tents", "Sleeping Bags", "Backpacks", "Camp Stoves", "Hiking Gear",
        "Fishing Gear", "Binoculars", "Lanterns", "Water Bottles", "Coolers",
    ),
    "Beauty": (
        "Skin Care", "Hair Care", "Makeup", "Fragrance", "Nail Care",
        "Shaving Supplies", "Oral Care", "Bath Products", "Styling Tools",
        "Personal Care Accessories",
    ),
    "Hobbies": (
        "Board Games", "Puzzles", "Books", "Art Supplies", "Craft Supplies",
        "Collectibles", "Model Kits", "Musical Instruments", "Video Games",
        "Stationery",
    ),
    "Pets": (
        "Dog Food", "Cat Food", "Pet Treats", "Pet Toys", "Pet Beds", "Carriers",
        "Grooming Supplies", "Aquarium Supplies", "Bird Supplies", "Leashes",
    ),
    "Garden and Tools": (
        "Seeds", "Planters", "Hand Tools", "Power Tools", "Irrigation", "Soil",
        "Garden Furniture", "Grills", "Safety Gear", "Hardware",
    ),
}
ADVERTISER_COUNT = 50
CATALOG_SIZE = 20
USER_COUNT = 1000
ORGANIC_ITEMS_PER_CATEGORY = 50
INTEREST_COUNT = 5
AFFINITY_VIEWS = 9
EXPLORATION_VIEWS = 3


def generate_marketplace(seed: int) -> dict:
    """Return a marketplace with catalog items, organic items, and user views."""
    rng = random.Random(seed)
    categories = [
        {"id": f"category-{number:03d}", "group": group, "name": name}
        for number, (group, name) in enumerate(
            ((group, name) for group, names in TAXONOMY.items() for name in names),
            start=1,
        )
    ]
    category_ids = [category["id"] for category in categories]

    advertisers = []
    next_catalog_item = 1
    for number in range(1, ADVERTISER_COUNT + 1):
        # Five advertisers share each group, with different assortments and budgets.
        group = list(TAXONOMY)[(number - 1) % len(TAXONOMY)]
        assortment = [category for category in categories if category["group"] == group]
        catalog = []
        for item_position in range(CATALOG_SIZE):
            # Cover all ten categories in a specialty before adding more inventory.
            category = (
                assortment[item_position]
                if item_position < len(assortment)
                else rng.choice(assortment)
            )
            catalog.append({
                "id": f"catalog-{next_catalog_item:04d}",
                "category_id": category["id"],
                "name": f"{category['name']} offer {next_catalog_item}",
                "price_cents": rng.randrange(500, 30_001, 100),
            })
            next_catalog_item += 1
        advertisers.append({
            "id": f"advertiser-{number:02d}",
            "name": f"{group} merchant {number:02d}",
            "specialty": group,
            "objective": rng.choice(("awareness", "traffic", "sales")),
            "daily_budget_cents": rng.randrange(1_000, 50_001, 100),
            "catalog": catalog,
        })

    organic_items = []
    organic_ids_by_category = {category_id: [] for category_id in category_ids}
    for category in categories:
        for _ in range(ORGANIC_ITEMS_PER_CATEGORY):
            number = len(organic_items) + 1
            item_id = f"organic-{number:04d}"
            organic_items.append({
                "id": item_id,
                "category_id": category["id"],
                "name": f"{category['name']} listing {number}",
                "price_cents": rng.randrange(500, 30_001, 100),
            })
            organic_ids_by_category[category["id"]].append(item_id)

    users = []
    browsing_events = []
    for number in range(1, USER_COUNT + 1):
        user_id = f"user-{number:04d}"
        interests = sorted(rng.sample(category_ids, INTEREST_COUNT))
        users.append({
            "id": user_id,
            "preferred_category_ids": interests,
            "shopping_style": rng.choice(("researcher", "deal_seeker", "casual")),
        })
        interested_ids = set(interests)
        affinity_pool = [
            item_id for category_id in interests
            for item_id in organic_ids_by_category[category_id]
        ]
        exploration_pool = [
            item_id for category_id in category_ids if category_id not in interested_ids
            for item_id in organic_ids_by_category[category_id]
        ]
        views = [
            (item_id, "affinity") for item_id in rng.sample(affinity_pool, AFFINITY_VIEWS)
        ] + [
            (item_id, "exploration")
            for item_id in rng.sample(exploration_pool, EXPLORATION_VIEWS)
        ]
        rng.shuffle(views)
        browsing_events.extend(
            {
                "user_id": user_id,
                "organic_item_id": item_id,
                "position": position,
                "reason": reason,
            }
            for position, (item_id, reason) in enumerate(views, start=1)
        )

    return {
        "schema_version": 1,
        "seed": seed,
        "currency": "USD",
        "categories": categories,
        "advertisers": advertisers,
        "users": users,
        "organic_items": organic_items,
        "browsing_events": browsing_events,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--seed", type=int, default=42)
    args = parser.parse_args()
    data = generate_marketplace(args.seed)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    output = args.output_dir / "marketplace.json"
    # Readers see either the old complete dataset or the new complete dataset.
    with NamedTemporaryFile(
        mode="w", encoding="utf-8", dir=args.output_dir, delete=False
    ) as temporary:
        temporary_path = Path(temporary.name)
        try:
            json.dump(data, temporary, separators=(",", ":"))
            temporary.write("\n")
            temporary.close()
            temporary_path.replace(output)
        finally:
            temporary_path.unlink(missing_ok=True)
    print(output)


if __name__ == "__main__":
    main()
