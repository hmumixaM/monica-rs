//! Deterministic, bounded presentation helpers for compact page projections.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const DIRECTORY_COLUMNS: [&str; 18] = [
    "id",
    "type",
    "name",
    "summary",
    "avatar_media_id",
    "tag_ids",
    "company_ids",
    "school_ids",
    "aliases",
    "providers",
    "headline",
    "city",
    "birthday_md",
    "last_contact_at",
    "direct_message_count",
    "member_count",
    "conversation_id",
    "group_names",
];

/// The graph is bounded by the union of two 150-person rankings. Optimize
/// modularity with deterministic node order and aggregate communities until stable.
pub fn communities(weights: &[Vec<f64>]) -> Vec<Vec<usize>> {
    let n = weights.len();
    let mut graph = weights.to_vec();
    let mut members: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    loop {
        let degree: Vec<f64> = graph.iter().map(|r| r.iter().sum()).collect();
        let total: f64 = degree.iter().sum();
        if total <= 0.0 {
            return members;
        }
        let mut group: Vec<usize> = (0..graph.len()).collect();
        let mut totals = degree.clone();
        for _ in 0..100 {
            let mut moved = false;
            for i in 0..graph.len() {
                let old = group[i];
                totals[old] -= degree[i];
                let mut neighbors = BTreeMap::<usize, f64>::new();
                for (j, &weight) in graph[i].iter().enumerate() {
                    if j != i && weight > 0.0 {
                        *neighbors.entry(group[j]).or_default() += weight;
                    }
                }
                let score = |g: usize| {
                    neighbors.get(&g).copied().unwrap_or_default() - degree[i] * totals[g] / total
                };
                let mut best = old;
                let mut best_gain = score(old);
                for &candidate in neighbors.keys() {
                    let gain = score(candidate);
                    if gain > best_gain + 1e-10 {
                        best = candidate;
                        best_gain = gain;
                    }
                }
                group[i] = best;
                totals[best] += degree[i];
                moved |= old != best;
            }
            if !moved {
                break;
            }
        }
        let keys: BTreeSet<_> = group.iter().copied().collect();
        if keys.len() == graph.len() {
            return members;
        }
        let indices: BTreeMap<_, _> = keys.into_iter().enumerate().map(|(i, k)| (k, i)).collect();
        let mut next = vec![vec![0.0; indices.len()]; indices.len()];
        let mut next_members = vec![Vec::new(); indices.len()];
        for i in 0..graph.len() {
            let a = indices[&group[i]];
            next_members[a].extend(&members[i]);
            for j in 0..graph.len() {
                next[a][indices[&group[j]]] += graph[i][j];
            }
        }
        graph = next;
        members = next_members;
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Coordinates {
    pub label: String,
    pub latitude: f64,
    pub longitude: f64,
    pub precision: String,
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str()?.parse().ok())
        .filter(|n| n.is_finite())
}
pub fn coordinates(v: &Value, format: Option<&str>) -> Option<Coordinates> {
    let mut lat = number(v.get("latitude").or_else(|| v.get("lat"))?)?;
    let mut lon = number(
        v.get("longitude")
            .or_else(|| v.get("lon"))
            .or_else(|| v.get("lng"))?,
    )?;
    if format == Some("chatlog_sns_xml") {
        std::mem::swap(&mut lat, &mut lon);
    }
    if !(-90.0..=90.0).contains(&lat)
        || !(-180.0..=180.0).contains(&lon)
        || (lat == 0.0 && lon == 0.0)
    {
        return None;
    }
    let label = [
        "name", "poiName", "poi_name", "poi", "address", "city", "province", "country", "label",
    ]
    .iter()
    .find_map(|k| v[*k].as_str().filter(|s| !s.is_empty() && *s != "0"))
    .map(str::to_owned)
    .unwrap_or_else(|| format!("{lat:.4}, {lon:.4}"));
    Some(Coordinates {
        label,
        latitude: lat,
        longitude: lon,
        precision: v["precision"].as_str().unwrap_or("coordinate").into(),
    })
}

pub fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        Value::Array(a) => {
            let a: Vec<_> = a.iter().filter_map(text).collect();
            (!a.is_empty()).then(|| a.join(" · "))
        }
        _ => None,
    }
}
pub fn field(fields: &Value, keys: &[&str]) -> Value {
    keys.iter()
        .find_map(|k| text(&fields[*k]))
        .map_or(Value::Null, |v| json!(v))
}

/// The presentation contract in pinned profile_facts.py accepts scalar text and
/// numbers, preserving source preference order rather than alphabetizing aliases.
pub fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => (!value.trim().is_empty()).then(|| value.trim().to_owned()),
        Value::Number(_) => String::from_utf8(crate::identity::canonical_json(value)).ok(),
        _ => None,
    }
}
fn folded(value: &str) -> String {
    crate::identity::casefold(value)
}

pub fn aliases(name: &str, fields: &Value, identities: &[Value]) -> Vec<String> {
    let mut seen = BTreeSet::from([folded(name)]);
    let candidates = [
        "wechat.remark",
        "wechat.nickname",
        "wechat.alias",
        "monica.nickname",
    ]
    .into_iter()
    .map(|key| &fields[key])
    .chain(
        identities
            .iter()
            .flat_map(|pair| pair.as_array().into_iter().flatten()),
    );
    candidates
        .filter_map(scalar_text)
        .filter(|value| seen.insert(folded(value)))
        .take(8)
        .collect()
}
pub fn group_names(value: &Value, name: &str, aliases: &[String]) -> Vec<String> {
    let mut seen: BTreeSet<_> = std::iter::once(name)
        .chain(aliases.iter().map(String::as_str))
        .map(folded)
        .collect();
    scalar_text(value)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let name = line
                .split_once(['|', '｜'])
                .map_or(line, |(_, name)| name)
                .trim();
            (!name.is_empty() && seen.insert(folded(name))).then(|| name.to_owned())
        })
        .collect()
}
pub fn current_job(facts: &[Value]) -> Option<&Value> {
    let jobs: Vec<_> = facts
        .iter()
        .filter(|fact| {
            fact["type"] == "employment"
                && (scalar_text(&fact["company"]).is_some()
                    || scalar_text(&fact["title"]).is_some())
        })
        .collect();
    jobs.iter()
        .copied()
        .find(|fact| is_current(fact))
        .or_else(|| jobs.first().copied())
}
fn is_current(fact: &Value) -> bool {
    if let Some(value) = fact["is_current"].as_bool() {
        return value;
    }
    let Some(ended) = scalar_text(&fact["end_date"]) else {
        return true;
    };
    let ended = ended.to_lowercase();
    ["present", "current", "now", "至今", "现在"]
        .iter()
        .any(|word| ended.contains(word))
}
pub fn ordered_experience(facts: Vec<Value>) -> Vec<Value> {
    let mut seen = BTreeSet::new();
    let mut unique: Vec<_> = facts
        .into_iter()
        .filter(|fact| {
            let organization = if fact["company"].is_null() {
                &fact["school"]
            } else {
                &fact["company"]
            };
            let role = if fact["title"].is_null() {
                &fact["degree"]
            } else {
                &fact["title"]
            };
            seen.insert(crate::identity::canonical_json(&json!([
                fact["type"],
                organization,
                role,
                fact["start_date"],
                fact["end_date"]
            ])))
        })
        .collect();
    unique.sort_by(|a, b| {
        (a["type"] != "employment")
            .cmp(&(b["type"] != "employment"))
            .then_with(|| (!is_current(a)).cmp(&!is_current(b)))
            .then_with(|| {
                scalar_text(&b["start_date"])
                    .unwrap_or_default()
                    .cmp(&scalar_text(&a["start_date"]).unwrap_or_default())
            })
    });
    unique
}
pub fn headline(fields: &Value, facts: &[Value]) -> Value {
    let parts: Vec<String> = if let Some(job) = current_job(facts) {
        ["company", "title"]
            .iter()
            .filter_map(|key| scalar_text(&job[*key]))
            .collect()
    } else {
        ["monica.company", "monica.job"]
            .iter()
            .filter_map(|key| scalar_text(&fields[*key]))
            .collect()
    };
    if !parts.is_empty() {
        json!(parts.join(" · "))
    } else {
        scalar_text(&fields["linkedin.headline"]).map_or(Value::Null, Value::String)
    }
}
pub fn city(fields: &Value) -> Value {
    for key in [
        "monica.residence",
        "wechat.city",
        "linkedin.location",
        "wechat.province",
    ] {
        if let Some(value) = scalar_text(&fields[key]) {
            return json!(if key == "linkedin.location" {
                value.split(',').next().unwrap().trim().to_owned()
            } else {
                value
            });
        }
    }
    Value::Null
}

pub fn compact(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.retain(|_, value| match value {
            Value::Null => false,
            Value::String(text) => !text.is_empty(),
            Value::Array(items) => !items.is_empty(),
            Value::Object(items) => !items.is_empty(),
            Value::Number(number) => number.as_f64() != Some(0.),
            Value::Bool(value) => *value,
        });
    }
    value
}
pub fn excerpt(value: &Value, length: usize) -> Value {
    let Some(value) = value.as_str() else {
        return Value::Null;
    };
    let trimmed = value.trim();
    let lower = trimmed.to_lowercase();
    let placeholder = [
        "partial",
        "complete",
        "unknown",
        "unavailable",
        "expired",
        "missing",
        "instagram 动态",
        "linkedin 动态",
        "朋友圈",
    ]
    .contains(&lower.as_str())
        || ["", "instagram", "wechat", "linkedin"]
            .iter()
            .any(|provider| {
                lower.strip_prefix(provider).is_some_and(|tail| {
                    ["story", "stories", "post", "动态", "快拍"].contains(&tail.trim_start())
                })
            });
    if trimmed.is_empty() || placeholder {
        return Value::Null;
    }
    let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= length {
        json!(collapsed)
    } else {
        json!(format!(
            "{}…",
            collapsed
                .chars()
                .take(length.saturating_sub(1))
                .collect::<String>()
                .trim_end()
        ))
    }
}
pub fn presentation_experience(mut fact: Value) -> Value {
    if let Some(object) = fact.as_object_mut() {
        object.retain(|key, value| {
            [
                "type",
                "source",
                "company",
                "company_id",
                "school",
                "school_id",
                "title",
                "degree",
                "field_of_study",
                "location",
                "start_date",
                "end_date",
                "is_current",
                "description",
                "linkedin_url",
            ]
            .contains(&key.as_str())
                && !value.is_null()
                && value.as_str() != Some("")
                && !value.as_array().is_some_and(Vec::is_empty)
        });
    }
    fact
}

/// One coalesced, bounded worker tick; readers never trigger materialization.
pub async fn refresh_metrics(pool: &sqlx::PgPool) -> anyhow::Result<bool> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL statement_timeout='10s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await?;
    let changed = sqlx::query_scalar("SELECT crm.refresh_page_metrics()")
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn graph_separates_dense_groups_and_isolates() {
        let mut g = vec![vec![0.; 7]; 7];
        for group in [[0, 1, 2], [3, 4, 5]] {
            for a in group {
                for b in group {
                    if a != b {
                        g[a][b] = 1.;
                    }
                }
            }
        }
        g[2][3] = 0.01;
        g[3][2] = 0.01;
        let mut groups = communities(&g);
        for g in &mut groups {
            g.sort();
        }
        groups.sort();
        assert_eq!(groups, vec![vec![0, 1, 2], vec![3, 4, 5], vec![6]]);
    }
    #[test]
    fn pinned_python_presentation_oracle() {
        let golden: Value =
            serde_json::from_str(include_str!("pages_presentation_golden.json")).unwrap();
        for case in golden["aliases"].as_array().unwrap() {
            let args = &case["args"];
            assert_eq!(
                json!(aliases(
                    args[0].as_str().unwrap(),
                    &args[1],
                    args[2].as_array().unwrap()
                )),
                case["expected"]
            );
        }
        for case in golden["group_names"].as_array().unwrap() {
            let args = &case["args"];
            let known: Vec<String> = serde_json::from_value(args[2].clone()).unwrap();
            assert_eq!(
                json!(group_names(&args[0], args[1].as_str().unwrap(), &known)),
                case["expected"]
            );
        }
        for case in golden["headline"].as_array().unwrap() {
            assert_eq!(
                headline(&case["args"][0], case["args"][1].as_array().unwrap()),
                case["expected"]
            );
        }
        for case in golden["city"].as_array().unwrap() {
            assert_eq!(city(&case["args"]), case["expected"]);
        }
        for case in golden["excerpt"].as_array().unwrap() {
            assert_eq!(
                excerpt(&case["args"][0], case["args"][1].as_u64().unwrap() as usize),
                case["expected"]
            );
        }
        assert_eq!(
            json!(ordered_experience(
                golden["ordered_experience"]["args"]
                    .as_array()
                    .unwrap()
                    .clone()
            )),
            golden["ordered_experience"]["expected"]
        );
        assert_eq!(
            compact(golden["compact"]["args"].clone()),
            golden["compact"]["expected"]
        );
    }
    #[test]
    fn coordinates_validate_and_preserve_declared_source() {
        assert!(coordinates(&json!({"lat":"0","lng":"0"}), None).is_none());
        assert!(coordinates(&json!({"lat":91,"lng":1}), None).is_none());
        let p = coordinates(
            &json!({"latitude":"31.23","longitude":"121.47","name":"上海"}),
            Some("gcj02"),
        )
        .unwrap();
        assert_eq!(p.longitude, 121.47);
        let swapped = coordinates(
            &json!({"latitude":"2.35","longitude":"48.85","poiName":"Paris"}),
            Some("chatlog_sns_xml"),
        )
        .unwrap();
        assert_eq!((swapped.latitude, swapped.longitude), (48.85, 2.35));
        assert_eq!(swapped.label, "Paris");
        assert_eq!(p.label, "上海");
    }
}
