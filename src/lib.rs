//! Synthetic marketplace data for Recipes and SimRuns.

mod taxonomy;

use std::collections::HashSet;

use rand::seq::{SliceRandom, index};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};
use taxonomy::TAXONOMY;

pub const SCHEMA_VERSION: u32 = 1;
pub const CURRENCY: &str = "USD";
pub const DEFAULT_SEED: i64 = 42;
pub const ADVERTISER_COUNT: usize = 50;
pub const CATALOG_SIZE: usize = 20;
pub const USER_COUNT: usize = 1_000;
pub const ORGANIC_PER_CATEGORY: usize = 50;
pub const INTEREST_COUNT: usize = 5;
pub const AFFINITY_VIEWS: usize = 9;
pub const EXPLORATION_VIEWS: usize = 3;
pub const TAXONOMY_GROUP_COUNT: usize = 10;
pub const CATEGORIES_PER_GROUP: usize = 10;

const _: () = assert!(TAXONOMY.len() == TAXONOMY_GROUP_COUNT);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marketplace {
    pub schema_version: u32,
    pub seed: i64,
    pub currency: String,
    pub categories: Vec<Category>,
    pub advertisers: Vec<Advertiser>,
    pub users: Vec<User>,
    pub organic_items: Vec<Item>,
    pub browsing_events: Vec<BrowsingEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    pub id: String,
    pub group: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Advertiser {
    pub id: String,
    pub name: String,
    pub specialty: String,
    pub daily_budget_cents: u32,
    pub catalog: Vec<Item>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub category_id: String,
    pub name: String,
    pub price_cents: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub preferred_category_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowsingEvent {
    pub user_id: String,
    pub organic_item_id: String,
    pub position: usize,
    pub reason: BrowseReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowseReason {
    Affinity,
    Exploration,
}

/// Static configuration and derived entity counts for schema v1 marketplace generation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MarketplaceSpec {
    pub schema_version: u32,
    pub currency: String,
    pub default_seed: i64,
    pub taxonomy_group_count: usize,
    pub categories_per_group: usize,
    pub category_count: usize,
    pub advertiser_count: usize,
    pub catalog_size: usize,
    pub total_catalog_items: usize,
    pub user_count: usize,
    pub organic_per_category: usize,
    pub total_organic_items: usize,
    pub interest_count: usize,
    pub affinity_views: usize,
    pub exploration_views: usize,
    pub browsing_events_per_user: usize,
    pub total_browsing_events: usize,
}

/// Return the static 10x10 product category taxonomy in stable ID order.
pub fn taxonomy() -> &'static [(&'static str, [&'static str; 10])] {
    TAXONOMY
}

/// Return the compiled marketplace generation specification and derived counts.
pub fn marketplace_spec() -> MarketplaceSpec {
    let category_count = TAXONOMY_GROUP_COUNT * CATEGORIES_PER_GROUP;
    let browsing_events_per_user = AFFINITY_VIEWS + EXPLORATION_VIEWS;
    MarketplaceSpec {
        schema_version: SCHEMA_VERSION,
        currency: CURRENCY.into(),
        default_seed: DEFAULT_SEED,
        taxonomy_group_count: TAXONOMY_GROUP_COUNT,
        categories_per_group: CATEGORIES_PER_GROUP,
        category_count,
        advertiser_count: ADVERTISER_COUNT,
        catalog_size: CATALOG_SIZE,
        total_catalog_items: ADVERTISER_COUNT * CATALOG_SIZE,
        user_count: USER_COUNT,
        organic_per_category: ORGANIC_PER_CATEGORY,
        total_organic_items: category_count * ORGANIC_PER_CATEGORY,
        interest_count: INTEREST_COUNT,
        affinity_views: AFFINITY_VIEWS,
        exploration_views: EXPLORATION_VIEWS,
        browsing_events_per_user,
        total_browsing_events: USER_COUNT * browsing_events_per_user,
    }
}

/// Return 1 for an item in a preferred category, or 0 for any other item.
pub fn score_preference(user: &User, item: &Item) -> f64 {
    if user.preferred_category_ids.contains(&item.category_id) {
        1.0
    } else {
        0.0
    }
}

/// Generate the schema v1 baseline from a reproducible signed seed.
pub fn generate_marketplace(seed: i64) -> Marketplace {
    let mut rng = ChaCha8Rng::seed_from_u64(seed.cast_unsigned());
    let mut categories = Vec::new();
    for (group, names) in TAXONOMY {
        for name in names {
            categories.push(Category {
                id: format!("category-{:03}", categories.len() + 1),
                group: (*group).into(),
                name: (*name).into(),
            });
        }
    }

    let advertisers = generate_advertisers(&categories, &mut rng);
    let organic_items = generate_organic_items(&categories, &mut rng);
    let (users, browsing_events) = generate_browsing(&categories, &organic_items, &mut rng);

    Marketplace {
        schema_version: SCHEMA_VERSION,
        seed,
        currency: CURRENCY.into(),
        categories,
        advertisers,
        users,
        organic_items,
        browsing_events,
    }
}

fn generate_advertisers(categories: &[Category], rng: &mut ChaCha8Rng) -> Vec<Advertiser> {
    let mut next_item = 1;
    (1..=ADVERTISER_COUNT)
        .map(|number| {
            let (group, _) = &TAXONOMY[(number - 1) % TAXONOMY.len()];
            let assortment: Vec<_> = categories
                .iter()
                .filter(|category| category.group == *group)
                .collect();
            // Every merchant carries its entire specialty plus extra items from it.
            let catalog = (0..CATALOG_SIZE)
                .map(|position| {
                    let category = assortment
                        .get(position)
                        .copied()
                        .unwrap_or_else(|| assortment[rng.random_range(0..assortment.len())]);
                    let item = Item {
                        id: format!("catalog-{next_item:04}"),
                        category_id: category.id.clone(),
                        name: format!("{} offer {next_item}", category.name),
                        price_cents: rng.random_range(5..=300) * 100,
                    };
                    next_item += 1;
                    item
                })
                .collect();
            Advertiser {
                id: format!("advertiser-{number:02}"),
                name: format!("{group} merchant {number:02}"),
                specialty: (*group).into(),
                daily_budget_cents: rng.random_range(10..=500) * 100,
                catalog,
            }
        })
        .collect()
}

fn generate_organic_items(categories: &[Category], rng: &mut ChaCha8Rng) -> Vec<Item> {
    let mut items = Vec::with_capacity(categories.len() * ORGANIC_PER_CATEGORY);
    for category in categories {
        for _ in 0..ORGANIC_PER_CATEGORY {
            let number = items.len() + 1;
            items.push(Item {
                id: format!("organic-{number:04}"),
                category_id: category.id.clone(),
                name: format!("{} listing {number}", category.name),
                price_cents: rng.random_range(5..=300) * 100,
            });
        }
    }
    items
}

fn generate_browsing(
    categories: &[Category],
    organic_items: &[Item],
    rng: &mut ChaCha8Rng,
) -> (Vec<User>, Vec<BrowsingEvent>) {
    let mut users = Vec::with_capacity(USER_COUNT);
    let mut events = Vec::with_capacity(USER_COUNT * (AFFINITY_VIEWS + EXPLORATION_VIEWS));
    let mut preference_sets = HashSet::with_capacity(USER_COUNT);

    for number in 1..=USER_COUNT {
        let user_id = format!("user-{number:04}");
        let interests = loop {
            let mut sample = index::sample(rng, categories.len(), INTEREST_COUNT).into_vec();
            sample.sort_unstable();
            if preference_sets.insert(sample.clone()) {
                break sample;
            }
        };
        let outside_interests: Vec<_> = (0..categories.len())
            .filter(|category| !interests.contains(category))
            .collect();

        let user = User {
            id: user_id.clone(),
            preferred_category_ids: interests
                .iter()
                .map(|&i| categories[i].id.clone())
                .collect(),
        };
        let affinity = sample_organic_items(rng, &interests, AFFINITY_VIEWS)
            .map(|item_index| (item_index, BrowseReason::Affinity));
        let exploration = sample_organic_items(rng, &outside_interests, EXPLORATION_VIEWS)
            .map(|item_index| (item_index, BrowseReason::Exploration));
        let mut views: Vec<_> = affinity.chain(exploration).collect();
        views.shuffle(rng);
        events.extend(
            views
                .into_iter()
                .enumerate()
                .map(|(position, (item_index, reason))| BrowsingEvent {
                    user_id: user_id.clone(),
                    organic_item_id: organic_items[item_index].id.clone(),
                    position: position + 1,
                    reason,
                }),
        );
        users.push(user);
    }

    (users, events)
}

fn sample_organic_items<'a>(
    rng: &mut ChaCha8Rng,
    category_indices: &'a [usize],
    views: usize,
) -> impl Iterator<Item = usize> + 'a {
    // Organic items occupy contiguous runs of equal size in category order.
    index::sample(rng, category_indices.len() * ORGANIC_PER_CATEGORY, views)
        .into_iter()
        .map(|position| {
            let category = category_indices[position / ORGANIC_PER_CATEGORY];
            category * ORGANIC_PER_CATEGORY + position % ORGANIC_PER_CATEGORY
        })
}
