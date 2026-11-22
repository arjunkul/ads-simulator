use std::collections::{HashMap, HashSet};
use std::fs;
use std::process::Command;

use ads_simulator::{
    BrowseReason, Item, Marketplace, User, generate_marketplace, score_preference,
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
        data.users
            .iter()
            .map(|user| &user.preferred_category_ids)
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
            user.preferred_category_ids
                .iter()
                .collect::<HashSet<_>>()
                .len(),
            5
        );
        assert!(
            user.preferred_category_ids
                .iter()
                .all(|category| categories.contains(category))
        );
        assert_eq!(
            events
                .iter()
                .map(|event| &event.organic_item_id)
                .collect::<HashSet<_>>()
                .len(),
            12
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.reason == BrowseReason::Affinity)
                .count(),
            9
        );
        for (index, event) in events.iter().enumerate() {
            assert_eq!(event.user_id, user.id);
            assert_eq!(event.position, index + 1);
            assert_eq!(
                user.preferred_category_ids
                    .contains(&organic[&event.organic_item_id].category_id),
                event.reason == BrowseReason::Affinity,
            );
        }
    }
}

#[test]
fn only_an_items_category_affects_preference() {
    let former_user = r#"{
        "id": "a", "preferred_category_ids": ["books"], "shopping_style": "researcher",
        "preference": {"item_budget_cents": 1000, "price_sensitivity_pct": 100, "ad_receptiveness_pct": 0}
    }"#;
    let reader: User = serde_json::from_str(former_user).unwrap();
    assert_eq!(
        serde_json::to_value(&reader).unwrap(),
        serde_json::json!({"id": "a", "preferred_category_ids": ["books"]})
    );
    let player = User {
        id: "b".into(),
        preferred_category_ids: vec!["games".into()],
    };
    let organic_book = Item {
        id: "organic-book".into(),
        category_id: "books".into(),
        name: "Book".into(),
        price_cents: 1_000,
    };
    let ad_book = Item {
        id: "ad-book".into(),
        price_cents: 10_000,
        category_id: organic_book.category_id.clone(),
        name: organic_book.name.clone(),
    };
    for book in [organic_book, ad_book] {
        assert_eq!(score_preference(&reader, &book), 1.0);
        assert_eq!(score_preference(&player, &book), 0.0);
    }
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
    assert_ne!(data.users, generate_marketplace(43).users);
    assert_eq!(run_export(), first);
    assert_eq!(fs::read_dir(output_dir).unwrap().count(), 1);
}
