use monica_core::link_matching::{self as m, AccountSignals, Snapshot};
use serde_json::{Value, json};
#[test]
fn pinned_python_normalization_scoring_and_candidate_keys() {
    let fixture: Value =
        serde_json::from_str(include_str!("../data/link-matching/contract-vectors.json")).unwrap();
    for v in fixture["normalization"].as_array().unwrap() {
        let s = v["input"].as_str().unwrap();
        assert_eq!(m::words(s), v["words"], "words {s}");
        assert_eq!(m::compact(s), v["compact"], "compact {s}");
        assert_eq!(m::pinyin_compact(s), v["pinyin"], "pinyin {s}");
        assert_eq!(m::username_words(s), v["username"], "username {s}");
    }
    for (i, v) in fixture["fuzzy"].as_array().unwrap().iter().enumerate() {
        let a = v["a"].as_str().unwrap();
        let b = v["b"].as_str().unwrap();
        assert!(
            (m::ratio(a, b) - v["ratio"].as_f64().unwrap()).abs() < 1e-9,
            "ratio {i}"
        );
        assert!(
            (m::weighted_ratio(a, b) - v["weighted"].as_f64().unwrap()).abs() < 1e-9,
            "weighted {i}: {a:?} {b:?} {} != {}",
            m::weighted_ratio(a, b),
            v["weighted"]
        );
    }
    for (i, v) in fixture["scores"].as_array().unwrap().iter().enumerate() {
        let a: AccountSignals = serde_json::from_value(v["source"].clone()).unwrap();
        let b: AccountSignals = serde_json::from_value(v["target"].clone()).unwrap();
        assert_eq!(
            serde_json::to_value(m::score_pair(&a, &b)).unwrap(),
            v["expected"],
            "score vector {i}"
        );
        assert_eq!(
            json!(m::candidate_keys(&a)),
            v["source_keys"],
            "keys vector {i}"
        );
    }
}
fn account(id: &str, provider: &str, profile: Option<&str>) -> AccountSignals {
    AccountSignals {
        account_id: id.into(),
        provider: provider.into(),
        external_id: format!("{provider}_{id}"),
        display_name: "Alice Smith".into(),
        profile_id: profile.map(str::to_owned),
        profile_name: "Alice Smith".into(),
        ready: true,
        ..Default::default()
    }
}
#[test]
fn candidates_respect_ownership_rejection_reverse_moves_and_ambiguous_claims() {
    let mut s = Snapshot {
        accounts: vec![
            account("wechat", "wechat", Some("home")),
            account("instagram", "instagram", Some("other")),
            account("second", "instagram", None),
        ],
        ..Default::default()
    };
    s.providers.insert("home".into(), ["wechat".into()].into());
    s.providers
        .insert("other".into(), ["instagram".into()].into());
    let rows = m::queue(&s);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r["provider"] == "instagram"));
    assert!(rows.iter().all(|r| r["conflict"] == true));
    s.decisions
        .insert(("second".into(), "home".into()), "rejected".into());
    let rows = m::queue(&s);
    assert_eq!(
        rows.iter().find(|r| r["account_id"] == "second").unwrap()["status"],
        "no_match"
    );
    s.decisions
        .insert(("second".into(), "home".into()), "skipped".into());
    assert_eq!(
        m::queue(&s)
            .iter()
            .find(|r| r["account_id"] == "second")
            .unwrap()["status"],
        "skipped"
    );
    s.providers
        .get_mut("home")
        .unwrap()
        .insert("instagram".into());
    assert!(m::queue(&s).iter().all(|r| r["provider"] != "instagram"));
}
