//! Monica Next's post/Story vision path: numbered originals, posters or frames.
use anyhow::{Context, Result, ensure};
use base64::Engine;
use image::ImageDecoder;
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use std::{collections::BTreeMap, path::Path};
use uuid::Uuid;

pub const MAX_IMAGES: usize = 10;

fn slot_number(role: &str) -> Option<(usize, bool)> {
    if let Some(value) = role.strip_prefix("media-") {
        return value.strip_suffix("-poster").map_or_else(
            || value.parse().ok().map(|n| (n, false)),
            |number| number.parse().ok().map(|n| (n, true)),
        );
    }
    role.strip_prefix("thumbnail-")
        .and_then(|s| s.strip_prefix("media-").unwrap_or(s).parse().ok())
        .map(|n| (n, true))
}

pub fn slots(rows: &[Value]) -> Vec<Value> {
    let mut originals = BTreeMap::new();
    let mut posters = BTreeMap::new();
    for row in rows {
        let Some((index, poster)) = row["role"].as_str().and_then(slot_number) else {
            continue;
        };
        if poster {
            if row["media_type"]
                .as_str()
                .is_some_and(|s| s.starts_with("image/"))
            {
                posters.insert(index, row);
            }
        } else {
            originals.insert(index, row);
        }
    }
    let numbers: std::collections::BTreeSet<_> =
        originals.keys().chain(posters.keys()).copied().collect();
    numbers.into_iter().take(MAX_IMAGES).enumerate().map(|(position, number)| {
        let primary = originals.get(&number).or_else(|| posters.get(&number)).unwrap();
        let video = !originals.contains_key(&number) || primary["media_type"].as_str().is_some_and(|s| s.starts_with("video/"));
        let source = if video { posters.get(&number).unwrap_or(primary) } else { primary };
        json!({"index":position+1,"slot":number,"media_id":primary["id"],"kind":if video{"video"}else{"photo"},
               "read_from":source["id"],"sha256":source["sha256"],"media_type":source["media_type"],
               "duration_ms":source["duration_ms"],"frame":video && !posters.contains_key(&number)})
    }).collect()
}

pub async fn snapshot(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<(Value, Vec<Value>)> {
    let post: Value = sqlx::query_scalar("SELECT jsonb_build_object('id',id,'kind',event_type,'text',left(coalesce(content->>'text',content->>'caption',''),400),'version',version,'occurred_at',occurred_at) FROM crm.timeline_events WHERE id=$1 AND status<>'deleted'")
        .bind(id).fetch_optional(&mut **tx).await?.context("unknown post")?;
    let rows: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('role',l.role,'id',a.id,'sha256',a.sha256,'media_type',a.media_type,'duration_ms',a.duration_ms) FROM crm.media_links l JOIN crm.media_assets a ON a.id=l.media_id WHERE l.entity_id=$1 AND l.entity_type=$2 AND (a.media_type LIKE 'image/%' OR a.media_type LIKE 'video/%') ORDER BY l.role,a.created_at,a.id")
        .bind(id).bind(if post["kind"]=="story"{"social_story"}else{"social_post"}).fetch_all(&mut **tx).await?;
    let images = slots(&rows);
    Ok((post, images))
}

/// Next queues a Story read in the same transaction that attaches its media.
pub async fn enqueue_import(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    role: &str,
) -> Result<Option<Uuid>> {
    if slot_number(role).is_none() {
        return Ok(None);
    }
    let eligible:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.ai_settings s JOIN crm.timeline_events e ON e.id=$1 WHERE s.id AND s.enabled AND e.status<>'deleted')")
        .bind(id).fetch_one(&mut **tx).await?;
    if !eligible {
        return Ok(None);
    }
    let (_, images) = snapshot(tx, id).await?;
    if images.is_empty() {
        return Ok(None);
    }
    let cached:Option<Value>=sqlx::query_scalar("SELECT items FROM crm.media_readings WHERE entity_id=$1 AND entity_type=(SELECT CASE WHEN event_type='story' THEN 'social_story' ELSE 'social_post'END FROM crm.timeline_events WHERE id=$1)")
        .bind(id).fetch_optional(&mut **tx).await?;
    let sources: Vec<Uuid> = images
        .iter()
        .filter_map(|i| i["read_from"].as_str().and_then(|s| s.parse().ok()))
        .collect();
    let completed:Vec<Uuid>=sqlx::query_scalar("SELECT media_id FROM crm.image_ocr_readings WHERE media_id=ANY($1) AND(status='complete'OR generated_at IS NOT NULL)AND(coalesce(btrim(text),'')<>''OR coalesce(btrim(description),'')<>'')")
        .bind(sources).fetch_all(&mut **tx).await?;
    let read = images.iter().all(|image| {
        let single = image["read_from"]
            .as_str()
            .and_then(|s| s.parse::<Uuid>().ok())
            .is_some_and(|id| completed.contains(&id));
        single
            || cached
                .as_ref()
                .and_then(Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        (item["media_id"] == image["media_id"] || item["id"] == image["media_id"])
                            && ["text", "description"]
                                .into_iter()
                                .any(|key| item[key].as_str().is_some_and(|s| !s.trim().is_empty()))
                    })
                })
    });
    if read {
        return Ok(None);
    }
    let job = super::enqueue_in_tx(tx, "ai.post_media", &id.to_string()).await?;
    sqlx::query("UPDATE crm.ai_jobs SET input=input||'{\"ingested\":true}'::jsonb WHERE id=$1 AND status='queued'")
        .bind(job).execute(&mut **tx).await?;
    Ok(Some(job))
}

/// Next's unread scan includes archived Stories and uses newest publications first.
pub async fn pending_subjects(pool: &sqlx::PgPool, cursor: Option<&str>) -> Result<Vec<String>> {
    let cursor = cursor.map(str::parse::<Uuid>).transpose()?;
    Ok(sqlx::query_scalar(r#"
SELECT e.id::text FROM crm.timeline_events e
WHERE e.status<>'deleted'
 AND ($1::uuid IS NULL OR (coalesce(e.occurred_at,'-infinity'),e.id)<
     (SELECT coalesce(c.occurred_at,'-infinity'),c.id FROM crm.timeline_events c WHERE c.id=$1))
 AND EXISTS(
   SELECT 1 FROM (
     SELECT l.media_id,l.role,a.media_type,dense_rank()OVER(ORDER BY substring(l.role FROM '[0-9]+')::integer) AS position
     FROM crm.media_links l JOIN crm.media_assets a ON a.id=l.media_id
     WHERE l.entity_id=e.id AND l.entity_type=CASE WHEN e.event_type='story' THEN 'social_story' ELSE 'social_post' END
       AND l.role~'^(media-[0-9]+(-poster)?|thumbnail-(media-)?[0-9]+)$'
       AND (a.media_type LIKE 'image/%' OR a.media_type LIKE 'video/%')
   ) m WHERE m.position<=10
   AND NOT EXISTS(
     SELECT 1 FROM crm.media_readings r CROSS JOIN LATERAL jsonb_array_elements(r.items) item
     WHERE r.entity_id=e.id AND r.entity_type=CASE WHEN e.event_type='story' THEN 'social_story' ELSE 'social_post' END
       AND (coalesce(item->>'media_id',item->>'id')=m.media_id::text OR EXISTS(
         SELECT 1 FROM crm.media_links primary_media WHERE primary_media.entity_id=e.id
           AND primary_media.entity_type=r.entity_type
           AND primary_media.role='media-'||substring(m.role FROM '[0-9]+')
           AND primary_media.media_id::text=coalesce(item->>'media_id',item->>'id')))
       AND (coalesce(btrim(item->>'text'),'')<>'' OR coalesce(btrim(item->>'description'),'')<>'')
   )
   AND NOT EXISTS(
     SELECT 1 FROM crm.image_ocr_readings r WHERE r.media_id=m.media_id AND r.status='complete'
       AND (coalesce(btrim(r.text),'')<>'' OR coalesce(btrim(r.description),'')<>'')
   )
 )
ORDER BY e.occurred_at DESC NULLS LAST,e.id DESC LIMIT 25
"#).bind(cursor).fetch_all(pool).await?)
}

/// The same preparation as Next's data_url: EXIF transpose, RGB, 1280px, JPEG80.
pub fn data_url(path: &Path, frame: bool, duration_ms: Option<u64>) -> Result<String> {
    let mut image = if frame {
        let duration_ms = match duration_ms {
            Some(duration) => Some(duration),
            None => {
                let probe = std::process::Command::new("ffprobe")
                    .args([
                        "-v",
                        "error",
                        "-show_entries",
                        "format=duration",
                        "-of",
                        "default=noprint_wrappers=1:nokey=1",
                    ])
                    .arg(path)
                    .output()?;
                std::str::from_utf8(&probe.stdout)
                    .ok()
                    .and_then(|s| s.trim().parse::<f64>().ok())
                    .map(|seconds| (seconds * 1000.0) as u64)
            }
        };
        let mut command = std::process::Command::new("ffmpeg");
        command
            .args(["-v", "error", "-threads", "1", "-i"])
            .arg(path);
        if duration_ms.is_some_and(|duration| duration > 2000) {
            command.args(["-ss", "1"]);
        }
        let result = command
            .args([
                "-frames:v",
                "1",
                "-vf",
                "scale=w='min(1280,iw)':h='min(1280,ih)':force_original_aspect_ratio=decrease",
                "-f",
                "image2pipe",
                "-vcodec",
                "mjpeg",
                "-",
            ])
            .output()?;
        ensure!(result.status.success(), "video frame extraction failed");
        image::load_from_memory(&result.stdout)?
    } else {
        let mut reader = image::ImageReader::open(path)?.with_guessed_format()?;
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(128 * 1024 * 1024);
        reader.limits(limits);
        let mut decoder = reader.into_decoder()?;
        let orientation = decoder.orientation()?;
        let mut image = image::DynamicImage::from_decoder(decoder)?;
        image.apply_orientation(orientation);
        image
    };
    if image.width() > 1280 || image.height() > 1280 {
        image = image.thumbnail(1280, 1280);
    }
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 80)
        .encode_image(&image.to_rgb8())?;
    Ok(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

pub async fn hydrate(input: &mut Value, root: &Path) -> Result<()> {
    let images = input["images"].as_array().context("missing post images")?;
    let mut prepared = Vec::new();
    for image in images {
        let path = root.join(crate::media::object_path(
            image["sha256"].as_str().context("missing image hash")?,
        )?);
        let frame = image["frame"] == true;
        let duration = image["duration_ms"].as_u64();
        let url = tokio::task::spawn_blocking(move || data_url(&path, frame, duration)).await??;
        prepared.push(json!({"index":image["index"],"kind":image["kind"],"url":url}));
    }
    input["image_data"] = json!(prepared);
    Ok(())
}

fn clip(value: &str, limit: usize) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if value.chars().count() <= limit {
        value
    } else {
        value
            .chars()
            .take(limit - 1)
            .chain(std::iter::once('…'))
            .collect()
    }
}

/// Like Next's tidy: map the model's numbered answer to saved media IDs in code.
pub fn tidy(output: &Value, shown: &[Value]) -> Result<Value> {
    let decoded;
    let items = if let Some(value) = output["items"].as_str() {
        decoded = serde_json::from_str::<Value>(value)?;
        decoded.as_array()
    } else {
        output["items"].as_array()
    }
    .context("missing per-image readings")?;
    let mut result = Vec::new();
    for slot in shown {
        let item = items
            .iter()
            .find(|item| item["index"] == slot["index"])
            .context("missing numbered image reading")?;
        let text = item["text"].as_str().context("missing image text")?;
        let description = item["description"]
            .as_str()
            .context("missing image description")?;
        let category = item["category"]
            .as_str()
            .context("missing image category")?;
        ensure!(
            !text.trim().is_empty() || !description.trim().is_empty(),
            "empty image reading"
        );
        result.push(json!({"index":slot["index"],"media_id":slot["media_id"],"kind":slot["kind"],"status":"complete",
                           "category":category,"text":clip(text,200),"description":clip(description,40)}));
    }
    Ok(
        json!({"summary":clip(output["summary"].as_str().context("missing post summary")?,60),"items":result}),
    )
}

pub fn response_format() -> Value {
    json!({"type":"json_schema","json_schema":{"name":"PostMediaReading","strict":true,"schema":{
        "type":"object","additionalProperties":false,"required":["summary","items"],"properties":{
            "summary":{"type":"string"},"items":{"type":"array","items":{"type":"object","additionalProperties":false,
                "required":["index","category","text","description"],"properties":{
                    "index":{"type":"integer"},"category":{"type":"string","enum":["人像","合照","自拍","食物","风景","城市街景","室内","宠物","截图","海报活动","文档票据","商品","交通出行","运动","其他"]},
                    "text":{"type":"string"},"description":{"type":"string"}}}}}}}})
}

pub async fn save_image_cache(
    tx: &mut Transaction<'_, Postgres>,
    input: &Value,
    output: &Value,
    model: &str,
    hash: &str,
) -> Result<()> {
    for slot in input["images"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|slot| slot["frame"] != true)
    {
        let item = output["items"]
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item["media_id"] == slot["media_id"])
            })
            .context("missing image result")?;
        let media: Uuid = slot["read_from"]
            .as_str()
            .context("missing source image")?
            .parse()?;
        sqlx::query("INSERT INTO crm.image_ocr_readings(media_id,status,model,text,description,category,summary,generated_at,input_hash,job_id,error)VALUES($1,'complete',$2,$3,$4,$5,$4,now(),$6,NULL,NULL) ON CONFLICT(media_id)DO UPDATE SET status='complete',model=excluded.model,text=excluded.text,description=excluded.description,category=excluded.category,summary=excluded.summary,generated_at=excluded.generated_at,input_hash=excluded.input_hash,job_id=NULL,error=NULL WHERE image_ocr_readings.status<>'complete'OR(coalesce(btrim(image_ocr_readings.text),'')=''AND coalesce(btrim(image_ocr_readings.description),'')='')")
            .bind(media).bind(model).bind(item["text"].as_str()).bind(item["description"].as_str()).bind(item["category"].as_str()).bind(format!("next-post-media:{hash}")).execute(&mut **tx).await?;
        sqlx::query("UPDATE crm.ai_jobs SET status='cancelled',token=NULL,lease_until=NULL,completed_at=now(),error='Reused post image reading'WHERE kind='ai.image_ocr'AND subject=$1 AND status='queued'")
            .bind(media.to_string()).execute(&mut **tx).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn next_slots_keep_video_identity_and_read_its_poster() {
        let shown = slots(&[
            json!({"role":"media-3","id":"video","media_type":"video/mp4","sha256":"v"}),
            json!({"role":"thumbnail-3","id":"poster","media_type":"image/jpeg","sha256":"p"}),
            json!({"role":"media-1","id":"photo","media_type":"image/jpeg","sha256":"a"}),
        ]);
        assert_eq!(shown.len(), 2);
        assert_eq!(shown[0]["media_id"], "photo");
        assert_eq!(shown[1]["media_id"], "video");
        assert_eq!(shown[1]["read_from"], "poster");
        assert_eq!(shown[1]["frame"], false);
        let output = tidy(&json!({"summary":"两张","items":"[{\"index\":2,\"description\":\"视频封面\",\"text\":\"\",\"category\":\"其他\"},{\"index\":1,\"description\":\"街景\",\"text\":\"\",\"category\":\"城市街景\"}]"}),&shown).unwrap();
        assert_eq!(output["items"][0]["media_id"], "photo");
        assert_eq!(output["items"][1]["description"], "视频封面");
        assert!(tidy(&json!({"summary":"Empty","items":[{"index":1,"text":"","description":"","category":"其他"}]}),&shown).is_err());
    }

    #[test]
    fn short_video_without_a_poster_reads_its_first_frame() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("short.mp4");
        let result = std::process::Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=red:s=160x90:d=0.2",
                "-c:v",
                "mpeg4",
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "synthetic video: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let url = data_url(&path, true, None).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(url.strip_prefix("data:image/jpeg;base64,").unwrap())
            .unwrap();
        let image = image::load_from_memory(&bytes).unwrap();
        assert_eq!((image.width(), image.height()), (160, 90));
    }
}
