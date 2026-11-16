//! Live imported records, with ordinary, system and unknown directions separate.
use super::{Result, invalid};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub(crate) async fn rows(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    timezone: &str,
) -> Result<Vec<Value>> {
    if matches!(timezone, "UTC" | "America/Chicago") {
        return Ok(sqlx::query_scalar("SELECT jsonb_build_object('month',s.period,'sent',sum(s.sent),'received',sum(s.received),'system',sum(s.system),'unknown',sum(s.unknown),'total',sum(s.sent+s.received),'records',sum(s.sent+s.received+s.system+s.unknown)) FROM crm.message_stat_counts s JOIN crm.conversations c ON c.id=s.conversation_id WHERE c.profile_id=$1 AND c.conversation_type='direct' AND s.scope='month:'||$2 GROUP BY s.period HAVING sum(s.sent+s.received+s.system+s.unknown)>0 ORDER BY s.period")
            .bind(id).bind(timezone).fetch_all(&mut **tx).await?);
    }
    let valid: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_timezone_names WHERE name=$1)")
            .bind(timezone)
            .fetch_one(&mut **tx)
            .await?;
    if !valid {
        return Err(invalid());
    }
    Ok(sqlx::query_scalar("WITH messages AS(SELECT to_char(m.occurred_at AT TIME ZONE $2,'YYYY-MM') AS month,crm.message_stat_kind(m.direction,m.sender,m.account,m.message_type) kind FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id WHERE c.profile_id=$1 AND c.conversation_type='direct' AND NOT m.tombstone) SELECT jsonb_build_object('month',month,'sent',count(*) FILTER(WHERE kind='sent'),'received',count(*) FILTER(WHERE kind='received'),'system',count(*) FILTER(WHERE kind='system'),'unknown',count(*) FILTER(WHERE kind='unknown'),'total',count(*) FILTER(WHERE kind IN('sent','received')),'records',count(*)) FROM messages GROUP BY month ORDER BY month")
        .bind(id).bind(timezone).fetch_all(&mut **tx).await?)
}

pub(crate) async fn bounds(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<Value> {
    // Two indexed endpoints per conversation; do not scan its entire history.
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('first',min(f.occurred_at),'last',max(l.occurred_at)) FROM crm.conversations c LEFT JOIN LATERAL(SELECT occurred_at FROM crm.message_search WHERE conversation_id=c.id AND NOT tombstone ORDER BY occurred_at,message_id LIMIT 1) f ON true LEFT JOIN LATERAL(SELECT occurred_at FROM crm.message_search WHERE conversation_id=c.id AND NOT tombstone ORDER BY occurred_at DESC,message_id DESC LIMIT 1) l ON true WHERE c.profile_id=$1 AND c.conversation_type='direct'")
        .bind(id).fetch_one(&mut **tx).await?)
}

pub(crate) async fn summary(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<Value> {
    let rows: Value=sqlx::query_scalar("SELECT jsonb_build_object('total',coalesce(sum(s.sent+s.received) FILTER(WHERE c.conversation_type='direct'),0),'sent',coalesce(sum(s.sent) FILTER(WHERE c.conversation_type='direct'),0),'received',coalesce(sum(s.received) FILTER(WHERE c.conversation_type='direct'),0),'system',coalesce(sum(s.system) FILTER(WHERE c.conversation_type='direct'),0),'unknown',coalesce(sum(s.unknown) FILTER(WHERE c.conversation_type='direct'),0),'record_total',coalesce(sum(s.sent+s.received+s.system+s.unknown) FILTER(WHERE c.conversation_type='direct'),0),'group_total',coalesce(sum(s.sent+s.received) FILTER(WHERE c.conversation_type='group'),0),'last_group_at',max(c.last_message_at) FILTER(WHERE c.conversation_type='group')) FROM crm.conversations c LEFT JOIN crm.message_stat_counts s ON s.conversation_id=c.id AND s.scope='all' WHERE c.profile_id=$1")
        .bind(id).fetch_one(&mut **tx).await?;
    let mut rows = rows;
    let time = bounds(tx, id).await?;
    rows["first_contact_at"] = time["first"].clone();
    rows["last_contact_at"] = time["last"].clone();
    rows["count_basis"] = json!("imported_non_deleted_records");
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    #[sqlx::test(migrations = "../../migrations")]
    async fn imported_counts_follow_corrections_tombstones_and_ownership(pool: PgPool) {
        let person = Uuid::new_v4();
        let target = Uuid::new_v4();
        let conversation = Uuid::new_v4();
        sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) SELECT id,'person','Synthetic' FROM unnest($1::uuid[]) id")
            .bind(vec![person,target]).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO crm.conversations(id,provider,account,external_id,conversation_type,profile_id) VALUES($1,'wechat','self','peer','direct',$2)")
            .bind(conversation).bind(person).execute(&pool).await.unwrap();
        // Local February, UTC March; an explicit unknown sender is not received.
        sqlx::query("INSERT INTO crm.message_search(message_id,conversation_id,provider,account,external_id,sender,occurred_at,body,message_type,direction) SELECT gen_random_uuid(),$1,'wechat','self',n::text,'peer','2026-03-01T00:30:00Z','Synthetic',CASE WHEN n=3 THEN '10000' ELSE '1' END,CASE WHEN n=1 THEN 'sent' WHEN n=4 THEN 'unknown' ELSE 'received' END FROM generate_series(1,4) n")
            .bind(conversation).execute(&pool).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        let local = rows(&mut tx, person, "America/Chicago").await.unwrap();
        assert_eq!(
            local[0],
            json!({"month":"2026-02","sent":1,"received":1,"system":1,"unknown":1,"total":2,"records":4})
        );
        assert_eq!(
            rows(&mut tx, person, "UTC").await.unwrap()[0]["month"],
            "2026-03"
        );
        assert_eq!(
            rows(&mut tx, person, "Asia/Shanghai").await.unwrap()[0]["records"],
            4
        );
        assert_eq!(summary(&mut tx, person).await.unwrap()["record_total"], 4);
        tx.commit().await.unwrap();
        sqlx::query("UPDATE crm.message_search SET direction='sent' WHERE conversation_id=$1 AND external_id='2'")
            .bind(conversation).execute(&pool).await.unwrap();
        sqlx::query("UPDATE crm.message_search SET tombstone=true WHERE conversation_id=$1 AND external_id='1'")
            .bind(conversation).execute(&pool).await.unwrap();
        // Replaying an unchanged projection must not double count.
        sqlx::query("UPDATE crm.message_search SET body=body WHERE conversation_id=$1")
            .bind(conversation)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE crm.conversations SET profile_id=$2 WHERE id=$1")
            .bind(conversation)
            .bind(target)
            .execute(&pool)
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        assert!(rows(&mut tx, person, "UTC").await.unwrap().is_empty());
        let current = rows(&mut tx, target, "UTC").await.unwrap();
        assert_eq!(current[0]["sent"], 1);
        assert_eq!(current[0]["received"], 0);
        assert_eq!(current[0]["records"], 3);
        tx.commit().await.unwrap();
        sqlx::query("DELETE FROM crm.message_search WHERE conversation_id=$1")
            .bind(conversation)
            .execute(&pool)
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        assert!(rows(&mut tx, target, "UTC").await.unwrap().is_empty());
        assert_eq!(summary(&mut tx, target).await.unwrap()["record_total"], 0);
    }
}
