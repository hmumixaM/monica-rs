use super::{Lookup, aggregations::month, arg, array, id, integer, next, optional_id, s};
use anyhow::{Context, Result, ensure};
use chrono::NaiveDate;
use serde_json::{Value, json};

impl Lookup {
    pub(super) async fn contact_calendar(&self, args: &Value) -> Result<Value> {
        let day = arg(args, "day")?;
        ensure!(
            day.is_empty() || args.get("year").is_none(),
            "year 和 day 请二选一。"
        );
        let (limit, offset) = paging(args, if day.is_empty() { 366 } else { 25 }, 366)?;
        let mut result = if day.is_empty() {
            let year = optional_integer(args, "year", 2000, 2200)?;
            let data = self
                .get("/api/v1/insights/calendar", &[("year", &year)])
                .await?;
            json!({"rows":data["days"],"year":data["year"],"years":data["years"],"max":data["max"],
                "message_snapshot_at":data["message_snapshot_at"]})
        } else {
            let date: NaiveDate = day.parse().context("day 必须为 YYYY-MM-DD。")?;
            ensure!(date.to_string() == day, "day 必须为 YYYY-MM-DD。");
            let data = self
                .get("/api/v1/insights/calendar/day", &[("day", day)])
                .await?;
            let people: Vec<_> = array(&data["people"]).iter().map(|person| {
                json!({"profile_id":person["profile_id"],"name":person["name"],"messages":person["messages"],
                    "calls":person["calls"],"link":format!("/profiles/{}",s(&person["profile_id"]))})
            }).collect();
            json!({"rows":people,"date":data["date"],"message_snapshot_at":data["message_snapshot_at"]})
        };
        let rows = array(&result["rows"]);
        let total = rows.len();
        result["rows"] = json!(rows.iter().skip(offset).take(limit).collect::<Vec<_>>());
        result["next_offset"] = json!(next(offset, limit, total));
        result["total"] = json!(total);
        result["timezone"] = json!("America/Chicago");
        result["message_basis"] = json!("weekly_snapshot");
        result["link"] = json!("/insights");
        Ok(result)
    }

    pub(super) async fn contact_activity(&self, args: &Value) -> Result<Value> {
        let profile = id(args, "profile_id")?;
        let (from, to) = month_range(args)?;
        let data = self
            .get(&format!("/api/v1/profiles/{profile}/ribbon"), &[])
            .await?;
        Ok(
            json!({"profile_id":profile,"timezone":data["timezone"],"months":months_in(&data["months"],from,to),
            "link":format!("/profiles/{profile}")}),
        )
    }

    pub(super) async fn contact_events_summary(&self, args: &Value) -> Result<Value> {
        let profile = id(args, "profile_id")?;
        let data = self
            .get(
                &format!("/api/v1/profiles/{profile}/contact-events/summary"),
                &[],
            )
            .await?;
        let profile = s(&data["profile_id"]);
        Ok(
            json!({"profile_id":profile,"calls":data["calls"],"money":data["money"],
            "money_amount_unit":"minor_currency_unit",
            "link":format!("/profiles/{profile}?tab=contact-events")}),
        )
    }

    pub(super) async fn source_coverage(&self, args: &Value) -> Result<Value> {
        let profile = id(args, "profile_id")?;
        let provider = arg(args, "provider")?;
        let data = self
            .get(&format!("/api/v1/profiles/{profile}/source-coverage"), &[])
            .await?;
        let sources: Vec<_> = array(&data["sources"]).iter()
            .filter(|row| provider.is_empty() || row["provider"] == provider)
            .map(|row| json!({"connection_id":row["connection_id"],"provider":row["provider"],"name":row["display_name"],
                "state":row["state"],"enabled":row["enabled"],"last_success_at":row["last_success_at"],"records":row["records"],
                "earliest_at":row["earliest_at"],"latest_at":row["latest_at"]}))
            .collect();
        let profile = s(&data["profile_id"]);
        Ok(
            json!({"profile_id":profile,"sources":sources,"coverage":data["coverage"],"link":format!("/profiles/{profile}")}),
        )
    }

    pub(super) async fn import_daily(&self, args: &Value) -> Result<Value> {
        let (limit, offset) = paging(args, 50, 100)?;
        let days = integer(args, "days", 7, 1, 366)?;
        let connection = optional_id(args, "connection_id")?
            .map(|id| id.to_string())
            .unwrap_or_default();
        let data = self
            .get(
                "/api/v1/imports/daily",
                &[
                    ("provider", arg(args, "provider")?),
                    ("connection_id", &connection),
                    ("days", &days.to_string()),
                    ("limit", &(limit + 1).to_string()),
                    ("offset", &offset.to_string()),
                    ("timezone", self.zone.name()),
                ],
            )
            .await?;
        let rows = array(&data["records"]);
        Ok(
            json!({"records":rows.iter().take(limit).collect::<Vec<_>>(),
            "next_offset":(rows.len()>limit).then(|| offset.saturating_add(limit)),
            "timezone":data["timezone"],"missing_days":data["missing_days"],"link":"/imports"}),
        )
    }

    pub(super) async fn group_members(&self, args: &Value) -> Result<Value> {
        let group = id(args, "group_id")?;
        let (limit, offset) = paging(args, 30, 99)?;
        let data = self
            .get(
                &format!("/api/v1/groups/{group}/members"),
                &[
                    ("limit", &(limit + 1).to_string()),
                    ("offset", &offset.to_string()),
                ],
            )
            .await?;
        let rows = array(&data);
        let members: Vec<_> = rows.iter().take(limit).map(|row| json!({
            "profile_id":row["person_profile_id"],"name":row["display_name"],"role":row["role"],
            "joined_at":row["joined_at"],"link":format!("/profiles/{}",s(&row["person_profile_id"]))
        })).collect();
        Ok(
            json!({"group_id":group,"members":members,"next_offset":(rows.len()>limit).then(|| offset.saturating_add(limit)),
            "link":format!("/profiles/{group}?tab=relationships")}),
        )
    }

    pub(super) async fn social_statistics(&self, args: &Value) -> Result<Value> {
        let profile = optional_id(args, "profile_id")?;
        let id = profile.map(|id| id.to_string()).unwrap_or_default();
        let provider = arg(args, "provider")?;
        ensure!(
            matches!(provider, "" | "wechat" | "instagram" | "linkedin"),
            "provider 可用 wechat、instagram 或 linkedin。"
        );
        let (from, to) = month_range(args)?;
        let data = self
            .get(
                "/api/v1/metrics/social",
                &[
                    ("profile_id", &id),
                    ("provider", provider),
                    ("posts_only", "true"),
                ],
            )
            .await?;
        let stats: Vec<_> = data["providers"].as_object().into_iter().flatten().map(|(provider,row)| json!({
            "provider":provider,"totals":{"posts":row["posts"],"likes":row["likes"],"comments":row["comments"],"authors":row["authors"]},
            "monthly":months_in(&row["monthly"],from,to),"first_at":row["first"],"latest_at":row["latest"]
        })).collect();
        let link = data["profile_id"].as_str().map_or_else(
            || "/insights".into(),
            |profile| format!("/profiles/{profile}"),
        );
        Ok(
            json!({"profile_id":data["profile_id"],"providers":stats,"as_of":data["as_of"],"timezone":"UTC","link":link}),
        )
    }

    pub(super) async fn profile_history(&self, args: &Value, structured: bool) -> Result<Value> {
        let profile = id(args, "profile_id")?;
        let limit = integer(args, "limit", 12, 1, 30)?;
        let take = (limit + 1).to_string();
        let before = arg(args, "before")?;
        let rows = if structured {
            let before_id = optional_id(args, "before_id")?
                .map(|id| id.to_string())
                .unwrap_or_default();
            ensure!(
                before.is_empty() == before_id.is_empty(),
                "请同时提供 next.before 和 next.before_id。"
            );
            self.get(
                &format!("/api/v1/profiles/{profile}/structured-history"),
                &[
                    ("fact_type", arg(args, "fact_type")?),
                    ("before", before),
                    ("before_id", &before_id),
                    ("limit", &take),
                    ("compact", "true"),
                ],
            )
            .await?
        } else {
            self.get(
                &format!("/api/v1/profiles/{profile}/history"),
                &[
                    ("field_key", arg(args, "field_key")?),
                    ("before", before),
                    ("limit", &take),
                    ("compact", "true"),
                ],
            )
            .await?
        };
        let rows = array(&rows);
        let cursor = rows.get(limit).map(|_| &rows[limit - 1]).map(|row| {
            if structured {
                json!({"before":row["observed_at"],"before_id":row["id"]})
            } else {
                row["cursor"].clone()
            }
        });
        let link = format!("/profiles/{profile}?tab=history");
        let revisions: Vec<_> = rows.iter().take(limit).map(|row| {
            let mut revision = json!({"id":row["id"],"value":row["value"],"value_truncated":row["value_truncated"]==true,
                "source_type":row["source_type"],"source_account_id":row["source_account_id"],
                "source_record_id":row["source_record_id"],"observed_at":row["observed_at"],"is_conflict":row["is_conflict"],"link":link});
            if structured {
                revision["fact_type"] = row["fact_type"].clone();
                revision["fact_key"] = row["fact_key"].clone();
                revision["organization"] = row["entity_name"].clone();
            } else {
                revision["field_key"] = row["field_key"].clone();
                revision["operation"] = row["operation"].clone();
            }
            revision
        }).collect();
        Ok(json!({"profile_id":profile,"revisions":revisions,"next":cursor,"link":link}))
    }
}

fn paging(args: &Value, default: usize, max: usize) -> Result<(usize, usize)> {
    Ok((
        integer(args, "limit", default, 1, max)?,
        integer(args, "offset", 0, 0, i64::MAX as usize)?,
    ))
}

fn optional_integer(args: &Value, key: &str, min: usize, max: usize) -> Result<String> {
    args.get(key)
        .map(|_| integer(args, key, min, min, max).map(|n| n.to_string()))
        .transpose()
        .map(Option::unwrap_or_default)
}

fn month_range(args: &Value) -> Result<(&str, &str)> {
    let from = month(args, "from_month")?;
    let to = month(args, "to_month")?;
    ensure!(
        from.is_empty() || to.is_empty() || from <= to,
        "from_month 不能晚于 to_month。"
    );
    Ok((from, to))
}

fn months_in<'a>(value: &'a Value, from: &str, to: &str) -> Vec<&'a Value> {
    array(value)
        .iter()
        .filter(|row| {
            let month = s(&row["month"]);
            (from.is_empty() || month >= from) && (to.is_empty() || month <= to)
        })
        .collect()
}
