use super::*;
use axum::{
    extract::{Path, Query, State},
    routing::get,
};
use monica_core::pages::{Coordinates, coordinates, text};
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Default, Deserialize)]
pub(super) struct Filter {
    provider: Option<String>,
    year: Option<i32>,
    kind: Option<String>,
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/metrics/locations", get(atlas))
        .route("/api/v1/profiles/{id}/locations", get(profile))
}
#[derive(Clone)]
struct Entry {
    point: Coordinates,
    profile: String,
    name: String,
    provider: String,
    kind: String,
    at: String,
    body: String,
    url: String,
    count: i64,
}
struct Point {
    point: Coordinates,
    count: i64,
    people: BTreeMap<String, String>,
    sources: BTreeSet<String>,
    events: Vec<Value>,
    months: BTreeMap<String, i64>,
    kind: String,
    first: String,
    last: String,
}
impl Point {
    fn value(self) -> Value {
        json!({"label":self.point.label,"latitude":self.point.latitude,"longitude":self.point.longitude,"precision":self.point.precision,"count":self.count,"sources":self.sources,"source_kind":self.kind,"people_count":self.people.len(),"people":self.people.into_iter().map(|(id,name)|json!({"id":id,"name":name})).collect::<Vec<_>>(),"events":self.events,"months":self.months,"first_seen":if self.first.is_empty(){None}else{Some(self.first)},"last_seen":if self.last.is_empty(){None}else{Some(self.last)}})
    }
}
pub(super) async fn build(
    tx: &mut Transaction<'_, Postgres>,
    id: Option<Uuid>,
    q: &Filter,
) -> Result<Value> {
    if let Some(id) = id {
        return profile_summary(tx, id).await;
    }
    let kind = q.kind.as_deref().unwrap_or("all");
    let mut entries = Vec::new();
    let mut unresolved = BTreeMap::<String, (BTreeMap<String, String>, BTreeSet<String>)>::new();
    let mut years = Vec::<i32>::new();
    if kind != "profile" {
        years=sqlx::query_scalar("SELECT DISTINCT extract(year from occurred_at)::int AS year_value FROM crm.page_posts WHERE occurred_at IS NOT NULL AND location IS NOT NULL AND location<>'null'::jsonb AND ($1::text IS NULL OR provider=$1) AND($2::uuid IS NULL OR profile_id=$2) ORDER BY year_value DESC").bind(&q.provider).bind(id).fetch_all(&mut **tx).await?;
        let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',e.id,'profile_id',p.id,'name',p.display_name,'provider',e.provider,'at',e.occurred_at,'text',left(e.text,240),'location',e.location,'coordinate_format',e.coordinate_format) FROM crm.page_posts e JOIN crm.directory_profiles p ON p.id=e.profile_id AND p.archived_at IS NULL AND p.profile_type='person' WHERE e.location IS NOT NULL AND e.location<>'null'::jsonb AND($1::text IS NULL OR e.provider=$1) AND($2::int IS NULL OR(e.occurred_at>=make_date($2,1,1)::timestamptz AND e.occurred_at<make_date($2+1,1,1)::timestamptz)) AND($3::uuid IS NULL OR e.profile_id=$3) ORDER BY e.occurred_at DESC NULLS LAST,e.id DESC").bind(&q.provider).bind(q.year).bind(id).fetch_all(&mut **tx).await?;
        for r in rows {
            if let Some(point) = coordinates(&r["location"], r["coordinate_format"].as_str()) {
                let pid = r["profile_id"].as_str().unwrap().to_owned();
                entries.push(Entry {
                    point,
                    profile: pid.clone(),
                    name: r["name"].as_str().unwrap().into(),
                    provider: r["provider"].as_str().unwrap().into(),
                    kind: "post".into(),
                    at: r["at"].as_str().unwrap_or("").into(),
                    body: r["text"].as_str().unwrap_or("").into(),
                    url: format!(
                        "/profiles/{pid}?tab=timeline&event={}",
                        r["id"].as_str().unwrap()
                    ),
                    count: 1,
                });
            }
        }
    }
    if kind != "post" {
        let fields:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('profile_id',p.id,'name',p.display_name,'field_key',c.field_key,'value',r.value,'source_type',r.source_type,'updated_at',r.observed_at) FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id JOIN crm.directory_profiles p ON p.id=c.profile_id AND p.archived_at IS NULL AND p.profile_type='person' WHERE ($1::uuid IS NULL OR p.id=$1) AND(c.field_key IN('wechat.city','wechat.province','wechat.country','wechat.region','linkedin.location','instagram.location','monica.city') OR c.field_key ~ '(address|addresses|location_history)$') ORDER BY p.id,c.field_key").bind(id).fetch_all(&mut **tx).await?;
        let mut profiles = BTreeMap::<String, BTreeMap<String, Value>>::new();
        let mut names = BTreeMap::new();
        for r in &fields {
            let pid = r["profile_id"].as_str().unwrap().to_owned();
            names.insert(pid.clone(), r["name"].as_str().unwrap().to_owned());
            profiles.entry(pid).or_default().insert(
                r["field_key"].as_str().unwrap().to_owned(),
                r["value"].clone(),
            );
        }
        for (pid, values) in &profiles {
            for provider in ["wechat", "linkedin", "instagram", "monica"] {
                if q.provider.as_deref().is_some_and(|p| p != provider) {
                    continue;
                }
                let read = |key: &str| values.get(key).and_then(text).unwrap_or_default();
                let label = if provider == "wechat" {
                    let city = read("wechat.city");
                    if city.is_empty() {
                        read("wechat.region")
                    } else {
                        city
                    }
                } else if provider == "monica" {
                    read("monica.city")
                } else {
                    read(&format!("{provider}.location"))
                };
                if label.is_empty() {
                    continue;
                }
                let country = if provider == "wechat" {
                    read("wechat.country")
                } else {
                    String::new()
                };
                let region = if provider == "wechat" {
                    read("wechat.province")
                } else {
                    String::new()
                };
                if let Some(point) = super::cities::resolve(&label, &country, &region) {
                    entries.push(Entry {
                        point,
                        profile: pid.clone(),
                        name: names[pid].clone(),
                        provider: provider.into(),
                        kind: "profile".into(),
                        at: String::new(),
                        body: names[pid].clone(),
                        url: format!("/profiles/{pid}"),
                        count: 1,
                    });
                } else {
                    let title = [&country, &region, &label]
                        .into_iter()
                        .filter(|value| !value.is_empty())
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" · ");
                    let u = unresolved.entry(title).or_default();
                    u.0.insert(pid.clone(), names[pid].clone());
                    u.1.insert(provider.into());
                }
            }
        }
        // Archived visit coordinates are a profile fallback only when there are no
        // actual post coordinates for that profile, matching the pinned profile map.
        for r in &fields {
            let key = r["field_key"].as_str().unwrap();
            if !key.ends_with("address")
                && !key.ends_with("addresses")
                && !key.ends_with("location_history")
            {
                continue;
            }
            let pid = r["profile_id"].as_str().unwrap();
            if id.is_some() && entries.iter().any(|e| e.profile == pid && e.kind == "post") {
                continue;
            }
            let provider = r["source_type"].as_str().unwrap_or("monica");
            if q.provider.as_deref().is_some_and(|p| p != provider) {
                continue;
            }
            let candidates = if let Some(a) = r["value"].as_array() {
                a.clone()
            } else if let Some(a) = r["value"]["places"].as_array() {
                a.clone()
            } else {
                vec![r["value"].clone()]
            };
            for v in candidates {
                if let Some(point) = coordinates(&v, None) {
                    entries.push(Entry {
                        point,
                        profile: pid.into(),
                        name: r["name"].as_str().unwrap().into(),
                        provider: provider.into(),
                        kind: "profile".into(),
                        at: v["last"].as_str().unwrap_or("").into(),
                        body: String::new(),
                        url: format!("/profiles/{pid}"),
                        count: v["visits"].as_i64().unwrap_or(1).max(0),
                    });
                }
            }
        }
    }
    entries.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| b.url.cmp(&a.url)));
    let mut points = BTreeMap::<(i64, i64, String), Point>::new();
    let mut people = BTreeSet::new();
    let mut sources = BTreeMap::<String, i64>::new();
    let mut post_records = 0;
    let mut profile_records = 0;
    for e in entries {
        people.insert(e.profile.clone());
        *sources.entry(e.provider.clone()).or_default() += e.count;
        if e.kind == "post" {
            post_records += e.count;
        } else {
            profile_records += e.count;
        }
        let key = (
            (e.point.latitude * 1000.).round() as i64,
            (e.point.longitude * 1000.).round() as i64,
            if id.is_some() {
                String::new()
            } else {
                e.kind.clone()
            },
        );
        let p = points.entry(key).or_insert_with(|| Point {
            point: e.point.clone(),
            count: 0,
            people: BTreeMap::new(),
            sources: BTreeSet::new(),
            events: Vec::new(),
            months: BTreeMap::new(),
            kind: e.kind.clone(),
            first: e.at.clone(),
            last: e.at.clone(),
        });
        p.count += e.count;
        p.people.insert(e.profile, e.name.clone());
        p.sources.insert(e.provider.clone());
        if !e.at.is_empty() {
            if p.first.is_empty() || e.at < p.first {
                p.first = e.at.clone();
            }
            if e.at > p.last {
                p.last = e.at.clone();
            }
            if e.at.len() >= 7 {
                *p.months.entry(e.at[..7].into()).or_default() += e.count;
            }
        }
        if p.events.len() < 8 {
            p.events.push(json!({"at":e.at,"text":if e.body.is_empty(){e.name}else{e.body},"source":e.provider,"url":e.url}));
        }
    }
    let mut points: Vec<Value> = points.into_values().map(Point::value).collect();
    points.sort_by(|a, b| {
        b["count"]
            .as_i64()
            .cmp(&a["count"].as_i64())
            .then_with(|| a["label"].as_str().cmp(&b["label"].as_str()))
    });
    let months: BTreeSet<String> = points
        .iter()
        .flat_map(|p| p["months"].as_object().unwrap().keys().cloned())
        .collect();
    let unresolved:Vec<Value>=unresolved.into_iter().map(|(label,(people,sources))|json!({"label":label,"count":people.len(),"sources":sources,"people":people.into_iter().map(|(id,name)|json!({"id":id,"name":name})).collect::<Vec<_>>()})).collect();
    let place_count = points.len();
    if id.is_some() {
        points.truncate(500);
    }
    Ok(
        json!({"points":points,"people":people.len(),"place_count":place_count,"located_records":post_records+profile_records,"post_records":post_records,"profile_records":profile_records,"source_counts":sources,"years":years,"months":months,"unresolved":unresolved,"city_reference":"GeoNames cities15000; exact names and country/state constraints; city centers only"}),
    )
}
async fn atlas(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Filter>,
) -> Result<Response> {
    if q.provider
        .as_deref()
        .is_some_and(|p| !["wechat", "instagram", "linkedin"].contains(&p))
        || q.kind
            .as_deref()
            .is_some_and(|k| !["all", "profile", "post"].contains(&k))
        || q.year.is_some_and(|y| !(1900..=2100).contains(&y))
    {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid location filter",
        ));
    }
    let mut tx = snapshot(&s, &h).await?;
    let value = build(&mut tx, None, &q).await?;
    tx.commit().await?;
    cached(
        &s,
        &h,
        format!("atlas:{:?}:{:?}:{:?}", q.provider, q.year, q.kind),
        value,
    )
    .await
}
async fn profile(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Response> {
    let mut tx = snapshot(&s, &h).await?;
    let id = canonical(&mut tx, id).await?;
    let value = build(&mut tx, Some(id), &Filter::default()).await?;
    tx.commit().await?;
    cached(&s, &h, format!("locations:{id}"), value).await
}

// A profile's map has the pinned coordinate-only contract. A city-center lookup
// belongs to the global atlas; it must not become a fictitious visited place.
async fn profile_summary(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<Value> {
    let kind: String = sqlx::query_scalar("SELECT profile_type FROM crm.profiles WHERE id=$1")
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    if kind == "group" {
        return Ok(
            json!({"profile_id":id,"total_posts_with_location":0,"place_count":0,"points":[],"field_sources":[]}),
        );
    }
    let mut rows: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',id,'provider',provider,'at',occurred_at,'location',location,'coordinate_format',coordinate_format,'text',left(text,120)) FROM crm.page_posts WHERE profile_id=$1 AND location IS NOT NULL AND location<>'null'::jsonb ORDER BY occurred_at DESC NULLS LAST,id DESC LIMIT 50000")
        .bind(id).fetch_all(&mut **tx).await?;
    let fields: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('field_key',c.field_key,'value',r.value,'source_type',r.source_type,'updated_at',r.observed_at) FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1 AND(c.field_key ILIKE '%address%' OR c.field_key ILIKE '%location%') ORDER BY r.observed_at DESC,c.field_key")
        .bind(id).fetch_all(&mut **tx).await?;
    if !rows
        .iter()
        .any(|row| coordinates(&row["location"], row["coordinate_format"].as_str()).is_some())
    {
        for field in &fields {
            let value = &field["value"];
            let candidates: Vec<&Value> = if let Some(items) = value.as_array() {
                items.iter().collect()
            } else if value.is_object() {
                value["places"]
                    .as_array()
                    .map_or_else(|| vec![value], |items| items.iter().collect())
            } else {
                Vec::new()
            };
            for candidate in candidates.into_iter().filter(|value| value.is_object()) {
                let count = candidate["visits"]
                    .as_i64()
                    .or_else(|| candidate["visits"].as_str().and_then(|v| v.parse().ok()))
                    .unwrap_or(0);
                let events: Vec<_> = candidate["moments"].as_array().into_iter().flatten()
                    .filter(|event| event["at"].as_str().is_some_and(|value| !value.is_empty()))
                    .map(|event| json!({"at":event["at"],"text":event.get("text").cloned().unwrap_or(json!("")),"source":field["source_type"],"url":null})).collect();
                rows.push(json!({"provider":field["source_type"],"location":candidate,"count":count,"first":candidate["first"],"last":candidate["last"],"kind":if candidate.get("visits").is_some(){"archived_visits"}else{"profile_address"},"events":events}));
            }
        }
    }
    let mut points = BTreeMap::<(String, i64, i64), Value>::new();
    for row in rows {
        let Some(point) = coordinates(&row["location"], row["coordinate_format"].as_str()) else {
            continue;
        };
        let key = (
            point.label.clone(),
            (point.latitude * 100000.).round_ties_even() as i64,
            (point.longitude * 100000.).round_ties_even() as i64,
        );
        let entry=points.entry(key).or_insert_with(|| json!({"label":point.label,"latitude":point.latitude,"longitude":point.longitude,"count":0,"first_seen":null,"last_seen":null,"sources":[],"events":[],"map_url":format!("https://www.openstreetmap.org/?mlat={}&mlon={}#map=14/{}/{}",point.latitude,point.longitude,point.latitude,point.longitude)}));
        entry["count"] =
            json!(entry["count"].as_i64().unwrap() + row["count"].as_i64().unwrap_or(1));
        let provider = row["provider"].as_str().unwrap_or("wechat");
        let sources = entry["sources"].as_array_mut().unwrap();
        if !sources.iter().any(|source| source == provider) {
            sources.push(json!(provider));
            sources.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        }
        if let Some(at) = row["at"].as_str() {
            if entry["first_seen"].as_str().is_none_or(|first| at < first) {
                entry["first_seen"] = json!(at);
            }
            if entry["last_seen"].as_str().is_none_or(|last| at > last) {
                entry["last_seen"] = json!(at);
            }
            entry["events"].as_array_mut().unwrap().push(json!({"at":at,"text":row["text"].as_str().unwrap_or(""),"source":provider,"url":format!("/profiles/{id}?tab=timeline&event={}",row["id"].as_str().unwrap())}));
        }
        for (from, to) in [
            ("first", "first_seen"),
            ("last", "last_seen"),
            ("kind", "source_kind"),
        ] {
            if row[from].as_str().is_some_and(|value| !value.is_empty()) {
                entry[to] = row[from].clone();
            }
        }
        if let Some(events) = row["events"].as_array() {
            entry["events"]
                .as_array_mut()
                .unwrap()
                .extend(events.iter().cloned());
        }
    }
    let mut points: Vec<_> = points.into_values().collect();
    points.sort_by(|a, b| {
        b["count"]
            .as_i64()
            .cmp(&a["count"].as_i64())
            .then_with(|| a["label"].as_str().cmp(&b["label"].as_str()))
    });
    let total: i64 = points
        .iter()
        .map(|point| point["count"].as_i64().unwrap())
        .sum();
    Ok(
        json!({"profile_id":id,"total_posts_with_location":total,"place_count":points.len(),"points":points,"field_sources":fields.iter().map(|field|json!({"field_key":field["field_key"],"source_type":field["source_type"],"updated_at":field["updated_at"]})).collect::<Vec<_>>()}),
    )
}
