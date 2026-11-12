//! Synthetic differential driver. The harness supplies a disposable database.
use anyhow::Result;
use monica_core::{Provider, connections::*};
use serde_json::{Value, json};
#[tokio::main]
async fn main() -> Result<()> {
    let pool = sqlx::PgPool::connect(&std::env::var("DATABASE_URL")?).await?;
    monica_core::migrate(&pool).await?;
    let scenario: Value =
        serde_json::from_str(&std::fs::read_to_string(std::env::args().nth(1).unwrap())?)?;
    let mut snapshots = vec![];
    for op in scenario.as_array().unwrap() {
        let provider = Provider::parse(op["provider"].as_str().unwrap())?;
        let account = op["account"].as_str().unwrap();
        let cid = connection_id(provider, account);
        sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,state,enabled) VALUES($1,$2,$3,$3,'local','connected',true) ON CONFLICT DO NOTHING").bind(cid).bind(provider.schema()).bind(account).execute(&pool).await?;
        let mut tx = pool.begin().await?;
        let changed = commit_page(
            &mut tx,
            provider,
            cid,
            "records",
            op["records"].as_array().unwrap(),
            &op["cursor"],
        )
        .await?;
        tx.commit().await?;
        deliver(&pool, provider, cid, 1000).await?;
        if op["relink"] == true {
            relink_pending(&pool, 1000).await?;
        }
        let mut tx = pool.begin().await?;
        let cursor = cursor(&mut tx, provider, cid, "records").await?;
        tx.rollback().await?;
        let records:Vec<Value>=sqlx::query_scalar(&format!("SELECT jsonb_build_array(external_id,revision,content_hash,payload) FROM {}.connection_records WHERE connection_id=$1 ORDER BY external_id",provider.schema())).bind(cid).fetch_all(&pool).await?;
        let evidence:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_array(c.external_account,e.external_id,e.revision,e.content_hash,e.kind,to_char(e.occurred_at AT TIME ZONE 'UTC','YYYY-MM-DD HH24:MI:SS'),e.body,e.included,e.deleted,e.metadata) FROM crm.connection_evidence e JOIN crm.source_connections c ON c.id=e.connection_id ORDER BY c.external_account,e.external_id").fetch_all(&pool).await?;
        let people:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_array(c.external_account,p.external_id,p.display_name,p.is_self,p.resolved_profile_id) FROM crm.connection_people p JOIN crm.source_connections c ON c.id=p.connection_id ORDER BY c.external_account,p.external_id").fetch_all(&pool).await?;
        let candidates:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_array(c.external_account,p.external_id,p.profile_id,p.reason) FROM crm.connection_identity_candidates p JOIN crm.source_connections c ON c.id=p.connection_id ORDER BY c.external_account,p.external_id,p.profile_id").fetch_all(&pool).await?;
        let messages:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_array(cn.external_account,c.source_external_id,c.conversation_type,c.profile_id,m.body,to_char(m.occurred_at AT TIME ZONE 'UTC','YYYY-MM-DD HH24:MI:SS'),m.sender_profile_id) FROM crm.message_search m JOIN crm.conversations c ON c.id=m.conversation_id JOIN crm.source_connections cn ON cn.id=c.connection_id ORDER BY cn.external_account,c.source_external_id,m.occurred_at,m.body").fetch_all(&pool).await?;
        let mut snapshot = json!({"changed":changed,"cursor":cursor,"records":records,"evidence":evidence,"people":people,"candidates":candidates,"messages":messages});
        if op["labels"] == true {
            snapshot["labels"]=json!(sqlx::query_scalar::<_,Value>("SELECT jsonb_build_array(c.provider,n.external_account,c.source_external_id,c.conversation_type,c.profile_id,c.source_display_name,to_char(c.source_display_at AT TIME ZONE 'UTC','YYYY-MM-DD HH24:MI:SS')) FROM crm.conversations c JOIN crm.source_connections n ON n.id=c.connection_id ORDER BY c.provider,n.external_account,c.source_external_id").fetch_all(&pool).await?);
        }
        snapshots.push(snapshot);
    }
    println!("{}", serde_json::to_string(&snapshots)?);
    Ok(())
}
