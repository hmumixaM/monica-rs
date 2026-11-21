use monica_core::ai::{self, provider::Provider};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use uuid::Uuid;

struct Offline {
    calls: Mutex<Vec<(String, String, String)>>,
    important: Uuid,
    update: Uuid,
    person: Uuid,
    fail_next: AtomicBool,
}
impl Provider for Offline {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn complete<'a>(
        &'a self,
        job: &'a ai::Lease,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let text = job.input.to_string();
            assert!(
                text.len() < 128 * 1024,
                "one model request exceeded its bounded input"
            );
            self.calls
                .lock()
                .unwrap()
                .push((job.kind.clone(), job.model.clone(), text));
            if self.fail_next.swap(false, Ordering::SeqCst) {
                anyhow::bail!("synthetic-provider-error-must-not-be-persisted");
            }
            let content = if job.kind == "ai.week_chunk" {
                &job.input["content"]
            } else {
                &job.input["window_summary"]
            };
            let evidence: Vec<&Value> = content["sources"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(
                    content["parts"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .flat_map(|p| p["evidence"].as_array().into_iter().flatten()),
                )
                .chain(content["evidence"].as_array().into_iter().flatten())
                .collect();
            let mut ids: Vec<Value> = evidence
                .iter()
                .filter_map(|e| e["id"].as_str())
                .map(|id| json!(id))
                .collect();
            let mut events = Vec::new();
            for (id, kind) in [(self.important, "travel"), (self.update, "new_job")] {
                if ids.iter().any(|v| *v == id.to_string()) {
                    events.push(json!({"post_id":id,"profile_id":self.person,"kind":kind,"text":"Synthetic supported candidate"}));
                    ids.insert(0, json!(id));
                }
            }
            ids.truncate(16);
            Ok(
                json!({"summary":"Offline week summary","sources":ids,"life_events":events,"suggestions":[]}),
            )
        })
    }
}
async fn run(pool: &PgPool, provider: &Offline) {
    for _ in 0..1000 {
        ai::week_window::refresh_observations(pool).await.unwrap();
        ai::week_window::enqueue_pending(pool).await.unwrap();
        let before = provider.calls.lock().unwrap().len();
        if !ai::run_one(pool, provider).await.unwrap() {
            return;
        }
        assert!(
            provider.calls.lock().unwrap().len() - before <= 1,
            "more than one inference in a lane turn"
        );
    }
    panic!("bounded week work did not finish");
}

#[sqlx::test(migrations = "../../migrations")]
async fn whole_week_volume_privacy_citations_and_role_cache(pool: PgPool) {
    let person = Uuid::new_v4();
    let account = Uuid::new_v4();
    let conversation = Uuid::new_v4();
    let previous = Uuid::new_v4();
    let update = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Synthetic full week')").bind(person).execute(&pool).await.unwrap();
    sqlx::query(r#"UPDATE crm.ai_settings SET enabled=true,execution_enabled=true,provider='fake',include_messages=true,include_private_notes=true,model='default',model_routes='{"batch":"saved-batch","person":"saved-person","writer":"saved-writer"}'"#)
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.source_accounts(id,provider,external_id,object_kind,display_name) VALUES($1,'linkedin','week-author','person','Synthetic')").bind(account).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,source_account_id,profile_id,event_type,external_id,occurred_at,content,content_hash,version) SELECT gen_random_uuid(),'linkedin',gen_random_uuid(),$1,$2,'post',n::text,'2026-11-10T12:00:00Z',jsonb_build_object('text','PUBLIC-'||n),'synthetic',1 FROM generate_series(1,181) n")
        .bind(account).bind(person).execute(&pool).await.unwrap();
    let important: Uuid =
        sqlx::query_scalar("SELECT id FROM crm.timeline_events WHERE external_id='181'")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("INSERT INTO crm.conversations(id,provider,account,external_id,conversation_type,profile_id) VALUES($1,'wechat','self','friend','direct',$2)").bind(conversation).bind(person).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.message_search(message_id,conversation_id,provider,account,external_id,sender,direction,occurred_at,body) SELECT gen_random_uuid(),$1,'wechat','self',n::text,'friend','received','2026-11-10T13:00:00Z','CHAT-'||n||repeat('x',4300)||'CHAT-END' FROM generate_series(1,150) n")
        .bind(conversation).execute(&pool).await.unwrap();
    sqlx::query("WITH added AS (INSERT INTO crm.activities(id,activity_type,occurred_at,title,body,source_type,content_hash) SELECT gen_random_uuid(),'note','2026-11-10T14:00:00Z','Synthetic','NOTE-'||n||repeat('y',8400)||'NOTE-END','manual','note' FROM generate_series(1,45) n RETURNING id) INSERT INTO crm.activity_participants(activity_id,profile_id) SELECT id,$1 FROM added")
        .bind(person).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.field_revisions(id,profile_id,field_key,value,content_hash,source_type,operation,observed_at,previous_revision_id) VALUES($1,$3,'monica.company','\"Old\"','old','manual','set','2020-01-01',NULL),($2,$3,'monica.company','\"Updated field\"','changed','manual','set','2026-11-11T12:00:00Z',$1)")
        .bind(previous).bind(update).bind(person).execute(&pool).await.unwrap();
    sqlx::query("WITH added AS (INSERT INTO crm.media_assets(id,sha256,media_type,byte_length,object_path) SELECT gen_random_uuid(),lpad(n::text,64,'0'),'image/png',0,'synthetic-unused' FROM generate_series(1,4) n RETURNING id), readings AS (INSERT INTO crm.image_ocr_readings(media_id,status,text,model,generated_at) SELECT id,'complete',repeat('z',7980)||'SAVED-OCR-END','historical-vision',now() FROM added) INSERT INTO crm.media_links(entity_type,entity_id,media_id,role) SELECT 'social_post',p.id,a.id,'media-0' FROM (SELECT id FROM crm.timeline_events ORDER BY id LIMIT 20) p CROSS JOIN added a")
        .execute(&pool).await.unwrap();
    let provider = Offline {
        calls: Mutex::new(Vec::new()),
        fail_next: AtomicBool::new(false),
        important,
        update,
        person,
    };
    while ai::week_window::refresh_observations(&pool).await.unwrap() > 0 {}
    let subject = format!("{person}:2026-11-09");
    let first = ai::enqueue(&pool, "ai.person_week", &subject)
        .await
        .unwrap();
    run(&pool, &provider).await;
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM crm.ai_week_evidence WHERE job_id=$1")
            .bind(first)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 377); // 181 posts, 150 messages, 45 notes, one observed field update.
    let bytes: i64 = sqlx::query_scalar(
        "SELECT sum(octet_length(payload::text))::bigint FROM crm.ai_week_evidence WHERE job_id=$1",
    )
    .bind(first)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(bytes > 512 * 1024);
    let all = provider
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|(_, _, s)| s.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for marker in [
        "PUBLIC-181",
        "CHAT-150",
        "CHAT-END",
        "NOTE-45",
        "NOTE-END",
        "SAVED-OCR-END",
    ] {
        assert!(all.contains(marker), "missing {marker}");
    }
    assert!(
        provider
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|(k, m, _)| if k == "ai.week_chunk" {
                m == "saved-batch"
            } else {
                k == "ai.person_week" && m == "saved-person"
            })
    );
    let output: Value=sqlx::query_scalar("SELECT output FROM crm.person_weekly_updates WHERE profile_id=$1 AND week_start='2026-11-09'").bind(person).fetch_one(&pool).await.unwrap();
    assert_eq!(
        output["coverage"],
        json!({"complete":true,"processed":377,"sources":377})
    );
    let before = provider.calls.lock().unwrap().len();
    sqlx::query("UPDATE crm.ai_settings SET model_routes=jsonb_set(model_routes,'{person}','\"new-person\"')").execute(&pool).await.unwrap();
    ai::enqueue(&pool, "ai.person_week", &subject)
        .await
        .unwrap();
    run(&pool, &provider).await;
    {
        let calls = provider.calls.lock().unwrap();
        assert_eq!(
            calls.len() - before,
            1,
            "person-only model change must reuse batch parts"
        );
        assert_eq!(calls.last().unwrap().1, "new-person");
    }
    sqlx::query("UPDATE crm.ai_settings SET include_messages=false,include_private_notes=false")
        .execute(&pool)
        .await
        .unwrap();
    provider.calls.lock().unwrap().clear();
    ai::enqueue(&pool, "ai.digest", "2026-11-09").await.unwrap();
    run(&pool, &provider).await;
    {
        let calls = provider.calls.lock().unwrap();
        assert!(
            calls
                .iter()
                .all(|(_, _, s)| !s.contains("CHAT-") && !s.contains("NOTE-"))
        );
        assert_eq!(calls.last().unwrap().1, "saved-writer");
    }
    let digest: Value =
        sqlx::query_scalar("SELECT output FROM crm.weekly_digests WHERE week_start='2026-11-09'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(digest["coverage"]["sources"], 182);
    assert!(
        digest["life_events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["post_id"] != update.to_string())
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn retained_observation_dates_recovery_and_stale_window_retry(pool: PgPool) {
    use monica_core::{Provider as Source, Record};
    let person = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Observed source')").bind(person).execute(&pool).await.unwrap();
    sqlx::query("UPDATE crm.ai_settings SET enabled=true,execution_enabled=true,provider='fake',model_routes='{}'")
        .execute(&pool).await.unwrap();
    let account = monica_core::identity::account_id(Source::Linkedin, "observed-author").unwrap();
    for (object, observed) in [
        ("known-old", Some("2026-11-11T12:00:00Z")),
        ("unknown-old", None),
    ] {
        let record = Record {
            provider: Source::Linkedin,
            account: "observed-author".into(),
            object: object.into(),
            kind: "post".into(),
            event: "1".into(),
            payload: json!({"text":object}),
        };
        monica_core::ingest(&pool, &record).await.unwrap();
        let id = monica_core::identity::object_id(account, "post", object);
        if let Some(observed) = observed {
            sqlx::query("INSERT INTO linkedin.revision_metadata(object_id,version,occurred_at,observed_at) VALUES($1,1,'2020-01-02T12:00:00Z',$2::text::timestamptz)")
                .bind(id).bind(observed).execute(&pool).await.unwrap();
        }
    }
    while let Some(lease) = monica_core::claim(&pool).await.unwrap() {
        monica_core::project(&pool, &lease).await.unwrap();
    }
    sqlx::query("UPDATE crm.timeline_events SET occurred_at='2020-01-02T12:00:00Z' WHERE external_id='unknown-old'")
        .execute(&pool).await.unwrap();
    monica_core::linking::set_link(
        &pool,
        monica_core::linking::LinkChange {
            account_id: account,
            profile_id: Some(person),
            expected_profile_id: None,
        },
        None,
    )
    .await
    .unwrap();
    let important = monica_core::identity::object_id(account, "post", "known-old");
    let provider = Offline {
        calls: Mutex::new(Vec::new()),
        fail_next: AtomicBool::new(false),
        important,
        update: Uuid::nil(),
        person,
    };
    assert_eq!(
        ai::enqueue(&pool, "ai.digest", "2026-11-09")
            .await
            .unwrap_err()
            .to_string(),
        ai::week_window::CONTEXT_PENDING
    );
    let dispatch = ai::enqueue_dispatch(&pool, "ai.digest", "2026-11-16", false)
        .await
        .unwrap();
    assert!(ai::dispatch_one(&pool).await.unwrap());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT error_count FROM crm.ai_dispatches WHERE id=$1")
            .bind(dispatch)
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    while ai::week_window::refresh_observations(&pool).await.unwrap() > 0 {}
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM crm.ai_social_observed WHERE first_observed_at IS NOT NULL"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    ai::week_window::enqueue_pending(&pool).await.unwrap();
    let first = ai::claim(&pool).await.unwrap().unwrap();
    assert_eq!(first.input["window"]["source_count"], 1);
    assert!(
        ai::week_window::prepare(&pool, &provider, &first)
            .await
            .unwrap()
            .is_none()
    );
    let frozen: Value =
        sqlx::query_scalar("SELECT payload FROM crm.ai_week_evidence WHERE job_id=$1")
            .bind(first.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(frozen["discovered"], true);
    assert!(frozen["at"].as_str().unwrap().starts_with("2020-01-02"));
    assert!(
        frozen["discovered_at"]
            .as_str()
            .unwrap()
            .starts_with("2026-11-11")
    );
    let calls = provider.calls.lock().unwrap().len();
    sqlx::query(
        "UPDATE crm.ai_jobs SET lease_until=clock_timestamp()-interval '1 second' WHERE id=$1",
    )
    .bind(first.id)
    .execute(&pool)
    .await
    .unwrap();
    let recovered = ai::claim(&pool).await.unwrap().unwrap();
    assert_ne!(first.token, recovered.token);
    assert!(!ai::heartbeat(&pool, &first).await.unwrap());
    assert!(
        ai::week_window::prepare(&pool, &provider, &recovered)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        ai::week_window::prepare(&pool, &provider, &recovered)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        provider.calls.lock().unwrap().len(),
        calls,
        "recovery repeated an already cached model call"
    );
    sqlx::query("UPDATE crm.timeline_events SET content='{\"text\":\"Corrected old source\"}',version=2 WHERE id=$1")
        .bind(important).execute(&pool).await.unwrap();
    assert!(
        !ai::finish(
            &pool,
            &recovered,
            json!({"summary":"stale","life_events":[],"suggestions":[]})
        )
        .await
        .unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.ai_week_refresh")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    run(&pool, &provider).await;
    let published: Value =
        sqlx::query_scalar("SELECT output FROM crm.weekly_digests WHERE week_start='2026-11-09'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        published["life_events"].as_array().unwrap().is_empty(),
        "old content was presented as a new life event"
    );
    assert_eq!(published["coverage"]["sources"], 1);
    sqlx::query("UPDATE crm.ai_week_runs SET captured_at=now()-interval '91 days'")
        .execute(&pool)
        .await
        .unwrap();
    // Cleanup remains bounded to one terminal window per turn.
    for _ in 0..3 {
        ai::week_window::prune_completed(&pool).await.unwrap();
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.ai_week_evidence")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.timeline_events")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM crm.weekly_digests")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    // A newer published input may replace this output; retrying the original
    // unchanged input must rebuild a capture already pruned by retention.
    sqlx::query("UPDATE crm.ai_outputs SET input_hash='replacement-input' WHERE kind='ai.digest'")
        .execute(&pool)
        .await
        .unwrap();
    let replay = ai::enqueue(&pool, "ai.digest", "2026-11-09").await.unwrap();
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT pruned FROM crm.ai_week_runs WHERE job_id=$1")
            .bind(replay)
            .fetch_one(&pool)
            .await
            .unwrap()
    );
    run(&pool, &provider).await;
    let replayed: Value = sqlx::query_scalar("SELECT output FROM crm.ai_outputs WHERE job_id=$1")
        .bind(replay)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(replayed["coverage"]["sources"], 1);
    assert_eq!(replayed["coverage"]["processed"], 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn native_source_policy_contact_updates_and_chunk_failure_retry(pool: PgPool) {
    use monica_core::{Provider as Source, connections};
    let person = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name) VALUES($1,'person','Synthetic native week')").bind(person).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO crm.person_identifiers(profile_id,kind,value,confirmed) VALUES($1,'email','native@example.test',true)").bind(person).execute(&pool).await.unwrap();
    sqlx::query("UPDATE crm.ai_settings SET enabled=true,execution_enabled=true,provider='fake',include_messages=true")
        .execute(&pool).await.unwrap();
    let mut ids = Vec::new();
    for (provider, value) in [
        (
            Source::AppleContacts,
            json!({"external_id":"friend-contact","kind":"contact","emails":["native@example.test"],"organization":"GENUINE-NATIVE-COMPANY","participants":[{"id":"friend-contact","role":"contact","emails":["native@example.test"]}]}),
        ),
        (
            Source::AppleMessages,
            json!({"external_id":"native-message","kind":"message","thread_id":"friend","occurred_at":"2026-11-10T12:00:00Z","body":"PRIVATE-NATIVE".to_owned()+&"x".repeat(85000),"participants":[{"id":"friend-message","role":"sender","emails":["native@example.test"]},{"id":"me","role":"recipient","is_self":true}]}),
        ),
    ] {
        let cid = connections::connection_id(provider, "synthetic-week");
        sqlx::query("INSERT INTO crm.source_connections(id,provider,external_account,display_name,connection_method,enabled) VALUES($1,$2,'synthetic-week','Synthetic','local',true)").bind(cid).bind(provider.schema()).execute(&pool).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        connections::commit_page(&mut tx, provider, cid, "fixture", &[value], &json!({}))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        connections::deliver(&pool, provider, cid, 100)
            .await
            .unwrap();
        ids.push(cid);
    }
    sqlx::query(
        "UPDATE crm.field_revisions SET observed_at='2026-11-11T12:00:00Z' WHERE profile_id=$1",
    )
    .bind(person)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO crm.field_revisions(id,profile_id,field_key,value,content_hash,source_type,operation,observed_at) VALUES(gen_random_uuid(),$1,'monica.company','\"LEGACY-IMPORT-ONLY\"','legacy','manual','import','2026-11-11T12:00:00Z')").bind(person).execute(&pool).await.unwrap();
    let fields:Vec<Value>=sqlx::query_scalar("SELECT payload FROM crm.ai_week_sources('2026-11-09',$1,true,false) WHERE payload->>'kind'='profile'").bind(person).fetch_all(&pool).await.unwrap();
    assert_eq!(fields.len(), 2);
    assert!(
        fields
            .iter()
            .any(|f| f["field_key"] == "apple_contacts.organization"
                && f["text"] == "GENUINE-NATIVE-COMPANY")
    );
    assert!(!json!(fields).to_string().contains("LEGACY-IMPORT-ONLY"));
    let provider = Offline {
        calls: Mutex::new(Vec::new()),
        fail_next: AtomicBool::new(false),
        important: Uuid::nil(),
        update: Uuid::nil(),
        person,
    };
    // Disabled source data remains searchable, but must not be sent to AI.
    sqlx::query("UPDATE crm.source_connections SET enabled=false WHERE id=$1")
        .bind(ids[1])
        .execute(&pool)
        .await
        .unwrap();
    ai::enqueue(&pool, "ai.digest", "2026-11-09").await.unwrap();
    run(&pool, &provider).await;
    assert!(
        !provider
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, _, text)| text.contains("PRIVATE-NATIVE"))
    );
    sqlx::query("UPDATE crm.source_connections SET enabled=true WHERE id=$1")
        .bind(ids[1])
        .execute(&pool)
        .await
        .unwrap();
    let job = ai::enqueue(&pool, "ai.digest", "2026-11-09").await.unwrap();
    provider.calls.lock().unwrap().clear();
    assert!(ai::run_one(&pool, &provider).await.unwrap());
    let first = provider.calls.lock().unwrap()[0].2.clone();
    provider.fail_next.store(true, Ordering::SeqCst);
    assert!(ai::run_one(&pool, &provider).await.unwrap());
    let state: (String, Option<Uuid>, Option<String>) =
        sqlx::query_as("SELECT status,token,error FROM crm.ai_jobs WHERE id=$1")
            .bind(job)
            .fetch_one(&pool)
            .await
            .unwrap();
    // The failed call waits, unowned, for its retry a few minutes later.
    assert_eq!(state.0, "running");
    assert!(state.1.is_none());
    assert_eq!(
        state.2.as_deref(),
        Some("weekly evidence processing failed")
    );
    assert_eq!(
        ai::enqueue(&pool, "ai.digest", "2026-11-09").await.unwrap(),
        job
    );
    assert!(!ai::run_one(&pool, &provider).await.unwrap());
    sqlx::query(
        "UPDATE crm.ai_jobs SET lease_until=clock_timestamp()-interval '1 second' WHERE id=$1",
    )
    .bind(job)
    .execute(&pool)
    .await
    .unwrap();
    run(&pool, &provider).await;
    assert_eq!(
        provider
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, _, text)| text == &first)
            .count(),
        1,
        "retry lost the completed part cache"
    );
    let output: Value = sqlx::query_scalar("SELECT output FROM crm.ai_outputs WHERE job_id=$1")
        .bind(job)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(output["coverage"]["sources"], 3);
    // A source disabled after capture invalidates publication of that window.
    sqlx::query("UPDATE crm.message_search SET body=body||'new revision' WHERE account=$1")
        .bind(ids[1].to_string())
        .execute(&pool)
        .await
        .unwrap();
    ai::enqueue(&pool, "ai.digest", "2026-11-09").await.unwrap();
    let lease = ai::claim(&pool).await.unwrap().unwrap();
    while ai::week_window::prepare(&pool, &provider, &lease)
        .await
        .unwrap()
        .is_none()
    {}
    sqlx::query("UPDATE crm.source_connections SET enabled=false WHERE id=$1")
        .bind(ids[1])
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !ai::finish(
            &pool,
            &lease,
            json!({"summary":"must not publish","life_events":[],"suggestions":[]})
        )
        .await
        .unwrap()
    );
    let before = provider.calls.lock().unwrap().len();
    assert!(
        ai::week_window::prepare(&pool, &provider, &lease)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        provider.calls.lock().unwrap().len(),
        before,
        "source disabled after capture triggered another provider call"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn digest_keeps_supported_post_only_suggestions_without_final_source_array(pool: PgPool) {
    let person = Uuid::new_v4();
    let post = Uuid::new_v4();
    sqlx::query("INSERT INTO crm.profiles(id,profile_type,display_name,created_at) VALUES($1,'person','Established post-only contact','2020-01-01')")
        .bind(person).execute(&pool).await.unwrap();
    sqlx::query("UPDATE crm.ai_settings SET enabled=true,execution_enabled=true,provider='fake'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO crm.timeline_events(id,provider,object_id,profile_id,event_type,external_id,occurred_at,content,content_hash,version) VALUES($1,'linkedin',$1,$2,'post','ordinary','2026-11-10T12:00:00Z','{\"text\":\"Ordinary update\"}','fixture',1)")
        .bind(post).bind(person).execute(&pool).await.unwrap();
    while ai::week_window::refresh_observations(&pool).await.unwrap() > 0 {}
    ai::enqueue(&pool, "ai.digest", "2026-11-09").await.unwrap();
    let lease = ai::claim(&pool).await.unwrap().unwrap();
    for key in [
        "top_contacts",
        "new_people",
        "new_links",
        "upcoming_dates",
        "newly_dormant",
    ] {
        assert!(
            lease.input["facts"][key].as_array().unwrap().is_empty(),
            "fixture unexpectedly qualifies via weekly facts"
        );
    }
    let provider = ai::provider::FakeProvider {
        output: json!({"summary":"Retained ordinary update","sources":[post],"life_events":[],"suggestions":[]}),
    };
    while ai::week_window::prepare(&pool, &provider, &lease)
        .await
        .unwrap()
        .is_none()
    {}
    // Compliant final digest contract deliberately omits a sources field.
    assert!(ai::finish(&pool,&lease,json!({"summary":"Weekly summary","life_events":[],"suggestions":[{"profile_id":person,"reason":"Acknowledge their update"},{"profile_id":Uuid::new_v4(),"reason":"Unsupported stranger"}]})).await.unwrap());
    let output: Value =
        sqlx::query_scalar("SELECT output FROM crm.weekly_digests WHERE week_start='2026-11-09'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        output["suggestions"],
        json!([{"profile_id":person,"reason":"Acknowledge their update"}])
    );
    assert_eq!(output["coverage"]["sources"], 1);
}
