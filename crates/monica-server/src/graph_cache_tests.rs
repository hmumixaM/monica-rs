use super::*;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use tower::ServiceExt;

async fn request(
    app: &Router,
    path: &str,
    etag: Option<&str>,
    authenticated: bool,
) -> axum::response::Response {
    let mut request = Request::builder()
        .uri(path)
        .header("host", "localhost:17494");
    if authenticated {
        request = request.header("cookie", "crm_session=graph-cache-session");
    }
    if let Some(etag) = etag {
        request = request.header("if-none-match", etag);
    }
    app.clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}
async fn graph(app: &Router, path: &str) -> (String, Value) {
    let response = request(app, path, None, true).await;
    assert_eq!(response.status(), StatusCode::OK);
    let etag = response.headers()["etag"].to_str().unwrap().to_owned();
    let value =
        serde_json::from_slice(&to_bytes(response.into_body(), 2_000_000).await.unwrap()).unwrap();
    (etag, value)
}

#[sqlx::test(migrations = "../../migrations")]
async fn relationship_cache_reuses_results_and_tracks_committed_facts(pool: sqlx::PgPool) {
    let (state, actor, author, _root) = super::tests::fixture(&pool).await;
    let owner = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Peer')",
    )
    .bind(owner)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 hour')").bind(Uuid::new_v4()).bind(actor).bind(monica_core::identity::bytes_hash(b"graph-cache-session")).execute(&pool).await.unwrap();
    for (id, external) in [(author, "wxid_author"), (owner, "wxid_peer")] {
        sqlx::query("INSERT INTO crm.identities(id,profile_id,provider,external_id,status) VALUES($1,$2,'wechat',$3,'active')").bind(Uuid::new_v4()).bind(id).bind(external).execute(&pool).await.unwrap();
    }
    let relationship = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.relationship_edges(id,from_profile_id,to_profile_id,relationship_type,source_type,confidence) VALUES($1,$2,$3,'friend','manual',0.8)").bind(relationship).bind(author).bind(owner).execute(&pool).await.unwrap();
    let app = crate::auth::routes(state);
    let path = "/api/v1/relationships/graph?format=monica";
    let (first_tag, first) = graph(&app, path).await;
    assert_eq!(first["edges"].as_array().unwrap().len(), 1);
    assert_eq!(
        graph(&app, path).await,
        (first_tag.clone(), first.clone()),
        "unchanged graph reuses the complete result, including build time"
    );
    assert_eq!(
        request(&app, path, Some(&first_tag), true).await.status(),
        StatusCode::NOT_MODIFIED
    );
    assert_eq!(
        request(&app, path, Some(&first_tag), false).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let (_, focused) = graph(&app, &format!("{path}&focus={author}&limit=50")).await;
    assert_eq!(focused["scope"]["focus"], author.to_string());
    assert_eq!(focused["scope"]["limit"], 50);
    let (_, filtered) = graph(&app, "/api/v1/relationships/graph?source_type=linkedin").await;
    assert_eq!(filtered["total_edges"], 0);
    let (_, unfiltered) = graph(&app, "/api/v1/relationships/graph").await;
    assert_eq!(unfiltered["total_edges"], 1);

    sqlx::query("UPDATE crm.profiles SET display_name='Renamed author' WHERE id=$1")
        .bind(author)
        .execute(&pool)
        .await
        .unwrap();
    let (renamed_tag, renamed) = graph(&app, path).await;
    assert_ne!(renamed_tag, first_tag);
    assert!(
        renamed["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["name"] == "Renamed author")
    );
    let community = renamed["communities"][0]["key"].as_str().unwrap();
    sqlx::query("INSERT INTO crm.community_names(community_id,label) VALUES($1,'Friends')")
        .bind(community)
        .execute(&pool)
        .await
        .unwrap();
    let (named_tag, named) = graph(&app, path).await;
    assert_ne!(named_tag, renamed_tag);
    assert_eq!(named["communities"][0]["label"], "Friends");

    // An externally resolved mention has no interaction_months projection.
    // It must still invalidate the graph as soon as its transaction commits.
    let post = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,profile_id,event_type,external_id,content,content_hash,version) VALUES($1,'wechat',$1,$2,'post','cache-post','{}','synthetic',1)").bind(post).bind(owner).execute(&pool).await.unwrap();
    let interaction = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.social_interactions(id,post_id,provider,external_id,interaction_type,author_external_id,occurred_at,content) VALUES($1,$2,'wechat','cache-mention','mention','wxid_author','2026-01-01','{\"target_external_id\":\"wxid_peer\"}')").bind(interaction).bind(post).execute(&pool).await.unwrap();
    let (mention_tag, mention) = graph(&app, path).await;
    assert_ne!(mention_tag, named_tag);
    assert_eq!(mention["edges"][0]["direct"], 2);
    sqlx::query("UPDATE crm.social_interactions SET occurred_at='2026-01-02' WHERE id=$1")
        .bind(interaction)
        .execute(&pool)
        .await
        .unwrap();
    let (updated_tag, updated) = graph(&app, path).await;
    assert_ne!(updated_tag, mention_tag);
    assert_eq!(updated["edges"][0]["last"], "2026-01-02");
    sqlx::query("DELETE FROM crm.social_interactions WHERE id=$1")
        .bind(interaction)
        .execute(&pool)
        .await
        .unwrap();
    let (_, deleted) = graph(&app, path).await;
    assert_eq!(deleted["edges"][0]["direct"], 1);
    sqlx::query("UPDATE crm.relationship_edges SET relationship_type='colleague' WHERE id=$1")
        .bind(relationship)
        .execute(&pool)
        .await
        .unwrap();
    let (_, changed) = graph(&app, path).await;
    assert_eq!(
        changed["edges"][0]["relationship_types"],
        json!(["colleague"])
    );
    sqlx::query("UPDATE crm.profiles SET archived_at=now() WHERE id=$1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    let (_, archived) = graph(&app, path).await;
    assert!(archived["edges"].as_array().unwrap().is_empty());
}
