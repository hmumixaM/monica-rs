//! Source-backed annotations; no guessed identity, location or effect names.
use serde_json::{Value, json};

fn text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        _ => v.to_string(),
    }
}
fn valid_user(v: &Value) -> Option<&str> {
    v.as_str().filter(|s| {
        !s.is_empty()
            && s.len() <= 30
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.".contains(&c))
    })
}
fn number(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str()?.parse().ok())
}
fn unique(items: &mut Vec<Value>, value: Value) {
    if !items.contains(&value) {
        items.push(value);
    }
}
fn extra(items: &mut Vec<Value>, kind: &str, label: &str, value: &str, url: Option<String>) {
    if value.trim().is_empty() {
        return;
    }
    let mut item = json!({"kind":kind,"label":label,"text":value.trim()});
    if let Some(url) = url {
        item["url"] = json!(url);
    }
    unique(items, item);
}
pub fn normalize(
    provider: &str,
    kind: &str,
    external: &Value,
    occurred: &Value,
    payload: &Value,
) -> Value {
    let mut locations = vec![];
    let places = payload["locations"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| vec![payload["location"].clone()]);
    for place in places
        .iter()
        .filter(|v| v.as_object().is_some_and(|o| !o.is_empty()))
    {
        let mut point = number(place.get("latitude").unwrap_or(&place["lat"])).zip(number(
            place
                .get("longitude")
                .or(place.get("lon"))
                .unwrap_or(&place["lng"]),
        ));
        if payload["raw_xml_sha256"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
        {
            point = point.map(|(a, b)| (b, a));
        }
        point = point.filter(|(a, b)| {
            (-90.0..=90.0).contains(a) && (-180.0..=180.0).contains(b) && (*a != 0.0 || *b != 0.0)
        });
        let label = [
            "name", "poiName", "poi_name", "poi", "address", "city", "province", "country",
        ]
        .iter()
        .map(|k| text(&place[k]))
        .find(|s| !s.is_empty() && s != "0")
        .unwrap_or_else(|| {
            let a = place.get("latitude").unwrap_or(&place["lat"]);
            let b = place
                .get("longitude")
                .or(place.get("lon"))
                .unwrap_or(&place["lng"]);
            if !a.is_null() && !b.is_null() && text(a) != "0" && text(b) != "0" {
                format!("{}, {}", text(a), text(b))
            } else {
                "未命名地点".into()
            }
        });
        if point.is_none() && label == "未命名地点" {
            continue;
        }
        let id = text(place.get("id").unwrap_or(&place["pk"]));
        let mut entry = json!({"label":label,"location_id":if id.is_empty(){Value::Null}else{json!(id)},"address":place["address"],"coordinate_source":if kind=="story"{"地点贴纸"}else{"动态地点"}});
        if let Some((lat, lon)) = point {
            // Python float formatting retains .0 for integer coordinates.
            let a = format!("{lat:?}");
            let b = format!("{lon:?}");
            entry["latitude"] = json!(lat);
            entry["longitude"] = json!(lon);
            entry["map_url"] = json!(format!(
                "https://www.openstreetmap.org/?mlat={a}&mlon={b}#map=16/{a}/{b}"
            ));
        }
        unique(&mut locations, entry);
    }
    let raw = &payload["archive_manifest"]["raw_metadata"];
    let phone = &raw["iphone_struct"];
    let effects = &payload["archive_manifest"]["effect_metadata"];
    let stickers = |key: &str| {
        phone
            .get(key)
            .or_else(|| effects.get(format!("iphone_struct.{key}")))
            .unwrap_or(&Value::Null)
    };
    let mut mentions = vec![];
    let reels = phone
        .get("reel_mentions")
        .or(raw.get("reel_mentions"))
        .unwrap_or(&payload["reel_mentions"]);
    for item in reels.as_array().into_iter().flatten() {
        let user = &item["user"];
        if let Some(name) = valid_user(&user["username"]) {
            unique(
                &mut mentions,
                json!({"label":format!("@{name}"),"external_id":text(user.get("pk").unwrap_or(&user["id"])),"url":format!("https://www.instagram.com/{name}/")}),
            );
        }
    }
    for item in payload["interactions"].as_array().into_iter().flatten() {
        if item["interaction_type"] == "mention"
            && item["is_deleted"] != true
            && !item["target_external_id"].is_null()
        {
            unique(
                &mut mentions,
                json!({"label":text(item.get("target_name").unwrap_or(&item["target_external_id"])),"external_id":text(&item["target_external_id"])}),
            );
        }
    }
    let mut post = Value::Null;
    if provider == "instagram" && kind == "post" {
        let mut caption_mentions = payload["caption_mentions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if caption_mentions.is_empty() {
            static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
            let pattern = PATTERN.get_or_init(|| {
                regex::Regex::new(
                    r"(?:^|[^\w@])@([A-Za-z0-9_](?:[A-Za-z0-9_.]{0,28}[A-Za-z0-9_])?)",
                )
                .unwrap()
            });
            let caption = payload["text"]
                .as_str()
                .or(payload["caption"].as_str())
                .unwrap_or("");
            for capture in pattern.captures_iter(caption) {
                unique(&mut caption_mentions, json!({"username":&capture[1]}));
            }
        }
        for item in &caption_mentions {
            if let Some(name) = valid_user(&item["username"]) {
                mentions.push(json!({"label":format!("@{name}"),"external_id":text(item.get("id").or(item.get("pk")).unwrap_or(&item["userid"])),"url":format!("https://www.instagram.com/{name}/")}));
            }
        }
        let media_type = payload.get("media_type").cloned().unwrap_or_else(|| {
            if payload["media"].as_array().is_some_and(|v| v.len() > 1) {
                json!("carousel")
            } else {
                payload["media"][0]["media_type"].clone()
            }
        });
        post = json!({"post_id":payload.get("post_id").filter(|v|v.as_str()!=Some("")).unwrap_or(external),"shortcode":payload["shortcode"],"permalink":payload["permalink"],"published_at":payload.get("published_at").unwrap_or(occurred),"media_type":media_type,"tagged_users":payload["tagged_users"],"caption_mentions":payload["caption_mentions"]});
    }
    let mut extras = vec![];
    if kind == "story" {
        for item in stickers("story_music_stickers")
            .as_array()
            .into_iter()
            .flatten()
        {
            let asset = &item["music_asset_info"];
            let parts = [asset["title"].as_str(), asset["display_artist"].as_str()]
                .into_iter()
                .flatten()
                .filter(|s| !s.trim().is_empty())
                .collect::<Vec<_>>();
            extra(&mut extras, "music", "背景音乐", &parts.join(" · "), None);
        }
        for item in stickers("story_feed_media")
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| v.is_object())
        {
            let code = item["media_code"].as_str().filter(|s| {
                !s.is_empty()
                    && s.bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
            });
            extra(
                &mut extras,
                "reshare",
                "转发帖子",
                if code.is_some() {
                    "查看原帖"
                } else {
                    "已收录转发贴纸"
                },
                code.map(|s| format!("https://www.instagram.com/p/{s}/")),
            );
        }
        let creative = stickers("creative_config");
        if let Some(label) = match creative["capture_type"].as_str() {
            Some("boomerang") => Some("Boomerang 循环"),
            Some("superzoom") => Some("超级变焦"),
            Some("layout") => Some("拼图"),
            _ => None,
        } {
            extra(&mut extras, "effect", "拍摄效果", label, None);
        }
        if let Some(name) = valid_user(&creative["attribution_user"]["username"]) {
            extra(&mut extras, "effect", "创作来源", &format!("@{name}"), None);
        }
        let mode = stickers("create_mode_attribution");
        if let Some(name) = mode["name"].as_str().filter(|s| !s.is_empty()) {
            extra(
                &mut extras,
                "effect",
                "创作模式",
                if mode["type"] == "TYPE" {
                    "文字"
                } else {
                    name
                },
                None,
            );
        }
    }
    json!({"event_type":kind,"locations":locations,"mentions":mentions,"post":post,"extras":extras})
}
