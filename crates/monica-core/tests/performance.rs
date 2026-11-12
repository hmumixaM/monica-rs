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
            sqlx::query("TRUNCATE wechat.message_corrections,wechat.messages,crm.conversation_state,crm.conversation_work,crm.conversation_summaries").execute(&pool).await.unwrap();
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

#[sqlx::test(migrations = "../../migrations")]
#[ignore]
async fn connection_journal_and_projection_scaling(pool: PgPool) {
    use monica_core::connections;
    use serde_json::json;
    sqlx::query("CREATE EXTENSION pg_stat_statements")
        .execute(&pool)
        .await
        .unwrap();
    for size in [10_000, 100_000] {
        let cid = connections::connection_id(Provider::AppleMessages, &format!("synthetic-{size}"));
        sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,enabled) VALUES($1,'apple_messages',$2,'Synthetic','local',true)").bind(cid).bind(format!("synthetic-{size}")).execute(&pool).await.unwrap();
        let before = calls(&pool).await;
        let wal: String = sqlx::query_scalar("SELECT pg_current_wal_insert_lsn()::text")
            .fetch_one(&pool)
            .await
            .unwrap();
        let started = Instant::now();
        for offset in (0..size).step_by(1000) {
            let rows:Vec<_>=(offset..offset+1000).map(|i|json!({"external_id":format!("m{i:09}"),"kind":"message","thread_id":format!("thread-{}",i%100),"occurred_at":chrono::DateTime::from_timestamp(1700000000+i as i64,0).unwrap().to_rfc3339(),"body":format!("Synthetic message {i}"),"participants":[{"id":"self","role":"sender","is_self":true},{"id":format!("friend-{}",i%100),"role":"recipient","emails":[format!("friend-{}@example.test",i%100)]}]})).collect();
            let mut tx = pool.begin().await.unwrap();
            assert_eq!(
                connections::commit_page(
                    &mut tx,
                    Provider::AppleMessages,
                    cid,
                    "messages",
                    &rows,
                    &json!({"after":offset+1000})
                )
                .await
                .unwrap(),
                1000
            );
            tx.commit().await.unwrap();
            assert_eq!(
                connections::deliver(&pool, Provider::AppleMessages, cid, 1000)
                    .await
                    .unwrap(),
                1000
            );
        }
        let elapsed = started.elapsed().as_secs_f64();
        let sql_calls = calls(&pool).await - before - 1;
        let wal_bytes: i64 = sqlx::query_scalar(
            "SELECT pg_wal_lsn_diff(pg_current_wal_insert_lsn(),$1::pg_lsn)::bigint",
        )
        .bind(wal)
        .fetch_one(&pool)
        .await
        .unwrap();
        let per_thousand = sql_calls as f64 * 1000. / size as f64;
        if per_thousand >= 100. {
            let top:Vec<(i64,String)>=sqlx::query_as("SELECT calls,query FROM pg_stat_statements WHERE dbid=(SELECT oid FROM pg_database WHERE datname=current_database()) ORDER BY calls DESC LIMIT 12").fetch_all(&pool).await.unwrap();
            println!("CONNECTION_TOP_SQL {top:?}");
        }
        assert!(
            per_thousand < 100.,
            "new messages regressed to per-record SQL: {per_thousand}"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT sum(message_count)::bigint FROM crm.conversations WHERE connection_id=$1"
            )
            .bind(cid)
            .fetch_one(&pool)
            .await
            .unwrap(),
            size as i64
        );
        println!(
            "CONNECTION_BENCH {}",
            json!({"messages":size,"batch_records":1000,"seconds":elapsed,"sql_calls_per_1000":per_thousand,"wal_bytes_per_1000":wal_bytes as f64*1000./size as f64,"notes":"immutable source journal + CRM delivery + ACK; 100 conversations; debug build; isolated cluster WAL"})
        );
    }
}
