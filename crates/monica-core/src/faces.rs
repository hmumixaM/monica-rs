//! Face annotations from a managed external device. No embedding or model call
//! runs on this server; chat/message images never cross the device boundary.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub const ELIGIBLE: &str = "SELECT l.media_id,bool_or(l.entity_type<>'social_post') AS avatar,(array_agg(CASE WHEN l.entity_type='profile' THEN l.entity_id ELSE sal.profile_id END) FILTER(WHERE l.entity_type<>'social_post'))[1] AS owner_profile_id FROM crm.media_links l JOIN crm.media_assets a ON a.id=l.media_id LEFT JOIN crm.source_account_links sal ON l.entity_type='source_account' AND sal.account_id=l.entity_id WHERE a.media_type IN ('image/jpeg','image/jpg','image/png','image/webp') AND ((l.entity_type IN ('profile','source_account') AND l.role IN ('wechat_avatar','wechat_avatar_big','linkedin_avatar','instagram_avatar','manual_photo')) OR (l.entity_type='social_post' AND l.role~'^media-[0-9]+$')) GROUP BY l.media_id";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Face {
    pub id: Uuid,
    pub r#box: Vec<f32>,
    pub quality: f32,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Scan {
    pub media_id: Uuid,
    pub faces: Vec<Face>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default = "success")]
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
}
fn success() -> String {
    "success".into()
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Suggestion {
    pub face_id: Uuid,
    pub cluster_key: Option<String>,
    pub profile_id: Option<Uuid>,
    pub score: Option<f32>,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Batch {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub scans: Vec<Scan>,
    #[serde(default)]
    pub suggestions: Vec<Suggestion>,
}

pub async fn batch(pool: &PgPool, input: &Batch) -> Result<Value> {
    ensure!(
        input.scans.len() <= 1000 && input.suggestions.len() <= 50000,
        "face batch exceeds limits"
    );
    let mut ids = BTreeSet::new();
    let mut seen_faces = BTreeSet::new();
    let mut total = 0;
    for scan in &input.scans {
        ensure!(ids.insert(scan.media_id), "duplicate scan media id");
        ensure!(
            scan.model
                .as_ref()
                .or(input.model.as_ref())
                .is_some_and(|m| !m.is_empty() && m.len() <= 255),
            "scan requires a model label"
        );
        ensure!(
            ["success", "retryable_error", "permanent_error"].contains(&scan.status.as_str()),
            "invalid scan state"
        );
        ensure!(
            scan.error.as_ref().is_none_or(|e| e.len() <= 2000),
            "scan error too long"
        );
        for face in &scan.faces {
            ensure!(seen_faces.insert(face.id), "duplicate face identity");
            ensure!(
                face.r#box.len() == 4
                    && face
                        .r#box
                        .iter()
                        .all(|x| x.is_finite() && (-1.0..=2.0).contains(x))
                    && face.r#box[2] > 0.0
                    && face.r#box[3] > 0.0,
                "invalid face rectangle"
            );
            ensure!(
                face.quality.is_finite() && (0.0..=1.0).contains(&face.quality),
                "invalid face quality"
            );
            total += 1;
        }
    }
    ensure!(total <= 50000, "too many detections");
    for suggestion in &input.suggestions {
        ensure!(
            suggestion
                .score
                .is_none_or(|x| x.is_finite() && (0.0..=1.0).contains(&x))
                && suggestion
                    .cluster_key
                    .as_ref()
                    .is_none_or(|x| x.len() <= 255),
            "invalid face suggestion"
        );
    }
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL statement_timeout='15s'")
        .execute(&mut *tx)
        .await?;
    let allowed: Vec<Uuid> = sqlx::query_scalar(&format!(
        "SELECT media_id FROM ({ELIGIBLE}) eligible WHERE media_id=ANY($1)"
    ))
    .bind(ids.iter().copied().collect::<Vec<_>>())
    .fetch_all(&mut *tx)
    .await?;
    ensure!(
        allowed.len() == ids.len(),
        "device may only scan eligible avatar/post media"
    );
    let mut face_count = 0;
    for scan in &input.scans {
        let model = scan.model.as_ref().or(input.model.as_ref()).unwrap();
        let legacy = model
            .rsplit(':')
            .next()
            .filter(|x| ["timeout", "missing", "unreadable"].contains(x));
        let status = if legacy.is_some() {
            "retryable_error"
        } else {
            &scan.status
        };
        if status == "success" {
            let records = serde_json::to_value(&scan.faces)?;
            let conflict:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM jsonb_to_recordset($1) AS i(id uuid) JOIN crm.faces f ON f.id=i.id WHERE f.media_id<>$2)").bind(&records).bind(scan.media_id).fetch_one(&mut *tx).await?;
            ensure!(!conflict, "face identity belongs to another media asset");
            sqlx::query("INSERT INTO crm.faces(id,media_id,box,quality,in_post) SELECT i.id,$2,ARRAY(SELECT jsonb_array_elements_text(i.box)::real),i.quality,EXISTS(SELECT 1 FROM crm.media_links l WHERE l.media_id=$2 AND l.entity_type='social_post') FROM jsonb_to_recordset($1) AS i(id uuid,box jsonb,quality real) ORDER BY i.id ON CONFLICT(id) DO NOTHING").bind(records).bind(scan.media_id).execute(&mut *tx).await?;
            face_count += scan.faces.len();
        }
        sqlx::query("INSERT INTO crm.face_scans(media_id,model,faces,status,error,next_attempt_at) VALUES($1,$2,$3,$4,$5,CASE WHEN $4='retryable_error' THEN now()+interval '10 minutes' END) ON CONFLICT(media_id) DO UPDATE SET model=excluded.model,faces=excluded.faces,status=excluded.status,error=excluded.error,next_attempt_at=excluded.next_attempt_at,scanned_at=now() WHERE crm.face_scans.status<>'success' OR excluded.status='success'")
            .bind(scan.media_id).bind(model).bind(if status=="success"{scan.faces.len() as i32}else{0}).bind(status).bind(legacy.or(scan.error.as_deref())).execute(&mut *tx).await?;
    }
    let changed=sqlx::query("UPDATE crm.faces f SET cluster_key=i.cluster_key,suggested_profile_id=CASE WHEN EXISTS(SELECT 1 FROM crm.profiles p WHERE p.id=i.profile_id) AND i.profile_id IS DISTINCT FROM f.rejected_profile_id THEN i.profile_id END,suggestion_score=i.score FROM jsonb_to_recordset($1) AS i(face_id uuid,cluster_key text,profile_id uuid,score real) WHERE f.id=i.face_id AND f.status='pending' AND (f.cluster_key,f.suggested_profile_id,f.suggestion_score) IS DISTINCT FROM (i.cluster_key,i.profile_id,i.score)")
        .bind(serde_json::to_value(&input.suggestions)?).execute(&mut *tx).await?.rows_affected();
    sqlx::query("UPDATE crm.face_device_runtime SET state='ready',model=coalesce($1,model),last_batch_at=now(),updated_at=now(),error=NULL WHERE singleton").bind(&input.model).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(json!({"faces":face_count,"scans":input.scans.len(),"suggestions":changed}))
}

pub async fn work(pool: &PgPool, limit: i64) -> Result<Value> {
    ensure!((1..=500).contains(&limit), "invalid work limit");
    let rows:Vec<Value>=sqlx::query_scalar(&format!("WITH eligible AS ({ELIGIBLE}),page AS (SELECT a.id AS media_id,e.avatar,e.owner_profile_id,a.media_type,a.byte_length,a.created_at FROM eligible e JOIN crm.media_assets a ON a.id=e.media_id WHERE NOT EXISTS(SELECT 1 FROM crm.face_scans s WHERE s.media_id=a.id AND (s.status<>'retryable_error' OR s.next_attempt_at>now())) ORDER BY e.avatar DESC,a.created_at DESC,a.id LIMIT $1) SELECT jsonb_build_object('media_id',media_id,'avatar',avatar,'owner_profile_id',coalesce(owner_profile_id,(SELECT p.profile_id FROM crm.media_links l JOIN crm.social_posts p ON p.id=l.entity_id WHERE l.media_id=page.media_id AND l.entity_type='social_post' AND l.role~'^media-[0-9]+$' AND p.profile_id IS NOT NULL ORDER BY p.occurred_at LIMIT 1)),'media_type',media_type,'byte_length',byte_length) FROM page ORDER BY avatar DESC,created_at DESC,media_id")).bind(limit).fetch_all(pool).await?;
    Ok(json!({"items":rows}))
}
pub async fn anchors(pool: &PgPool) -> Result<Value> {
    let anchors:Vec<Value>=sqlx::query_scalar(&format!("WITH eligible AS ({ELIGIBLE}),known AS (SELECT f.id AS face_id,e.owner_profile_id AS profile_id,'avatar' AS kind FROM crm.faces f JOIN eligible e ON e.media_id=f.media_id AND e.avatar AND e.owner_profile_id IS NOT NULL WHERE f.status='pending' AND f.quality>=0.6 AND f.rejected_profile_id IS DISTINCT FROM e.owner_profile_id AND (SELECT count(*) FROM crm.faces o WHERE o.media_id=f.media_id)=1 UNION ALL SELECT id,profile_id,'confirmed' FROM crm.faces WHERE status='confirmed' AND profile_id IS NOT NULL) SELECT to_jsonb(known) FROM known")).fetch_all(pool).await?;
    let rejected:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('face_id',id,'profile_id',rejected_profile_id) FROM crm.faces WHERE rejected_profile_id IS NOT NULL").fetch_all(pool).await?;
    let ignored: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM crm.faces WHERE status='ignored'")
        .fetch_all(pool)
        .await?;
    Ok(json!({"anchors":anchors,"rejected":rejected,"ignored":ignored}))
}
pub async fn avatar_matches(pool: &PgPool, media: &[Uuid]) -> Result<BTreeMap<(Uuid, Uuid), f64>> {
    let rows=sqlx::query("SELECT media_id,CASE WHEN status='confirmed' THEN profile_id ELSE suggested_profile_id END AS profile_id,max(CASE WHEN status='confirmed' THEN 1.0 ELSE suggestion_score END)::float8 AS score FROM crm.faces WHERE media_id=ANY($1) AND status<>'ignored' AND (status='confirmed' OR (suggested_profile_id IS NOT NULL AND suggested_profile_id IS DISTINCT FROM rejected_profile_id AND quality>=0.6)) GROUP BY 1,2").bind(media).fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some((
                (r.get("media_id"), r.get::<Option<Uuid>, _>("profile_id")?),
                ((r.get::<Option<f64>, _>("score")?) * 100.0).round() / 100.0,
            ))
        })
        .collect())
}
#[derive(Debug, Deserialize)]
pub struct Decision {
    pub face_ids: Vec<Uuid>,
    pub action: String,
    pub profile_id: Option<Uuid>,
}
pub async fn decide(pool: &PgPool, decision: &Decision) -> Result<Value> {
    ensure!(
        !decision.face_ids.is_empty()
            && decision.face_ids.len() <= 200
            && ["confirm", "not", "ignore"].contains(&decision.action.as_str()),
        "invalid face decision"
    );
    ensure!(
        decision.action != "confirm" || decision.profile_id.is_some(),
        "confirm needs a person"
    );
    let mut tx = pool.begin().await?;
    if let Some(profile) = decision.profile_id {
        let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM crm.profiles WHERE id=$1 AND profile_type='person' AND archived_at IS NULL)").bind(profile).fetch_one(&mut *tx).await?;
        ensure!(exists, "person not found");
    }
    let before:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'status',status,'profile_id',profile_id,'suggested_profile_id',suggested_profile_id,'rejected_profile_id',rejected_profile_id,'decided_at',decided_at) FROM crm.faces WHERE id=ANY($1) ORDER BY id FOR UPDATE").bind(&decision.face_ids).fetch_all(&mut *tx).await?;
    let change = match decision.action.as_str() {
        "confirm" => "status='confirmed',profile_id=$2",
        "not" => {
            "rejected_profile_id=coalesce($2,suggested_profile_id,rejected_profile_id),suggested_profile_id=NULL,cluster_key=NULL,profile_id=NULL,status='pending'"
        }
        _ => {
            "status='ignored',profile_id=CAST(NULL AS uuid),suggested_profile_id=CASE WHEN $2::uuid IS NULL THEN suggested_profile_id ELSE suggested_profile_id END"
        }
    };
    sqlx::query(&format!(
        "UPDATE crm.faces SET {change},decided_at=now() WHERE id=ANY($1)"
    ))
    .bind(&decision.face_ids)
    .bind(decision.profile_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(json!({"previous":before}))
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FaceState {
    pub id: Uuid,
    pub status: String,
    pub profile_id: Option<Uuid>,
    pub suggested_profile_id: Option<Uuid>,
    pub rejected_profile_id: Option<Uuid>,
    pub decided_at: Option<String>,
}
pub async fn restore(pool: &PgPool, states: &[FaceState]) -> Result<Value> {
    ensure!(
        !states.is_empty() && states.len() <= 200,
        "invalid restore size"
    );
    ensure!(
        states.iter().all(
            |s| ["pending", "confirmed", "ignored"].contains(&s.status.as_str())
                && (s.status != "confirmed" || s.profile_id.is_some())
        ),
        "invalid face state"
    );
    let mut tx = pool.begin().await?;
    let mut sorted = states.to_vec();
    sorted.sort_by_key(|s| s.id);
    for state in sorted {
        sqlx::query("UPDATE crm.faces SET status=$2,profile_id=$3,suggested_profile_id=$4,rejected_profile_id=$5,decided_at=$6::timestamptz WHERE id=$1").bind(state.id).bind(state.status).bind(state.profile_id).bind(state.suggested_profile_id).bind(state.rejected_profile_id).bind(state.decided_at).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(json!({"restored":states.len()}))
}
