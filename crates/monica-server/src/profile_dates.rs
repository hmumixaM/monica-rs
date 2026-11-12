use super::{Result, invalid, lock};
use crate::auth::{ApiError, AppState, user};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{post, put},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/profiles/{id}/dates", post(create))
        .route(
            "/api/v1/profiles/{id}/dates/{date}",
            put(update).delete(remove),
        )
}

#[derive(Debug, Deserialize, Serialize)]
struct DateWrite {
    kind: String,
    label: Option<String>,
    precision: String,
    year: Option<i32>,
    month: Option<i32>,
    day: Option<i32>,
}

impl DateWrite {
    fn normalize(mut self, today: (i32, i32, i32)) -> Result<Self> {
        if !["birthday", "anniversary", "other"].contains(&self.kind.as_str()) {
            return Err(invalid("kind"));
        }
        if !["month_day", "approximate", "exact"].contains(&self.precision.as_str()) {
            return Err(invalid("precision"));
        }
        if self.label.as_ref().is_some_and(|s| s.chars().count() > 60) {
            return Err(invalid("label_length"));
        }
        self.label = self
            .label
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        if self.kind == "birthday" {
            self.label = None;
        }
        if self.year.is_some_and(|y| !(1800..=2200).contains(&y)) {
            return Err(invalid("year_range"));
        }
        if self.month.is_some_and(|m| !(1..=12).contains(&m))
            || self.day.is_some_and(|d| !(1..=31).contains(&d))
        {
            return Err(invalid("invalid_day"));
        }
        match self.precision.as_str() {
            "exact" => {
                if self.year.is_none() || self.month.is_none() || self.day.is_none() {
                    return Err(invalid("missing_exact_parts"));
                }
            }
            "month_day" => {
                if self.year.is_some() {
                    return Err(invalid("month_day_has_year"));
                }
                if self.month.is_none() || self.day.is_none() {
                    return Err(invalid("missing_month_day"));
                }
            }
            _ => {
                if self.year.is_none() && self.month.is_none() {
                    return Err(invalid("approximate_empty"));
                }
                if self.day.is_some() && self.month.is_none() {
                    return Err(invalid("day_without_month"));
                }
            }
        }
        if let (Some(month), Some(day)) = (self.month, self.day) {
            let year = self.year.unwrap_or(2024);
            let max = match month {
                4 | 6 | 9 | 11 => 30,
                2 => {
                    if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                        29
                    } else {
                        28
                    }
                }
                _ => 31,
            };
            if day > max {
                return Err(invalid("invalid_day"));
            }
        }
        if self.kind == "birthday"
            && self.year.is_some_and(|year| {
                year > today.0
                    || year == today.0
                        && self.month.is_some_and(|month| {
                            month > today.1
                                || month == today.1 && self.day.is_some_and(|day| day > today.2)
                        })
            })
        {
            return Err(invalid("future"));
        }
        Ok(self)
    }
}

async fn person(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<()> {
    lock(tx, id).await?;
    let kind: String = sqlx::query_scalar("SELECT profile_type FROM crm.profiles WHERE id=$1")
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    if kind != "person" {
        return Err(invalid("not_person"));
    }
    Ok(())
}

async fn presented(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<Value> {
    Ok(
        sqlx::query_scalar("SELECT value FROM crm.profile_dates_presented WHERE id=$1")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?,
    )
}

async fn save(
    s: &AppState,
    id: Uuid,
    date: Option<Uuid>,
    actor: Uuid,
    input: DateWrite,
) -> Result<Value> {
    let mut tx = s.pool.begin().await?;
    person(&mut tx, id).await?;
    let today:(i32,i32,i32)=sqlx::query_as("SELECT extract(year from now() AT TIME ZONE 'UTC')::int,extract(month from now() AT TIME ZONE 'UTC')::int,extract(day from now() AT TIME ZONE 'UTC')::int").fetch_one(&mut *tx).await?;
    let p = input.normalize(today)?;
    if let Some(date) = date {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM crm.profile_dates WHERE id=$1 AND profile_id=$2)",
        )
        .bind(date)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if !exists {
            return Err(ApiError(StatusCode::NOT_FOUND, "not_found"));
        }
    }
    let same:Option<Uuid>=sqlx::query_scalar("SELECT id FROM crm.profile_dates WHERE profile_id=$1 AND kind=$2 AND label IS NOT DISTINCT FROM $3 AND date_precision=$4 AND year IS NOT DISTINCT FROM $5 AND month IS NOT DISTINCT FROM $6 AND day IS NOT DISTINCT FROM $7")
        .bind(id).bind(&p.kind).bind(&p.label).bind(&p.precision).bind(p.year).bind(p.month).bind(p.day).fetch_optional(&mut *tx).await?;
    if let Some(same) = same {
        if date.is_some_and(|date| date != same) {
            return Err(ApiError(StatusCode::CONFLICT, "birthday_exists"));
        }
        let row = presented(&mut tx, same).await?;
        tx.commit().await?;
        return Ok(row);
    }
    if p.kind == "birthday" {
        let conflict:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.profile_dates WHERE profile_id=$1 AND kind='birthday' AND ($2::uuid IS NULL OR id<>$2))").bind(id).bind(date).fetch_one(&mut *tx).await?;
        if conflict {
            return Err(ApiError(StatusCode::CONFLICT, "birthday_exists"));
        }
    }
    let saved = date.unwrap_or_else(Uuid::new_v4);
    sqlx::query("INSERT INTO crm.profile_dates(id,profile_id,kind,label,date_precision,year,month,day,updated_by) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT(id) DO UPDATE SET kind=excluded.kind,label=excluded.label,date_precision=excluded.date_precision,year=excluded.year,month=excluded.month,day=excluded.day,updated_by=excluded.updated_by,updated_at=now()")
        .bind(saved).bind(id).bind(p.kind).bind(p.label).bind(p.precision).bind(p.year).bind(p.month).bind(p.day).bind(actor).execute(&mut *tx).await?;
    let row = presented(&mut tx, saved).await?;
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id,metadata) VALUES($1,$2,'profile',$3,$4)").bind(actor).bind(if date.is_some(){"profile.date.updated"}else{"profile.date.added"}).bind(id).bind(&row).execute(&mut *tx).await?;
    sqlx::query("UPDATE crm.profiles SET generation=generation+1 WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(row)
}

async fn create(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(p): Json<DateWrite>,
) -> Result<(StatusCode, Json<Value>)> {
    let (actor, _) = user(&s, &h).await?;
    Ok((
        StatusCode::CREATED,
        Json(save(&s, id, None, actor, p).await?),
    ))
}
async fn update(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((id, date)): Path<(Uuid, Uuid)>,
    Json(p): Json<DateWrite>,
) -> Result<Json<Value>> {
    let (actor, _) = user(&s, &h).await?;
    Ok(Json(save(&s, id, Some(date), actor, p).await?))
}
async fn remove(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((id, date)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode> {
    let (actor, _) = user(&s, &h).await?;
    let mut tx = s.pool.begin().await?;
    person(&mut tx, id).await?;
    let count = sqlx::query("DELETE FROM crm.profile_dates WHERE id=$1 AND profile_id=$2")
        .bind(date)
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if count == 0 {
        return Err(ApiError(StatusCode::NOT_FOUND, "not_found"));
    }
    sqlx::query("INSERT INTO crm.audit_events(user_id,action,entity_type,entity_id,metadata) VALUES($1,'profile.date.removed','profile',$2,jsonb_build_object('id',$3::uuid))").bind(actor).bind(id).bind(date).execute(&mut *tx).await?;
    sqlx::query("UPDATE crm.profiles SET generation=generation+1 WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pinned_python_date_normalization() {
        for fixture in [
            include_str!("../../../docs/contracts/crm-dates.json"),
            include_str!("../../../docs/contracts/desktop-dates.json"),
        ] {
            let data: Value = serde_json::from_str(fixture).unwrap();
            for case in data["cases"].as_array().unwrap() {
                let input: DateWrite = serde_json::from_value(case["input"].clone()).unwrap();
                match input.normalize((2026, 11, 11)) {
                    Ok(value) => assert_eq!(serde_json::to_value(value).unwrap(), case["value"]),
                    Err(error) => assert_eq!(error.1, case["error"].as_str().unwrap()),
                }
            }
        }
    }
    #[test]
    fn exact_and_partial_dates() {
        for raw in [
            r#"{"kind":"birthday","precision":"month_day","month":2,"day":29}"#,
            r#"{"kind":"anniversary","precision":"exact","year":2200,"month":1,"day":1}"#,
            r#"{"kind":"birthday","precision":"approximate","year":2000}"#,
        ] {
            serde_json::from_str::<DateWrite>(raw)
                .unwrap()
                .normalize((2026, 11, 11))
                .unwrap();
        }
        for (raw, code) in [
            (
                r#"{"kind":"birthday","precision":"exact","year":2026,"month":11,"day":12}"#,
                "future",
            ),
            (
                r#"{"kind":"birthday","precision":"exact","year":1900,"month":2,"day":29}"#,
                "invalid_day",
            ),
            (
                r#"{"kind":"other","precision":"month_day","year":2000,"month":2,"day":29}"#,
                "month_day_has_year",
            ),
            (
                r#"{"kind":"other","precision":"approximate","day":1}"#,
                "approximate_empty",
            ),
        ] {
            assert_eq!(
                serde_json::from_str::<DateWrite>(raw)
                    .unwrap()
                    .normalize((2026, 11, 11))
                    .unwrap_err()
                    .1,
                code
            );
        }
    }
}
