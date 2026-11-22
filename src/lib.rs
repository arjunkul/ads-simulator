//! Synthetic marketplace data for Recipes and SimRuns.

mod preference;
mod taxonomy;

use std::collections::HashSet;

use rand::seq::{SliceRandom, index};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};
use taxonomy::TAXONOMY;

pub use preference::{PreferenceProfile, PreferenceWeights, ScoredItem, score_preference};

const ADVERTISER_COUNT: usize = 50;
const CATALOG_SIZE: usize = 20;
const USER_COUNT: usize = 1_000;
const ORGANIC_PER_CATEGORY: usize = 50;
const INTEREST_COUNT: usize = 5;
const AFFINITY_VIEWS: usize = 9;
const EXPLORATION_VIEWS: usize = 3;

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    pub id: String,
    pub group: String,
    pub name: String,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Advertiser {
    pub id: String,
    pub name: String,
    pub specialty: String,
    pub objective: String,
    pub daily_budget_cents: u32,
    pub catalog: Vec<Item>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub category_id: String,
    pub name: String,
    pub price_cents: u32,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub preferred_category_ids: Vec<String>,
    pub shopping_style: String,
    #[serde(default)]
    pub preference: PreferenceProfile,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
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
        schema_version: 1,
        seed,
        currency: "USD".into(),
        categories,
        advertisers,
        users,
        organic_items,
        browsing_events,
    }
}

fn generate_advertisers(categories: &[Category], rng: &mut ChaCha8Rng) -> Vec<Advertiser> {
    let objectives = ["awareness", "traffic", "sales"];
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
                objective: objectives[rng.random_range(0..objectives.len())].into(),
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
    let mut profiles = HashSet::with_capacity(USER_COUNT);
    let weights = PreferenceWeights::default();
    let shopping_styles = ["researcher", "deal_seeker", "casual"];

    for number in 1..=USER_COUNT {
        let user_id = format!("user-{number:04}");
        let mut interests = index::sample(rng, categories.len(), INTEREST_COUNT).into_vec();
        interests.sort_unstable();
        let outside_interests: Vec<_> = (0..categories.len())
            .filter(|category| !interests.contains(category))
            .collect();

        let preference = loop {
            let profile = PreferenceProfile {
                item_budget_cents: rng.random_range(2_000..=30_000),
                price_sensitivity_pct: rng.random_range(0..=100),
                ad_receptiveness_pct: rng.random_range(0..=100),
            };
            if profiles.insert(profile) {
                break profile;
            }
        };
        let user = User {
            id: user_id.clone(),
            preferred_category_ids: interests
                .iter()
                .map(|&i| categories[i].id.clone())
                .collect(),
            shopping_style: shopping_styles[rng.random_range(0..shopping_styles.len())].into(),
            preference,
        };
        let ranked_sample = |rng: &mut ChaCha8Rng, category_indices: &[usize], views: usize| {
            // Rank a limited random slate so browsing still varies within each category.
            let mut candidates: Vec<_> = sample_organic_items(rng, category_indices, views * 3)
                .map(|item_index| {
                    let score = score_preference(
                        &user,
                        ScoredItem::Organic(&organic_items[item_index]),
                        weights,
                    );
                    (item_index, score)
                })
                .collect();
            candidates.sort_by(|(_, left), (_, right)| right.total_cmp(left));
            candidates.into_iter().take(views).map(|(index, _)| index)
        };
        let affinity = ranked_sample(rng, &interests, AFFINITY_VIEWS)
            .map(|item_index| (item_index, BrowseReason::Affinity));
        let exploration = ranked_sample(rng, &outside_interests, EXPLORATION_VIEWS)
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
