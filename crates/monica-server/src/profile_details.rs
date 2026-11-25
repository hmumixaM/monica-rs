//! Atomic, row-level notebook edits: rename/remove bundled details in one tx.
use super::{Field, Result, change_locked, invalid, lock};
use crate::auth::{ApiError, AppState, user};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::put,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/profiles/{id}/details", put(save).delete(remove))
}

#[derive(Deserialize, Serialize)]
pub(crate) struct DetailRef {
    pub(crate) field_key: String,
    pub(crate) item: Option<String>,
}
#[derive(Deserialize)]
pub(crate) struct Save {
    pub(crate) field_key: String,
    pub(crate) value: String,
    #[serde(default)]
    pub(crate) replaces: Vec<DetailRef>,
}
#[derive(Deserialize)]
struct Remove {
    refs: Vec<DetailRef>,
}

fn valid_ref(r: &DetailRef) -> bool {
    (1..=120).contains(&r.field_key.chars().count())
        && r.item
            .as_ref()
            .is_none_or(|s| (1..=120).contains(&s.chars().count()))
}

async fn current(tx: &mut Transaction<'_, Postgres>, id: Uuid, key: &str) -> Result<Option<Value>> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('source',r.source_type,'value',r.value) FROM crm.field_current c JOIN crm.field_revisions r ON r.id=c.revision_id WHERE c.profile_id=$1 AND c.field_key=$2")
        .bind(id).bind(key).fetch_optional(&mut **tx).await?)
}

async fn erase(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    actor: Uuid,
    r: &DetailRef,
) -> Result<()> {
    let row = current(tx, id, &r.field_key).await?;
    let (value, operation) = removed(row.as_ref(), r)?;
    change_locked(
        tx,
        id,
        actor,
        Field {
            field_key: r.field_key.clone(),
            value,
            source_type: None,
        },
        operation,
    )
    .await?;
    Ok(())
}

fn removed(row: Option<&Value>, r: &DetailRef) -> Result<(Value, &'static str)> {
    if !valid_ref(r) {
        return Err(invalid("invalid detail reference"));
    }
    let Some(row) = row.filter(|v| {
        !v["value"].is_null()
            && matches!(v["source"].as_str(), Some("manual" | "monica"))
            && !matches!(r.field_key.as_str(), "display_name" | "summary")
    }) else {
        return Err(ApiError(StatusCode::NOT_FOUND, "editable detail not found"));
    };
    let (value, operation) = if let Some(item) = &r.item {
        let mut bundle = row["value"]
            .as_object()
            .cloned()
            .ok_or(ApiError(StatusCode::NOT_FOUND, "detail not found in field"))?;
        if bundle.remove(item).is_none() {
            return Err(ApiError(StatusCode::NOT_FOUND, "detail not found in field"));
        }
        if bundle.is_empty() {
            (Value::Null, "manual_delete")
        } else {
            (Value::Object(bundle), "manual_edit")
        }
    } else {
        (Value::Null, "manual_delete")
    };
    Ok((value, operation))
}

async fn reserve(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    refs: &[&DetailRef],
    target: Option<Value>,
) -> Result<()> {
    let mut state = std::collections::BTreeMap::<String, Option<Value>>::new();
    let mut values = Vec::new();
    for r in refs {
        if !state.contains_key(&r.field_key) {
            state.insert(r.field_key.clone(), current(tx, id, &r.field_key).await?);
        }
        let row = state.get_mut(&r.field_key).unwrap();
        let (value, _) = removed(row.as_ref(), r)?;
        row.as_mut().unwrap()["value"] = value.clone();
        values.push(value);
    }
    if let Some(target) = target {
        values.push(target);
    }
    monica_core::content::register_many(tx, "field_value", &values)
        .await
        .map_err(|error| {
            tracing::error!(%error,"detail content registration failed");
            ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "Field content storage unavailable",
            )
        })?;
    Ok(())
}

async fn save(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(p): Json<Save>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    let mut tx = s.pool.begin().await?;
    let result = save_locked(&mut tx, id, actor, p).await?;
    tx.commit().await?;
    Ok(Json(result))
}

pub(crate) async fn save_locked(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    actor: Uuid,
    mut p: Save,
) -> Result<Value> {
    if !(1..=120).contains(&p.field_key.chars().count())
        || !(1..=20000).contains(&p.value.chars().count())
        || p.replaces.len() > 20
        || p.replaces.iter().any(|r| !valid_ref(r))
    {
        return Err(invalid("invalid detail"));
    }
    p.field_key = p.field_key.trim().into();
    p.value = p.value.trim().replace('\0', "");
    if p.field_key.is_empty() || p.value.is_empty() {
        return Err(invalid("A detail needs a name and a value"));
    }
    if matches!(p.field_key.as_str(), "display_name" | "summary") {
        return Err(invalid("Edit the name and summary from the profile header"));
    }
    lock(tx, id).await?;
    if let Some(row) = current(tx, id, &p.field_key).await?
        && !row["value"].is_null()
        && (!matches!(row["source"].as_str(), Some("manual" | "monica"))
            || row["value"].is_object())
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Imported data cannot be overwritten here",
        ));
    }
    let retired: Vec<_> = p
        .replaces
        .iter()
        .filter(|r| !(r.field_key == p.field_key && r.item.is_none()))
        .collect();
    reserve(tx, id, &retired, Some(json!(p.value))).await?;
    for r in &retired {
        erase(tx, id, actor, r).await?;
    }
    change_locked(
        tx,
        id,
        actor,
        Field {
            field_key: p.field_key.clone(),
            value: json!(p.value),
            source_type: None,
        },
        "manual_edit",
    )
    .await?;
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id,metadata) VALUES($1,'profile.detail.saved','profile',$2,$3)")
        .bind(actor).bind(id).bind(json!({"field_key":p.field_key,"replaced":retired})).execute(&mut **tx).await?;
    Ok(json!({"field_key":p.field_key,"value":p.value}))
}

async fn remove(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(p): Json<Remove>,
) -> Result<StatusCode> {
    let (actor, _) = user(&s, &h).await?;
    if !(1..=20).contains(&p.refs.len()) || p.refs.iter().any(|r| !valid_ref(r)) {
        return Err(invalid("invalid detail references"));
    }
    let mut tx = s.pool.begin().await?;
    lock(&mut tx, id).await?;
    reserve(&mut tx, id, &p.refs.iter().collect::<Vec<_>>(), None).await?;
    for r in &p.refs {
        erase(&mut tx, id, actor, r).await?;
    }
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id,metadata) VALUES($1,'profile.detail.removed','profile',$2,$3)")
        .bind(actor).bind(id).bind(json!({"refs":p.refs})).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
