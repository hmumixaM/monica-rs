//! Low-frequency operational snapshots from explicitly configured collectors.
//! Never imports raw log strings, payloads, URLs or authentication material.
use anyhow::Result;
use serde_json::{Value, json};
use sqlx::PgPool;
pub async fn refresh(pool: &PgPool) -> Result<()> {
    let Some(root) = std::env::var_os("MONICA_COLLECTOR_DATA_ROOT") else {
        return Ok(());
    };
    refresh_root(pool, std::path::Path::new(&root)).await
}
async fn refresh_root(pool: &PgPool, root: &std::path::Path) -> Result<()> {
    for provider in ["instagram", "linkedin"] {
        let path = root.join("collectors").join(provider).join("status.json");
        let Ok(metadata) = tokio::fs::symlink_metadata(&path).await else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > 1024 * 1024 {
            continue;
        }
        let Ok(raw) = tokio::fs::read(&path).await else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<Value>(&raw) else {
            continue;
        };
        let Some(run) = value["run_id"]
            .as_str()
            .and_then(|s| s.parse::<uuid::Uuid>().ok())
        else {
            continue;
        };
        let Some(updated) = value["updated_at"]
            .as_f64()
            .filter(|n| n.is_finite() && *n > 0. && *n < 1e11)
        else {
            continue;
        };
        let start = value["started_at"]
            .as_f64()
            .filter(|n| n.is_finite() && *n > 0. && *n <= updated)
            .unwrap_or(updated);
        let state = value["state"]
            .as_str()
            .filter(|s| {
                [
                    "collecting",
                    "ready",
                    "needs_login",
                    "paused",
                    "failed",
                    "stopped",
                ]
                .contains(s)
            })
            .unwrap_or("unknown");
        let phase = value["phase"]
            .as_str()
            .filter(|s| {
                [
                    "followees",
                    "profiles",
                    "posts",
                    "stories",
                    "highlights",
                    "connections",
                    "profiles_posts",
                    "messages",
                    "browser_login",
                    "idle",
                ]
                .contains(s)
            })
            .unwrap_or("collecting");
        let next = value["next_run_at"]
            .as_f64()
            .filter(|n| n.is_finite() && *n > 0. && *n < 1e11);
        let mut tx = pool.begin().await?;
        let previous: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT state,phase FROM crm.import_activity WHERE id=$1")
                .bind(format!("collector:{provider}:{run}"))
                .fetch_optional(&mut *tx)
                .await?;
        let changed=sqlx::query("INSERT INTO crm.import_activity(id,provider,stream,kind,state,phase,received_at,updated_at,heartbeat_at,next_run_at,failures) VALUES($1,$2,'collection','collection',$3,$4,to_timestamp($5),to_timestamp($6),to_timestamp($6),to_timestamp($7),$8) ON CONFLICT(id) DO UPDATE SET state=excluded.state,phase=excluded.phase,updated_at=excluded.updated_at,heartbeat_at=excluded.heartbeat_at,next_run_at=excluded.next_run_at,failures=greatest(crm.import_activity.failures,excluded.failures) WHERE crm.import_activity.updated_at<excluded.updated_at")
   .bind(format!("collector:{provider}:{run}")).bind(provider).bind(state).bind(phase).bind(start).bind(updated).bind(next).bind(i64::from(state=="failed")).execute(&mut *tx).await?.rows_affected();
        if changed > 0
            && previous
                .as_ref()
                .is_none_or(|(old, old_phase)| old != state || old_phase.as_deref() != Some(phase))
        {
            sqlx::query("INSERT INTO crm.import_run_events(run_id,provider,state,phase,details) VALUES($1,$2,$3,$4,$5)").bind(format!("collector:{provider}:{run}")).bind(provider).bind(state).bind(phase).bind(json!({"next_run_at":next})).execute(&mut *tx).await?;
        }
        tx.commit().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[sqlx::test(migrations = "../../migrations")]
    async fn snapshots_keep_transitions_dedupe_heartbeats_and_exclude_secrets(pool: sqlx::PgPool) {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("collectors/linkedin");
        tokio::fs::create_dir_all(&folder).await.unwrap();
        let run = uuid::Uuid::new_v4();
        let stamp = chrono::Utc::now().timestamp() as f64;
        for (offset, state, phase) in [
            (0., "collecting", "connections"),
            (0., "collecting", "connections"),
            (1., "collecting", "connections"),
            (2., "failed", "profiles_posts"),
        ] {
            let value = json!({"run_id":run,"started_at":stamp,"updated_at":stamp+offset,"state":state,"phase":phase,"error":"secret-cookie-must-not-escape"});
            tokio::fs::write(folder.join("status.json"), value.to_string())
                .await
                .unwrap();
            refresh_root(&pool, root.path()).await.unwrap();
        }
        let rows: Vec<Value> = sqlx::query_scalar("SELECT to_jsonb(a) FROM crm.import_activity a")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["state"], "failed");
        assert_eq!(rows[0]["failures"], 1);
        assert!(!json!(rows).to_string().contains("secret-cookie"));
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.import_run_events")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(events, 2);
        let failures: i64 =
            sqlx::query_scalar("SELECT sum(failures)::bigint FROM crm.import_activity_daily")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(failures, 1);
    }
}
