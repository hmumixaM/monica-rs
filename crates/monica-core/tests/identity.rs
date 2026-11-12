use monica_core::{Provider, identity};
use serde_json::Value;

#[test]
fn pinned_python_golden_vectors() {
    for fixture in [
        include_str!("../../../docs/contracts/crm-golden.json"),
        include_str!("../../../docs/contracts/desktop-golden.json"),
    ] {
        let data: Value = serde_json::from_str(fixture).unwrap();
        for case in data["canonical"].as_array().unwrap() {
            let value = &case["input"];
            assert_eq!(
                String::from_utf8(identity::canonical_json(value)).unwrap(),
                case["canonical_utf8"].as_str().unwrap(),
                "input {value}"
            );
            assert_eq!(
                identity::observation_hash(value),
                case["observation_sha256"]
            );
            assert_eq!(identity::content_hash(value), case["content_sha256"]);
        }
        for case in data["identities"].as_array().unwrap() {
            let provider: Provider = serde_json::from_value(case["provider"].clone()).unwrap();
            let account =
                identity::account_id(provider, case["account"].as_str().unwrap()).unwrap();
            assert_eq!(account.to_string(), case["account_id"]);
            let object = identity::object_id(
                account,
                case["kind"].as_str().unwrap(),
                case["external_id"].as_str().unwrap(),
            );
            assert_eq!(object.to_string(), case["object_id"]);
        }
        for case in data["lexical"].as_array().unwrap() {
            let value: Value = serde_json::from_str(case["input_json"].as_str().unwrap()).unwrap();
            identity::validate_json(&value).unwrap();
            assert_eq!(
                String::from_utf8(identity::canonical_json(&value)).unwrap(),
                case["canonical_utf8"]
            );
            assert_eq!(
                identity::observation_hash(&value),
                case["observation_sha256"]
            );
        }
        for case in data["invalid_accounts"].as_array().unwrap() {
            if let Ok(provider) = serde_json::from_value::<Provider>(case["provider"].clone()) {
                assert_eq!(
                    identity::account_id(provider, case["account"].as_str().unwrap())
                        .unwrap_err()
                        .to_string(),
                    case["error"]
                );
            } else {
                assert_eq!(case["provider"], "unknown");
            }
        }
    }
}

#[test]
fn nonfinite_overflow_rejected_before_persistence() {
    let value: Value = serde_json::from_str(r#"{"nested":[1e999]}"#).unwrap();
    assert!(identity::validate_json(&value).is_err());
}

#[test]
fn record_uses_explicit_legacy_object_kind() {
    let mut record: monica_core::Record = serde_json::from_str(
        r#"{"provider":"wechat","account":"synthetic-account","object":"synthetic-object","event":"e","payload":{}}"#,
    )
    .unwrap();
    let account = identity::account_id(record.provider, &record.account).unwrap();
    assert_eq!(record.kind, "record");
    assert_eq!(
        monica_core::object_id(&record).unwrap(),
        identity::object_id(account, "record", &record.object)
    );
    record.kind = "post".into();
    assert_eq!(
        monica_core::object_id(&record).unwrap(),
        identity::object_id(account, "post", &record.object)
    );
}

#[test]
fn actual_python_identity_presentation() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../docs/contracts/identity-presentation.json"
    ))
    .unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let actual = monica_core::identity::presented_identity_key(
            case["provider"].as_str().unwrap(),
            case["external_id"].as_str().unwrap(),
            case["profile_url"].as_str(),
        );
        assert_eq!(serde_json::json!(actual), case["key"], "{case}");
    }
    assert_eq!(
        serde_json::json!(monica_core::identity::present_identities(
            fixture["rows"].as_array().unwrap().clone()
        )),
        fixture["presented"]
    );
}

#[test]
fn actual_python_source_timestamps() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("../../../docs/contracts/source-times.json")).unwrap();
    for case in cases.as_array().unwrap() {
        assert_eq!(
            serde_json::json!(monica_core::source_time::iso(&case["input"])),
            case["expected"],
            "{case}"
        );
    }
}
