use super::*;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use tower::ServiceExt;

#[sqlx::test(migrations = false)]
async fn retained_contacts_backfill_directory_search_graph_and_preserve_history(
    pool: sqlx::PgPool,
) {
    // Exercise the upgrade with already-imported contacts, not just a fresh
    // database whose insert triggers happen to classify a synthetic row.
    let migrations = sqlx::migrate!("../../migrations");
    for migration in migrations.iter().filter(|m| m.version < 78) {
        sqlx::raw_sql(&migration.sql).execute(&pool).await.unwrap();
    }
    let names = [
        "Synthetic official",
        "Synthetic enterprise",
        "Synthetic person",
        "Synthetic archived",
        "Synthetic mixed",
        "Synthetic manual",
        "Synthetic identity only",
        "Synthetic native",
    ];
    let ids: Vec<Uuid> = names.iter().map(|_| Uuid::new_v4()).collect();
    let externals = [
        "wxid_synthetic_publisher",
        "synthetic@openim",
        "wxid_synthetic_person",
        "archived@openim",
        "wxid_synthetic_mixed",
        "wxid_synthetic_manual",
        "gh_identity_only",
        "wxid_synthetic_native",
    ];
    for (i, name) in names.iter().enumerate() {
        sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name,archived_at) VALUES($1,'person',$2,CASE WHEN $3 THEN now() END)")
            .bind(ids[i]).bind(name).bind(i==3).execute(&pool).await.unwrap();
        if i < 6 {
            let account = Uuid::new_v4();
            sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'wechat',$2,'person',$2)").bind(account).bind(externals[i]).execute(&pool).await.unwrap();
            sqlx::query(
                "INSERT INTO crm.source_account_links(account_id,profile_id) VALUES($1,$2)",
            )
            .bind(account)
            .bind(ids[i])
            .execute(&pool)
            .await
            .unwrap();
        }
    }
    // Migrated notebook contacts carry a Monica provenance account in addition
    // to their real source identity. It must not count as a second platform
    // that turns an official WeChat account back into a person.
    sqlx::query("INSERT INTO crm.identities(id,profile_id,provider,external_id,status) VALUES($1,$2,'monica','contact:synthetic-official','active')")
        .bind(Uuid::new_v4()).bind(ids[0]).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.identities(id,profile_id,provider,external_id,status) VALUES($1,$2,'monica','contact:synthetic-native','active')")
        .bind(Uuid::new_v4()).bind(ids[7]).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.identities(id,profile_id,provider,external_id,status) VALUES($1,$2,'wechat',$3,'active')").bind(Uuid::new_v4()).bind(ids[6]).bind(externals[6]).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.identities(id,profile_id,provider,external_id,status) VALUES($1,$2,'linkedin','synthetic-real-person','active')").bind(Uuid::new_v4()).bind(ids[4]).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.audit_events(action,entity_type,entity_id) VALUES('profile.created','profile',$1)").bind(ids[5]).execute(&pool).await.unwrap();
    let cid:Uuid=sqlx::query_scalar("SELECT id FROM crm.source_connections WHERE provider='wechat' AND external_account='legacy-default'").fetch_one(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.connection_people(connection_id,external_id,display_name,resolved_profile_id) VALUES($1,$2,'Synthetic native',$3)").bind(cid).bind(externals[7]).bind(ids[7]).execute(&pool).await.unwrap();
    let native = json!({"kind":"contact","raw_contact":{"verify_flag":24},"participants":[{"id":externals[7],"role":"contact"}]});
    let hash = monica_core::identity::observation_hash(&native);
    sqlx::query("INSERT INTO wechat.connection_payloads(content_hash,payload) VALUES($1,$2)")
        .bind(&hash)
        .bind(&native)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO wechat.connection_revision_refs(connection_id,external_id,revision,content_hash,observed_at) VALUES($1,'native-contact',1,$2,'2026-01-01')").bind(cid).bind(&hash).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO wechat.connection_record_heads(connection_id,external_id,revision,content_hash) VALUES($1,'native-contact',1,$2)").bind(cid).bind(&hash).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.connection_evidence(id,connection_id,external_id,revision,content_hash,kind,included,metadata) VALUES($1,$2,'native-contact',1,$3,'contact',true,$4)").bind(Uuid::new_v4()).bind(cid).bind(&hash).bind(&native).execute(&pool).await.unwrap();
    let snapshot = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.legacy_chatlog_snapshots(id,manifest,own_id,created_at) VALUES($1,'{}','synthetic-own','2026-01-01')").bind(snapshot).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.legacy_chatlog_tables(snapshot_id,file_name,table_name,mode,expected,reason) VALUES($1,'contact/contact.db','contact','history',7,'synthetic')").bind(snapshot).execute(&pool).await.unwrap();
    // The native contact has no Chatlog/source-field fallback: migration must
    // actually read its already-projected native receipt.
    for (i, external) in externals.iter().take(7).enumerate() {
        let flag = if [0, 4, 5, 7].contains(&i) { 24 } else { 0 };
        sqlx::query("INSERT INTO crm.legacy_chatlog_history VALUES($1,'contact/contact.db','contact',$2,$3)")
            .bind(snapshot).bind(i as i64).bind(json!({"username":external,"verify_flag":flag,"local_type":if i==1 {5}else{1}})).execute(&pool).await.unwrap();
    }
    let edge = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.relationship_edges(id,from_profile_id,to_profile_id,relationship_type) VALUES($1,$2,$3,'synthetic')").bind(edge).bind(ids[0]).bind(ids[2]).execute(&pool).await.unwrap();
    for migration in migrations.iter().filter(|m| m.version >= 78) {
        sqlx::raw_sql(&migration.sql).execute(&pool).await.unwrap();
    }
    let kinds:Vec<Option<String>>=sqlx::query_scalar("SELECT crm.wechat_contact_kind(external,payload) FROM (VALUES ('wxid_x','{\"verify_flag\":8}'::jsonb),('wxid_x','{\"verify_flag\":56}'),('wxid_x','{\"verify_flag\":5}'),('service@openim','{\"verify_flag\":24}'),('wxid_x','{\"verify_flag\":\"bad\"}'),('wxid_x','{\"verify_flag\":9999999999999999999999999999}'),('wxid_x','{}'))x(external,payload)").fetch_all(&pool).await.unwrap();
    assert_eq!(
        kinds,
        vec![
            Some("official".into()),
            Some("official".into()),
            Some("person".into()),
            Some("enterprise".into()),
            None,
            None,
            None
        ]
    );
    let visible: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM crm.directory_profiles ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    let mut expected = vec![ids[1], ids[2], ids[4], ids[5]];
    expected.sort();
    assert_eq!(
        visible, expected,
        "official-only profiles are hidden, enterprise/mixed/manual contacts survive, archived decisions stay archived"
    );
    let (raw_profiles,raw_contacts,raw_edges):(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM crm.profiles),(SELECT count(*) FROM crm.legacy_chatlog_history),(SELECT count(*) FROM crm.relationship_edges)").fetch_one(&pool).await.unwrap();
    assert_eq!((raw_profiles, raw_contacts, raw_edges), (8, 7, 1));
    let retained_native:Value=sqlx::query_scalar("SELECT payload FROM wechat.connection_records WHERE connection_id=$1 AND external_id='native-contact'").bind(cid).fetch_one(&pool).await.unwrap();
    assert_eq!(retained_native, native);
    // Matching must distinguish a genuinely unlinked account from a link to a
    // profile omitted by the directory view. Both have the same candidate name.
    sqlx::query(
        "UPDATE crm.source_accounts SET display_name='Synthetic person' WHERE external_id=ANY($1)",
    )
    .bind(vec![externals[2], externals[3]])
    .execute(&pool)
    .await
    .unwrap();
    let unlinked = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'instagram','synthetic-unlinked','person','Synthetic person')").bind(unlinked).execute(&pool).await.unwrap();
    let matching = monica_core::link_matching::load(&pool).await.unwrap();
    assert!(
        !matching
            .accounts
            .iter()
            .any(|a| a.profile_id.as_deref() == Some(&ids[3].to_string()))
    );
    assert!(
        matching
            .accounts
            .iter()
            .any(|a| a.account_id == unlinked.to_string() && a.profile_id.is_none())
    );
    assert!(
        !json!(monica_core::link_matching::queue(&matching))
            .to_string()
            .contains(&ids[3].to_string())
    );

    let (s, actor, extra, _root) = super::tests::fixture(&pool).await;
    sqlx::query("INSERT INTO crm.sessions(id,user_id,token_hash,expires_at) VALUES($1,$2,$3,now()+interval '1 hour')").bind(Uuid::new_v4()).bind(actor).bind(monica_core::identity::bytes_hash(b"directory-policy-session")).execute(&pool).await.unwrap();
    let app = crate::auth::routes(s);
    async fn get(app: &Router, path: &str) -> Value {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("host", "localhost:17494")
                    .header("cookie", "crm_session=directory-policy-session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        serde_json::from_slice(
            &to_bytes(response.into_body(), 2 * 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap()
    }
    let profiles = get(&app, "/api/v1/profiles").await;
    let ids_returned: Vec<Uuid> = profiles
        .as_array()
        .unwrap()
        .iter()
        .map(|p| Uuid::parse_str(p["id"].as_str().unwrap()).unwrap())
        .collect();
    assert!(ids_returned.contains(&ids[1]));
    assert!(!ids_returned.contains(&ids[0]));
    assert!(ids_returned.contains(&extra));
    let directory = get(&app, "/api/v1/directory").await;
    assert_eq!(directory["profiles"].as_array().unwrap().len(), 5);
    // Date reads retain directory policy and multiple reminders per person.
    for (owner, days, kind, precision) in [
        (ids[0], 5, "birthday", "exact"),
        (ids[1], 10, "birthday", "exact"),
        (ids[1], 20, "anniversary", "month_day"),
        (ids[5], 30, "other", "approximate"),
        (ids[2], 61, "birthday", "month_day"),
        (ids[3], 5, "birthday", "exact"),
    ] {
        sqlx::query("INSERT INTO crm.profile_dates(id,profile_id,kind,label,date_precision,year,month,day) SELECT $1,$2,$3,'Synthetic reminder',$4,CASE WHEN $4='exact' THEN 2000 END,extract(month from d.reminder_date)::int,extract(day from d.reminder_date)::int FROM(SELECT (now() AT TIME ZONE 'UTC')::date+$5::int AS reminder_date) d")
            .bind(Uuid::new_v4()).bind(owner).bind(kind).bind(precision).bind(days)
            .execute(&pool).await.unwrap();
    }
    let insights = get(&app, "/api/v1/insights?dormant_days=90").await;
    assert_eq!(insights["dormant_days"], 90);
    let reminders = insights["upcoming_dates"].as_array().unwrap();
    assert_eq!(reminders.len(), 3);
    assert_eq!(reminders[0]["profile_id"], ids[1].to_string());
    assert_eq!(reminders[0]["kind"], "birthday");
    assert_eq!(reminders[0]["days_until"], 10);
    let next_year: i32 = reminders[0]["date"].as_str().unwrap()[..4].parse().unwrap();
    assert_eq!(reminders[0]["years"], next_year - 2000);
    assert_eq!(reminders[1]["profile_id"], ids[1].to_string());
    assert_eq!(reminders[1]["kind"], "anniversary");
    assert!(reminders[1]["years"].is_null());
    assert_eq!(reminders[2]["profile_id"], ids[5].to_string());
    assert_eq!(reminders[2]["label"], "Synthetic reminder");
    assert!(reminders[2]["years"].is_null());

    get(&app, "/api/v1/insights/calendar/day?day=2026-01-01").await;
    get(&app, "/api/v1/insights/orbit").await;
    let search = get(&app, "/api/v1/search?q=Synthetic%20official").await;
    assert!(search.as_array().unwrap().is_empty());
    let graph = get(&app, "/api/v1/relationships/graph").await;
    assert_eq!(graph["total_edges"], 0);
    assert_eq!(graph["total_nodes"], 5);
    let monica_graph = get(&app, "/api/v1/relationships/graph?format=monica").await;
    assert!(!monica_graph.to_string().contains("Synthetic official"));
    assert_eq!(get(&app, "/api/v1/metrics/summary").await["people"], 5);
    assert!(
        !get(&app, "/api/v1/home")
            .await
            .to_string()
            .contains("Synthetic official")
    );
    // Exact profile and historical evidence remain accessible for inspection.
    assert_eq!(
        get(&app, &format!("/api/v1/profiles/{}", ids[0])).await["id"],
        ids[0].to_string()
    );
    // A later correction changes presentation and invalidates cached directory;
    // stale replay must not reinstate the old classification.
    sqlx::query("SELECT crm.observe_wechat_contact($1,'{\"verify_flag\":0}','2026-02-01','synthetic-correction')").bind(externals[0]).execute(&pool).await.unwrap();
    sqlx::query(
        "SELECT crm.observe_wechat_contact($1,'{\"verify_flag\":24}','2026-01-01','stale-replay')",
    )
    .bind(externals[0])
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        get(&app, "/api/v1/directory").await["profiles"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
    assert!(
        get(&app, "/api/v1/insights?dormant_days=90").await["upcoming_dates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["profile_id"] == ids[0].to_string())
    );
}
