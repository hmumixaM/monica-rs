//! Embedded social interactions retain the connection's explicit identity map.
use super::*;
pub(super) async fn project_interactions(
    tx: &mut Transaction<'_, Postgres>,
    cid: Uuid,
    event: Uuid,
    provider: Provider,
    value: &Value,
) -> Result<()> {
    for item in value["interactions"].as_array().into_iter().flatten() {
        let external = item["external_id"]
            .as_str()
            .context("interaction requires stable identity")?;
        let kind = item["interaction_type"].as_str().unwrap_or("comment");
        anyhow::ensure!(
            ["like", "comment", "reply", "mention", "reaction"].contains(&kind),
            "invalid interaction type"
        );
        let id = identity::object_id(event, "interaction", external);
        let author = item["author_external_id"].as_str();
        let (account,profile):(Option<Uuid>,Option<Uuid>)=sqlx::query_as("SELECT a.id,CASE WHEN p.external_id IS NOT NULL THEN CASE WHEN m.blocked THEN NULL WHEN m.confirmed THEN m.profile_id ELSE p.resolved_profile_id END ELSE l.profile_id END FROM (SELECT $1::text external_id) wanted LEFT JOIN crm.connection_people p ON p.connection_id=$2 AND p.external_id=wanted.external_id LEFT JOIN crm.connection_identity_links m ON m.connection_id=p.connection_id AND m.external_id=p.external_id LEFT JOIN crm.source_accounts a ON a.provider=$3 AND a.external_id=wanted.external_id LEFT JOIN crm.source_account_links l ON l.account_id=a.id")
            .bind(author).bind(cid).bind(provider.schema()).fetch_one(&mut **tx).await?;
        let mut metadata = item
            .get("metadata")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or(json!({}));
        for key in [
            "target_external_id",
            "ref_author_external_id",
            "ref_external_id",
            "media",
        ] {
            if let Some(v) = item.get(key) {
                metadata[key] = v.clone();
            }
        }
        sqlx::query("INSERT INTO crm.social_interactions(id,post_id,provider,external_id,interaction_type,author_account_id,author_profile_id,author_external_id,author_name,occurred_at,content,metadata,is_deleted) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10::text::timestamptz,$11,$12,$13) ON CONFLICT(id) DO UPDATE SET interaction_type=excluded.interaction_type,author_account_id=excluded.author_account_id,author_profile_id=excluded.author_profile_id,author_external_id=excluded.author_external_id,author_name=excluded.author_name,occurred_at=excluded.occurred_at,content=excluded.content,metadata=excluded.metadata,is_deleted=excluded.is_deleted")
            .bind(id).bind(event).bind(provider.schema()).bind(external).bind(kind).bind(account).bind(profile).bind(author)
            .bind(item["author_name"].as_str()).bind(item["occurred_at"].as_str()).bind(item).bind(metadata)
            .bind(truth(item.get("is_deleted"),false)||truth(value.get("deleted"),false)).execute(&mut **tx).await?;
    }
    Ok(())
}
