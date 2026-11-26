use std::collections::{HashMap, HashSet};
use std::fs;
use std::process::Command;

use ads_simulator::{
    ADVERTISER_COUNT, AFFINITY_VIEWS, BrowseReason, CATALOG_SIZE, CATEGORIES_PER_GROUP, CURRENCY,
    DEFAULT_SEED, EXPLORATION_VIEWS, Marketplace, MarketplaceSpec, ORGANIC_PER_CATEGORY,
    SCHEMA_VERSION, TAXONOMY_GROUP_COUNT, USER_COUNT, User, generate_marketplace,
    marketplace_spec, taxonomy,
};

#[test]
fn generates_the_requested_inventory_and_personas() {
    let data = generate_marketplace(42);
    let categories: HashSet<_> = data
        .categories
        .iter()
        .map(|category| &category.id)
        .collect();
    assert_eq!((data.categories.len(), categories.len()), (100, 100));
    assert_eq!(data.advertisers.len(), 50);
    assert_eq!(data.users.len(), 1_000);
    assert_eq!(data.organic_items.len(), 5_000);
    assert_eq!(
        data.users
            .iter()
            .map(|user| &user.id)
            .collect::<HashSet<_>>()
            .len(),
        data.users.len(),
    );
    assert_eq!(
        data.advertisers
            .iter()
            .map(|advertiser| &advertiser.id)
            .collect::<HashSet<_>>()
            .len(),
        data.advertisers.len(),
    );
    assert!(
        data.advertisers
            .iter()
            .all(|advertiser| advertiser.catalog.len() == 20)
    );

    let catalog: Vec<_> = data
        .advertisers
        .iter()
        .flat_map(|advertiser| &advertiser.catalog)
        .collect();
    assert_eq!(
        catalog
            .iter()
            .map(|item| &item.category_id)
            .collect::<HashSet<_>>(),
        categories
    );
    assert_eq!(
        catalog
            .into_iter()
            .chain(&data.organic_items)
            .map(|item| &item.id)
            .collect::<HashSet<_>>()
            .len(),
        6_000,
    );
    let mut organic_counts = HashMap::new();
    for item in &data.organic_items {
        *organic_counts.entry(&item.category_id).or_insert(0) += 1;
    }
    assert_eq!(organic_counts.len(), categories.len());
    assert!(
        categories
            .iter()
            .all(|category| organic_counts.get(category) == Some(&50))
    );
}

#[test]
fn every_user_browses_distinct_organic_items_with_affinity_and_exploration() {
    let data = generate_marketplace(-9);
    let organic: HashMap<_, _> = data
        .organic_items
        .iter()
        .map(|item| (&item.id, item))
        .collect();
    let categories: HashSet<_> = data
        .categories
        .iter()
        .map(|category| &category.id)
        .collect();
    assert_eq!(data.browsing_events.len(), 12_000);

    for (user, events) in data.users.iter().zip(data.browsing_events.chunks_exact(12)) {
        assert_eq!(
            events
                .iter()
                .map(|event| &event.organic_item_id)
                .collect::<HashSet<_>>()
                .len(),
            12
        );
        let mut affinity_categories = HashSet::new();
        let mut exploration_categories = HashSet::new();
        for (index, event) in events.iter().enumerate() {
            assert_eq!(event.user_id, user.id);
            assert_eq!(event.position, index + 1);
            let cat = &organic[&event.organic_item_id].category_id;
            assert!(categories.contains(cat));
            match event.reason {
                BrowseReason::Affinity => {
                    affinity_categories.insert(cat.as_str());
                }
                BrowseReason::Exploration => {
                    exploration_categories.insert(cat.as_str());
                }
            }
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| event.reason == BrowseReason::Affinity)
                .count(),
            9
        );
        assert!(affinity_categories.len() <= 5);
        assert!(affinity_categories.is_disjoint(&exploration_categories));
    }
}

#[test]
fn legacy_user_fields_deserialize_and_reserialize_as_id_only() {
    let former_user = r#"{
        "id": "a", "preferred_category_ids": ["books"], "shopping_style": "researcher",
        "preference": {"item_budget_cents": 1000, "price_sensitivity_pct": 100, "ad_receptiveness_pct": 0}
    }"#;
    let reader: User = serde_json::from_str(former_user).unwrap();
    assert_eq!(
        serde_json::to_value(&reader).unwrap(),
        serde_json::json!({"id": "a"})
    );
}

#[test]
fn cli_export_round_trips_and_reproduces_the_same_seed() {
    let temp = tempfile::tempdir().unwrap();
    let output_dir = temp.path().join("output");
    let run_export = || {
        let result = Command::new(env!("CARGO_BIN_EXE_ads-simulator"))
            .args(["--seed", "-42", "--output-dir"])
            .arg(&output_dir)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        fs::read(output_dir.join("marketplace.json")).unwrap()
    };

    let first = run_export();
    let data: Marketplace = serde_json::from_slice(&first).unwrap();
    assert_eq!(data, generate_marketplace(-42));
    assert_ne!(data.browsing_events, generate_marketplace(43).browsing_events);
    assert_eq!(run_export(), first);
    assert_eq!(fs::read_dir(output_dir).unwrap().count(), 1);
}

#[test]
fn taxonomy_matches_generated_categories_in_exact_order() {
    let groups = taxonomy();
    assert_eq!(groups.len(), TAXONOMY_GROUP_COUNT);
    assert_eq!(TAXONOMY_GROUP_COUNT, 10);
    assert_eq!(CATEGORIES_PER_GROUP, 10);

    let mut group_names = HashSet::new();
    let mut expected = Vec::with_capacity(TAXONOMY_GROUP_COUNT * CATEGORIES_PER_GROUP);
    for (group, names) in groups {
        assert!(!group.is_empty());
        assert!(group_names.insert(*group), "duplicate taxonomy group: {group}");
        assert_eq!(names.len(), CATEGORIES_PER_GROUP);
        for name in names {
            assert!(!name.is_empty());
            expected.push((
                format!("category-{:03}", expected.len() + 1),
                (*group).to_string(),
                (*name).to_string(),
            ));
        }
    }
    assert_eq!(expected.len(), 100);

    let data = generate_marketplace(DEFAULT_SEED);
    let actual: Vec<_> = data
        .categories
        .iter()
        .map(|category| {
            (
                category.id.clone(),
                category.group.clone(),
                category.name.clone(),
            )
        })
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn marketplace_spec_matches_constants_generated_data_and_round_trips_json() {
    let spec = marketplace_spec();
    assert_eq!(spec.schema_version, SCHEMA_VERSION);
    assert_eq!(spec.currency, CURRENCY);
    assert_eq!(spec.default_seed, DEFAULT_SEED);
    assert_eq!(spec.taxonomy_group_count, TAXONOMY_GROUP_COUNT);
    assert_eq!(spec.categories_per_group, CATEGORIES_PER_GROUP);
    assert_eq!(
        spec.category_count,
        TAXONOMY_GROUP_COUNT * CATEGORIES_PER_GROUP
    );
    assert_eq!(spec.advertiser_count, ADVERTISER_COUNT);
    assert_eq!(spec.catalog_size, CATALOG_SIZE);
    assert_eq!(spec.total_catalog_items, ADVERTISER_COUNT * CATALOG_SIZE);
    assert_eq!(spec.user_count, USER_COUNT);
    assert_eq!(spec.organic_per_category, ORGANIC_PER_CATEGORY);
    assert_eq!(
        spec.total_organic_items,
        spec.category_count * ORGANIC_PER_CATEGORY
    );
    assert_eq!(spec.affinity_views, AFFINITY_VIEWS);
    assert_eq!(spec.exploration_views, EXPLORATION_VIEWS);
    assert_eq!(
        spec.browsing_events_per_user,
        AFFINITY_VIEWS + EXPLORATION_VIEWS
    );
    assert_eq!(
        spec.total_browsing_events,
        USER_COUNT * spec.browsing_events_per_user
    );

    assert_eq!(
        spec,
        MarketplaceSpec {
            schema_version: 1,
            currency: "USD".into(),
            default_seed: 42,
            taxonomy_group_count: 10,
            categories_per_group: 10,
            category_count: 100,
            advertiser_count: 50,
            catalog_size: 20,
            total_catalog_items: 1_000,
            user_count: 1_000,
            organic_per_category: 50,
            total_organic_items: 5_000,
            affinity_views: 9,
            exploration_views: 3,
            browsing_events_per_user: 12,
            total_browsing_events: 12_000,
        }
    );

    // Cross-check against actual runtime output for DEFAULT_SEED.
    let data = generate_marketplace(DEFAULT_SEED);
    assert_eq!(data.schema_version, spec.schema_version);
    assert_eq!(data.currency, spec.currency);
    assert_eq!(data.seed, spec.default_seed);
    assert_eq!(data.categories.len(), spec.category_count);
    assert_eq!(data.advertisers.len(), spec.advertiser_count);
    assert!(
        data.advertisers
            .iter()
            .all(|advertiser| advertiser.catalog.len() == spec.catalog_size)
    );
    assert_eq!(
        data.advertisers
            .iter()
            .map(|advertiser| advertiser.catalog.len())
            .sum::<usize>(),
        spec.total_catalog_items
    );
    assert_eq!(data.users.len(), spec.user_count);
    assert_eq!(data.organic_items.len(), spec.total_organic_items);
    assert_eq!(data.browsing_events.len(), spec.total_browsing_events);
    assert_eq!(
        data.browsing_events
            .iter()
            .filter(|event| event.reason == BrowseReason::Affinity)
            .count(),
        spec.user_count * spec.affinity_views
    );
    assert_eq!(
        data.browsing_events
            .iter()
            .filter(|event| event.reason == BrowseReason::Exploration)
            .count(),
        spec.user_count * spec.exploration_views
    );

    // Verify CLI default seed (src/main.rs) matches DEFAULT_SEED without modifying src/main.rs.
    let temp = tempfile::tempdir().unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_ads-simulator"))
        .args(["--output-dir"])
        .arg(temp.path())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let cli_data: Marketplace =
        serde_json::from_slice(&fs::read(temp.path().join("marketplace.json")).unwrap()).unwrap();
    assert_eq!(cli_data, data);

    // Verify serde_json round-trip and Clone/Eq derives.
    let json = serde_json::to_string(&spec).unwrap();
    let round_tripped: MarketplaceSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(round_tripped, spec);
    assert_eq!(spec.clone(), spec);
}

#[test]
fn advertiser_omits_objective_and_ignores_legacy_objective_field() {
    let data = generate_marketplace(DEFAULT_SEED);
    for adv in &data.advertisers {
        let value = serde_json::to_value(adv).unwrap();
        assert!(
            value.get("objective").is_none(),
            "serialized Advertiser must not include dropped objective field"
        );
    }

    let legacy_json = r#"{
        "id": "advertiser-01",
        "name": "Electronics merchant 01",
        "specialty": "Electronics",
        "objective": "sales",
        "daily_budget_cents": 25000,
        "catalog": []
    }"#;
    let parsed: ads_simulator::Advertiser = serde_json::from_str(legacy_json).unwrap();
    assert_eq!(parsed.id, "advertiser-01");
    assert!(serde_json::to_value(&parsed).unwrap().get("objective").is_none());
}

