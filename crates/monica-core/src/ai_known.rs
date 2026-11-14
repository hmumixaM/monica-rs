//! Structured facts are already useful records, not missing generic fields.
use serde_json::Value;
use sqlx::{Executor, Postgres};
use std::collections::BTreeMap;
use uuid::Uuid;

pub async fn structured<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    profiles: &[Uuid],
) -> Result<BTreeMap<Uuid, Vec<Value>>, sqlx::Error> {
    let rows: Vec<(Uuid,Value)>=sqlx::query_as("SELECT profile_id,jsonb_build_object('kind',fact_type,'revision_id',current_revision_id,'source_type',current_source_type,'company',left(coalesce(company_name,value->>'company',''),512),'job',left(coalesce(value->>'title',value->>'job_title',''),512),'school',left(coalesce(school_name,value->>'school',''),512),'residence',left(coalesce(value->>'city',value->>'locality',value->>'formatted',''),512),'observed_at',updated_at) FROM crm.profile_structured_current WHERE profile_id=ANY($1) ORDER BY profile_id,fact_type,fact_key,current_revision_id")
        .bind(profiles).fetch_all(executor).await?;
    let mut known = BTreeMap::<Uuid, Vec<Value>>::new();
    for (profile, value) in rows {
        known.entry(profile).or_default().push(value);
    }
    Ok(known)
}

fn normal(value: &str) -> String {
    crate::collector::casefold(value)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

pub fn represented(item: &Value, known: &[Value]) -> bool {
    let kind = item["kind"].as_str().unwrap_or_default();
    let fact = match kind {
        "company" | "job" => "employment",
        "school" => "education",
        "residence" => "address",
        _ => return false,
    };
    let value = normal(item["value"].as_str().unwrap_or_default());
    if value.is_empty() {
        return false;
    }
    let evidence = item["evidence"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let background_only = !evidence.is_empty()
        && evidence.iter().all(|e| {
            matches!(
                e["kind"].as_str(),
                Some("profile" | "field" | "structured" | "employment" | "education" | "address")
            )
        });
    let linkedin_background = background_only
        && evidence.iter().all(|e| {
            e["provider"] == "linkedin"
                || e["source_type"] == "linkedin"
                || e["field_key"]
                    .as_str()
                    .is_some_and(|k| k.starts_with("linkedin."))
        });
    let quote = normal(item["quote"].as_str().unwrap_or_default());
    known.iter().filter(|k| k["kind"] == fact).any(|k| {
        let recorded = normal(k[kind].as_str().unwrap_or_default());
        if recorded.is_empty() {
            return false;
        }
        value == recorded
            || (k["source_type"] == "linkedin"
                && (linkedin_background
                    || (background_only
                        && recorded.chars().count() >= 3
                        && quote.contains(&recorded))))
    })
}
