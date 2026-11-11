use monica_core::{
    Provider,
    messages::{self, Message},
};
use sqlx::PgPool;
use std::time::Instant;

async fn calls(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT coalesce(sum(calls),0)::bigint FROM pg_stat_statements WHERE dbid=(SELECT oid FROM pg_database WHERE datname=current_database()) AND query NOT LIKE '%pg_stat_statements%'").fetch_one(pool).await.unwrap()
}

/// Run separately in an isolated cluster with pg_stat_statements preloaded.
#[sqlx::test(migrations = "../../migrations")]
#[ignore]
async fn synthetic_scaling(pool: PgPool) {
    sqlx::query("CREATE EXTENSION pg_stat_statements")
        .execute(&pool)
        .await
        .unwrap();
    for round in 1..=3 {
        for size in [10_000, 100_000] {
            sqlx::query("TRUNCATE wechat.messages,crm.conversation_state,crm.conversation_work,crm.conversation_summaries").execute(&pool).await.unwrap();
            let before = calls(&pool).await;
            let wal: String = sqlx::query_scalar("SELECT pg_current_wal_insert_lsn()::text")
                .fetch_one(&pool)
                .await
                .unwrap();
            let start = Instant::now();
            for offset in (0..size).step_by(1000) {
                let records: Vec<_> = (offset..offset + 1000)
                    .map(|i| Message {
                        account: "owner".into(),
                        conversation: format!("conversation-{:03}", i % 100),
                        external_id: format!("m{i:09}"),
                        sent_at_ms: i as i64,
                        sender: "sender".into(),
                        body: format!("Synthetic message {i}"),
                    })
                    .collect();
                messages::append(&pool, Provider::Wechat, &records)
                    .await
                    .unwrap();
            }
            let seconds = start.elapsed().as_secs_f64();
            let wal_bytes: i64 = sqlx::query_scalar(
                "SELECT pg_wal_lsn_diff(pg_current_wal_insert_lsn(),$1::pg_lsn)::bigint",
            )
            .bind(wal)
            .fetch_one(&pool)
            .await
            .unwrap();
            let sql_calls = calls(&pool).await - before - 2; // Exclude the two WAL probes.
            let work: i64 = sqlx::query_scalar("SELECT count(*) FROM crm.conversation_work")
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(work, 100);
            while let Some(lease) = messages::claim(&pool).await.unwrap() {
                let state = messages::snapshot(&pool, &lease).await.unwrap();
                messages::finish(&pool, &lease, &state).await.unwrap();
            }
            sqlx::query("ANALYZE crm.conversation_summaries")
                .execute(&pool)
                .await
                .unwrap();
            let mut latency = Vec::new();
            for sample in 0..110 {
                let t = Instant::now();
                let rows:Vec<(String,i64,String)>=sqlx::query_as("SELECT conversation,message_count,preview FROM crm.conversation_summaries WHERE provider='wechat' AND account='owner' ORDER BY last_at_ms DESC,conversation LIMIT 50").fetch_all(&pool).await.unwrap();
                assert_eq!(rows.len(), 50);
                if sample >= 10 {
                    latency.push(t.elapsed().as_secs_f64() * 1000.0);
                }
            }
            latency.sort_by(f64::total_cmp);
            println!(
                "BENCH {}",
                serde_json::json!({"round":round,"messages":size,"conversations":100,"batch_records":1000,"ingest_seconds":seconds,"sql_calls_per_1000":sql_calls as f64*1000.0/size as f64,"wal_bytes_per_1000":wal_bytes as f64*1000.0/size as f64,"summary_query_p95_ms":latency[94],"summary_query_p99_ms":latency[98],"notes":"warm SQL query; not full page/API; WAL is cluster-wide in isolated test; Rust debug build"})
            );
        }
    }
}
