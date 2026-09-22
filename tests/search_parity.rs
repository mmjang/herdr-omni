use herdr_omni::search;
use serde_json::{json, Value};
#[test]
fn matches_original_typescript_search_fixtures() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/search-parity.json")).unwrap();
    let items = fixture["items"].as_array().unwrap();
    for case in fixture["queries"].as_array().unwrap() {
        let query = case["query"].as_str().unwrap();
        let actual: Vec<_> = search::search(items, query, "All", &json!({}))
            .into_iter()
            .map(|v| v["id"].clone())
            .collect();
        assert_eq!(json!(actual), case["ids"], "query {query:?}");
    }
    for case in fixture["fuzzy"].as_array().unwrap() {
        let q = case["query"].as_str().unwrap();
        let value = case["value"].as_str().unwrap();
        let actual = search::fuzzy_score(q, value);
        if let Some(expected) = case["score"].as_f64() {
            assert_eq!(actual, expected, "score {q:?} {value:?}");
        } else {
            assert!(!actual.is_finite());
        }
        assert_eq!(
            json!(search::matching_positions(q, value)),
            case["positions"],
            "positions {q:?} {value:?}"
        );
    }
}

#[test]
fn matches_original_typescript_search_behavior_fixture() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/search-behavior-parity.json")).unwrap();
    let items = fixture["items"].as_array().unwrap();
    let history = &fixture["history"];
    for case in fixture["cases"].as_array().unwrap() {
        let query = case["query"].as_str().unwrap();
        let category = case["category"].as_str().unwrap();
        let actual: Vec<_> = search::search(items, query, category, history)
            .into_iter()
            .map(|item| {
                json!({
                    "id": item["id"],
                    "section": if item["category"] == "Worktrees" {
                        "Workspace"
                    } else {
                        item["category"].as_str().unwrap_or("")
                    }
                })
            })
            .collect();
        assert_eq!(
            json!(actual),
            case["expected"],
            "query {query:?} category {category:?}"
        );
    }
}

#[test]
fn matches_original_typescript_randomized_search_behavior() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/search-behavior-parity.json")).unwrap();
    let pool = fixture["random"]["pool"].as_array().unwrap();
    for scenario in fixture["random"]["scenarios"].as_array().unwrap() {
        let items: Vec<_> = scenario["indices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|index| pool[index.as_u64().unwrap() as usize].clone())
            .collect();
        let history = &scenario["history"];
        for case in scenario["cases"].as_array().unwrap() {
            let query = case["query"].as_str().unwrap();
            let category = case["category"].as_str().unwrap();
            let actual: Vec<_> = search::search(&items, query, category, history)
                .into_iter()
                .map(|item| {
                    json!({
                        "id": item["id"],
                        "section": if item["category"] == "Worktrees" {
                            "Workspace"
                        } else {
                            item["category"].as_str().unwrap_or("")
                        }
                    })
                })
                .collect();
            assert_eq!(
                json!(actual),
                case["expected"],
                "query {query:?} category {category:?}"
            );
        }
    }
    for case in fixture["random"]["fuzzy"].as_array().unwrap() {
        let query = case["query"].as_str().unwrap();
        let value = case["value"].as_str().unwrap();
        let actual = search::fuzzy_score(query, value);
        if let Some(expected) = case["score"].as_f64() {
            assert_eq!(actual, expected, "random score {query:?} {value:?}");
        } else {
            assert!(!actual.is_finite(), "random score {query:?} {value:?}");
        }
        assert_eq!(
            json!(search::matching_positions(query, value)),
            case["positions"],
            "random positions {query:?} {value:?}"
        );
    }
}
