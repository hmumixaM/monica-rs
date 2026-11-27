use super::{Lookup, arg, array, cut, id, integer, next, optional_id, s};
use anyhow::{Context, Result, bail, ensure};
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::{Value, json};
use std::cmp::Reverse;

#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum PlaceNear {
    City {
        city: String,
        radius_km: f64,
    },
    Coordinates {
        latitude: f64,
        longitude: f64,
        radius_km: f64,
    },
}

impl PlaceNear {
    fn query(self) -> Result<(String, String, String, Option<String>)> {
        let (latitude, longitude, radius_km, city) = match self {
            Self::City { city, radius_km } => {
                let center = crate::pages::resolve_city(&city, "", "")
                    .context("无法唯一识别中心城市。请补充州或国家，或提供中心坐标。")?;
                (
                    center.latitude,
                    center.longitude,
                    radius_km,
                    Some(center.label),
                )
            }
            Self::Coordinates {
                latitude,
                longitude,
                radius_km,
            } => (latitude, longitude, radius_km, None),
        };
        ensure!(
            (-90.0..=90.0).contains(&latitude) && (-180.0..=180.0).contains(&longitude),
            "near 的纬度必须在 -90 到 90 之间，经度必须在 -180 到 180 之间。"
        );
        ensure!(
            radius_km.is_finite() && radius_km > 0.0,
            "near.radius_km 必须为正数。"
        );
        Ok((
            latitude.to_string(),
            longitude.to_string(),
            radius_km.to_string(),
            city,
        ))
    }
}

impl Lookup {
    pub(super) async fn image_notes(&self, args: &Value) -> Result<Value> {
        let note = |item: &Value, id: &Value| {
            json!({
                "image_id":id,"summary":cut(&item["summary"],500),"description":cut(&item["description"],600),
                "text":cut(&item["text"],800),"link":id.as_str().map(|id|format!("/api/v1/media/{id}")),
            })
        };
        match (
            optional_id(args, "post_id")?,
            optional_id(args, "image_id")?,
        ) {
            (Some(id), None) => {
                let row = self.get(&format!("/api/v1/timeline/{id}"), &[]).await?;
                let reading = &row["media_reading"];
                let notes: Vec<_> = array(&reading["items"])
                    .iter()
                    .filter(|item| {
                        ["summary", "description", "text"]
                            .iter()
                            .any(|key| !s(&item[*key]).is_empty())
                    })
                    .take(10)
                    .map(|item| note(item, &item["media_id"]))
                    .collect();
                Ok(
                    json!({"notes":notes,"summary":cut(&reading["summary"],600),"link":format!("/timeline?event={id}")}),
                )
            }
            (None, Some(id)) => {
                let row = self.get(&format!("/api/v1/media/{id}/ocr"), &[]).await?;
                let notes = if ["summary", "description", "text"]
                    .iter()
                    .any(|key| !s(&row[*key]).is_empty())
                {
                    vec![note(&row, &json!(id))]
                } else {
                    Vec::new()
                };
                Ok(json!({"notes":notes,"link":format!("/api/v1/media/{id}")}))
            }
            _ => bail!("请提供 post_id 或 image_id，二选一。"),
        }
    }

    pub(super) async fn birthdays(&self, args: &Value) -> Result<Value> {
        let month = args
            .get("month")
            .map(|_| integer(args, "month", 0, 1, 12))
            .transpose()?
            .map(|month| month.to_string())
            .unwrap_or_default();
        let limit = integer(args, "limit", 50, 1, 100)?.to_string();
        let offset = integer(args, "offset", 0, 0, i64::MAX as usize)?.to_string();
        self.get(
            "/api/v1/dates/birthdays",
            &[("month", &month), ("limit", &limit), ("offset", &offset)],
        )
        .await
    }

    pub(super) async fn profile_suggestions(&self, args: &Value) -> Result<Value> {
        let profile = optional_id(args, "profile_id")?
            .map(|id| id.to_string())
            .unwrap_or_default();
        let kind = arg(args, "kind")?;
        let limit = integer(args, "limit", 10, 1, 25)?;
        let offset = integer(args, "offset", 0, 0, usize::MAX)?;
        let mut row = self
            .get(
                "/api/v1/ai/suggestions",
                &[
                    ("profile_id", &profile),
                    ("kind", kind),
                    ("limit", &limit.to_string()),
                    ("offset", &offset.to_string()),
                ],
            )
            .await?;
        if let Some(people) = row["people"].as_array_mut() {
            for person in people {
                person["link"] = json!(profile_link(&person["profile_id"]));
                if let Some(items) = person["items"].as_array_mut() {
                    for item in items {
                        item["quote"] = json!(cut(&item["quote"], 400));
                        if let Some(evidence) = item["evidence"].as_array_mut() {
                            evidence.retain(Value::is_object);
                            evidence.truncate(4);
                            for source in evidence {
                                source["text"] = json!(cut(&source["text"], 400));
                            }
                        }
                    }
                }
            }
        }
        row["next_offset"] = json!(next(
            offset,
            limit,
            row["total"].as_u64().unwrap_or_default() as usize
        ));
        row["link"] = json!("/suggestions");
        Ok(row)
    }

    pub(super) async fn profile_summary(&self, args: &Value) -> Result<Value> {
        let id = id(args, "profile_id")?;
        let week = week(args)?;
        let row = self
            .get(
                &format!("/api/v1/ai/profiles/{id}/summary"),
                &[("week", week)],
            )
            .await?;
        let profile = &row["profile_id"];
        let weeks: Vec<_> = array(&row["weekly_updates"])
            .iter()
            .map(|update| self.person_week(update, profile))
            .collect();
        Ok(
            json!({"profile_id":profile,"enabled":row["enabled"],"memory":row["memory"],
            "recent_weeks":weeks,"link":profile_link(profile)}),
        )
    }

    pub(super) async fn weekly_people(&self, args: &Value) -> Result<Value> {
        let week = week(args)?;
        let after = arg(args, "after")?;
        ensure!(
            after.is_empty() || !week.is_empty(),
            "继续分页时请提供 week。"
        );
        let limit = integer(args, "limit", 12, 1, 25)?.to_string();
        let row = self
            .get(
                "/api/v1/ai/weekly-people",
                &[("week", week), ("after", after), ("limit", &limit)],
            )
            .await?;
        let people: Vec<_> = array(&row["people"])
            .iter()
            .map(|update| {
                let mut person = self.person_week(update, &update["profile_id"]);
                person["name"] = update["name"].clone();
                person
            })
            .collect();
        let week = s(&row["week_start"]);
        let link = if week.is_empty() {
            "/digest?view=people".into()
        } else {
            format!("/digest?view=people&week={week}")
        };
        Ok(
            json!({"enabled":row["enabled"],"week":row["week_start"],"weeks":row["weeks"],
            "people":people,"total":row["total"],"next":row["next_cursor"],"link":link}),
        )
    }

    fn person_week(&self, update: &Value, profile: &Value) -> Value {
        let sources: Vec<_> = array(&update["sources"]).iter().filter(|source| source.is_object())
            .take(8).map(|source| {
                let link = source["citation"].as_str().filter(|link|
                    ["/timeline?", "/profiles/", "/messages?", "/evidence/"].iter().any(|prefix| link.starts_with(prefix))
                ).map(str::to_owned).or_else(|| match s(&source["kind"]) {
                    "post" | "story" => source["id"].as_str().map(|id| format!("/timeline?event={id}")),
                    "profile" | "note" => Some(profile_link(profile)),
                    _ => source["conversation_id"].as_str().zip(source["id"].as_str())
                        .map(|(conversation, id)| format!("/messages?conversation={conversation}&message={id}")),
                });
                json!({"kind":source["kind"],"at":self.date(&source["at"],true),"discovered":source["discovered"],
                    "quote":cut(&source["quote"],200),"link":link})
            }).collect();
        json!({"profile_id":profile,"week":update["week_start"],"summary":excerpt(&update["summary"],1000),
            "generated_at":update["generated_at"],"sources":sources,"link":profile_link(profile)})
    }

    pub(super) async fn weekly_review(&self, args: &Value) -> Result<Value> {
        let week = week(args)?;
        if week.is_empty() {
            let offset = integer(args, "offset", 0, 0, usize::MAX)?;
            let limit = integer(args, "limit", 12, 1, 30)?;
            let row = self.get("/api/v1/digests", &[]).await?;
            let digests = array(&row["digests"]);
            let page: Vec<_> = digests
                .iter()
                .skip(offset)
                .take(limit)
                .map(|digest| {
                    json!({"week":digest["week_start"],"generated_at":digest["generated_at"],
                    "summary":excerpt(&digest["summary"],500),"life_events":digest["life_events"],
                    "link":format!("/digest?week={}",s(&digest["week_start"]))})
                })
                .collect();
            return Ok(
                json!({"enabled":row["enabled"],"digests":page,"total":digests.len(),
                "next_offset":next(offset,limit,digests.len()),"link":"/digest"}),
            );
        }
        let row = self.get(&format!("/api/v1/digests/{week}"), &[]).await?;
        let mut narrative = row["narrative"].clone();
        if narrative.is_object() {
            for key in ["life_events", "suggestions"] {
                if let Some(items) = narrative.get_mut(key).and_then(Value::as_array_mut) {
                    for item in items {
                        let profile = s(&item["profile_id"]);
                        let link = item["post_id"].as_str().map_or_else(
                            || profile_link(&item["profile_id"]),
                            |post| format!("/timeline?event={post}"),
                        );
                        let name = row["people"][profile]["name"].clone();
                        item["name"] = name;
                        item["link"] = json!(link);
                    }
                }
            }
        }
        let mut facts = row["facts"].clone();
        for key in [
            "top_contacts",
            "new_people",
            "new_links",
            "upcoming_dates",
            "newly_dormant",
        ] {
            if let Some(items) = facts.get_mut(key).and_then(Value::as_array_mut) {
                for item in items {
                    item["link"] = json!(profile_link(&item["profile_id"]));
                }
            }
        }
        Ok(
            json!({"enabled":true,"week":row["week_start"],"generated_at":row["generated_at"],
            "facts":facts,"narrative":narrative,"link":format!("/digest?week={week}")}),
        )
    }

    pub(super) async fn places(&self, args: &Value) -> Result<Value> {
        let profile = optional_id(args, "profile_id")?;
        let query = arg(args, "query")?;
        let provider = arg(args, "provider")?;
        let kind = arg(args, "kind")?;
        ensure!(
            profile.is_none()
                || (provider.is_empty() && kind.is_empty() && args.get("year").is_none()),
            "人物地图支持 query 和 near。provider、year 和 kind 用于全局地图。"
        );
        let (latitude, longitude, radius_km, city) = match args.get("near") {
            Some(near) => PlaceNear::deserialize(near)
                .context("near 需要 city 或 latitude 和 longitude，以及数字 radius_km。")?
                .query()?,
            None => Default::default(),
        };
        let year = args
            .get("year")
            .map(|_| integer(args, "year", 0, 1900, 2100).map(|year| year.to_string()))
            .transpose()?
            .unwrap_or_default();
        let limit = integer(args, "limit", 20, 1, 50)?.to_string();
        let offset = integer(args, "offset", 0, 0, usize::MAX)?.to_string();
        let path = profile.map_or_else(
            || "/api/v1/metrics/locations".into(),
            |id| format!("/api/v1/profiles/{id}/locations"),
        );
        let mut row = self
            .get(
                &path,
                &[
                    ("query", query),
                    ("provider", provider),
                    ("kind", kind),
                    ("year", &year),
                    ("limit", &limit),
                    ("offset", &offset),
                    ("center_latitude", &latitude),
                    ("center_longitude", &longitude),
                    ("radius_km", &radius_km),
                ],
            )
            .await?;
        if let Some(city) = city {
            row["near"]["city"] = json!(city);
        }
        row["link"] = json!(if profile.is_some() {
            profile_link(&row["profile_id"])
        } else {
            "/relationships?view=map".into()
        });
        Ok(row)
    }

    pub(super) async fn relationship_network(&self, args: &Value) -> Result<Value> {
        let focus = optional_id(args, "profile_id")?
            .map(|id| id.to_string())
            .unwrap_or_default();
        let limit = integer(args, "limit", 20, 1, 50)?;
        let graph = self
            .get(
                "/api/v1/relationships/graph",
                &[("format", "monica"), ("limit", "100"), ("focus", &focus)],
            )
            .await?;
        let nodes = array(&graph["nodes"]);
        let endpoint = |edge: &Value, key: &str| -> Option<Value> {
            let node = nodes.get(usize::try_from(edge[key].as_u64()?).ok()?)?;
            Some(
                json!({"profile_id":node["profile_id"],"name":node["name"],"link":profile_link(&node["profile_id"])}),
            )
        };
        let mut ranked: Vec<_> = array(&graph["edges"]).iter().collect();
        ranked.sort_by_key(|edge| {
            (
                Reverse(edge["combined"].as_i64()),
                Reverse(edge["direct"].as_i64()),
            )
        });
        let edges: Vec<_> = ranked.into_iter().take(limit).filter_map(|edge| {
            Some(json!({"from":endpoint(edge,"a")?,"to":endpoint(edge,"b")?,
                "interactions_a_to_b":interactions(&edge["ab"]),"interactions_b_to_a":interactions(&edge["ba"]),
                "shared_groups":edge["group"],"shared_audience":edge["co"],
                "relationship_types":edge.get("relationship_types").cloned().unwrap_or(json!([])),"last_at":edge["last"]}))
        }).collect();
        let link = if focus.is_empty() {
            "/relationships?view=graph".into()
        } else {
            format!("/relationships?view=graph&focus={focus}")
        };
        Ok(
            json!({"edges":edges,"projection":graph["stats"],"scope":graph["scope"],
            "support":"social_interactions_confirmed_relationships_and_small_groups","link":link}),
        )
    }

    pub(super) async fn chat_statistics(&self, args: &Value) -> Result<Value> {
        let profile = id(args, "profile_id")?;
        let from = month(args, "from_month")?;
        let to = month(args, "to_month")?;
        ensure!(
            from.is_empty() || to.is_empty() || from <= to,
            "from_month 不能晚于 to_month。"
        );
        let row = self
            .get(
                &format!("/api/v1/profiles/{profile}/chat-months"),
                &[("timezone", self.zone.name())],
            )
            .await?;
        let months: Vec<_> = array(&row["months"])
            .iter()
            .filter(|row| {
                let month = s(&row["month"]);
                (from.is_empty() || month >= from) && (to.is_empty() || month <= to)
            })
            .collect();
        let sum = |key: &str| {
            months
                .iter()
                .filter_map(|month| month[key].as_i64())
                .sum::<i64>()
        };
        Ok(
            json!({"profile_id":profile,"chat":"direct","months":months,"total":sum("total"),"sent":sum("sent"),
            "received":sum("received"),"system":sum("system"),"unknown":sum("unknown"),"records":sum("records"),
            "active_months":months.iter().filter(|month| month["total"].as_i64().unwrap_or(0)>0).count(),
            "from_month":args.get("from_month"),"to_month":args.get("to_month"),
            "first_all_time":self.date(&row["first"],true),"last_all_time":self.date(&row["last"],true),
            "count_basis":row["count_basis"],"timezone":row["timezone"],"link":format!("/profiles/{profile}?tab=messages")}),
        )
    }
}

fn week(args: &Value) -> Result<&str> {
    let value = arg(args, "week")?;
    if !value.is_empty() {
        let date =
            NaiveDate::parse_from_str(value, "%Y-%m-%d").context("week 格式必须为 YYYY-MM-DD。")?;
        ensure!(
            date.format("%Y-%m-%d").to_string() == value,
            "week 格式必须为 YYYY-MM-DD。"
        );
    }
    Ok(value)
}

fn month<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    let value = arg(args, key)?;
    if !value.is_empty() {
        let date = NaiveDate::parse_from_str(&format!("{value}-01"), "%Y-%m-%d")
            .with_context(|| format!("{key} 格式必须为 YYYY-MM。"))?;
        ensure!(
            value.len() == 7 && date.format("%Y-%m").to_string() == value,
            "{key} 格式必须为 YYYY-MM。"
        );
    }
    Ok(value)
}

fn profile_link(profile: &Value) -> String {
    format!("/profiles/{}", s(profile))
}

fn excerpt(text: &Value, limit: usize) -> Value {
    text.as_str()
        .map(|_| json!(cut(text, limit)))
        .unwrap_or(Value::Null)
}

fn interactions(counts: &Value) -> Value {
    json!({"likes":counts[0],"comments":counts[1],"replies":counts[2],"mentions":counts[3]})
}
